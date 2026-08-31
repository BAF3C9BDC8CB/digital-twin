"""Digital Twin memory provider for Hermes — uses dt CLI backed by Memgraph+Qdrant.

Stores all memories in the 'memory' world of the digital twin, scoped by project.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from typing import Any, Dict, List, Optional

from agent.memory_provider import MemoryProvider, RecallStatus
from hermes_constants import get_hermes_home

logger = __import__("logging").getLogger(__name__)

# ---------------------------------------------------------------------------
# Config & constants
# ---------------------------------------------------------------------------

_DT_BIN = os.path.expanduser("~/.local/bin/dt")
_DEFAULT_PROJECT = "hermes-memory"
_PREFETCH_TIMEOUT = 8.0      # seconds
_PREFETCH_LIMIT = 8          # max hits to inject
_MIN_SCORE = 0.5             # relevance floor — below this, noise, don't inject
_MIN_QUERY_LEN = 8           # skip trivial queries
_MAX_INJECT_CHARS = 1600     # hard cap on injected text (~600 tokens)
_WRITER_QUEUE_MAX = 1000

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
    """Hermes memory provider backed by digital-twin (dt) CLI."""

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
    # System prompt
    # -----------------------------------------------------------------------

    def system_prompt_block(self) -> str:
        return (
            "[DT-MEMORY] 长期记忆由数字孪生(dt)提供，world=memory。"
            "每轮前自动注入相关记忆（recalled N）。"
            "显式写入可用 dt_memorize 工具或让 agent 判断。"
        )

    # -----------------------------------------------------------------------
    # Prefetch / recall
    # -----------------------------------------------------------------------

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        if not query or len(query.strip()) < _MIN_QUERY_LEN:
            return ""
        if self._context != "primary":
            return ""

        # Check cache first
        with self._cache_lock:
            cached = self._prefetch_cache.get(query)
            if cached and time.time() - cached.timestamp < 30:  # 30s TTL
                self._last_count = cached.count
                return cached.text

        # Do the search - knowledge world, project filter applied post-hoc
        # (dt search --project 过滤的是节点 project 字段，memorize 写入的条目该字段可能为 None)
        try:
            r = subprocess.run(
                [_DT_BIN, "search", "--json", "--limit", str(_PREFETCH_LIMIT), "--world", "knowledge", query],
                capture_output=True,
                text=True,
                timeout=_PREFETCH_TIMEOUT,
            )
            if r.returncode != 0:
                return ""
            data = json.loads(r.stdout)
        except Exception:
            return ""

        hits = data.get("hits", [])
        if not hits:
            return ""

        # Filter: project=hermes-memory already applied via --project; keep only
        # entries written by this provider / the archive hook
        def _is_ours(h):
            hid = h.get("id", "")
            return hid.startswith("mem-") or hid.startswith("hermes-memory") or hid.startswith("hermes-user")

        mem_hits = [h for h in hits if _is_ours(h)]

        # Relevance floor: only inject hits the search actually scored well.
        # Without this, low-score noise matches pollute context as memory grows.
        mem_hits = [
            h for h in mem_hits
            if (h.get("score") or 0) >= _MIN_SCORE
        ]
        # Strongest first, then cap count
        mem_hits.sort(key=lambda h: h.get("score") or 0, reverse=True)
        mem_hits = mem_hits[:_PREFETCH_LIMIT]
        if not mem_hits:
            return ""

        lines = []
        for h in mem_hits[:_PREFETCH_LIMIT]:
            title = h.get("title", "")
            if title in ("KnowledgeAdded", "Decision", "Environment", "Dependencies"):
                title = ""
            snippet = h.get("snippet") or h.get("llm_analysis") or h.get("content") or ""
            proj = h.get("project", "")
            tag = f" [{proj}]" if proj else ""
            # Prefer whichever of title/snippet carries more information
            body = title if len(title) >= len(snippet) else snippet
            alt = snippet if body == title else title
            entry = f"- {body}{tag}" + (f" ({alt})" if alt and alt != body else "")
            if body:
                lines.append(entry)

        # Hard cap on injected size — context explosion guard
        text_lines = []
        total = 0
        for ln in lines:
            if total + len(ln) > _MAX_INJECT_CHARS:
                break
            text_lines.append(ln)
            total += len(ln)

        if not text_lines:
            return ""
        text = "DT 相关记忆:\n" + "\n".join(text_lines)
        count = len(text_lines)

        # Cache it
        with self._cache_lock:
            self._prefetch_cache[query] = _CachedPrefetch(query, text, count, time.time())

        self._last_count = count
        return text

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
        # Queue for background processing; actual extraction happens in on_session_end
        self._writer_queue.append({
            "type": "turn",
            "user": user_content,
            "assistant": assistant_content,
            "session_id": session_id or self._session_id,
            "timestamp": time.time(),
        })
        # Trim queue
        if len(self._writer_queue) > _WRITER_QUEUE_MAX:
            self._writer_queue = self._writer_queue[-_WRITER_QUEUE_MAX:]

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        if self._context != "primary" or not messages:
            return
        # Extract and persist key facts from the full conversation
        self._extract_and_store(messages)

    def on_pre_compress(self, messages: List[Dict[str, Any]]) -> str:
        if self._context != "primary" or not messages:
            return ""
        # Extract from messages about to be compressed
        extracted = self._extract_from_messages(messages, limit=5)
        return "\n".join(extracted) if extracted else ""

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
            "description": "显式写入一条长期记忆到数字孪生 (world=memory)",
            "parameters": {
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "记忆内容"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "可选标签"},
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
        tag_str = "; tags: " + ",".join(tags) if tags else ""
        # First line becomes the searchable name/title
        first_line = content.split("\n")[0].strip()[:60] or content[:60]
        details = f"name: {first_line}; origin: user_explicit; content: {content}{tag_str}"

        # Generate unique entity_id
        h = hashlib.sha1(f"{self._session_id}:{content}:{time.time()}".encode()).hexdigest()[:12]
        entity_id = f"mem-{h}"

        try:
            r = subprocess.run(
                [_DT_BIN, "memorize", "KnowledgeAdded", entity_id, details, "--project", self._project],
                capture_output=True,
                text=True,
                timeout=15,
            )
            if r.returncode == 0:
                return json.dumps({"ok": True, "entity_id": entity_id})
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
                "description": "Digital Twin 项目名，用于记忆作用域隔离",
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
        # Try to find a .git root or use directory name
        try:
            # Check if we're in a known project dir
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

    def _extract_from_messages(self, messages: List[Dict[str, Any]], limit: int = 3) -> List[str]:
        # Simple heuristic: look for decision/preference signals in last N user messages
        user_msgs = [m.get("content", "") for m in messages if m.get("role") == "user"]
        extracted = []
        for msg in user_msgs[-limit:]:
            if any(kw in msg.lower() for kw in ["记住", "记下来", "偏好", "决定", "约定", "记忆"]):
                h = hashlib.sha1(f"auto:{msg}:{time.time()}".encode()).hexdigest()[:12]
                entity_id = f"auto-{h}"
                details = f"origin: auto_extracted; content: {msg[:500]}"
                self._writer_queue.append({
                    "type": "memorize",
                    "entity_id": entity_id,
                    "details": details,
                })
                extracted.append(f"[auto] {msg[:80]}")
        return extracted

    def _extract_and_store(self, messages: List[Dict[str, Any]]) -> None:
        # Delegate to queue; actual write happens in writer thread
        self._extract_from_messages(messages, limit=10)

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
                self._dt_memorize(item["entity_id"], item["details"])
            elif item["type"] == "memory_write":
                self._mirror_memory_write(item)
            elif item["type"] == "turn":
                # Could do LLM-based extraction here; for now rely on on_session_end
                pass
        except Exception as e:
            logger.debug("dt memory writer error: %s", e)

    def _dt_memorize(self, entity_id: str, details: str) -> bool:
        try:
            r = subprocess.run(
                [_DT_BIN, "memorize", "KnowledgeAdded", entity_id, details, "--project", self._project],
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
        if action == "remove":
            details = f"[REMOVED] {old_text}"
        elif action == "replace":
            details = f"[REPLACED] {old_text} -> {content}"
        else:
            details = f"[ADDED] {content}"

        first_line = details.split("\n")[0].strip()[:60] or details[:60]
        full = f"name: {prefix}-{first_line}; origin: agent_curated; content: {details}"
        h = hashlib.sha1(f"{action}:{target}:{old_text}:{content}:{time.time()}".encode()).hexdigest()[:12]
        entity_id = f"hermes-{prefix.lower()}-{action}-{h}"

        self._dt_memorize(entity_id, full)
