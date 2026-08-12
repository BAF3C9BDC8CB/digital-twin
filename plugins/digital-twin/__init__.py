"""Digital-twin knowledge-graph memory provider for Hermes.

每轮 prefetch：按用户消息调用 `dt search --world knowledge --limit 5 --json`
召回知识图谱中的相关记忆（项目知识、历史决策、部署信息等），渲染为纯文本
注入下一轮上下文。设计原则：

- **只读（prefetch-only）**：写入由 dt 的 hook 系统（code_modified / decision_made
  / bug_fix_recorded 等）与 agent 主动 dt_memorize 完成，本插件不写 KG，
  避免对话噪音污染图谱。
- **fail-open**：dt CLI 不可用 / 超时 / 解析失败 → 返回空串，绝不影响主流程。
- **轻量**：subprocess 超时 6s（外部 provider prefetch 有 8s 上限），单次召回
  渲染 ≤ 1500 字符。

配置（可选环境变量）：
  DT_BIN         dt 可执行文件路径（默认 ~/.local/bin/dt）
  DT_PREFETCH_LIMIT   每次召回条数（默认 5）
  DT_PREFETCH_MAX_CHARS 注入上下文硬上限（默认 1500）
"""

from __future__ import annotations

import json
import logging
import os
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)

DT_BIN = os.environ.get("DT_BIN", str(Path.home() / ".local/bin/dt"))
PREFETCH_LIMIT = int(os.environ.get("DT_PREFETCH_LIMIT", "5"))
MAX_CHARS = int(os.environ.get("DT_PREFETCH_MAX_CHARS", "1500"))
SEARCH_TIMEOUT = 6.0  # subprocess 超时（外部 provider 总预算 8s）

# entity_type 召回优先级：知识/决策/项目级 > 代码级。代码检索 agent 会用
# world=code 主动查，prefetch 只注入知识层记忆。
_PREFER_ORDER = ("Decision", "Knowledge", "KnowledgeAdded", "Standard", "Api", "Config", "Service")

# 尝试继承 MemoryProvider ABC；import 失败（环境异常）时退化为普通类，
# is_available() 会拦下不可用场景。
try:
    from agent.memory_provider import MemoryProvider as _MemoryProviderABC
except Exception:  # pragma: no cover
    _MemoryProviderABC = None  # type: ignore[assignment]


class DigitalTwinMemoryProvider(_MemoryProviderABC or object):  # type: ignore[misc]
    """MemoryProvider for digital-twin KG recall (prefetch-only)."""

    def __init__(self) -> None:
        self._session_id = ""
        self._agent_context = ""
        self._dt_bin = Path(DT_BIN)

    # -- MemoryProvider 接口 --------------------------------------------------

    @property
    def name(self) -> str:
        return "digital-twin"

    def is_available(self) -> bool:
        """只做本地检查：dt CLI 存在且可执行。不做网络调用。"""
        try:
            return self._dt_bin.exists() and os.access(self._dt_bin, os.X_OK)
        except Exception:
            return False

    def initialize(self, session_id: str, **kwargs: Any) -> None:
        self._session_id = str(session_id or "").strip()
        # agent_context: primary/subagent/cron/flush — 只在主会话召回
        self._agent_context = str(kwargs.get("agent_context", "") or "")
        logger.debug(
            "digital-twin memory provider initialized (session=%s ctx=%s)",
            self._session_id, self._agent_context,
        )

    def system_prompt_block(self) -> str:
        return (
            "## Digital-Twin KG 记忆\n"
            "每轮会自动从知识图谱召回相关记忆（项目知识/历史决策/部署信息）注入上下文。\n"
            "需要更精确的代码/服务检索时用 dt_search_kg / dt search 主动查询。\n"
        )

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        """按用户消息召回 KG 记忆，返回渲染文本（空串=无召回/失败）。"""
        if not query or not query.strip():
            return ""
        # 跳过子代理/后台上下文，避免重复注入
        if self._agent_context in ("subagent", "cron", "flush"):
            return ""
        q = query.strip()
        # 过短查询跳过（省一次进程开销）
        if len(q) < 2:
            return ""
        hits = self._search(q)
        if not hits:
            return ""
        return self._render(hits)

    def sync_turn(
        self,
        user_content: str,
        assistant_content: str,
        *,
        session_id: str = "",
        **kwargs: Any,
    ) -> None:
        """只读 provider：不写回。事件写入由 dt hook 系统负责。"""
        return None

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        """纯上下文注入模式：不暴露额外工具。"""
        return []

    # -- 内部实现 -------------------------------------------------------------

    def _search(self, query: str) -> List[Dict[str, Any]]:
        """调 dt search --world knowledge，返回原始 hit 列表。"""
        try:
            proc = subprocess.run(
                [str(self._dt_bin), "search", query, "--world", "knowledge",
                 "--limit", str(PREFETCH_LIMIT), "--json"],
                capture_output=True,
                text=True,
                timeout=SEARCH_TIMEOUT,
            )
        except Exception as exc:
            logger.debug("dt memory prefetch: search failed: %s", exc)
            return []
        if proc.returncode != 0:
            logger.debug(
                "dt memory prefetch: dt search exit=%s stderr=%s",
                proc.returncode, (proc.stderr or "")[:200],
            )
            return []
        try:
            data = json.loads(proc.stdout)
        except Exception as exc:
            logger.debug("dt memory prefetch: bad JSON: %s", exc)
            return []
        hits = data.get("hits", []) or []
        # 过滤无意义命中（空标题/占位）
        clean = []
        for h in hits:
            title = (h.get("title") or "").strip()
            if not title or title in ("?", "unknown"):
                continue
            clean.append(h)
        # 按 entity_type 优先级稳定排序（保持分数内序）
        rank = {t: i for i, t in enumerate(_PREFER_ORDER)}
        clean.sort(key=lambda h: rank.get((h.get("entity_type") or ""), 99))
        return clean[:PREFETCH_LIMIT]

    def _render(self, hits: List[Dict[str, Any]]) -> str:
        """渲染为纯文本块（每条：类型|标题 — [project] 摘要前 100 字符）。"""
        lines = ["[KG 记忆]"]
        for h in hits:
            et = (h.get("entity_type") or "?").strip()
            title = (h.get("title") or "").strip()
            proj = self._extract_project(h)
            snippet = (h.get("snippet") or "").strip().replace("\n", " ")
            if proj:
                snippet = f"[{proj}] " + snippet
            if snippet:
                snippet = snippet[:110]
            if snippet:
                lines.append(f"- ({et}) {title}: {snippet}")
            else:
                lines.append(f"- ({et}) {title}")
        text = "\n".join(lines)
        if len(text) > MAX_CHARS:
            text = text[:MAX_CHARS]
        return text

    @staticmethod
    def _extract_project(hit: Dict[str, Any]) -> str:
        """从 payload 提取项目名：project 字段 > source_ref/file_path 的 dt:// 前缀。"""
        p = (hit.get("project") or "").strip()
        if p:
            return p
        for key in ("source_ref", "file_path", "id"):
            ref = hit.get(key) or ""
            if isinstance(ref, str) and ref.startswith("dt://"):
                # dt://doc/{project}/... | dt://entity/{project}/... | dt://knowledge/{project}/...
                parts = ref.split("/")
                if len(parts) >= 4 and parts[2] != "nacos":
                    return parts[3]
                if len(parts) >= 3 and parts[2]:
                    return parts[2]
        return ""


def register(ctx: Any) -> None:
    """plugin-style register(ctx) — 通过 collector 注册 provider 实例。

    memory_manager 的 _load_provider_from_dir 调用 `mod.register(collector)`，
    collector 提供 `register_memory_provider(provider)` 方法捕获实例。
    """
    try:
        p = DigitalTwinMemoryProvider()
        if p.is_available():
            ctx.register_memory_provider(p)
    except Exception as exc:  # pragma: no cover
        logger.debug("digital-twin memory provider register failed: %s", exc)
