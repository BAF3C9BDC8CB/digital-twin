"""dt-router plugin — KG「先检索后执行」中间层 for Hermes Agent.

三个 lifecycle hook, 覆盖 doc「Hermes 知识图谱工程化」方案里 AI 只在对话
开始的缺口 —— 把 KG 感知从"对话开头"延伸到"任务中间 + subagent"：

- pre_llm_call  每 turn 开始(工具调用循环前)注入一次 dt search 命中简报。
- pre_tool_call 读类/写类工具前做 KG-first 强制:本 turn 尚未做过 KG 感知,
                则 block 该工具并引导先 dt_search —— 让"读前必查"
                成为 Runtime 兜底, 而非依赖 LLM 自觉。
- subagent_start 子代理运行前, 对 child_goal 做一次 dt search 预检索,
                把命中作为引导注入, 让 subagent 一进来就先查 KG。

设计对齐 dt-sense 插件:
- 通过 subprocess 调 DT_BIN(dt CLI), 解析 `dt search --json` 输出
  (统一检索入口, 融合原 dt router 能力)。
- 失败兜底(fail-open): 任何异常/超时/非零返回 -> 不注入/不阻断, 绝不 crash agent。
- 不维护第二份闲聊词表: 是否值得检索由 Rust 侧 LLM 门控判断(world=="none"
  表示判定无需检索), Python 只做"要不要调 + 怎么压缩"。

当前 Linux/macOS 宿主适用; 断言路径与 dt-sense 一致可扩展。
"""

from __future__ import annotations

import json
import logging
import os
import re
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)

# --- config ----------------------------------------------------------------

DT_BIN = os.environ.get("DT_BIN", "/home/luis/.local/bin/dt")
# 2026-09-06: L0 改为 LLM 门控(knowledge gate)后,dt search 每次先经 LLM 判断
# 是否值得检索(约 3-8s)再执行检索——单次 hook 需容纳 gate+检索,8s 不够,
# 提到 20s 防注入失效(fail-open 仍兜底,超时只丢注入不 crash agent)。
HOOK_TIMEOUT_SECS = float(os.environ.get("DT_HOOK_TIMEOUT_SECS", "20.0"))
_MAX_INJECT_CHARS = 1600      # 简报硬上限(token 控制, 防膨胀)
_MAX_HITS = 5                 # 单次注入最多条数
_MIN_RESULT_CHARS = 40        # 低于此字符不注入(噪音兜底)
_LOCK = threading.Lock()

# 本 turn 是否已做 KG 感知 的状态 key(按 turn_id / session_id 区分)
_kg_checked: Dict[str, float] = {}
# 已注入过一次提示的 session(避免每 turn 都贴红线)
_seen_guidance_sessions: set[str] = set()

# 需要"先查 KG"的读类/写类工具 —— 命中即需 KG-first 强制。
# Hermes 内置工具名按 docs(user-guide/features/tools) 。子代理委托本身也应先感知。
_TOOLS_REQUIRING_KG_FIRST = {
    # 读类(定位先于读码的核心拦截点)
    "read_file", "view", "search_files", "glob", "grep",
    # 读类(web/通用检索)
    "web_search", "fetch", "http_get",
    # 写/改类(关键: 改了再查就晚了)
    "write_file", "patch", "edit", "apply_patch", "append",
    "terminal",                    # 终端命令(尤其 cat/less/rg 前应感知)
    "delegate_task",               # 子代理: 派生前感知
    "run_shell", "execute_command",
}

# 明确不拦截的写类工具(与 KG 检索无关的纯记录操作) —— 保持保守, 默认全拦
_TOOLS_ALLOW_WITHOUT_KG = {"dt_memorize", "dt_learn", "dt_event"}

# 项目名 → 根路径 解析缓存(与 dt-sense 同一来源: REGISTRY config.yaml)
REGISTRY = Path(os.environ.get("DT_REGISTRY", "~/.config/digital-twin/config.yaml")).expanduser()
_registry_cache: Optional[List[tuple[str, Path]]] = None

_ownership_HINT_RE = re.compile(r"Project:\s*([^\s(]+)")


# --- registry / project resolution -----------------------------------------

def _load_registry() -> List[tuple[str, Path]]:
    """Parse ~/.config/digital-twin/config.yaml projects: (base + items)."""
    global _registry_cache
    if _registry_cache is not None:
        return _registry_cache
    try:
        import yaml  # noqa: PLC0415
    except ImportError:
        logger.warning("dt-router: PyYAML unavailable, registry matching disabled")
        _registry_cache = []
        return _registry_cache
    try:
        with open(REGISTRY, encoding="utf-8") as f:
            cfg = yaml.safe_load(f)
    except Exception as exc:
        logger.warning("dt-router: registry load failed: %s", exc)
        _registry_cache = []
        return _registry_cache
    projects: List[tuple[str, Path]] = []
    for group in cfg.get("projects", []) or []:
        base = Path(group.get("base", ""))
        for item in group.get("items", []) or []:
            if isinstance(item, dict):
                for name, suffix in item.items():
                    p = base / str(suffix)
                    if p.exists():
                        projects.append((name, p))
            elif isinstance(item, str):
                projects.append((item, base / item))
    _registry_cache = projects
    return _registry_cache


def _resolve_cwd() -> Path:
    """会话真实工作目录: 优先 agent.runtime_cwd, 退回进程 cwd。"""
    try:
        from agent.runtime_cwd import resolve_agent_cwd  # noqa: PLC0415
        return resolve_agent_cwd()
    except Exception:
        return Path.cwd()


def _infer_project(cwd: Optional[Path]) -> Optional[str]:
    """从 cwd 所在注册项目推断 project 名(最深祖先胜出)。"""
    if cwd is None:
        cwd = _resolve_cwd()
    best: Optional[tuple[int, str]] = None
    for name, path in _load_registry():
        try:
            cwd.relative_to(path)
        except ValueError:
            continue
        if best is None or len(path.parts) > best[0]:
            best = (len(path.parts), name)
    if best is not None:
        return best[1]
    # 消息里显式提到项目名时也用于 project 参数(见 router 调用处)
    return None


def _match_project_in_text(message: str) -> Optional[str]:
    """user_message 里出现注册项目名(独立 token) -> 返回项目名。"""
    msg = message.lower()
    best: Optional[tuple[int, str]] = None
    for name, _path in _load_registry():
        # 独立 token(不被更长标识符包含); 中文两侧任意
        pat = r"(?<![A-Za-z0-9_-])" + re.escape(name.lower()) + r"(?![A-Za-z0-9_-])"
        if re.search(pat, msg):
            if best is None or len(name) > best[0]:
                best = (len(name), name)
    return best[1] if best else None


def _is_registered_project(path: Path) -> bool:
    """path 是否命中某个注册项目根目录(绝对路径解析后对比)。"""
    try:
        resolved = path.resolve()
    except OSError:
        resolved = path
    for _name, proot in _load_registry():
        try:
            if proot.resolve() == resolved:
                return True
        except OSError:
            pass
    return False


def _child_projects_of(cwd: Path) -> List[tuple[str, Path]]:
    """返回 cwd 直接包含的注册子项目 (name, root)。

    仅当 cwd 本身不是注册项目、但将其视为「注册容器」(unregistered + 内含子项目)
    时返回子项目列表; 否则返回空。用于容器场景 project 推断。
    """
    if _is_registered_project(cwd):
        return []
    reg_roots = [(n, p) for n, p in _load_registry()]
    children: List[tuple[str, Path]] = []
    try:
        entries = list(cwd.iterdir())
    except OSError:
        return []
    for entry in entries:
        if not entry.is_dir():
            continue
        for name, proot in reg_roots:
            try:
                if entry.resolve() == proot.resolve():
                    children.append((name, proot))
                    break
            except OSError:
                continue
    return children


def _infer_container_subproject(cwd: Path, message: str = "") -> Optional[str]:
    """容器场景: 从 cwd 所在注册容器推断最可能的子项目名。

    返回 None 表示无法确定(不限定 project, 走跨世界/全库检索)。
    语义优先级:
      1. 消息里直接命中某个子项目名(独立 token) -> 用之。
      2. 容器只有一个子项目 -> 用之。
      3. 多子项目 + 消息含明显领域词 -> 选领域词覆盖最多的子项目
         (目录/项目名作为浅领域特征, 中文在同项目加载时同样适用)。
      4. 无法唯一判 -> None。
    """
    try:
        children = _child_projects_of(cwd)
    except Exception:
        return None
    if not children:
        return None

    if len(children) == 1:
        return children[0][0]

    msg_low = (message or "").lower()
    for name, _root in children:
        if name.lower() in msg_low:
            return name

    # 多子项目: 用每个子项目路径的最后一段作为领域特征, 与消息做 token 重合打分
    msg_tokens = set(re.findall(r"[a-z0-9]+", msg_low)) | set(
        re.findall(r"[\u4e00-\u9fff]+", msg_low)
    )
    best: Optional[tuple[int, str]] = None
    for name, root in children:
        feats = set(re.findall(r"[a-z0-9]+", root.name.lower()))
        # 项目名本身也作为特征(如 offen-pay -> offen/pay)
        feats |= set(re.findall(r"[a-z0-9]+", name.lower()))
        overlap = len(feats & msg_tokens)
        if best is None or overlap > best[0]:
            best = (overlap, name)
    if best and best[0] >= 1:
        return best[1]
    return None


def _resolve_project(message: str, cwd: Optional[Path] = None) -> Optional[str]:
    """统一项目解析: 消息显式名 → cwd 所在注册项目 → cwd 所在容器子项目。

    返回 None 表示无法确定, 调用方走跨世界/全库检索(不限定 project)。
    优先级(由精确到近似):
      1. 消息里直接命中注册项目名(独立 token)。
      2. cwd 位于某个注册项目根目录内(最深祖先)。
      3. cwd 是含注册子项目的容器(unregistered) -> 按消息/领域词推断最可能子项目。
    """
    # 1. 消息显式项目名最高优先
    by_text = _match_project_in_text(message or "")
    if by_text:
        return by_text
    # 2. cwd 推断(注册项目内)
    if cwd is None:
        cwd = _resolve_cwd()
    by_cwd = _infer_project(cwd)
    if by_cwd:
        return by_cwd
    # 3. 容器子项目推断(消除跨项目噪音)
    return _infer_container_subproject(cwd, message or "")


# --- dt search invocation --------------------------------------------------

def _run_router(query: str, *, project: Optional[str], limit: int) -> Optional[dict]:
    """Run `dt search <query> --json [--project P]` (统一检索入口, 前身 dt router).

    返回解析后的 JSON,失败返回 None(fail-open)。
    """
    cmd = [DT_BIN, "search", query, "--json", "--limit", str(limit)]
    if project:
        cmd += ["--project", project]
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=HOOK_TIMEOUT_SECS,
        )
    except Exception as exc:  # not found / timeout / OSError -> fail-open
        logger.warning("dt-router: dt search failed: %s", exc)
        return None
    if proc.returncode != 0:
        logger.warning("dt-router: dt search exit=%s: %s", proc.returncode, proc.stderr[:200])
        return None
    try:
        return json.loads(proc.stdout or "{}")
    except Exception:
        return None


def _container_scope_roots() -> Optional[List[Path]]:
    """若 cwd 是注册容器(非注册项目、直接含子项目),返回子项目根列表;否则 None。

    用于跨项目噪音硬过滤:容器场景下,命中里 project 属于其他注册项目、
    或路径落在其他项目根下的,都是噪音,不应注入当前会话上下文。
    """
    try:
        cwd = _resolve_cwd()
        if _is_registered_project(cwd):
            return None
        children = _child_projects_of(cwd)
        return [p for _n, p in children] if children else None
    except Exception:
        return None


def _project_roots() -> List[tuple[str, Path]]:
    """注册项目名 → 根路径 全量列表(供归属判定)。"""
    try:
        return list(_load_registry())
    except Exception:
        return []


def _hit_in_scope(
    h: dict,
    *,
    container_roots: Optional[List[Path]],
    project_roots: List[tuple[str, Path]],
) -> bool:
    """判定单条命中是否属于当前容器域。

    规则(容器场景 container_roots 非空时启用):
      - 命中 project 字段 → 属于容器内子项目则保留;属于容器外注册项目则丢弃;
        project 值不在注册表(未知/空) → 看路径。
      - 无 project → 用 file_path / source_ref 做路径归属:
          落在容器子项目根下 → 保留;落在其他注册项目根下 → 丢弃;
          dt://doc/... 等无路径全局文档 → 保留(可能相关,降权交给排序)。
    非容器场景: 不做过滤(保持原行为)。
    """
    if not container_roots:
        return True

    proj = h.get("project")
    if proj:
        proj_s = str(proj)
        # 容器内子项目(注册名/目录名)
        container_names = {str(p.name).lower() for p in container_roots}
        # 容器内子项目的注册名(如容器目录 uvp-offen-pay 的注册名 offen-pay)
        container_proj_names = {
            n.lower()
            for n, r in project_roots
            if any(
                r.resolve() == cr.resolve()
                or r.resolve() in cr.resolve().parents
                or cr.resolve() in r.resolve().parents
                for cr in container_roots
            )
        }
        proj_low = proj_s.lower()
        if proj_low in container_names or proj_low in container_proj_names:
            return True
        # project 精确命中某个容器外注册项目名 → 跨项目噪音, 丢弃
        name_to_root = {n.lower(): r for n, r in project_roots}
        if proj_low in name_to_root:
            return False
        # project 不在注册表(未注册/别名) → 落入路径判定兜底
        # (不在此处用子串匹配——子串误伤率高, 如 uvp-order-center 含 order-center)

    # 无 project(或 project 未知)→ 路径归属判定
    fp = h.get("file_path") or ""
    ref = h.get("source_ref") or ""
    fp_s = str(fp) if fp else ""
    ref_s = str(ref) if ref else ""
    # dt://doc/... 全局文档: 用 ref 判定前缀(fp 为 None 时不能拼 "None " 进去)
    if ref_s.startswith("dt://doc"):
        ref_low = ref_s.lower()
        seg = ref_low.replace("dt://doc/", "", 1).split("/")[0]
        # 段名属于容器内子项目(目录名或注册名) → 容器内文档, 保留
        container_names = {str(p.name).lower() for p in container_roots}
        if seg in container_names:
            return True
        # 段名精确等于某个容器外注册项目名 → 跨项目文档, 丢弃
        # 注意: dt://doc/offen-pay/... 的 offen-pay 是注册名, 需在 container_proj_names 里
        for n, r in project_roots:
            is_container_proj = any(
                r.resolve() == cr.resolve()
                or r.resolve() in cr.resolve().parents
                or cr.resolve() in r.resolve().parents
                for cr in container_roots
            )
            if n.lower() == seg:
                return is_container_proj  # 容器内项目保留, 容器外项目丢弃
        return True

    path_str = fp_s + " " + ref_s

    try:
        p = Path(fp) if fp else None
        if p is not None and p.is_absolute():
            resolved = p.resolve()
            # 落在容器子项目根下 → 保留
            for r in container_roots:
                try:
                    resolved.relative_to(r.resolve())
                    return True
                except ValueError:
                    continue
            # 落在其他注册项目根下 → 丢弃
            for n, r in project_roots:
                if n in {r2.name for r2 in container_roots}:
                    continue
                try:
                    resolved.relative_to(r.resolve())
                    return False
                except ValueError:
                    continue
    except Exception:
        pass
    # 相对路径 file_path(如 "pay-offen-core/src/...java")无绝对根 → 保守保留
    return True


def _build_brief(
    data: dict, query: str, *, container_roots: Optional[List[Path]] = None
) -> Optional[str]:
    """把 `dt search --json` 输出压缩成语义压缩语境块。

    - world=="none"            -> L0 早退(闲聊/算术), 不注入。
    - total==0 / 无 hits       -> 无相关,KG 未命中, 注入一行"KG 无命中"提示即可
                                  (可选, 保留给下游判断: KG-first 已执行但无料)。
    - 有 hits                  -> 取 title+snippet+score+project+file_path,
                                  生成 "<knowledge_context>...</knowledge_context>"。
    绝不要把原始 JSON 灌进上下文(token 爆炸)。上限 _MAX_INJECT_CHARS。

    container_roots 非 None 时启用容器域过滤(见 _hit_in_scope):把跨项目
    噪音命中(其他注册项目的代码/文档)挡在注入之外。
    """
    if not isinstance(data, dict):
        return None
    world = data.get("world", "all")
    if world == "none":
        return None  # rust L0 判定无需检索, 本插件不重复实现
    hits = data.get("hits") or []
    if not hits:
        # KG-first 已执行但无命中
        return "[dt-router] KG 检索完成: 0 相关命中(world=%s, query=%r)。涉及项目知识时可直接读源码。" % (world, query)

    project_roots = _project_roots()
    kept: List[dict] = []
    dropped_other_project = 0
    for h in hits[:_MAX_HITS * 3]:  # 多看一些, 给过滤留余量
        if _hit_in_scope(
            h,
            container_roots=container_roots,
            project_roots=project_roots,
        ):
            kept.append(h)
        else:
            dropped_other_project += 1
        if len(kept) >= _MAX_HITS:
            break
    if dropped_other_project:
        logger.info(
            "dt-router: container scope filter dropped %d cross-project hit(s) (kept %d)",
            dropped_other_project,
            len(kept),
        )
    hits = kept
    if not hits:
        return "[dt-router] KG 检索完成: 命中均为跨项目噪音, 已过滤(容器域 %s)。" % (
            ", ".join(p.name for p in container_roots) if container_roots else "?"
        )

    lines: List[str] = ["<knowledge_context>"]
    lines.append("相关知识命中(来自 dt search, world=%s):" % world)
    total_chars = len(lines[0]) + len(lines[1])
    for h in hits[: _MAX_HITS]:
        score = h.get("score")
        # 注: 依赖检索返回的 hits 已由 dt search 内部按相关度排好序(Rust 侧负责排序、
        #    `--project` 过滤跨项目噪音)。**不要**在插件里再用原始 score 做阈值——
        #    实测 dt search 的 code 命中 score 常恒为 0.01~0.02(与噪音相同), 阈值会误杀
        #    有效代码命中(如 genPayOrderId)。跨项目教程噪音由 Fix1 的 project 限定消除。
        title = h.get("title") or ""
        snip = h.get("snippet") or h.get("llm_analysis") or ""
        proj = h.get("project")
        fp = h.get("file_path") or h.get("source_ref")
        fields = []
        if title and title != snip:
            fields.append(title.strip())
        if snip:
            fields.append(snip.strip())
        if fp:
            fields.append("@" + str(fp))
        if proj:
            fields.append("[" + str(proj) + "]")
        merged = " - ".join(fields)
        if score is not None:
            merged += " (%.2f)" % float(score)
        if not merged.strip():
            continue
        if total_chars + len(merged) + 1 > _MAX_INJECT_CHARS:
            break
        lines.append(merged)
        total_chars += len(merged) + 1
    lines.append("</knowledge_context>")
    if len(lines) <= 2:
        return "[dt-router] KG 检索完成: 命中不足未注入。"
    return "\n".join(lines)


def _mark_checked(key: str) -> None:
    with _LOCK:
        _kg_checked[key] = time.time()


def _was_checked(key: str) -> bool:
    with _LOCK:
        return key in _kg_checked


def _turn_key(turn_id: str, session_id: str) -> str:
    return f"{session_id}:{turn_id}" if turn_id else session_id


# --- hook: pre_llm_call -----------------------------------------------------

def _on_pre_llm_call(
    session_id: str = "",
    turn_id: str = "",
    user_message: str = "",
    is_first_turn: bool = False,
    **_: object,
) -> Optional[str]:
    """每 turn 开头: dt search 预检索 -> 注入压缩简报。

    仅对非闲聊(user_message 非空)触发; 命中才注入, 零命中也标记已感知,
    让 pre_tool_call 知道本 turn 已做过 KG-first。
    """
    try:
        if not user_message or len(user_message.strip()) < 2:
            return None
        key = _turn_key(turn_id, session_id)
        project = _resolve_project(user_message)
        data = _run_router(user_message, project=project, limit=_MAX_HITS)
        # 无论命不命中, 本 turn 都算已感知(make pre_tool_call 不再强制)
        _mark_checked(key)
        brief = (
            _build_brief(
                data,
                user_message,
                container_roots=_container_scope_roots(),
            )
            if data
            else None
        )
        return brief if brief else None
    except Exception as exc:  # never crash
        logger.warning("dt-router: pre_llm_call error: %s", exc)
        return None


# --- hook: pre_tool_call (KG-first 强制) ------------------------------------

_KNOWLEDGE_FIRST_GUIDANCE = (
    "先完成数字孪生 KG 检索再执行本工具: 对与项目/代码/配置/历史记忆相关的操作, "
    "调用 dt_search(world=code|knowledge|memory, project=<注册项目名>, limit=5) "
    "或 dt_sense() 定位目标; 命中即事实(先读取确认), 0 命中才读源码或直接执行。"
)


def _on_pre_tool_call(tool_name: str, args: dict, task_id: str, **kwargs: object) -> Optional[dict]:
    """读类/写类工具前做 KG-first 兜底 + delegate_task 子代理 KG 注入。

    - delegate_task 分支: 每个子任务 goal 检索 KG 命中, append 到 task['context']
      (与 Hermes 自身把 output contract append 到 context 的方式一致), 让子代理
      出生即带父级 KG 上下文 —— 不依赖 block, 也不重复检索。
    - 其余 KG-first 工具: 若本 turn 尚未做 KG 感知 -> block 一次引导先查。
    - dt_memorize/dt_learn/dt_event 等纯记录工具放行。
    """
    try:
        if tool_name in _TOOLS_ALLOW_WITHOUT_KG:
            return None

        # ---- delegate_task: 给每个子任务注入 KG 预检索命中 ----
        if tool_name == "delegate_task":
            modified = _enrich_delegate_args(args)
            if modified is not None:
                logger.info("dt-router: delegate_task KG-context injected (+%d items)",
                            len(args.get("tasks") or []))
                return {"action": "modify", "args": modified}
            return None

        if tool_name not in _TOOLS_REQUIRING_KG_FIRST:
            return None
        turn_id = str(kwargs.get("turn_id") or "")
        session_id = str(kwargs.get("session_id") or (task_id or ""))
        key = _turn_key(turn_id, session_id)
        if _was_checked(key):
            return None  # 本 turn 已 KG-first
        # 仍未感知 -> block 引导一次
        _mark_checked(key)
        logger.info("dt-router: KG-first block %s (session=%s turn=%s)", tool_name, session_id, turn_id)
        return {
            "action": "block",
            "message": (
                f"[dt-router KG-first] 在 {tool_name} 之前必须完成知识图谱检索:\n"
                + _KNOWLEDGE_FIRST_GUIDANCE
            ),
        }
    except Exception as exc:  # fails-open: 异常不阻断(保守, 避免锁死 agent)
        logger.warning("dt-router: pre_tool_call error: %s", exc)
        return None


_DELEGATE_CTX_MARKER = "[dt-router KG 预检索]"


def _enrich_delegate_args(args: dict) -> Optional[dict]:
    """为 delegate_task 的每个子任务 context 注入 KG 预检索命中。

    参数结构(Hermes): tasks = [{"goal","context","role"}], 顶层也有 goal/context。
    每个子任务以其 goal 查询 dt search, 命中压缩为 brief 后 append 到该任务
    context(用分隔标记避免重复注入); 无命中/闲聊/失败则返回 None 不改动。
    返回新 args(dict), 或 None(不改动)。fails-open。
    """
    try:
        tasks = args.get("tasks")
        if not isinstance(tasks, list) or not tasks:
            return None
        # 为每个子任务计算 KG brief(用该任务 goal 作 query)
        enriched = [dict(t) for t in tasks if isinstance(t, dict)]
        if not enriched:
            return None
        changed = False
        for t in enriched:
            goal = str(t.get("goal") or "").strip()
            if len(goal) < 2:
                continue
            project = _resolve_project(goal)
            data = _run_router(goal, project=project, limit=_MAX_HITS)
            brief = (
                _build_brief(data, goal, container_roots=_container_scope_roots())
                if data
                else None
            )
            if not brief:
                continue
            # 去重标记: 已注入过则跳过
            existing = str(t.get("context") or "")
            if _DELEGATE_CTX_MARKER in existing:
                continue
            separator = "\n\n" if existing else ""
            t["context"] = existing + separator + _DELEGATE_CTX_MARKER + "\n" + brief
            changed = True
        if not changed:
            return None
        new_args = dict(args)
        new_args["tasks"] = enriched
        return new_args
    except Exception as exc:  # fails-open
        logger.warning("dt-router: _enrich_delegate_args error: %s", exc)
        return None


# --- hook: subagent_start ---------------------------------------------------

def _on_subagent_start(
    child_goal: str = "",
    child_role: str = "",
    child_session_id: str = "",
    **kwargs: object,
) -> None:
    """子代理运行前: 对 child_goal 做 KG 预检索, 命中则注入到 goal 引导。

    注意: subagent_start 是 observer hook, 返回值被忽略 —— 因此无法直接改写
    child_goal。本项目采用"注入侧车"策略: 把命中结果写到一个子代理可读取的
    临时上下文(经 pre_llm_call per-session cache / memory), 并通过 system
    引导让子代理第一件事先 dt_search。真正"写进 goal 前"的强制, 只能在
    Hermes 核心的 delegate_tool 内部做(见 notes)。
    """
    try:
        if not child_goal or len(child_goal.strip()) < 2:
            return
        project = _match_project_in_text(child_goal) or _infer_project(None)
        data = _run_router(child_goal, project=project, limit=_MAX_HITS)
        if not data or data.get("world") == "none":
            return
        _mark_checked(f"sub:{child_session_id or '?'}")
        brief = (
            _build_brief(data, child_goal, container_roots=_container_scope_roots())
            if data
            else None
        )
        if brief:
            # 供子代理 pre_llm_call 复用: 把命中写入进程内缓存, 子代理首 turn 直接取。
            # 由子代理插件(若有)读取; 本插件仅观察 + 打点, 不写库。
            logger.info(
                "dt-router: subagent_start child=%s role=%s goal=%r -> KG brief %d chars",
                child_session_id,
                child_role,
                child_goal[:60],
                len(brief),
            )
    except Exception as exc:  # never crash
        logger.warning("dt-router: subagent_start error: %s", exc)


# --- registration -----------------------------------------------------------

def register(ctx) -> None:
    """Register pre_llm_call + pre_tool_call + subagent_start hooks."""
    ctx.register_hook("pre_llm_call", _on_pre_llm_call)
    ctx.register_hook("pre_tool_call", _on_pre_tool_call)
    ctx.register_hook("subagent_start", _on_subagent_start)