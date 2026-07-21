#!/home/luis/.local/miniconda3/bin/python3
"""
dt-inference — Unified model inference service for Digital Twin.

Single gRPC server replacing dt-embed + dt-reranker.
Features:
- Lazy loading: models load on first use, not at startup
- Auto-download: if model not cached, download from HuggingFace automatically
- Unified API: one port, one proto, multiple model types

Usage:
    python3 server.py
    python3 server.py --port 50051
    python3 server.py --device cpu
"""

import argparse
import logging
import os
import sys
import time
from concurrent import futures
from typing import Optional

import grpc
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import inference_pb2
import inference_pb2_grpc
import common_pb2

# Backward-compat: old embed.proto + reranker.proto service stubs
import embed_pb2
import embed_pb2_grpc
import reranker_pb2
import reranker_pb2_grpc

logging.basicConfig(
    level=logging.INFO,
    format="[dt-inference] %(asctime)s %(levelname)s %(message)s",
)
logger = logging.getLogger("dt-inference")

# ── Default model config ────────────────────────────────────────────────
DEFAULT_EMBED_MODEL = os.environ.get("INFERENCE_EMBED_MODEL", "BAAI/bge-m3")
DEFAULT_RERANKER_MODEL = os.environ.get("INFERENCE_RERANKER_MODEL", "BAAI/bge-reranker-large")
DEVICE = os.environ.get("INFERENCE_DEVICE", "cpu")

MODEL_CACHE_DIR = os.environ.get(
    "INFERENCE_CACHE_DIR",
    os.path.expanduser("~/.cache/digital-twin/models"),
)


# ── Model Registry with lazy loading + auto-download ────────────────────

class ModelRegistry:
    """Manages model lifecycle: auto-download, lazy load, caching."""

    def __init__(self):
        self._embed_model: Optional[object] = None
        self._reranker_model: Optional[object] = None
        self._embed_load_ms: int = 0
        self._reranker_load_ms: int = 0
        self._embed_error: Optional[str] = None
        self._reranker_error: Optional[str] = None
        os.makedirs(MODEL_CACHE_DIR, exist_ok=True)

    # ── Public API ───────────────────────────────────────────────────

    def get_embed_model(self):
        """Lazy load and return the embedding model."""
        if self._embed_model is None and self._embed_error is None:
            self._embed_model, ms, err = self._load_embed()
            self._embed_load_ms = ms
            self._embed_error = err
        if self._embed_error:
            raise RuntimeError(f"Embed model failed to load: {self._embed_error}")
        return self._embed_model

    def get_reranker_model(self):
        """Lazy load and return the reranker model."""
        if self._reranker_model is None and self._reranker_error is None:
            self._reranker_model, ms, err = self._load_reranker()
            self._reranker_load_ms = ms
            self._reranker_error = err
        if self._reranker_error:
            raise RuntimeError(f"Reranker model failed to load: {self._reranker_error}")
        return self._reranker_model

    def status(self) -> dict:
        """Return model status for health check."""
        return {
            DEFAULT_EMBED_MODEL: {
                "loaded": self._embed_model is not None,
                "available": True,
                "load_ms": self._embed_load_ms,
                "error": self._embed_error or "",
            },
            DEFAULT_RERANKER_MODEL: {
                "loaded": self._reranker_model is not None,
                "available": True,
                "load_ms": self._reranker_load_ms,
                "error": self._reranker_error or "",
            },
        }

    def list_models(self) -> list:
        """Return model list."""
        return [
            {"name": DEFAULT_EMBED_MODEL, "type": "embed",
             "loaded": self._embed_model is not None,
             "cached": self._is_cached(DEFAULT_EMBED_MODEL.split("/")[-1]),
             "load_ms": self._embed_load_ms},
            {"name": DEFAULT_RERANKER_MODEL, "type": "reranker",
             "loaded": self._reranker_model is not None,
             "cached": self._is_cached(DEFAULT_RERANKER_MODEL.split("/")[-1]),
             "load_ms": self._reranker_load_ms},
        ]

    # ── Download helpers ─────────────────────────────────────────────

    @staticmethod
    def _ensure_downloaded(model_name: str) -> str:
        """Download model from HuggingFace if not cached. Returns local path."""
        from huggingface_hub import snapshot_download

        # Use huggingface_hub's built-in cache system
        local_path = snapshot_download(
            repo_id=model_name,
            cache_dir=os.path.expanduser("~/.cache/huggingface/hub"),
            resume_download=True,
            ignore_patterns=["*.h5", "*.ot", "*.msgpack"],
        )
        logger.info("Model %s ready at %s", model_name, local_path)
        return local_path

    def _is_cached(self, model_name: str) -> bool:
        """Check if model is already in HF cache."""
        import os
        cache_dir = os.path.expanduser("~/.cache/huggingface/hub")
        model_dir_name = f"models--{model_name.replace('/', '--')}"
        return os.path.isdir(os.path.join(cache_dir, model_dir_name))

    # ── Model loaders ────────────────────────────────────────────────

    def _load_embed(self):
        """Load BGE-M3 embedding model."""
        try:
            logger.info("Loading embed model %s ...", DEFAULT_EMBED_MODEL)
            t0 = time.time()
            self._ensure_downloaded(DEFAULT_EMBED_MODEL)
            from sentence_transformers import SentenceTransformer
            import torch
            model = SentenceTransformer(
                DEFAULT_EMBED_MODEL,
                device=DEVICE,
                trust_remote_code=True,
                local_files_only=False,
                model_kwargs={"torch_dtype": torch.float16},
            )
            model.max_seq_length = 512
            elapsed = int((time.time() - t0) * 1000)
            logger.info("Embed model loaded in %dms (dim=%d)", elapsed, model.get_sentence_embedding_dimension())
            return model, elapsed, None
        except Exception as e:
            logger.error("Embed model load failed: %s", e)
            return None, 0, str(e)

    def _load_reranker(self):
        """Load BGE reranker model."""
        try:
            logger.info("Loading reranker model %s ...", DEFAULT_RERANKER_MODEL)
            t0 = time.time()
            self._ensure_downloaded(DEFAULT_RERANKER_MODEL)
            from FlagEmbedding import FlagReranker
            model = FlagReranker(
                DEFAULT_RERANKER_MODEL,
                use_fp16=(DEVICE == "cuda"),
                device=DEVICE,
            )
            elapsed = int((time.time() - t0) * 1000)
            logger.info("Reranker model loaded in %dms", elapsed)
            return model, elapsed, None
        except Exception as e:
            logger.error("Reranker model load failed: %s", e)
            return None, 0, str(e)


# ── gRPC Service Implementation ────────────────────────────────────────

class InferenceServiceImpl(inference_pb2_grpc.InferenceServiceServicer):
    """Unified inference gRPC service."""

    def __init__(self, registry: ModelRegistry):
        self.registry = registry

    def Embed(self, request, context):
        """Text → vector embedding."""
        texts = list(request.texts)
        if not texts:
            return inference_pb2.EmbedResponse(embeddings=[], model_used=DEFAULT_EMBED_MODEL)

        try:
            model = self.registry.get_embed_model()
            t0 = time.time()
            vectors = model.encode(texts, normalize_embeddings=True, show_progress_bar=False)
            elapsed = int((time.time() - t0) * 1000)

            embeddings = []
            for vec in vectors:
                embedding = inference_pb2.Embedding()
                embedding.vector.extend(vec.tolist() if hasattr(vec, 'tolist') else vec)
                embeddings.append(embedding)

            logger.info("Embed: %d texts → %d vectors in %dms", len(texts), len(embeddings), elapsed)
            return inference_pb2.EmbedResponse(
                embeddings=embeddings,
                model_used=DEFAULT_EMBED_MODEL,
                elapsed_ms=elapsed,
            )
        except Exception as e:
            logger.error("Embed failed: %s", e)
            context.set_code(grpc.StatusCode.INTERNAL)
            context.set_details(str(e))
            return inference_pb2.EmbedResponse()

    def Rerank(self, request, context):
        """Query + candidate texts → relevance scores."""
        query = request.query
        texts = list(request.texts)

        if not query or not texts:
            return inference_pb2.RerankResponse(scores=[], model_used=DEFAULT_RERANKER_MODEL)

        try:
            model = self.registry.get_reranker_model()
            t0 = time.time()
            pairs = [[query, text] for text in texts]
            scores = model.compute_score(pairs)
            elapsed = int((time.time() - t0) * 1000)

            logger.info("Rerank: 1 query x %d texts in %dms", len(texts), elapsed)
            return inference_pb2.RerankResponse(
                scores=[float(s) for s in scores],
                model_used=DEFAULT_RERANKER_MODEL,
                elapsed_ms=elapsed,
            )
        except Exception as e:
            logger.error("Rerank failed: %s", e)
            context.set_code(grpc.StatusCode.INTERNAL)
            context.set_details(str(e))
            return inference_pb2.RerankResponse()

    def Health(self, request, context):
        """Health check with per-model status."""
        status = self.registry.status()
        all_healthy = all(
            not s["error"] for s in status.values()
        )
        return inference_pb2.HealthResponse(
            healthy=all_healthy,
            models={
                name: inference_pb2.ModelStatus(
                    loaded=s["loaded"],
                    available=s["available"],
                    load_ms=s["load_ms"],
                    error=s["error"],
                )
                for name, s in status.items()
            },
        )

    def ListModels(self, request, context):
        """List available models."""
        models = self.registry.list_models()
        return inference_pb2.ListModelsResponse(
            models=[
                inference_pb2.ModelInfo(
                    name=m["name"],
                    type=m["type"],
                    loaded=m["loaded"],
                    cached=m["cached"],
                    load_ms=m["load_ms"],
                )
                for m in models
            ],
        )


# ── Backward-compatible legacy servicers ────────────────────────────────
# These wrap the same ModelRegistry but expose the OLD proto interfaces
# (embed.proto and reranker.proto) so old clients don't need code changes.

class LegacyEmbedServiceImpl(embed_pb2_grpc.EmbedServiceServicer):
    """Backward-compatible wrapper: old embed.proto → new registry."""

    def __init__(self, registry: ModelRegistry):
        self.registry = registry

    def Embed(self, request, context):
        texts = list(request.texts)
        if not texts:
            return embed_pb2.EmbedResponse(embeddings=[])
        try:
            model = self.registry.get_embed_model()
            vectors = model.encode(texts, normalize_embeddings=True, show_progress_bar=False)
            embeddings = []
            for vec in vectors:
                e = embed_pb2.Embedding()
                e.vector.extend(vec.tolist() if hasattr(vec, 'tolist') else vec)
                embeddings.append(e)
            return embed_pb2.EmbedResponse(embeddings=embeddings, model_used=DEFAULT_EMBED_MODEL)
        except Exception as e:
            logger.error("Legacy embed failed: %s", e)
            context.set_code(grpc.StatusCode.INTERNAL)
            context.set_details(str(e))
            return embed_pb2.EmbedResponse()

    def Health(self, request, context):
        return common_pb2.Empty()


class LegacyRerankerServiceImpl(reranker_pb2_grpc.RerankerServiceServicer):
    """Backward-compatible wrapper: old reranker.proto → new registry."""

    def __init__(self, registry: ModelRegistry):
        self.registry = registry

    def Rerank(self, request, context):
        query = request.query
        texts = list(request.texts)
        if not query or not texts:
            return reranker_pb2.RerankResponse(scores=[])
        try:
            model = self.registry.get_reranker_model()
            pairs = [[query, text] for text in texts]
            scores = model.compute_score(pairs)
            return reranker_pb2.RerankResponse(
                scores=[float(s) for s in scores],
                model_used=DEFAULT_RERANKER_MODEL,
            )
        except Exception as e:
            logger.error("Legacy rerank failed: %s", e)
            context.set_code(grpc.StatusCode.INTERNAL)
            context.set_details(str(e))
            return reranker_pb2.RerankResponse()

    def Health(self, request, context):
        return common_pb2.Empty()


# ── Server ─────────────────────────────────────────────────────────────

def serve(port: int = 50051, max_workers: int = 4):
    """Start the unified inference server."""
    registry = ModelRegistry()
    server = grpc.server(
        futures.ThreadPoolExecutor(max_workers=max_workers),
        options=[
            ("grpc.max_send_message_length", 256 * 1024 * 1024),
            ("grpc.max_receive_message_length", 64 * 1024 * 1024),
        ],
    )
    inference_pb2_grpc.add_InferenceServiceServicer_to_server(
        InferenceServiceImpl(registry), server,
    )
    # Backward-compat: old embed.proto + reranker.proto clients
    embed_pb2_grpc.add_EmbedServiceServicer_to_server(
        LegacyEmbedServiceImpl(registry), server,
    )
    reranker_pb2_grpc.add_RerankerServiceServicer_to_server(
        LegacyRerankerServiceImpl(registry), server,
    )

    addr = f"[::1]:{port}"
    server.add_insecure_port(addr)
    server.start()

    logger.info(
        "dt-inference gRPC server listening on %s (workers=%d)",
        addr, max_workers,
    )
    logger.info("  Embed model:   %s", DEFAULT_EMBED_MODEL)
    logger.info("  Reranker model: %s", DEFAULT_RERANKER_MODEL)
    logger.info("  Device: %s", DEVICE)
    logger.info("  Cache dir: %s", MODEL_CACHE_DIR)
    logger.info("  Models loaded lazily on first request")

    try:
        server.wait_for_termination()
    except KeyboardInterrupt:
        logger.info("Shutting down...")
        server.stop(grace=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="dt-inference gRPC server")
    parser.add_argument("--port", type=int, default=50051, help="gRPC listen port (default: 50051)")
    parser.add_argument("--workers", type=int, default=4, help="Max thread pool workers (default: 4)")
    parser.add_argument("--device", default=None, help="Device override (cuda/cpu)")
    args = parser.parse_args()

    if args.device:
        os.environ["INFERENCE_DEVICE"] = args.device

    serve(port=args.port, max_workers=args.workers)
