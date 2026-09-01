"""LLM 驱动的对话记忆提取器（dt-memory v3）。

从最近对话中自动提炼"值得记住"的事实/决策/偏好/约定，不依赖用户
显式说"记住"。复用 Hermes 的 LLM 配置（config.yaml model 段 + env key），
与主 agent 使用同一模型端点，避免重复配置。

输出：结构化 JSON 列表
  [{"type": "fact|decision|preference|convention",
    "summary": "一句话摘要（会成为 KG 节点 name/title）",
    "detail": "详细内容（会成为 content，建议带文件路径/位置便于定位）",
    "tags": ["..."],
    "importance": 1-5}]
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from typing import Any, Dict, List, Optional

import yaml


def _get_secret_env(name: str) -> str:
    """读取密钥环境变量，与 Hermes 运行环境保持一致。

    解析顺序（2026-09-01 修复：dt-memory 后台线程跑在 gateway 进程里，
    裸 os.environ 看不到 Hermes 从 ~/.hermes/.env 加载的密钥）：
    1. Hermes agent.secret_scope.get_secret() —— 与主 agent 同源的密钥解析
       （含 profile scope / .env overlay），最准确
    2. 直接解析 HERMES_HOME/.env（fallback，import 失败或 scope 不可用时）
    3. os.environ（最后兜底）
    """
    # 1. Hermes secret_scope
    try:
        from agent.secret_scope import get_secret as _scoped_get
        val = _scoped_get(name)
        if val:
            return val
    except Exception:
        pass
    # 2. HERMES_HOME/.env 文件
    try:
        home = (
            os.environ.get("HERMES_HOME")
            or os.path.expanduser("~/.hermes")
        )
        env_path = os.path.join(home, ".env")
        if os.path.isfile(env_path):
            with open(env_path, encoding="utf-8") as f:
                for raw in f:
                    line = raw.strip()
                    if not line or line.startswith("#") or "=" not in line:
                        continue
                    k, _, v = line.partition("=")
                    k = k.strip().strip('"').strip("'")
                    v = v.strip().strip('"').strip("'")
                    if k == name:
                        return v
    except Exception:
        pass
    # 3. 进程环境
    return os.environ.get(name, "")


def load_hermes_llm_config(hermes_home: str) -> Optional[Dict[str, str]]:
    """从 Hermes config.yaml 读取 LLM 端点配置。

    返回 {base_url, model, api_key, api_mode}；失败返回 None（调用方降级为不提取）。
    model 优先取配置 default；若为 'auto' 则调用 /models 发现真实模型 id。

    降级链（2026-09-01 修复）：
    1. config.yaml providers 表（自定义 provider，如 my-newapi）
    2. Hermes 内置 PROVIDER_REGISTRY（hermes_cli.auth）—— 解决
       model.provider 指向内置 provider（如 deepseek）但 config providers 表
       无对应条目时，插件拿不到 base_url/key_env 而静默失效的问题。
    """
    cfg_path = os.path.join(hermes_home, "config.yaml")
    try:
        with open(cfg_path, encoding="utf-8") as f:
            cfg = yaml.safe_load(f) or {}
    except Exception:
        return None

    model_cfg = cfg.get("model", {}) or {}
    provider_name = model_cfg.get("provider", "") or ""
    providers = cfg.get("providers", {}) or {}
    prov_cfg = providers.get(provider_name, {}) if provider_name else {}

    base_url = model_cfg.get("base_url") or prov_cfg.get("api") or prov_cfg.get("base_url")
    model = model_cfg.get("default") or prov_cfg.get("model") or "auto"
    key_env = prov_cfg.get("key_env") or model_cfg.get("key_env") or ""
    api_key = _get_secret_env(key_env) if key_env else ""

    # 降级：config providers 表没有该 provider → 查 Hermes 内置注册表
    if (not base_url or not api_key) and provider_name:
        try:
            from hermes_cli.auth import PROVIDER_REGISTRY
            reg = PROVIDER_REGISTRY.get(provider_name)
            if reg:
                if not base_url:
                    base_url = reg.inference_base_url
                if not key_env and reg.api_key_env_vars:
                    key_env = reg.api_key_env_vars[0]
                    api_key = _get_secret_env(key_env) or api_key
                # 兼容 DEEPSEEK_BASE_URL 这类 base_url 覆盖环境变量
                if reg.base_url_env_var and os.environ.get(reg.base_url_env_var):
                    base_url = os.environ[reg.base_url_env_var]
        except Exception:
            pass

    api_mode = model_cfg.get("api_mode", "chat_completions")

    if not base_url or not api_key:
        return None
    cfg_out = {
        "base_url": str(base_url).rstrip("/"),
        "model": str(model),
        "api_key": api_key,
        "api_mode": str(api_mode),
    }
    # model=auto → 发现真实模型
    if str(model).lower() in ("auto", "default"):
        discovered = _discover_model(cfg_out)
        if discovered:
            cfg_out["model"] = discovered
    return cfg_out


def _discover_model(cfg: Dict[str, str], timeout: float = 10.0) -> Optional[str]:
    """查询 /models 端点发现第一个可用模型 id。失败返回 None。"""
    import urllib.request

    url = cfg["base_url"] + "/models"
    req = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {cfg['api_key']}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception:
        return None
    ids = [m.get("id") for m in data.get("data", []) if isinstance(m, dict) and m.get("id")]
    return ids[0] if ids else None


def extract_memories_from_conversation(
    messages: List[Dict[str, Any]],
    *,
    hermes_home: str = "",
    timeout: float = 30.0,
    llm_cfg: Optional[Dict[str, str]] = None,
) -> List[Dict[str, Any]]:
    """用 LLM 从对话中提炼记忆条目。

    messages: OpenAI 风格 [{role, content}]，至少含 2 条 user 消息才值得提。
    返回归一化的记忆条目列表（可能为空）。
    失败（LLM 不可用/超时/解析失败）返回 []，绝不抛异常 — 记忆提取是尽力而为。
    """
    if not llm_cfg:
        llm_cfg = load_hermes_llm_config(hermes_home)
    if not llm_cfg:
        return []

    # 只取最近的对话（避免长会话 token 爆炸）：最多 20 条消息
    recent = messages[-20:] if len(messages) > 20 else messages
    user_msgs = [m for m in recent if m.get("role") == "user"]
    if len(user_msgs) < 2:
        return []  # 一轮对话不值得提炼

    # 压缩消息为纯文本（截断超长内容）
    transcript = []
    for m in recent:
        role = m.get("role", "")
        content = m.get("content", "")
        if not isinstance(content, str) or not content.strip():
            continue
        content = content[:800]
        transcript.append(f"[{role}] {content}")
    transcript_text = "\n".join(transcript)[:8000]

    prompt = f"""你是长期记忆整理器。从下面的对话中提取"值得长期记住"的信息。

值得记住的类别：
- fact: 客观事实（环境地址、账号、版本、命令、流程步骤）
- decision: 决策（选型、架构取舍、为什么）
- preference: 用户偏好（风格、习惯、工具选择）
- convention: 约定（命名规范、提交规范、代码约定）

要求：
1. 只提取真正长期有价值的内容；寒暄、一次性问题、代码细节不要记
2. 每条一个 summary（≤30字，作为标题）+ detail（完整上下文）
3. detail 内注明文件路径/位置（如 `文件: /data/.../bootstrap.yml`），便于检索后定位实际位置（记忆统一全局，不分项目/全局）
4. importance 1-5，5 最高；低于 3 的不要输出
5. 严格输出 JSON 数组，不要任何其他文字

对话：
{transcript_text}
"""

    payload = {
        "model": llm_cfg["model"],
        "messages": [
            {"role": "system", "content": "You are a memory curator. Output ONLY valid JSON."},
            {"role": "user", "content": prompt},
        ],
        "temperature": 0.2,
        "max_tokens": 1500,
    }

    try:
        resp = _chat_completion(llm_cfg, payload, timeout)
        if not resp:
            return []
        entries = _parse_json_response(resp)
    except Exception:
        return []

    # 归一化 + 过滤低重要度
    normalized = []
    for e in entries:
        if not isinstance(e, dict):
            continue
        summary = str(e.get("summary", "")).strip()
        if not summary:
            continue
        try:
            importance = int(e.get("importance", 3))
        except (TypeError, ValueError):
            importance = 3
        if importance < 3:
            continue
        etype = str(e.get("type", "fact")).strip().lower()
        if etype not in ("fact", "decision", "preference", "convention"):
            etype = "fact"
        # 记忆统一全局（2026-09-01）：不分项目/全局，全部按全局写入
        scope = "global"
        tags = e.get("tags", [])
        if not isinstance(tags, list):
            tags = []
        tags = [str(t).strip() for t in tags if str(t).strip()][:5]
        normalized.append({
            "type": etype,
            "summary": summary[:60],
            "detail": str(e.get("detail", ""))[:1000],
            "scope": scope,
            "importance": importance,
            "tags": tags,
        })
    return normalized


def _chat_completion(
    llm_cfg: Dict[str, str], payload: Dict[str, Any], timeout: float
) -> Optional[str]:
    """调用 chat_completions API，返回助手文本。失败返回 None。"""
    url = llm_cfg["base_url"] + "/chat/completions"
    import urllib.request

    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {llm_cfg['api_key']}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception:
        return None
    try:
        return data["choices"][0]["message"]["content"]
    except (KeyError, IndexError, TypeError):
        return None


def _parse_json_response(text: str) -> List[Dict[str, Any]]:
    """从 LLM 输出解析 JSON 数组，容忍 markdown 围栏。"""
    if not text:
        return []
    # 去掉 ```json ... ``` 围栏
    m = re.search(r"```(?:json)?\s*(.*?)\s*```", text, re.DOTALL)
    if m:
        text = m.group(1)
    text = text.strip()
    # 直接解析
    try:
        data = json.loads(text)
        if isinstance(data, list):
            return data
        if isinstance(data, dict) and isinstance(data.get("memories"), list):
            return data["memories"]
        if isinstance(data, dict) and isinstance(data.get("items"), list):
            return data["items"]
    except json.JSONDecodeError:
        pass
    # 尝试截取第一个 [ 到最后一个 ]
    start, end = text.find("["), text.rfind("]")
    if start != -1 and end > start:
        try:
            data = json.loads(text[start:end + 1])
            if isinstance(data, list):
                return data
        except json.JSONDecodeError:
            pass
    return []
