"""Inference Worker: async event loop + GPU thread pool isolation."""

import asyncio
import logging
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Optional

from .router import LOW_BATCH_SIZE, LOW_FLUSH_SEC, TaskRouter
from .task import InferenceTask, Priority

logger = logging.getLogger("dt-inference.worker")


class InferenceWorker:
    """Async worker that processes tasks from the router queue.

    Uses ThreadPoolExecutor for GPU inference to avoid blocking
    the asyncio event loop.
    """

    def __init__(
        self,
        router: TaskRouter,
        executor: Optional[ThreadPoolExecutor] = None,
    ):
        self.router = router
        self.executor = executor or ThreadPoolExecutor(max_workers=2, thread_name_prefix="infer-")
        self._worker_task: Optional[asyncio.Task] = None

    async def start(self):
        self._worker_task = asyncio.create_task(self.run())
        logger.info("InferenceWorker started")

    async def stop(self):
        if self._worker_task:
            self._worker_task.cancel()
            try:
                await self._worker_task
            except asyncio.CancelledError:
                pass
        self.executor.shutdown(wait=False)
        logger.info("InferenceWorker stopped")

    async def run(self):
        """Main worker loop with priority-biased dequeue and batching."""
        low_batch: list[InferenceTask] = []
        normal_embed_batch: list[InferenceTask] = []
        NORMAL_EMBED_MAX = 8
        NORMAL_EMBED_TIMEOUT = 0.1

        while self.router.running:
            try:
                task = await self._dequeue_with_timeout(
                    low_batch, normal_embed_batch,
                    NORMAL_EMBED_MAX, NORMAL_EMBED_TIMEOUT,
                )
            except asyncio.TimeoutError:
                # No new tasks within timeout window — flush accumulated batches
                if normal_embed_batch:
                    await self._process_normal_embed_batch(normal_embed_batch)
                    normal_embed_batch.clear()
                if low_batch:
                    await self._batch_infer(low_batch)
                    low_batch.clear()
                continue

            if task.priority == Priority.HIGH:
                # HIGH: process immediately, single
                await self._process_one(task)

            elif task.priority == Priority.NORMAL:
                if task.task_type == "embed":
                    # NORMAL embed: accumulate for small batch
                    normal_embed_batch.append(task)
                    if len(normal_embed_batch) >= NORMAL_EMBED_MAX:
                        await self._process_normal_embed_batch(normal_embed_batch)
                        normal_embed_batch.clear()
                else:
                    await self._process_one(task)

            elif task.priority == Priority.LOW:
                low_batch.append(task)
                if len(low_batch) >= LOW_BATCH_SIZE:
                    await self._batch_infer(low_batch)
                    low_batch.clear()

        # Final drain
        if normal_embed_batch:
            await self._process_normal_embed_batch(normal_embed_batch)
        if low_batch:
            await self._batch_infer(low_batch)

    # ── Dequeue ───────────────────────────────────────────────────────────

    async def _dequeue_with_timeout(
        self,
        low_batch: list,
        normal_embed_batch: list,
        normal_embed_max: int,
        normal_embed_timeout: float,
    ) -> InferenceTask:
        """Wait for next task, with flush timeout for accumulated batches."""
        has_pending = bool(low_batch) or bool(normal_embed_batch)
        timeout = LOW_FLUSH_SEC if has_pending else None

        while True:
            try:
                if timeout is not None:
                    task = await asyncio.wait_for(
                        self.router.queue.get(), timeout=timeout
                    )
                else:
                    task = await self.router.queue.get()
                return task
            except asyncio.TimeoutError:
                # Timeout — flush and restart
                if normal_embed_batch:
                    await self._process_normal_embed_batch(normal_embed_batch)
                    normal_embed_batch.clear()
                if low_batch:
                    await self._batch_infer(low_batch)
                    low_batch.clear()
                timeout = None  # No more pending, wait indefinitely
                continue

    # ── Single task processing ────────────────────────────────────────────

    async def _process_one(self, task: InferenceTask):
        """Process a single HIGH or NORMAL task via executor thread."""
        loop = asyncio.get_event_loop()
        try:
            result = await loop.run_in_executor(
                self.executor, self.router.dispatch, task
            )
            if task.future and not task.future.done():
                task.future.set_result(result)
        except Exception as e:
            logger.error("Task %s (%s) failed: %s", task.task_id, task.task_type, e)
            if task.future and not task.future.done():
                task.future.set_exception(e)

    async def _process_normal_embed_batch(self, tasks: list[InferenceTask]):
        """Process a batch of NORMAL priority embed tasks."""
        if not tasks:
            return
        t0 = time.time()
        results = self.router.batch_embed(tasks)
        elapsed = time.time() - t0
        logger.debug("NORMAL embed batch: %d tasks in %.1fs", len(tasks), elapsed)
        for t in tasks:
            if t.task_id in results and t.future and not t.future.done():
                t.future.set_result(results[t.task_id])

    # ── Batch processing ──────────────────────────────────────────────────

    async def _batch_infer(self, tasks: list[InferenceTask]):
        """Process a batch of LOW-priority tasks."""
        if not tasks:
            return
        t0 = time.time()
        embed_tasks = [t for t in tasks if t.task_type == "embed"]
        other_tasks = [t for t in tasks if t.task_type != "embed"]

        # Batch embed tasks together
        if embed_tasks:
            results = self.router.batch_embed(embed_tasks)
            for t in embed_tasks:
                if t.task_id in results and t.future and not t.future.done():
                    t.future.set_result(results[t.task_id])

        # Process other tasks individually
        for t in other_tasks:
            await self._process_one(t)

        elapsed = time.time() - t0
        logger.debug("LOW batch: %d tasks in %.1fs", len(tasks), elapsed)
