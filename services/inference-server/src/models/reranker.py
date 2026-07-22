"""BGE-reranker model loader."""

import logging
import os
import time

logger = logging.getLogger("dt-inference.models.reranker")

DEFAULT_RERANKER_MODEL = os.environ.get(
    "INFERENCE_RERANKER_MODEL", "BAAI/bge-reranker-large"
)
DEFAULT_DEVICE = os.environ.get("INFERENCE_DEVICE", "cpu")


def load_reranker_model():
    """Load BGE-reranker. Returns (model, load_ms, error)."""
    from .loader import ensure_downloaded

    logger.info("Loading reranker: %s", DEFAULT_RERANKER_MODEL)
    t0 = time.time()

    try:
        from FlagEmbedding import FlagReranker

        try:
            model = FlagReranker(
                DEFAULT_RERANKER_MODEL,
                use_fp16=(DEFAULT_DEVICE == "cuda"),
                device=DEFAULT_DEVICE,
                local_files_only=True,
            )
        except Exception:
            logger.info("Reranker not cached, downloading...")
            ensure_downloaded(DEFAULT_RERANKER_MODEL)
            model = FlagReranker(
                DEFAULT_RERANKER_MODEL,
                use_fp16=(DEFAULT_DEVICE == "cuda"),
                device=DEFAULT_DEVICE,
            )

        elapsed = int((time.time() - t0) * 1000)
        logger.info("Reranker loaded in %dms", elapsed)
        return model, elapsed, None

    except Exception as e:
        logger.error("Reranker load failed: %s", e)
        return None, 0, str(e)
