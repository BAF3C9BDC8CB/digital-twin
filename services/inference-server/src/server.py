#!/home/luis/.local/miniconda3/bin/python3
"""
dt-inference — Unified model inference service with async queue routing.

Architecture:
    server.py     — thin entry: parse args, assemble, start
    models/       — model loading, registry, download
    queue/        — priority queue, async worker, batching
    api/          — gRPC + REST protocol handlers

Usage:
    python3 server.py
    python3 server.py --port 50051 --llm-port 50052
    python3 server.py --device cpu --workers 4
"""

import argparse
import asyncio
import logging
import os
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

from aiohttp import web

# Ensure proto modules importable
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from models.registry import ModelRegistry
from models.spec import ModelSpec
from models.embed import load_embed_model, DEFAULT_EMBED_MODEL, DEFAULT_DEVICE
from models.reranker import load_reranker_model, DEFAULT_RERANKER_MODEL
from models.llm import load_llm_model, DEFAULT_LLM_MODEL, DEFAULT_DEVICE as LLM_DEVICE
from models.hanlp import load_hanlp_model, DEFAULT_HANLP_MODEL
from taskqueue.router import TaskRouter, QUEUE_MAXSIZE
from taskqueue.worker import InferenceWorker
from api.grpc_server import SharedEventLoop, serve_grpc
from api.rest_server import create_rest_app
from metrics import generate_metrics, _ensure_metrics

logging.basicConfig(
    level=logging.INFO,
    format="[dt-inference] %(asctime)s %(levelname)s %(name)s %(message)s",
)
logger = logging.getLogger("dt-inference")


def build_registry() -> ModelRegistry:
    """Register all available model specs."""
    registry = ModelRegistry()
    registry.register(ModelSpec(
        name=DEFAULT_EMBED_MODEL,
        model_type="embed",
        loader=load_embed_model,
        device=DEFAULT_DEVICE,
        batch_capable=True,
    ))
    registry.register(ModelSpec(
        name=DEFAULT_RERANKER_MODEL,
        model_type="reranker",
        loader=load_reranker_model,
        device=DEFAULT_DEVICE,
        batch_capable=False,
    ))
    registry.register(ModelSpec(
        name=DEFAULT_LLM_MODEL,
        model_type="llm",
        loader=load_llm_model,
        device=LLM_DEVICE,
        batch_capable=False,
        idle_ttl_sec=600,  # unload LLM after 10min idle
    ))
    registry.register(ModelSpec(
        name=DEFAULT_HANLP_MODEL,
        model_type="nlp",
        loader=load_hanlp_model,
        device="cpu",
        batch_capable=False,
    ))
    return registry


def serve(port: int = 50051, llm_port: int = 50052, max_workers: int = 4):
    """Start gRPC + REST servers with shared queue and models."""

    # ── 1. Registry + Router ──
    registry = build_registry()
    router = TaskRouter(registry)

    # ── 2. gRPC server (legacy embed + reranker) ──
    shared_loop = SharedEventLoop()
    grpc_server = serve_grpc(router, shared_loop, port=port, max_workers=max_workers)

    # ── 3. REST app (LLM chat + health + metrics) ──
    rest_app = create_rest_app(registry, router)
    _ensure_metrics()

    # ── 4. Start async event loop ──
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    shared_loop.set_loop(loop)

    # GPU inference executor (dedicated, small pool)
    inference_executor = ThreadPoolExecutor(max_workers=2, thread_name_prefix="infer-gpu-")
    worker = InferenceWorker(router, executor=inference_executor)

    async def run_all():
        await router.start()
        await worker.start()

        runner = web.AppRunner(rest_app)
        await runner.setup()
        site = web.TCPSite(runner, "0.0.0.0", llm_port)
        await site.start()
        logger.info("dt-inference REST listening on 0.0.0.0:%d", llm_port)

        # Idle eviction loop
        async def eviction_loop():
            while True:
                await asyncio.sleep(60)
                await registry.evict_idle()

        asyncio.create_task(eviction_loop())

        # Keep running
        while True:
            await asyncio.sleep(3600)

    try:
        loop.run_until_complete(run_all())
    except KeyboardInterrupt:
        logger.info("Shutting down...")
        loop.run_until_complete(worker.stop())
        loop.run_until_complete(router.stop())
        grpc_server.stop(grace=5)
        inference_executor.shutdown(wait=True)
        logger.info("Shutdown complete")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="dt-inference server")
    parser.add_argument("--port", type=int, default=50051, help="gRPC port (default: 50051)")
    parser.add_argument("--llm-port", type=int, default=50052, help="REST port (default: 50052)")
    parser.add_argument("--workers", type=int, default=4, help="gRPC thread pool (default: 4)")
    parser.add_argument("--device", default=None, help="Device for embed/reranker (cuda/cpu)")
    args = parser.parse_args()

    if args.device:
        os.environ["INFERENCE_DEVICE"] = args.device

    serve(port=args.port, llm_port=args.llm_port, max_workers=args.workers)
