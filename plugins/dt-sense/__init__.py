"""dt-sense plugin — inject digital-twin KG environment briefing at session start.

pre_llm_call hook: once per session (is_first_turn + session_id cache),
resolve the target project from user_message against the registry, run
``dt sense <path> --json``, render the compact briefing template, and
return it as hook context (appended to the user message, not system prompt).

Design: docs/superpowers/specs/2026-08-11-pre-llm-call-injection-final.md
Fail-open: any error -> return None (hook framework logs a warning; agent
never crashes, no context injected on failure).
"""

from __future__ import annotations

import json
import logging
import os
import subprocess
import threading
from datetime import datetime, timezone
from pathlib import Path

logger = logging.getLogger(__name__)

# --- config ----------------------------------------------------------------

DT_BIN = os.environ.get("DT_BIN", "/home/luis/.local/bin/dt")
REGISTRY = Path(os.environ.get("DT_REGISTRY", "~/.config/digital-twin/config.yaml")).expanduser()
HOOK_TIMEOUT_SECS = 8
MAX_BRIEF_CHARS = 2000  # hard cap on injected context size (spill threshold is 10k)

# Project-name aliases: display name -> path suffix used by dt sense resolution.
# The registry itself is the primary source; this map only fixes known aliases
# (e.g. user-center -> uvp-user-center). Key = exact match on user_message token.
ALIASES = {
    "user-center": "uvp-user-center",
    "api-gateway": "uvp-api-gateway",
    "app-center": "uvp-app-center",
    "comment-center": "uvp-comment-center",
    "im-center": "uvp-im-center",
    "knight-center": "uvp-knight-center",
    "label-center": "uvp-label-center",
    "med-alliance-center": "uvp-med-alliance-center",
    "medicals-center": "uvp-medicals-center",
    "nurse-center": "uvp-nurse-center",
    "oauth-center": "uvp-oauth-center",
    "pay-center": "uvp-pay-center",
    "user-auth-center": "uvp-user-auth-center",
    "warehouse": "warehouse-center",
    "digital-twin": "digital-twin-v2",
    "dt": "digital-twin-v2",
}

# --- registry --------------------------------------------------------------

_lock = threading.Lock()
_registry_cache: dict | None = None


def _load_registry() -> list[tuple[str, Path]]:
    """Parse ~/.config/digital-twin/config.yaml projects: (base + items)."""
    global _registry_cache
    with _lock:
        if _registry_cache is not None:
            return _registry_cache
        try:
            import yaml
        except ImportError:
            logger.warning("dt-sense: PyYAML unavailable, registry matching disabled")
            _registry_cache = []
            return _registry_cache
        try:
            with open(REGISTRY, encoding="utf-8") as f:
                cfg = yaml.safe_load(f)
        except Exception as exc:
            logger.warning("dt-sense: registry load failed: %s", exc)
            _registry_cache = []
            return _registry_cache
        projects: list[tuple[str, Path]] = []
        for group in cfg.get("projects", []) or []:
            base = Path(group.get("base", ""))
            for item in group.get("items", []) or []:
                if isinstance(item, dict):
                    for name, suffix in item.items():
                        p = base / str(suffix)
                        projects.append((name, p))
                elif isinstance(item, str):
                    projects.append((item, base / item))
        _registry_cache = projects
        return _registry_cache


import re

_TOKEN_RE_CACHE: dict[str, re.Pattern] = {}


def _token_pattern(token: str) -> re.Pattern:
    """Word-boundary-ish pattern: token must not be embedded in a longer
    alnum/underscore/hyphen token (so 'svc' doesn't match 'svc-order' or
    'update' contains 'dt'). Chinese chars are fine on either side."""
    p = _TOKEN_RE_CACHE.get(token)
    if p is None:
        p = re.compile(r"(?<![A-Za-z0-9_-])" + re.escape(token) + r"(?![A-Za-z0-9_-])")
        _TOKEN_RE_CACHE[token] = p
    return p


def _match_project(message: str) -> Path | None:
    """Return the registry root best matching user_message tokens (incl. aliases).

    Token match = exact project name as a standalone token (not embedded in a
    longer identifier). Longest matching project name wins (nested projects).
    """
    msg = message.lower()
    best: tuple[int, Path] | None = None
    for name, path in _load_registry():
        if _token_pattern(name.lower()).search(msg):
            # prefer longest project name (most specific)
            if best is None or len(name) > best[0]:
                best = (len(name), path)
    if best is not None:
        return best[1]
    # aliases after registry names so real names win
    for alias, target in ALIASES.items():
        if _token_pattern(alias.lower()).search(msg):
            for name, path in _load_registry():
                if name == target:
                    return path
    return None


# --- sense -----------------------------------------------------------------

def _run_sense(path: Path | None) -> dict | None:
    """Run dt sense --json for the given path (or cwd); return parsed JSON or None."""
    cmd = [DT_BIN, "sense", "--json"]
    if path is not None:
        cmd.append(str(path))
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=HOOK_TIMEOUT_SECS,
            cwd=str(path) if path is not None else None,
        )
    except Exception as exc:
        logger.warning("dt-sense: dt sense failed: %s", exc)
        return None
    if proc.returncode != 0:
        logger.warning("dt-sense: dt sense exit=%s: %s", proc.returncode, proc.stderr[:200])
        return None
    try:
        return json.loads(proc.stdout)
    except Exception as exc:
        logger.warning("dt-sense: bad JSON from dt sense: %s", exc)
        return None


# --- rendering -------------------------------------------------------------

def _fmt_ts(iso: str | None) -> str:
    if not iso:
        return "never"
    try:
        dt = datetime.fromisoformat(iso.replace("Z", "+00:00"))
        return dt.astimezone().strftime("%Y-%m-%d %H:%M")
    except Exception:
        return iso[:16]


def _render_brief(sense: dict, cwd: Path, projects_n: int) -> str:
    """Render the [DT-SENSE] briefing template (fixed ≤1.5KB)."""
    status = sense.get("status", "unknown")
    proj = sense.get("project") or {}
    stats = sense.get("stats") or {}
    degraded = sense.get("degraded") or []
    kg_status = "degraded:[" + ",".join(degraded) + "]" if degraded else "healthy"

    dirs = sense.get("dirs") or []
    langs = sense.get("languages") or []
    ents = sense.get("key_entities") or []
    dirs_str = ",".join(d["dir"] for d in dirs[:5]) if dirs else "-"
    langs_str = ",".join(f"{l.get('ext','?')}:{l.get('pct',0)}%" for l in langs[:5]) if langs else "-"
    ents_str = ",".join(f"{e.get('name','?')}({e.get('kind','?')},{e.get('in_degree',0)})" for e in ents[:5]) if ents else "-"

    candidates = sense.get("candidates") or []
    cand_str = ""
    if status == "unregistered" and candidates:
        tops = ",".join(str(c.get("path", "?")) for c in candidates[:3])
        cand_str = f"\ncandidates: {tops} 未注册, 建议 dt build --full"

    deg_str = f"\n⚠ KG degraded: [{','.join(degraded)}] 查询可能为空, 降级读磁盘" if degraded else ""

    # 已索引项目强信号：目标项目在 KG 里有实体时，明确引导 agent
    # 用 dt_search_kg(world=code) 定位——否则 agent 可能误判"KG 无内容"而纯读源码。
    indexed_hint = ""
    if status == "indexed" and stats.get("methods", 0) > 0:
        pname = proj.get("name") or "?"
        indexed_hint = (
            f"\n✅ 本项目已索引 {stats.get('methods',0)} 方法/{stats.get('classes',0)} 类——"
            f"代码问题先用 dt_search_kg(world=code, project={pname}, limit=5) 定位, "
            f"再读源码验证; 禁止只读源码跳过 KG"
        )

    return (
        f"[DT-SENSE] {proj.get('name') or '?'} | {status} | KG {kg_status}\n"
        f"path: {proj.get('path') or cwd}\n"
        f"stats: {stats.get('methods',0)}m {stats.get('classes',0)}c {stats.get('vectors',0)}v | build: {_fmt_ts(stats.get('last_build'))}\n"
        f"brief: dirs:{dirs_str} | langs:{langs_str} | 实体:{ents_str}\n"
        f"注册项目: {projects_n} 个"
        f"{cand_str}{deg_str}{indexed_hint}\n\n"
        f"可用dt工具: dt_search_kg(query,world=code|knowledge,project=<项目名>,limit≤5) — 代码问题推荐world=code+project; run_cypher_query(已知elementId走L2); dt_health; dt_sense\n"
        f"搜索触发: 服务/配置/凭据/部署/历史决策→dt_search_kg; 纯代码→先dt_search_kg(world=code)定位再读源码; 闲聊→不查; 每任务L1≤1次(漏参重查计入); 10s超时=降级\n"
        f"禁止: 凭记忆答项目事实; 伪造结果; 输出key/密码; KG故障阻塞任务→读磁盘并标⚠; 重复/碎查"
    )


# --- hook ------------------------------------------------------------------

_seen_sessions: set[str] = set()


def _on_pre_llm_call(
    session_id: str = "",
    user_message: str = "",
    is_first_turn: bool = False,
    **_: object,
) -> str | None:
    """Once per session: resolve project from message, inject dt sense briefing."""
    try:
        # Once per session only (turn-scoped injection would pollute prefix).
        with _lock:
            if session_id in _seen_sessions:
                return None
            if not is_first_turn and not _seen_sessions:
                # is_first_turn is authoritative; but some platforms may not
                # set it — fall back to session-first-seen semantics above.
                pass
            _seen_sessions.add(session_id)

        cwd = Path.cwd()
        target = _match_project(user_message) if user_message else None
        path = target or cwd

        sense = _run_sense(path)
        if sense is None:
            return None  # fail-open

        projects_n = len(_load_registry())
        brief = _render_brief(sense, cwd, projects_n)
        if len(brief) > MAX_BRIEF_CHARS:
            brief = brief[:MAX_BRIEF_CHARS]
        logger.info("dt-sense: injected briefing for %s (session=%s)", path, session_id)
        return brief
    except Exception as exc:  # never crash the agent
        logger.warning("dt-sense: hook error: %s", exc)
        return None


def register(ctx) -> None:
    """Register the pre_llm_call hook."""
    ctx.register_hook("pre_llm_call", _on_pre_llm_call)
