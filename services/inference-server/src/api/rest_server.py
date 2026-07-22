"""REST API handlers — LLM chat, embed, rerank, health, models, metrics."""

import json
import logging
import uuid

from aiohttp import web

from taskqueue.task import Priority

logger = logging.getLogger("dt-inference.rest")


def create_rest_app(registry, router) -> web.Application:
    """Create the aiohttp REST API application."""
    app = web.Application()
    app["registry"] = registry
    app["router"] = router

    app.router.add_post("/v1/chat/completions", handle_chat)
    app.router.add_post("/v1/embeddings", handle_embed)
    app.router.add_post("/v1/rerank", handle_rerank)
    app.router.add_get("/v1/models", handle_models)
    app.router.add_get("/health", handle_health)
    app.router.add_get("/metrics", handle_metrics)
    return app


# ── Chat ──────────────────────────────────────────────────────────────────

async def handle_chat(request: web.Request) -> web.Response:
    """POST /v1/chat/completions — OpenAI-compatible chat endpoint."""
    router = request.app["router"]
    try:
        body = await request.json()
    except json.JSONDecodeError:
        return web.json_response({"error": "Invalid JSON"}, status=400)

    messages = body.get("messages", [])
    if not messages:
        return web.json_response({"error": "messages is required"}, status=400)

    max_tokens = body.get("max_tokens", 512)
    temperature = body.get("temperature", 0.7)
    priority = Priority.HIGH if body.get("priority") == "high" else Priority.NORMAL

    try:
        result = await router.submit(
            "chat",
            {"messages": messages, "max_tokens": max_tokens, "temperature": temperature},
            priority=priority,
            sync=True,
        )
        return web.json_response({
            "id": f"chatcmpl-{uuid.uuid4().hex[:8]}",
            "object": "chat.completion",
            "model": result["model"],
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": result["response"]},
                "finish_reason": "stop",
            }],
            "usage": {"elapsed_ms": result["elapsed_ms"]},
        })
    except Exception as e:
        logger.error("Chat API failed: %s", e)
        return web.json_response({"error": str(e)}, status=500)


# ── Embed ─────────────────────────────────────────────────────────────────

async def handle_embed(request: web.Request) -> web.Response:
    """POST /v1/embeddings — sync or async embed."""
    router = request.app["router"]
    try:
        body = await request.json()
    except json.JSONDecodeError:
        return web.json_response({"error": "Invalid JSON"}, status=400)

    texts = body.get("input", [])
    if isinstance(texts, str):
        texts = [texts]
    if not texts:
        return web.json_response({"error": "input is required"}, status=400)

    sync = not body.get("async", False)
    priority = Priority.HIGH if sync else Priority.LOW

    try:
        result = await router.submit(
            "embed", {"texts": texts}, priority=priority, sync=sync,
        )
        if sync:
            return web.json_response({
                "object": "list",
                "data": [
                    {"embedding": v, "index": i}
                    for i, v in enumerate(result["embeddings"])
                ],
                "model": result["model"],
                "usage": {"elapsed_ms": result["elapsed_ms"]},
            })
        else:
            return web.json_response(result)
    except Exception as e:
        logger.error("Embed API failed: %s", e)
        return web.json_response({"error": str(e)}, status=500)


# ── Rerank ────────────────────────────────────────────────────────────────

async def handle_rerank(request: web.Request) -> web.Response:
    """POST /v1/rerank — re-rank candidate texts against a query."""
    router = request.app["router"]
    try:
        body = await request.json()
    except json.JSONDecodeError:
        return web.json_response({"error": "Invalid JSON"}, status=400)

    query = body.get("query", "")
    texts = body.get("texts", []) or body.get("documents", [])
    if not query or not texts:
        return web.json_response({"error": "query and texts are required"}, status=400)

    sync = not body.get("async", False)
    priority = Priority.HIGH if sync else Priority.LOW

    try:
        result = await router.submit(
            "rerank", {"query": query, "texts": texts},
            priority=priority, sync=sync,
        )
        if sync:
            return web.json_response({
                "object": "list",
                "data": [
                    {"index": i, "score": s}
                    for i, s in enumerate(result["scores"])
                ],
                "model": result["model"],
                "usage": {"elapsed_ms": result["elapsed_ms"]},
            })
        else:
            return web.json_response(result)
    except Exception as e:
        logger.error("Rerank API failed: %s", e)
        return web.json_response({"error": str(e)}, status=500)


# ── Health / Models / Metrics ─────────────────────────────────────────────

async def handle_health(request: web.Request) -> web.Response:
    """GET /health — overall health check."""
    registry = request.app["registry"]
    status = registry.status()
    all_ok = all(not s["error"] for s in status.values())
    return web.json_response({
        "status": "healthy" if all_ok else "degraded",
        "models": status,
    })


async def handle_models(request: web.Request) -> web.Response:
    """GET /v1/models — list all models and their status."""
    registry = request.app["registry"]
    return web.json_response(registry.status())


async def handle_metrics(request: web.Request) -> web.Response:
    """GET /metrics — Prometheus metrics endpoint."""
    try:
        from metrics import generate_metrics
        return web.Response(
            body=generate_metrics(), content_type="text/plain; version=0.0.4"
        )
    except ImportError:
        return web.Response(
            body="# prometheus_client not installed\n", content_type="text/plain"
        )
