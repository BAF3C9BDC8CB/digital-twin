"""
批处理流水线：将大量文本分块编码，支持进度回调。
无 HTTP / 队列依赖，纯本地计算。
"""

import gc
import os
import logging
import time
from typing import Callable, Optional

import numpy as np

from .engine import get_engine, EmbedEngine

logger = logging.getLogger("dt_embed.pipeline")

# 默认每块文本数（队列分块，非 GPU 前向 batch）
DEFAULT_CHUNK_SIZE = int(os.environ.get("EMBED_CHUNK_SIZE", "4096"))


def _chunk_list(lst: list, size: int) -> list[list]:
    """将列表切分为等大小的块。"""
    return [lst[i:i + size] for i in range(0, len(lst), size)]


class Pipeline:
    """单次批量编码任务，支持进度回调。"""

    def __init__(self, texts: list[str], chunk_size: int = DEFAULT_CHUNK_SIZE,
                 on_progress: Optional[Callable[[int, int], None]] = None):
        """
        Args:
            texts: 待编码文本列表
            chunk_size: 每块文本数（仅影响回调粒度）
            on_progress: 进度回调 (done_count, total)
        """
        self.texts = texts
        self.total = len(texts)
        self.chunk_size = chunk_size
        self.on_progress = on_progress

        self._chunks = _chunk_list(texts, chunk_size)
        self._done = 0
        self._batches: list[np.ndarray] = []   # 每块一个 (N, dim) 数组
        self._elapsed = 0.0
        self._error: Optional[str] = None

    @property
    def done_count(self) -> int:
        return self._done

    @property
    def progress_pct(self) -> float:
        return round(self._done / max(self.total, 1) * 100, 1)

    @property
    def error(self) -> Optional[str]:
        return self._error

    def run(self, engine: Optional[EmbedEngine] = None):
        """执行编码，结果存入 self._batches。"""
        if engine is None:
            engine = get_engine()
        if not engine.ready:
            raise RuntimeError("模型未加载")

        t0 = time.time()
        for i, chunk in enumerate(self._chunks):
            try:
                vecs = engine.encode(chunk)
                self._batches.append(vecs)
            except Exception as e:
                self._error = f"块 {i}: {e}"
                raise

            self._done += len(chunk)
            if self.on_progress:
                self.on_progress(self._done, self.total)

            if i % 10 == 0 and i > 0:
                gc.collect()

        self._elapsed = time.time() - t0
        logger.info("编码完成 %d 条 %.1fs (%.0f 条/s)",
                     self.total, self._elapsed,
                     self.total / max(self._elapsed, 0.001))

    def result_as_list(self) -> list[list[float]]:
        """返回 list[list[float]]，供 JSON 序列化。"""
        if self._error:
            raise RuntimeError(f"编码失败: {self._error}")
        if not self._batches:
            return []
        all_vecs = np.vstack(self._batches)
        return all_vecs.tolist()

    def result_as_numpy(self) -> np.ndarray:
        """返回 (N, dim) numpy 数组。"""
        if self._error:
            raise RuntimeError(f"编码失败: {self._error}")
        if not self._batches:
            return np.empty((0, 0))
        return np.vstack(self._batches)
