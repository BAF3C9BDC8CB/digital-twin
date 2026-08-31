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

import logging
import os
import subprocess
import threading
from pathlib import Path

logger = logging.getLogger(__name__)

# --- config ----------------------------------------------------------------

DT_BIN = os.environ.get("DT_BIN", "/home/luis/.local/bin/dt")
REGISTRY = Path(os.environ.get("DT_REGISTRY", "~/.config/digital-twin/config.yaml")).expanduser()
HOOK_TIMEOUT_SECS = 8
MAX_BRIEF_CHARS = 2000  # hard cap on injected context size (spill threshold is 10k)

# NOTE: 不维护硬编码项目名/别名表。项目来源唯一 = REGISTRY config.yaml
# (~/.config/digital-twin/config.yaml)。新增项目只需在 registry 注册，
# 插件自动可见；这里不写死任何项目名，避免"新增项目插件用不了"。

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
    """Return the registry root best matching user_message tokens.

    Token match = exact project name as a standalone token (not embedded in a
    longer identifier). Longest matching project name wins (nested projects).
    Source of project names = registry only (no hardcoded aliases).
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
    return None


def _match_cwd(cwd: Path) -> Path | None:
    """Return the registry root that cwd lives under (deepest ancestor wins).

    Used when the user message names no project but the session is already
    inside a registered project directory — keep the briefing then.

    Fallback: if cwd is NOT inside any registered project but IS a container
    of registered sub-projects (e.g. /data/aflmProjects/others/pay containing
    offen-pay + offenpay-ui), return cwd itself so dt sense emits the
    container briefing (base_children guidance) instead of skipping entirely.
    Without this, the agent sees no [DT-SENSE] block, guesses a project name
    from the directory basename (e.g. project=pay), filters out every KG hit,
    and falls back to disk spelunking — the exact failure observed in the
    "银盛支付手续费" session.
    """
    best: Path | None = None
    for _, path in _load_registry():
        try:
            cwd.relative_to(path)
        except ValueError:
            continue
        if best is None or len(path.parts) > len(best.parts):
            best = path
    if best is not None:
        return best
    # cwd 不在任何注册项目下：若它是已注册子项目的容器，注入容器简报
    if _is_container_of_registered(cwd):
        return cwd
    return None


def _is_container_of_registered(cwd: Path) -> bool:
    """True if cwd directly contains at least one registered project root."""
    reg_roots = [Path(p) for _, p in _load_registry()]
    try:
        for child in cwd.iterdir():
            if not child.is_dir():
                continue
            for root in reg_roots:
                if child.resolve() == root.resolve():
                    return True
    except OSError:
        return False
    return False


# --- sense -----------------------------------------------------------------

def _run_sense(path: Path | None) -> str | None:
    """Run ``dt sense <path>`` (text mode) and return its stdout verbatim.

    dt sense 的原生文本输出即注入内容（项目定位/索引状态/KG健康/容器子项目/
    候选项目），插件不做二次渲染——保证注入内容始终与 dt CLI 一致，
    新增项目/新状态自动反映，无需改插件。
    """
    cmd = [DT_BIN, "sense"]
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
    out = proc.stdout.strip()
    return out if out else None


# --- hook ------------------------------------------------------------------

_seen_sessions: set[str] = set()


def _resolve_cwd() -> Path:
    """获取会话真实工作目录。

    用 Hermes 的 resolve_agent_cwd()（优先级：会话级 cwd → TERMINAL_CWD →
    进程 cwd）。dt-sense 原来用 Path.cwd() 只拿到 gateway 启动目录
    （/home/luis/.hermes），导致 gateway 模式下永远匹配不到用户操作的项目。
    """
    try:
        from agent.runtime_cwd import resolve_agent_cwd
        return resolve_agent_cwd()
    except Exception:
        return Path.cwd()


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

        cwd = _resolve_cwd()
        target = _match_project(user_message) if user_message else None
        if target is None:
            target = _match_cwd(cwd)
        if target is None:
            # 无项目匹配（消息未提项目名 + cwd 不在注册目录/容器）：
            # 注入最小引导，避免 agent 完全无 KG 感知（曾导致盲查 KG / 带错 project）。
            brief = _minimal_brief(cwd)
            logger.info("dt-sense: no project match, minimal briefing (session=%s)", session_id)
            return brief

        path = target
        sense = _run_sense(path)
        if sense is None:
            return None  # fail-open

        # 注入内容 = dt sense 输出 + 一行检索引导（append，不改写 sense 内容）。
        # 引导补回 dt_search_kg 用法（透传后 dt sense 原生输出不含工具指引，
        # 曾导致 agent 只翻磁盘不用 KG）。
        brief = sense + _search_guidance(sense)
        if len(brief) > MAX_BRIEF_CHARS:
            brief = brief[:MAX_BRIEF_CHARS]
        logger.info("dt-sense: injected briefing for %s (session=%s)", path, session_id)
        return brief
    except Exception as exc:  # never crash the agent
        logger.warning("dt-sense: hook error: %s", exc)
        return None


def _search_guidance(sense: str) -> str:
    """根据 dt sense 输出生成一行 KG 检索引导。

    - 容器(unregistered + 子项目): 引导按子项目名查
    - indexed: 引导 dt_search_kg(world=code, project=<项目名>)
    - 其余: 通用提示先确认项目名
    """
    # 提取项目名（indexed 行: "Project: <name> (...)"）
    import re
    m_proj = re.search(r"Project:\s*([^\s(]+)", sense)
    m_container = re.search(r"注册容器", sense)
    m_unreg = re.search(r"Status:\s*unregistered", sense)
    if m_container:
        return (
            "\n[KG] 当前是注册容器——涉及子项目知识/代码/配置用 "
            "dt_search_kg(project=<子项目名>) 查；不要用目录名当 project（会滤掉全部命中）。"
        )
    if m_proj:
        return (
            f"\n[KG] 项目已索引——代码/知识/配置问题先 "
            f"dt_search_kg(project={m_proj.group(1)}, limit=5) 定位，命中直接采用，再读源码验证。"
        )
    if m_unreg:
        return "\n[KG] 未注册项目——KG 无此项目索引，可先 dt build 注册或直接读磁盘。"
    return ""


def _minimal_brief(cwd: Path) -> str:
    """无项目匹配时的最小引导：列出注册项目数 + 提示先 dt_sense 确认。

    成本 ≤200 字符，但让 agent 知道"KG 存在、项目名从哪确认"，
    避免完全无引导时猜测 project 名（如用目录名当 project 过滤掉全部命中）。
    """
    try:
        n = len(_load_registry())
    except Exception:
        n = 0
    return (
        f"[DT-SENSE] 未匹配到注册项目（cwd={cwd}）。"
        f"KG 有 {n} 个注册项目——涉及项目知识/代码/配置时，"
        f"先 dt_sense 或 dt_search_kg 确认项目名再查；"
        f"不要用目录名当 project 过滤，会滤掉全部命中。"
    )


def register(ctx) -> None:
    """Register the pre_llm_call hook."""
    ctx.register_hook("pre_llm_call", _on_pre_llm_call)
