"""Task router: priority queue with batching, model dispatch."""

import asyncio
import logging
import os
import time
from typing import Optional

from .task import InferenceTask, Priority

logger = logging.getLogger("dt-inference.queue")

QUEUE_MAXSIZE = int(os.environ.get("INFERENCE_QUEUE_SIZE", "200"))
LOW_BATCH_SIZE = int(os.environ.get("INFERENCE_LOW_BATCH", "64"))
LOW_FLUSH_SEC = float(os.environ.get("INFERENCE_LOW_FLUSH_SEC", "0.5"))


class TaskRouter:
    """async queue with priority lanes, batching, and model dispatch."""

    def __init__(self, registry):
        from models.registry import ModelRegistry
        self.registry: ModelRegistry = registry
        self._queue: asyncio.Queue[InferenceTask] = asyncio.Queue(maxsize=QUEUE_MAXSIZE)
        self._running = False

    async def start(self):
        self._running = True
        logger.info("TaskRouter started (maxsize=%d, low_batch=%d, low_flush=%.1fs)",
                     QUEUE_MAXSIZE, LOW_BATCH_SIZE, LOW_FLUSH_SEC)

    async def stop(self):
        self._running = False
        logger.info("TaskRouter stopped")

    @property
    def queue(self) -> asyncio.Queue:
        return self._queue

    @property
    def running(self) -> bool:
        return self._running

    # ── Public API ───────────────────────────────────────────────────────

    async def submit(
        self,
        task_type: str,
        payload: dict,
        priority: Priority = Priority.NORMAL,
        sync: bool = True,
    ) -> Optional[dict]:
        """Submit a task. sync=True returns result dict, sync=False returns task_id."""
        from models.embed import DEFAULT_EMBED_MODEL
        from models.reranker import DEFAULT_RERANKER_MODEL
        from models.llm import DEFAULT_LLM_MODEL

        model_map = {
            "embed": DEFAULT_EMBED_MODEL,
            "rerank": DEFAULT_RERANKER_MODEL,
            "chat": DEFAULT_LLM_MODEL,
        }

        task = InferenceTask(
            task_type=task_type,
            model_name=model_map.get(task_type, ""),
            payload=payload,
            priority=priority,
        )
        if sync:
            task.future = asyncio.Future()
            await self._queue.put(task)
            return await task.future
        else:
            await self._queue.put(task)
            return {"task_id": task.task_id, "status": "queued"}

    # ── Dispatch (sync, called from executor thread) ─────────────────────

    def dispatch(self, task: InferenceTask) -> dict:
        """Route task to model handler (synchronous, in executor thread)."""
        payload = task.payload

        if task.task_type == "embed":
            return self._dispatch_embed(payload)
        elif task.task_type == "rerank":
            return self._dispatch_rerank(payload)
        elif task.task_type == "chat":
            return self._dispatch_chat(payload)
        else:
            raise ValueError(f"Unknown task type: {task.task_type}")

    # ── Batching helpers (called by Worker) ──────────────────────────────

    def batch_embed(self, tasks: list[InferenceTask]) -> dict:
        """Batch embed multiple tasks together. Returns dict task_id→result."""
        from models.embed import DEFAULT_EMBED_MODEL

        all_texts = []
        task_map = []
        for t in tasks:
            start = len(all_texts)
            texts = t.payload.get("texts", [])
            all_texts.extend(texts)
            task_map.append((t, start, len(texts)))

        if not all_texts:
            return {}

        model = self.registry.get_embed_model()
        import numpy as np
        vectors = model.encode(
            all_texts, normalize_embeddings=True, show_progress_bar=False
        )
        results = {}
        for t, start, count in task_map:
            results[t.task_id] = {
                "embeddings": vectors[start:start + count].tolist(),
                "model": DEFAULT_EMBED_MODEL,
            }
        return results

    # ── Private dispatchers ───────────────────────────────────────────────

    def _dispatch_embed(self, payload: dict) -> dict:
        from models.embed import DEFAULT_EMBED_MODEL

        texts = payload.get("texts", [])
        model = self.registry.get_embed_model()
        t0 = time.time()
        vectors = model.encode(
            texts, normalize_embeddings=True, show_progress_bar=False
        )
        elapsed_ms = int((time.time() - t0) * 1000)
        logger.info("Embed: %d texts → %d vectors in %dms",
                     len(texts), len(vectors), elapsed_ms)
        return {
            "embeddings": [v.tolist() for v in vectors],
            "model": DEFAULT_EMBED_MODEL,
            "elapsed_ms": elapsed_ms,
        }

    def _dispatch_rerank(self, payload: dict) -> dict:
        from models.reranker import DEFAULT_RERANKER_MODEL

        query = payload.get("query", "")
        texts = payload.get("texts", [])
        model = self.registry.get_reranker_model()
        t0 = time.time()
        pairs = [[query, t] for t in texts]
        scores = model.compute_score(pairs)
        elapsed_ms = int((time.time() - t0) * 1000)
        logger.info("Rerank: 1 query × %d texts in %dms", len(texts), elapsed_ms)
        return {
            "scores": [float(s) for s in scores],
            "model": DEFAULT_RERANKER_MODEL,
            "elapsed_ms": elapsed_ms,
        }

    def _dispatch_chat(self, payload: dict) -> dict:
        from models.llm import DEFAULT_LLM_MODEL

        messages = payload.get("messages", [])
        max_tokens = payload.get("max_tokens", 512)
        temperature = payload.get("temperature", 0.7)
        model, tokenizer = self.registry.get_llm_model()
        t0 = time.time()

        import torch
        prompt = tokenizer.apply_chat_template(
            messages, tokenize=False, add_generation_prompt=True
        )
        inputs = tokenizer(prompt, return_tensors="pt").to(model.device)
        with torch.no_grad():
            outputs = model.generate(
                **inputs,
                max_new_tokens=max_tokens,
                temperature=temperature,
                do_sample=temperature > 0,
                pad_token_id=tokenizer.eos_token_id,
            )
        response_text = tokenizer.decode(
            outputs[0][inputs["input_ids"].shape[1]:],
            skip_special_tokens=True,
        )
        elapsed_ms = int((time.time() - t0) * 1000)
        logger.info("Chat: %d tokens in %dms",
                     len(outputs[0]) - inputs["input_ids"].shape[1], elapsed_ms)
        return {
            "response": response_text,
            "model": DEFAULT_LLM_MODEL,
            "elapsed_ms": elapsed_ms,
        }
