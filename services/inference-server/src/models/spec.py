"""Model specification and loaded model container."""

import time
from dataclasses import dataclass, field
from typing import Any, Callable, Optional


@dataclass
class ModelSpec:
    """Descriptor for a model registered in the registry."""

    name: str                               # "BAAI/bge-m3"
    model_type: str                         # "embed" | "rerank" | "llm" | "nlp"
    loader: Callable[[], Any]               # sync loader → returns (model, load_ms, error)
    device: str = "cpu"
    idle_ttl_sec: int = 0                   # 0 = never unload
    batch_capable: bool = False


@dataclass
class LoadedModel:
    """A loaded model instance with usage tracking."""

    spec: ModelSpec
    model: Any
    load_ms: int = 0
    load_error: Optional[str] = None
    last_used: float = field(default_factory=time.time)

    def touch(self):
        self.last_used = time.time()

    @property
    def is_loaded(self) -> bool:
        return self.model is not None and self.load_error is None

    @property
    def idle_seconds(self) -> float:
        return time.time() - self.last_used
