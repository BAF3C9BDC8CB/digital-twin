"""Digital Twin memory provider for Hermes — uses dt CLI backed by Memgraph+Qdrant.

按需检索式长期记忆（v2）。

设计变更（2026-08-31）：
- 不再每轮把记忆原文灌进上下文（原 prefetch 每轮 dt search + 最多 8 条原文注入，
  记忆增长时 token 爆炸）。
- 改为「引导 + 按需检索」：
  * system_prompt_block 注入检索方式（怎么用 dt_search_kg 查），记忆本体不自动注入。
  * prefetch 仅在用户消息含显式记忆意图词（记住/记一下/记忆/记得/上次/之前说）时
    做一次定向检索（统一全局若干条），其余情况一律返回空 —— 零 token 开销。
- 记忆统一全局（2026-09-01 用户确认）：不分项目/全局，全部全局统一检索；
  写入时 details 内带文件路径/位置标识，检索靠记忆内容定位实际文件。
  检索侧 dt search --world memory 不再按 project 过滤（search_memory.rs v3），
  project 参数仅作溯源字段返回，不参与过滤。
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import threading
import time
from dataclasses import dataclass
from typing import Any, Dict, List, Optional

from agent.memory_provider import MemoryProvider, RecallStatus
from hermes_constants import get_hermes_home

# v3: LLM 驱动的主动记忆整理（不依赖用户说"记住"）
try:
    from .llm_extract import extract_memories_from_conversation
except ImportError:  # 独立加载场景（测试/直接 import 本文件）
    from llm_extract import extract_memories_from_conversation

logger = __import__("logging").getLogger(__name__)

# ---------------------------------------------------------------------------
# Config & constants
# ---------------------------------------------------------------------------

_DT_BIN = os.path.expanduser("~/.local/bin/dt")
_DEFAULT_PROJECT = "hermes-memory"
_GLOBAL_PROJECT = "hermes-global"   # 全局记忆专用 project 名（写入目标；检索侧 project=hermes-global 由 Rust 映射为 scope='global' 过滤，或直接不带 project 查）
_PREFETCH_TIMEOUT = 8.0      # seconds
_PREFETCH_LIMIT = 4          # 定向检索每侧条数（项目/全局各 4，最多 8）
_MIN_SCORE = 0.5             # relevance floor
_MIN_QUERY_LEN = 8           # skip trivial queries
_MAX_INJECT_CHARS = 1200     # 定向检索注入硬上限（只在显式意图时用到）
_WRITER_QUEUE_MAX = 1000
_EXTRACT_EVERY_TURNS = 3     # 每 N 轮触发一次 LLM 记忆提取（省 token）
_DEDUP_THRESHOLD = 0.82      # 相似度 ≥ 此值视为重复 → 更新而非新增
_LLM_EXTRACT_TIMEOUT = 30.0

# 显式记忆意图词：命中才触发定向检索（否则不注入任何记忆）
_MEMORY_INTENT_KEYWORDS = (
    "记住", "记一下", "记下来", "记忆", "记得", "上次", "之前说",
    "recall", "memory", "remember",
)

# ---------------------------------------------------------------------------
# Internal state
# ---------------------------------------------------------------------------

@dataclass
class _CachedPrefetch:
    query: str
    text: str
    count: int
    timestamp: float


class DtMemoryProvider(MemoryProvider):
    """Hermes memory provider backed by digital-twin (dt) CLI — on-demand recall."""

    def __init__(self) -> None:
        self._session_id: str = ""
        self._project: str = _DEFAULT_PROJECT
        self._context: str = "primary"
        self._last_count: int = 0
        self._prefetch_cache: Dict[str, _CachedPrefetch] = {}
        self._cache_lock = threading.Lock()
        self._writer_queue: List[Dict[str, Any]] = []
        self._writer_thread: Optional[threading.Thread] = None
        self._writer_stop = threading.Event()
        self._initialized = False
        # v3: turn 累积 + LLM 主动提取
        self._turn_buffer: List[Dict[str, Any]] = []
        self._turn_count = 0
        self._hermes_home = ""

    # -----------------------------------------------------------------------
    # Identity
    # -----------------------------------------------------------------------

    @property
    def name(self) -> str:
        return "dt-memory"

    # -----------------------------------------------------------------------
    # Availability check
    # -----------------------------------------------------------------------

    def is_available(self) -> bool:
        if not os.path.exists(_DT_BIN):
            return False
        try:
            r = subprocess.run(
                [_DT_BIN, "health"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            return r.returncode == 0
        except Exception:
            return False

    def unavailable_reason(self) -> str:
        if not os.path.exists(_DT_BIN):
            return "dt CLI not found at ~/.local/bin/dt"
        return "dt health check failed — ensure Memgraph & Qdrant are running"

    # -----------------------------------------------------------------------
    # Lifecycle
    # -----------------------------------------------------------------------

    def initialize(
        self,
        session_id: str,
        *,
        hermes_home: str = "",
        platform: str = "cli",
        agent_context: str = "primary",
        **kwargs,
    ) -> None:
        self._session_id = session_id
        self._context = agent_context
        self._hermes_home = hermes_home or str(get_hermes_home())

        # Infer project from cwd if possible
        cwd = kwargs.get("cwd") or os.getcwd()
        self._project = self._infer_project(cwd) or _DEFAULT_PROJECT

        # Start background writer
        self._writer_stop.clear()
        self._writer_thread = threading.Thread(target=self._writer_loop, daemon=True, name="dt-memory-writer")
        self._writer_thread.start()

        self._initialized = True

    def shutdown(self) -> None:
        # Flush remaining writes
        self._writer_stop.set()
        if self._writer_thread and self._writer_thread.is_alive():
            self._writer_thread.join(timeout=5)

    # -----------------------------------------------------------------------
    # System prompt — 注入「检索方式」而非记忆本体
    # -----------------------------------------------------------------------

    def system_prompt_block(self) -> str:
        return (
            "[DT-MEMORY] 长期记忆由数字孪生(dt)提供(world=memory)，按需检索、不自动注入。\n"
            "需要历史记忆时用 MCP 工具 dt_search_kg 自行检索：\n"
            "- 记忆统一全局: dt_search_kg(world=memory, limit=5) 不分项目/全局，一次查完\n"
            "显式写入用 dt_memorize（记忆统一全局，details 内注明文件路径/位置便于定位）。\n"
            "[DT 行为准则] 每任务先 dt_sense 感知；服务/配置/凭据/部署/历史决策先 dt_search_kg(world=memory) 查一次，\n"
            "命中即事实，0 命中才读源码；定位代码用 dt_search_kg(world=code, project=注册项目名) 再读源码验证；\n"
            "禁止伪造结果/输出密钥；hop≥1 只当线索；用户说\"记忆/记一下\"立即 dt_memorize。"
        )

    # -----------------------------------------------------------------------
    # Prefetch / recall — 仅显式意图词触发定向检索
    # -----------------------------------------------------------------------

    def _has_memory_intent(self, query: str) -> bool:
        q = query.lower()
        return any(kw.lower() in q for kw in _MEMORY_INTENT_KEYWORDS)

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        if not query or len(query.strip()) < _MIN_QUERY_LEN:
            return ""
        if self._context != "primary":
            return ""
        # v2: 只有显式记忆意图词才触发检索；否则零注入（省 token）
        if not self._has_memory_intent(query):
            self._last_count = 0
            return ""

        # Check cache first
        with self._cache_lock:
            cached = self._prefetch_cache.get(query)
            if cached and time.time() - cached.timestamp < 30:  # 30s TTL
                self._last_count = cached.count
                return cached.text

        # 定向检索：记忆统一全局（不分项目/全局），一次查完
        all_hits = self._search_world(
            query, world="memory", project=None, limit=_PREFETCH_LIMIT * 2
        )

        lines = []
        for h in all_hits:
            body = self._render_hit(h)
            if body:
                lines.append(body)

        # Hard cap
        text_lines = []
        total = 0
        for ln in lines:
            if total + len(ln) > _MAX_INJECT_CHARS:
                break
            text_lines.append(ln)
            total += len(ln)

        if not text_lines:
            self._last_count = 0
            return ""
        text = "DT 相关记忆(按需):\n" + "\n".join(text_lines)
        count = len(text_lines)

        # Cache it
        with self._cache_lock:
            self._prefetch_cache[query] = _CachedPrefetch(query, text, count, time.time())

        self._last_count = count
        return text

    def _search_world(
        self, query: str, *, world: str, project: Optional[str], limit: int
    ) -> List[Dict[str, Any]]:
        """dt search --world <world> [--project <p>] — 返回 hits 列表。"""
        cmd = [_DT_BIN, "search", "--json", "--limit", str(limit), "--world", world]
        if project:
            cmd += ["--project", project]
        cmd.append(query)
        try:
            r = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=_PREFETCH_TIMEOUT,
            )
            if r.returncode != 0:
                return []
            data = json.loads(r.stdout)
        except Exception:
            return []
        hits = data.get("hits", [])
        if not hits:
            return []
        # 只保留本 provider 写入的记忆（id 前缀白名单）
        def _is_ours(h):
            hid = h.get("id", "")
            return hid.startswith("mem-") or hid.startswith("hermes-memory") or hid.startswith("hermes-user") or hid.startswith("auto-")
        mem_hits = [h for h in hits if _is_ours(h)]
        # 相关度地板
        mem_hits = [h for h in mem_hits if (h.get("score") or 0) >= _MIN_SCORE]
        mem_hits.sort(key=lambda h: h.get("score") or 0, reverse=True)
        return mem_hits[:limit]

    def _render_hit(self, h: Dict[str, Any]) -> str:
        """单条命中渲染：title/snippet 去重后输出，带 project 标签。"""
        title = h.get("title", "")
        if title in ("KnowledgeAdded", "Decision", "Environment", "Dependencies"):
            title = ""
        snippet = h.get("snippet") or h.get("llm_analysis") or h.get("content") or ""
        proj = h.get("project", "")
        tag = f" [{proj}]" if proj else ""
        body = title if len(title) >= len(snippet) else snippet
        alt = snippet if body == title else title
        entry = f"- {body}{tag}" + (f" ({alt})" if alt and alt != body else "")
        return entry if body else ""

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        # Fire-and-forget background prefetch for next turn
        def _bg():
            try:
                self.prefetch(query, session_id=session_id)
            except Exception:
                pass
        threading.Thread(target=_bg, daemon=True).start()

    def recall_status(self) -> Optional[RecallStatus]:
        if self._last_count:
            return RecallStatus(provider_label="dt-memory", count=self._last_count, glyph="🐋")
        return None

    # -----------------------------------------------------------------------
    # Write path
    # -----------------------------------------------------------------------

    def sync_turn(
        self,
        user_content: str,
        assistant_content: str,
        *,
        session_id: str = "",
        messages: Optional[List[Dict[str, Any]]] = None,
    ) -> None:
        if self._context != "primary":
            return
        # v3: 累积 turn，每 _EXTRACT_EVERY_TURNS 轮触发一次后台 LLM 提取
        self._turn_count += 1
        if messages:
            self._turn_buffer = messages  # 保留完整对话供提取
        self._writer_queue.append({
            "type": "turn",
            "user": user_content,
            "assistant": assistant_content,
            "session_id": session_id or self._session_id,
            "timestamp": time.time(),
        })
        if self._turn_count % _EXTRACT_EVERY_TURNS == 0 and self._turn_buffer:
            self._queue_llm_extract(self._turn_buffer)
        # Trim queue
        if len(self._writer_queue) > _WRITER_QUEUE_MAX:
            self._writer_queue = self._writer_queue[-_WRITER_QUEUE_MAX:]

    def _queue_llm_extract(self, messages: List[Dict[str, Any]]) -> None:
        """后台线程跑 LLM 提取 + 去重写入（不阻塞 agent 主循环）。"""
        def _bg():
            try:
                entries = extract_memories_from_conversation(
                    messages,
                    hermes_home=self._hermes_home,
                    timeout=_LLM_EXTRACT_TIMEOUT,
                )
                for e in entries:
                    self._write_with_dedup(e)
            except Exception:
                logger.debug("llm extract failed", exc_info=True)
        threading.Thread(target=_bg, daemon=True, name="dt-memory-extract").start()

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        if self._context != "primary" or not messages:
            return
        # v3: 会话结束用 LLM 提取完整对话（不依赖用户显式"记住"）
        self._queue_llm_extract(messages)

    def on_pre_compress(self, messages: List[Dict[str, Any]]) -> str:
        if self._context != "primary" or not messages:
            return ""
        # 压缩前也做一次提取（即将丢弃的消息里有价值信息）
        self._queue_llm_extract(messages)
        return ""

    def on_memory_write(
        self,
        action: str,
        target: str,
        content: str,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> None:
        """Mirror built-in memory writes to dt (memory world)."""
        if self._context != "primary":
            return
        self._writer_queue.append({
            "type": "memory_write",
            "action": action,
            "target": target,
            "content": content,
            "metadata": metadata or {},
            "timestamp": time.time(),
        })

    # -----------------------------------------------------------------------
    # Tools
    # -----------------------------------------------------------------------

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        return [{
            "name": "dt_memorize",
            "description": "显式写入一条长期记忆到数字孪生 (world=memory)。记忆统一全局，不分项目/全局；details 内注明文件路径/位置便于检索后定位",
            "parameters": {
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "记忆内容"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "可选标签"},
                    "project": {"type": "string", "description": "溯源字段（记忆统一全局，不参与检索过滤）"},
                    "scope": {"type": "string", "enum": ["project", "global"], "description": "兼容保留（记忆统一全局，检索不区分）"},
                },
                "required": ["content"],
            },
        }]

    def handle_tool_call(self, tool_name: str, args: Dict[str, Any], **kwargs) -> str:
        if tool_name != "dt_memorize":
            raise NotImplementedError(f"Provider dt does not handle tool {tool_name}")

        content = args.get("content", "").strip()
        if not content:
            return json.dumps({"error": "content required"})

        tags = args.get("tags", [])
        project = args.get("project") or self._project
        scope = args.get("scope", "project")
        if scope not in ("project", "global"):
            scope = "project"
        # 全局记忆: 强制挂 hermes-global 作用域, 并让 details 带 scope=global 落库
        if scope == "global":
            project = _GLOBAL_PROJECT
        tag_str = "; tags: " + ", ".join(tags) if tags else ""
        # First line becomes the searchable name/title
        first_line = content.split("\n")[0].strip()[:60] or content[:60]
        details = f"name: {first_line}; origin: user_explicit; scope: {scope}; content: {content}{tag_str}"

        # Generate unique entity_id
        h = hashlib.sha1(f"{self._session_id}:{content}:{time.time()}".encode()).hexdigest()[:12]
        entity_id = f"mem-{h}"

        try:
            r = subprocess.run(
                [_DT_BIN, "memorize", "KnowledgeAdded", entity_id, details, "--project", project],
                capture_output=True,
                text=True,
                timeout=15,
            )
            if r.returncode == 0:
                return json.dumps({"ok": True, "entity_id": entity_id, "project": project})
            return json.dumps({"error": r.stderr.strip()})
        except Exception as e:
            return json.dumps({"error": str(e)})

    # -----------------------------------------------------------------------
    # Config schema for `hermes memory setup`
    # -----------------------------------------------------------------------

    def get_config_schema(self) -> List[Dict[str, Any]]:
        return [
            {
                "key": "project",
                "description": "Digital Twin 溯源 project 名（记忆统一全局，检索不按此过滤；仅作内容溯源）",
                "default": _DEFAULT_PROJECT,
                "required": False,
            },
            {
                "key": "dt_bin",
                "description": "dt CLI 路径",
                "default": _DT_BIN,
                "required": False,
            },
        ]

    def save_config(self, values: Dict[str, Any], hermes_home: str) -> None:
        # Config is just env/project-level; nothing to persist here
        pass

    # -----------------------------------------------------------------------
    # Backup
    # -----------------------------------------------------------------------

    def backup_paths(self) -> List[str]:
        # dt stores in Memgraph/Qdrant/SQLite — handled by `dt backup`
        return []

    # -----------------------------------------------------------------------
    # Internal helpers
    # -----------------------------------------------------------------------

    def _infer_project(self, cwd: str) -> Optional[str]:
        """推断当前会话所属项目（dt 注册项目名）。

        优先用 dt sense 的注册项目映射：当 cwd 是注册容器目录（或其子目录）时，
        sense 返回 base_children，取**唯一**的注册子项目名（如 offen-pay），
        避免落到 git root 目录名（pay）或未知名——注册名才是 dt_search_kg
        project 过滤与 AGENTS.md 决策表约定的项目名。

        sense 不可用/超时/多候选时才回退 git root 目录名。
        """
        try:
            r = subprocess.run(
                [_DT_BIN, "sense", "--json"],
                capture_output=True,
                text=True,
                timeout=6,
                cwd=cwd,
            )
            if r.returncode == 0:
                data = json.loads(r.stdout)
                children = data.get("base_children") or []
                # 只取已注册子项目；多候选时不猜（避免映射错项目）
                registered = [c.get("name") for c in children if c.get("registered")]
                if len(registered) == 1:
                    return registered[0]
        except Exception:
            pass
        # 回退：git root 目录名
        try:
            r = subprocess.run(
                ["git", "-C", cwd, "rev-parse", "--show-toplevel"],
                capture_output=True,
                text=True,
                timeout=2,
            )
            if r.returncode == 0:
                root = r.stdout.strip()
                return os.path.basename(root)
        except Exception:
            pass
        return os.path.basename(cwd)

    def _write_with_dedup(self, entry: Dict[str, Any]) -> None:
        """写入一条 LLM 提取的记忆，带去重合并。

        先按 summary 检索相似记忆；score ≥ _DEDUP_THRESHOLD 视为重复，
        复用其 entity_id 做 MERGE 覆盖（更新而非新增，避免记忆膨胀）。
        scope=global 写到 hermes-global 项目；否则写到当前项目。
        """
        summary = entry.get("summary", "")
        if not summary:
            return
        scope = entry.get("scope", "project")  # project | global
        project = _GLOBAL_PROJECT if scope == "global" else self._project
        etype = entry.get("type", "fact")
        detail = entry.get("detail", "")
        tags = entry.get("tags", [])

        # 去重检索（带 project 过滤，避免跨项目误合并）
        similar = self._search_world(
            summary, world="memory", project=project, limit=3
        )
        entity_id = None
        if similar and (similar[0].get("score") or 0) >= _DEDUP_THRESHOLD:
            entity_id = similar[0].get("id")
        if not entity_id:
            h = hashlib.sha1(f"llm:{summary}:{time.time()}".encode()).hexdigest()[:12]
            entity_id = f"mem-{h}"

        tag_str = "; tags: " + ", ".join(tags) if tags else ""
        details = (
            f"name: {summary}; origin: llm_auto; type: {etype}; scope: {scope}; "
            f"content: {detail}{tag_str}"
        )
        self._dt_memorize(entity_id, details, project)

    def _extract_from_messages(self, messages: List[Dict[str, Any]], limit: int = 3) -> List[str]:
        # 保留兼容壳：v3 由 LLM 提取（_queue_llm_extract）承担，此路径不再使用
        return []

    def _extract_and_store(self, messages: List[Dict[str, Any]]) -> None:
        # v3 由 LLM 提取承担；保留空实现避免外部调用炸
        pass

    def _writer_loop(self) -> None:
        while not self._writer_stop.is_set():
            item = None
            if self._writer_queue:
                item = self._writer_queue.pop(0)
            if item:
                self._process_write_item(item)
            else:
                time.sleep(1)

    def _process_write_item(self, item: Dict[str, Any]) -> None:
        try:
            if item["type"] == "memorize":
                self._dt_memorize(item["entity_id"], item["details"], item.get("project"))
            elif item["type"] == "memory_write":
                self._mirror_memory_write(item)
            elif item["type"] == "turn":
                # turn 已由 _queue_llm_extract 后台处理；这里只做累积，不重复写
                pass
        except Exception as e:
            logger.debug("dt memory writer error: %s", e)

    def _dt_memorize(self, entity_id: str, details: str, project: Optional[str] = None) -> bool:
        proj = project or self._project
        try:
            r = subprocess.run(
                [_DT_BIN, "memorize", "KnowledgeAdded", entity_id, details, "--project", proj],
                capture_output=True,
                text=True,
                timeout=15,
            )
            return r.returncode == 0
        except Exception:
            return False

    def _mirror_memory_write(self, item: Dict[str, Any]) -> None:
        action = item["action"]
        target = item["target"]
        content = item["content"]
        old_text = item["metadata"].get("old_text", "")

        if not content and not old_text:
            return

        prefix = "MEMORY" if target == "memory" else "USER"

        # AI 验证记忆失效后的处置：
        # - remove → KG 真正删除（图+向量），不留 [REMOVED] 占位
        # - replace → KG 版本化更新（supersede），旧记忆归档保留版本链
        # - add → 新增
        if action == "remove":
            # 若 old_text 带 entity_id（如 "mem-xxxx"）则直接删；否则写删除标记记忆
            if old_text.startswith("mem-") or old_text.startswith("hermes-") or old_text.startswith("auto-"):
                self._dt_memorize_delete(old_text)
            else:
                details = f"[REMOVED] {old_text}"
                first_line = details.split("\n")[0].strip()[:60] or details[:60]
                full = f"name: {prefix}-{first_line}; origin: agent_curated; content: {details}"
                h = hashlib.sha1(f"{action}:{target}:{old_text}:{content}:{time.time()}".encode()).hexdigest()[:12]
                entity_id = f"hermes-{prefix.lower()}-{action}-{h}"
                self._dt_memorize(entity_id, full)
        elif action == "replace":
            # 版本化更新：新内容覆盖旧内容，走 EVOLVED_FROM 版本链
            self._dt_memorize_supersede(old_text, content, target)
        else:
            details = f"[ADDED] {content}"
            first_line = details.split("\n")[0].strip()[:60] or details[:60]
            full = f"name: {prefix}-{first_line}; origin: agent_curated; content: {details}"
            h = hashlib.sha1(f"{action}:{target}:{old_text}:{content}:{time.time()}".encode()).hexdigest()[:12]
            entity_id = f"hermes-{prefix.lower()}-{action}-{h}"
            self._dt_memorize(entity_id, full)

    def _dt_memorize_delete(self, entity_id: str) -> bool:
        """真正删除 KG 记忆节点（图 + 向量）。"""
        try:
            r = subprocess.run(
                [_DT_BIN, "memorize", "KnowledgeAdded", entity_id, "", "--action", "delete"],
                capture_output=True,
                text=True,
                timeout=15,
            )
            return r.returncode == 0
        except Exception:
            return False

    def _dt_memorize_supersede(self, old_id: str, new_content: str, target: str) -> bool:
        """版本化更新：新内容覆盖旧记忆，旧节点归档（EVOLVED_FROM 版本链）。"""
        try:
            first_line = new_content.split("\n")[0].strip()[:60] or new_content[:60]
            details = f"name: {first_line}; origin: agent_curated; content: {new_content}"
            r = subprocess.run(
                [_DT_BIN, "memorize", "KnowledgeAdded", old_id, details, "--action", "update", "--supersede", old_id],
                capture_output=True,
                text=True,
                timeout=15,
            )
            return r.returncode == 0
        except Exception:
            return False
