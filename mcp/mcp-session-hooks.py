"""
Digital Twin MCP Server — session lifecycle hooks.

Provides session-level lifecycle management, including the session-end
protocol that triggers Reasoning node stale-marking.

Architecture:
    This module sits between the MCP client and the dt-daemon gRPC server.
    When a session ends, it calls `_on_session_end()` which in turn marks
    all unverified Reasoning nodes (Observation, Analysis, Decision) as
    stale via the dt-daemon's lifecycle gRPC endpoint.

Usage:
    Integration point for MCP servers (e.g., FastMCP, mcp-python-sdk).
    Call `on_session_end(session_id)` when the MCP protocol signals
    session termination.

Two-level lifecycle:
    Level 1 (session-end): SET _stale_at = timestamp()
        → Nodes become invisible to Context Builder
        → Remain audit-able via dt_history for 30 days

    Level 2 (dt cleanup): DETACH DELETE where _stale_at > 30 days
        → Handled by `dt cleanup --targets reasoning`
"""

from __future__ import annotations

import logging
import os
from datetime import datetime, timezone
from typing import Optional

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Session-end protocol
# ---------------------------------------------------------------------------

def _on_session_end(session_id: str) -> None:
    """
    Called at session end to mark unverified Reasoning nodes as stale.

    This function coordinates with the dt-daemon lifecycle service:
    1. Connects to dt-daemon gRPC (default: localhost:50051)
    2. Calls the MarkStale RPC with the given session_id
    3. All Observation/Analysis/Decision nodes linked to the session
       that have NOT been confirmed are marked with `_stale_at = datetime()`

    TODO (Phase 2.x):
        - Implement actual gRPC call to dt-daemon's LifecycleService.MarkStale
        - Add retry logic with exponential backoff
        - Support daemon address configuration via env var DT_DAEMON_ADDR
        - Add metrics: stale_mark_count, stale_mark_failures

    For now, this framework logs the intent and provides the integration
    point for future gRPC wiring.

    Parameters
    ----------
    session_id : str
        The session identifier (e.g. "2026-07-10-001").
    """
    daemon_addr = os.environ.get("DT_DAEMON_ADDR", "localhost:50051")

    logger.info(
        "Session end: marking unverified reasoning nodes as stale "
        "session_id=%s daemon=%s",
        session_id,
        daemon_addr,
    )

    # TODO: gRPC call — when LifecycleService proto is compiled:
    #
    #   import grpc
    #   from proto import lifecycle_pb2, lifecycle_pb2_grpc
    #
    #   channel = grpc.insecure_channel(daemon_addr)
    #   stub = lifecycle_pb2_grpc.LifecycleServiceStub(channel)
    #
    #   request = lifecycle_pb2.MarkStaleRequest(session_id=session_id)
    #   response = stub.MarkStale(request, timeout=10)
    #
    #   logger.info(
    #       "Stale mark complete: %d nodes marked as stale",
    #       response.nodes_marked,
    #   )

    # For now, emit a log that downstream systems can use to trigger
    # the mark-stale operation via CLI:
    #   dt-daemon lifecycle mark-stale --session-id <session_id>
    logger.info(
        "Framework: mark-stale placeholder for session %s — "
        "use `dt-daemon lifecycle mark-stale --session-id %s` "
        "to execute manually until gRPC is wired",
        session_id,
        session_id,
    )


def on_session_end(session_id: str) -> None:
    """
    Public entry point for session-end lifecycle.

    Called by MCP server integrations when a session is terminated.
    Wraps `_on_session_end` with error handling to ensure the MCP
    server itself never crashes due to lifecycle failures.

    Parameters
    ----------
    session_id : str
        The session identifier (e.g. "2026-07-10-001").
    """
    try:
        _on_session_end(session_id)
    except Exception as exc:
        logger.error(
            "Session-end lifecycle failed for session %s: %s",
            session_id,
            exc,
            exc_info=True,
        )


# ---------------------------------------------------------------------------
# gRPC client helpers (framework — TODO: wire actual proto stubs)
# ---------------------------------------------------------------------------

async def _grpc_call(service: str, method: str, request: dict) -> dict:
    """
    Call a dt-daemon gRPC service.

    TODO: Replace with actual gRPC stub calls once the proto definitions
    are compiled.  For now this is a framework placeholder.

    Parameters
    ----------
    service : str
        Service name (e.g. "ContextService", "PlanService").
    method : str
        RPC method name (e.g. "BuildContext", "GeneratePlan").
    request : dict
        Request payload as a JSON-serialisable dict.

    Returns
    -------
    dict
        Response payload.
    """
    daemon_addr = os.environ.get("DT_DAEMON_ADDR", "localhost:50051")

    logger.debug(
        "gRPC call: %s.%s @ %s",
        service, method, daemon_addr,
    )

    # TODO: Implement actual gRPC call:
    #
    #   import grpc
    #   from proto import context_pb2, context_pb2_grpc
    #   channel = grpc.aio.insecure_channel(daemon_addr)
    #   stub = context_pb2_grpc.ContextServiceStub(channel)
    #   response = await stub.BuildContext(context_pb2.ContextRequest(**request))
    #   return {"context": response.context, "elapsed_ms": response.elapsed_ms}

    raise NotImplementedError(
        f"gRPC not yet wired for {service}.{method} — "
        "use `dt-daemon` CLI or the Rust crate directly."
    )


# ---------------------------------------------------------------------------
# Phase 4 MCP Tool Handlers (4.2–4.10)
# ---------------------------------------------------------------------------

# ── 4.2 dt_context ────────────────────────────────────────────────────────

async def dt_context(
    task: str,
    worlds: Optional[list[str]] = None,
    max_tokens: Optional[int] = None,
    thread_id: Optional[str] = None,
    min_score: Optional[float] = None,
    max_items_per_world: Optional[int] = None,
) -> dict:
    """
    Aggregate context from six knowledge worlds for a given task.

    MCP tool: dt_context(task, worlds?, max_tokens?, thread_id?)

    Returns: AggregatedContext JSON with reality/knowledge/memory/semantic/
             runtime/reasoning world slices plus alerts.
    """
    logger.info("dt_context: task=%s worlds=%s", task[:80], worlds)

    request = {
        "task": task,
        "worlds": worlds,
        "max_tokens": max_tokens,
        "thread_id": thread_id,
        "min_score": min_score,
        "max_items_per_world": max_items_per_world,
    }

    # TODO: gRPC call to ContextService.BuildContext
    # response = await _grpc_call("ContextService", "BuildContext", request)
    # return response

    # Placeholder: return a minimal structure
    return {
        "context": {
            "reality": {"world": "reality", "items": [], "count": 0},
            "knowledge": {"world": "knowledge", "items": [], "count": 0},
            "memory": {"world": "memory", "items": [], "count": 0},
            "semantic": {"world": "semantic", "items": [], "count": 0},
            "runtime": {"world": "runtime", "items": [], "count": 0},
            "reasoning": {"world": "reasoning", "items": [], "count": 0},
            "alerts": [],
            "estimated_tokens": 0,
        },
        "raw_count": 0,
        "retained_count": 0,
        "elapsed_ms": 0,
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.3 dt_plan ───────────────────────────────────────────────────────────

async def dt_plan(
    task: str,
    domain: Optional[str] = None,
    context_json: Optional[str] = None,
    style: Optional[str] = None,
) -> dict:
    """
    Generate an execution plan by matching known Playbooks against the task.

    MCP tool: dt_plan(task, domain?, context_json?, style?)

    Returns: ExecutionPlan with matched playbook, step list, and impact estimate.
    """
    logger.info("dt_plan: task=%s domain=%s", task[:80], domain)

    # TODO: gRPC call to PlanService.GeneratePlan
    # request = {"task": task, "domain": domain, "context_json": context_json, "style": style}
    # response = await _grpc_call("PlanService", "GeneratePlan", request)
    # return response

    return {
        "task": task,
        "matched_playbook": None,
        "plan": [
            {
                "order": 1,
                "action": f"Analyze: {task}",
                "target": None,
                "estimated_minutes": 10,
                "requires": [],
                "notes": "Understand the problem scope",
            },
            {
                "order": 2,
                "action": "Identify affected components",
                "target": None,
                "estimated_minutes": 10,
                "requires": [1],
                "notes": "Use dt_context or dt_dependency",
            },
            {
                "order": 3,
                "action": "Implement the fix",
                "target": None,
                "estimated_minutes": 30,
                "requires": [2],
                "notes": "Make necessary changes",
            },
            {
                "order": 4,
                "action": "Verify with dt_verify",
                "target": None,
                "estimated_minutes": 10,
                "requires": [3],
                "notes": "Run consistency checks",
            },
            {
                "order": 5,
                "action": "Record with dt_learn",
                "target": None,
                "estimated_minutes": 5,
                "requires": [4],
                "notes": "Capture patterns and pitfalls",
            },
        ],
        "estimated_impact": {
            "services": [],
            "configs": [],
            "risk": "unknown",
            "total_minutes": 65,
        },
        "is_generic": True,
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.4 dt_domain ─────────────────────────────────────────────────────────

async def dt_domain(
    domain: str,
    depth: Optional[int] = None,
) -> dict:
    """
    Query the Knowledge World for a domain's concepts, services, and playbooks.

    MCP tool: dt_domain(domain, depth?)

    Returns: DomainModel with concepts, related services, playbooks, and sub-domains.
    """
    logger.info("dt_domain: domain=%s depth=%s", domain, depth)

    # TODO: gRPC call to DomainQueryService.QueryDomain
    # request = {"domain": domain, "depth": depth or 1}
    # response = await _grpc_call("DomainQueryService", "QueryDomain", request)
    # return response

    return {
        "domain": domain,
        "concepts": [],
        "services": [],
        "playbooks": [],
        "sub_domains": [],
        "total_count": 0,
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.5 dt_history ────────────────────────────────────────────────────────

async def dt_history(
    task: str,
    limit: Optional[int] = None,
    since: Optional[str] = None,
    entity_types: Optional[list[str]] = None,
    project: Optional[str] = None,
) -> dict:
    """
    Search the Memory World for similar historical tasks, bug fixes,
    deployments, and decisions.

    MCP tool: dt_history(task, limit?, since?, entity_types?, project?)

    Returns: HistoryResult with similar tasks scored by relevance.
    """
    logger.info("dt_history: task=%s limit=%s", task[:80], limit)

    # TODO: gRPC call to HistoryService.SearchHistory
    # request = {
    #     "task": task, "limit": limit, "since": since,
    #     "entity_types": entity_types, "project": project,
    # }
    # response = await _grpc_call("HistoryService", "SearchHistory", request)
    # return response

    return {
        "query": task,
        "similar_tasks": [],
        "total_found": 0,
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.6 dt_dependency ─────────────────────────────────────────────────────

async def dt_dependency(
    target: str,
    direction: Optional[str] = None,
    max_depth: Optional[int] = None,
    project: Optional[str] = None,
) -> dict:
    """
    Analyse call-chain dependencies for a target entity (service, file, config).

    MCP tool: dt_dependency(target, direction?, max_depth?, project?)

    Returns: DependencyGraph with upstream/downstream entities and impact analysis.
    """
    logger.info("dt_dependency: target=%s direction=%s", target, direction)

    # TODO: gRPC call to DependencyService.AnalyseDependencies
    # request = {
    #     "target": target, "direction": direction,
    #     "max_depth": max_depth, "project": project,
    # }
    # response = await _grpc_call("DependencyService", "AnalyseDependencies", request)
    # return response

    return {
        "target": target,
        "upstream": {"entities": [], "count": 0},
        "downstream": {"entities": [], "count": 0},
        "impact_analysis": {
            "services": [],
            "configs": [],
            "affected_upstream_count": 0,
            "affected_downstream_count": 0,
            "risk": "unknown",
        },
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.7 dt_verify ─────────────────────────────────────────────────────────

async def dt_verify(
    files: list[str],
    project: Optional[str] = None,
    thorough: Optional[bool] = None,
) -> dict:
    """
    Run post-modification consistency checks: code ↔ config ↔ DB ↔ API ↔ KG.

    MCP tool: dt_verify(files, project?, thorough?)

    Returns: VerifyReport with checks, overall status, and suggestions.
    """
    logger.info("dt_verify: files=%s project=%s", files, project)

    # TODO: gRPC call to VerifyService.RunVerification
    # request = {"files": files, "project": project, "thorough": thorough}
    # response = await _grpc_call("VerifyService", "RunVerification", request)
    # return response

    return {
        "checks": [],
        "overall": "Pass",
        "suggestions": [],
        "passed": 0,
        "warned": 0,
        "failed": 0,
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.8 dt_search ─────────────────────────────────────────────────────────

async def dt_search(
    query: str,
    world: Optional[str] = None,
    limit: Optional[int] = None,
    project: Optional[str] = None,
) -> dict:
    """
    Search across all worlds (code, knowledge, documents, vectors).

    MCP tool: dt_search(query, world?, limit?, project?)

    Returns: CrossWorldResult with blended hits from all searched worlds.
    """
    logger.info("dt_search: query=%s world=%s", query[:80], world)

    # TODO: gRPC call to SearchService.CrossWorldSearch
    # request = {"query": query, "world": world, "limit": limit, "project": project}
    # response = await _grpc_call("SearchService", "CrossWorldSearch", request)
    # return response

    return {
        "query": query,
        "world": world or "all",
        "hits": [],
        "total": 0,
        "per_world_counts": {},
        "_note": "placeholder — gRPC not yet wired",
    }


# ── 4.9 dt_learn ──────────────────────────────────────────────────────────

async def dt_learn(
    task: str,
    entities: Optional[list[str]] = None,
    pattern: Optional[str] = None,
    pitfalls: Optional[list[str]] = None,
    decisions: Optional[list[str]] = None,
    thread_id: Optional[str] = None,
    success: Optional[bool] = None,
    project: Optional[str] = None,
) -> dict:
    """
    Learn from a completed task: extract patterns, pitfalls, and decisions
    into the Knowledge World.

    MCP tool: dt_learn(task, entities?, pattern?, pitfalls?, decisions?,
                       thread_id?, success?, project?)

    Returns: LearnReport with counts of created knowledge/experiences/playbooks.
    """
    logger.info("dt_learn: task=%s success=%s", task[:80], success)

    # TODO: gRPC call to LearnService.ExecuteLearn
    # request = {
    #     "task": task, "entities": entities or [],
    #     "pattern": pattern, "pitfalls": pitfalls or [],
    #     "decisions": decisions or [], "thread_id": thread_id,
    #     "success": success, "project": project,
    # }
    # response = await _grpc_call("LearnService", "ExecuteLearn", request)
    # return response

    return {
        "knowledge_created": 0,
        "experiences_created": 0,
        "playbook_updated": False,
        "summary": f"Learn v2 placeholder for: {task}",
        "_note": "placeholder — gRPC not yet wired; Rust LearnService is in dt-knowledge crate",
    }


# ── 4.10 dt_thread ────────────────────────────────────────────────────────

async def dt_thread(
    action: str,
    thread_id: Optional[str] = None,
    title: Optional[str] = None,
    description: Optional[str] = None,
    session_id: Optional[str] = None,
    summary: Optional[str] = None,
    decision: Optional[str] = None,
    reason: Optional[str] = None,
    impact: Optional[str] = None,
    outcome: Optional[str] = None,
    project: Optional[str] = None,
    limit: Optional[int] = None,
) -> dict:
    """
    Manage Digital Thread nodes for tracking long-running conversations,
    investigations, and multi-task workflows.

    MCP tool: dt_thread(action, thread_id?, title?, description?, session_id?,
                        summary?, decision?, reason?, impact?, outcome?,
                        project?, limit?)

    Actions: create / add_session / add_decision / get / list / close

    Returns: ThreadResponse with thread info or list.
    """
    logger.info("dt_thread: action=%s thread_id=%s", action, thread_id)

    # TODO: gRPC call to ThreadService.ExecuteAction
    # request = {
    #     "action": action, "thread_id": thread_id, "title": title,
    #     "description": description, "session_id": session_id,
    #     "summary": summary, "decision": decision, "reason": reason,
    #     "impact": impact, "outcome": outcome, "project": project, "limit": limit,
    # }
    # response = await _grpc_call("ThreadService", "ExecuteAction", request)
    # return response

    return {
        "action": action,
        "thread": None,
        "list": None,
        "message": f"dt_thread placeholder: action={action}",
        "_note": "placeholder — gRPC not yet wired",
    }


# ---------------------------------------------------------------------------
# Session-start protocol (future)
# ---------------------------------------------------------------------------

def on_session_start(session_id: str) -> None:
    """
    Called at session start to initialize reasoning context.

    TODO (Phase 3.x):
        - Create a new ReasoningChain for the session
        - Warm up the Context Builder cache
        - Record session metadata in the knowledge graph

    Parameters
    ----------
    session_id : str
        The session identifier.
    """
    logger.info("Session start: session_id=%s", session_id)


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="[%(name)s] %(levelname)s %(message)s",
    )

    print("=== mcp-server.py self-test ===")

    test_session_id = datetime.now(timezone.utc).strftime("%Y-%m-%d-%H%M%S")
    print(f"Testing with session_id={test_session_id}")

    on_session_start(test_session_id)
    on_session_end(test_session_id)

    print("Self-test complete.")
