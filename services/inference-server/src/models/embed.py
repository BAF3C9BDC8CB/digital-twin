"""BGE-M3 embedding model loader."""

import logging
import os
import time

logger = logging.getLogger("dt-inference.models.embed")

DEFAULT_EMBED_MODEL = os.environ.get("INFERENCE_EMBED_MODEL", "BAAI/bge-m3")
DEFAULT_DEVICE = os.environ.get("INFERENCE_DEVICE", "cpu")


def load_embed_model():
    """Load BGE-M3 sentence transformer. Returns (model, load_ms, error)."""
    from .loader import ensure_downloaded

    logger.info("Loading embed: %s (device=%s)", DEFAULT_EMBED_MODEL, DEFAULT_DEVICE)
    t0 = time.time()

    try:
        from sentence_transformers import SentenceTransformer
        import torch

        try:
            model = SentenceTransformer(
                DEFAULT_EMBED_MODEL, device=DEFAULT_DEVICE,
                trust_remote_code=True, local_files_only=True,
                model_kwargs={"torch_dtype": torch.float16},
            )
        except Exception:
            logger.info("Model not cached, downloading...")
            ensure_downloaded(DEFAULT_EMBED_MODEL)
            model = SentenceTransformer(
                DEFAULT_EMBED_MODEL, device=DEFAULT_DEVICE,
                trust_remote_code=True,
                model_kwargs={"torch_dtype": torch.float16},
            )

        model.max_seq_length = 512
        elapsed = int((time.time() - t0) * 1000)
        dim = model.get_sentence_embedding_dimension()
        logger.info("Embed loaded in %dms (dim=%d)", elapsed, dim)
        return model, elapsed, None

    except Exception as e:
        logger.error("Embed load failed: %s", e)
        return None, 0, str(e)
