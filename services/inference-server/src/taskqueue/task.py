"""Task definitions and priority enum."""

import time
import uuid
from asyncio import Future
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Optional


class Priority(IntEnum):
    LOW = 0       # background sync / fire-and-forget
    NORMAL = 1    # code indexing / build
    HIGH = 2      # user search / query


@dataclass
class InferenceTask:
    """A task routed through the inference queue."""

    task_id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])
    task_type: str = ""         # "embed", "rerank", "chat"
    model_name: str = ""
    payload: dict = field(default_factory=dict)
    priority: Priority = Priority.NORMAL
    future: Optional[Future] = None  # None = fire-and-forget
    created_at: float = field(default_factory=time.time)
