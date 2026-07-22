"""Model registry: plugin registration, lazy loading, idle eviction."""

import logging
import os
import time
from typing import Dict, Optional

import torch

from .spec import ModelSpec, LoadedModel

logger = logging.getLogger("dt-inference.models.registry")


class ModelRegistry:
    """Manages model lifecycle: registration, lazy load, eviction, status."""

    def __init__(self):
        self._specs: Dict[str, ModelSpec] = {}
        self._loaded: Dict[str, LoadedModel] = {}
        os.makedirs(
            os.environ.get("INFERENCE_CACHE_DIR",
                           os.path.expanduser("~/.cache/digital-twin/models")),
            exist_ok=True,
        )

    def register(self, spec: ModelSpec):
        """Register a model specification."""
        self._specs[spec.name] = spec
        logger.debug("Registered model: %s (type=%s)", spec.name, spec.model_type)

    def get(self, name: str) -> LoadedModel:
        """Get or lazy-load a model by name.

        Raises RuntimeError if the model fails to load.
        """
        if name not in self._specs:
            raise KeyError(f"Model not registered: {name}")

        spec = self._specs[name]

        if name in self._loaded:
            loaded = self._loaded[name]
            if loaded.load_error:
                raise RuntimeError(f"Model {name} load error: {loaded.load_error}")
            loaded.touch()
            return loaded

        # Lazy load
        loaded = self._load(spec)
        if loaded.load_error:
            self._loaded[name] = loaded  # cache the error too
            raise RuntimeError(f"Model {name} load failed: {loaded.load_error}")
        self._loaded[name] = loaded
        return loaded

    def _load(self, spec: ModelSpec) -> LoadedModel:
        """Execute model loader and wrap result."""
        try:
            model, load_ms, error = spec.loader()
        except Exception as e:
            return LoadedModel(spec=spec, model=None, load_ms=0, load_error=str(e))

        return LoadedModel(spec=spec, model=model, load_ms=load_ms, load_error=error)

    def get_embed_model(self):
        """Convenience: get the embed model registered with type='embed'."""
        for spec in self._specs.values():
            if spec.model_type == "embed":
                return self.get(spec.name).model
        raise RuntimeError("No embed model registered")

    def get_reranker_model(self):
        """Convenience: get the reranker model registered with type='reranker'."""
        for spec in self._specs.values():
            if spec.model_type == "reranker":
                return self.get(spec.name).model
        raise RuntimeError("No reranker model registered")

    def get_llm_model(self):
        """Convenience: get the (model, tokenizer) tuple for the registered LLM."""
        for spec in self._specs.values():
            if spec.model_type == "llm":
                return self.get(spec.name).model
        raise RuntimeError("No LLM model registered")

    def get_by_type(self, model_type: str):
        """Get the loaded model for a given type."""
        for spec in self._specs.values():
            if spec.model_type == model_type:
                return self.get(spec.name).model
        raise RuntimeError(f"No model registered for type: {model_type}")

    async def evict_idle(self):
        """Unload models that have been idle longer than their idle_ttl_sec."""
        now = time.time()
        evicted = []
        for name in list(self._loaded.keys()):
            loaded = self._loaded[name]
            if loaded.spec.idle_ttl_sec <= 0:
                continue
            if now - loaded.last_used > loaded.spec.idle_ttl_sec:
                del self._loaded[name]
                evicted.append(name)
                logger.info("evicted idle model: %s (idle %.0fs)",
                            name, now - loaded.last_used)
        if evicted and torch.cuda.is_available():
            torch.cuda.empty_cache()

    def status(self) -> dict:
        """Return status of all registered models."""
        result = {}
        for name, spec in self._specs.items():
            if name in self._loaded:
                loaded = self._loaded[name]
                result[name] = {
                    "type": spec.model_type,
                    "loaded": loaded.is_loaded,
                    "load_ms": loaded.load_ms,
                    "error": loaded.load_error or "",
                    "idle_seconds": int(loaded.idle_seconds),
                }
            else:
                result[name] = {
                    "type": spec.model_type,
                    "loaded": False,
                    "load_ms": 0,
                    "error": "",
                    "idle_seconds": 0,
                }
        return result

    @property
    def is_healthy(self) -> bool:
        """All registered models loaded without errors."""
        status = self.status()
        return all(not s["error"] for s in status.values())
