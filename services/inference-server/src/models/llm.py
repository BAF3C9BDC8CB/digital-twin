"""Qwen3-4B LLM model loader with 4-bit quantization."""

import logging
import os
import time

logger = logging.getLogger("dt-inference.models.llm")

DEFAULT_LLM_MODEL = os.environ.get("INFERENCE_LLM_MODEL", "Qwen/Qwen3-4B")
DEFAULT_DEVICE = os.environ.get("INFERENCE_LLM_DEVICE", "cuda")


def load_llm_model():
    """Load Qwen3-4B with 4-bit quantization. Returns ((model, tokenizer), load_ms, error)."""
    from .loader import ensure_downloaded

    logger.info("Loading LLM: %s (device=%s, 4-bit)", DEFAULT_LLM_MODEL, DEFAULT_DEVICE)
    t0 = time.time()

    try:
        import torch
        from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig

        bnb_config = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_compute_dtype=torch.float16,
            bnb_4bit_use_double_quant=True,
            bnb_4bit_quant_type="nf4",
        )

        try:
            tokenizer = AutoTokenizer.from_pretrained(
                DEFAULT_LLM_MODEL, trust_remote_code=True, local_files_only=True,
            )
            model = AutoModelForCausalLM.from_pretrained(
                DEFAULT_LLM_MODEL,
                quantization_config=bnb_config,
                device_map="auto",
                trust_remote_code=True,
                local_files_only=True,
            )
        except Exception:
            logger.info("LLM not cached, downloading...")
            ensure_downloaded(DEFAULT_LLM_MODEL)
            tokenizer = AutoTokenizer.from_pretrained(
                DEFAULT_LLM_MODEL, trust_remote_code=True,
            )
            model = AutoModelForCausalLM.from_pretrained(
                DEFAULT_LLM_MODEL,
                quantization_config=bnb_config,
                device_map="auto",
                trust_remote_code=True,
            )

        elapsed = int((time.time() - t0) * 1000)
        logger.info("LLM loaded in %dms (%s)", elapsed, DEFAULT_LLM_MODEL)
        return (model, tokenizer), elapsed, None

    except Exception as e:
        logger.error("LLM load failed: %s", e)
        return (None, None), 0, str(e)
