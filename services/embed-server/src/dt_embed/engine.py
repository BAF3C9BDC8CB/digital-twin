"""
GPU 模型加载与推理核心。
无 HTTP 依赖，可被 CLI / Server / 库直接导入。
"""

import gc
import os
import sys
import time
import logging
from typing import Optional

import numpy as np

logger = logging.getLogger("dt_embed.engine")

# ── 环境配置 ────────────────────────────────────────────────────────────────
os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

MODEL_NAME = os.environ.get("EMBED_MODEL", "BAAI/bge-m3")
DEVICE = os.environ.get("EMBED_DEVICE", "cuda")
_LOCAL_ONLY = os.environ.get("EMBED_OFFLINE", "1") == "1"
_USE_FP16 = os.environ.get("EMBED_FP16", "1") == "1"
_USE_COMPILE = os.environ.get("EMBED_COMPILE", "0") == "1"   # 默认关, 不够稳定
_INFER_BATCH = int(os.environ.get("EMBED_INFER_BATCH", "256"))


# ── 全局单例 ────────────────────────────────────────────────────────────────
_engine: Optional["EmbedEngine"] = None


def get_engine() -> "EmbedEngine":
    """获取全局单例引擎（惰性加载, 线程安全）"""
    global _engine
    if _engine is None:
        _engine = EmbedEngine()
    return _engine


# ── 引擎 ────────────────────────────────────────────────────────────────────
class EmbedEngine:
    """封装 sentence-transformers 模型的加载与推理。"""

    def __init__(self):
        self._model = None
        self._dim = 0
        self._model_name = MODEL_NAME

    # ── 属性 ────────────────────────────────────────────────────────────────
    @property
    def dim(self) -> int:
        return self._dim

    @property
    def model_name(self) -> str:
        return self._model_name

    @property
    def fp16(self) -> bool:
        return _USE_FP16

    @property
    def compiled(self) -> bool:
        return _USE_COMPILE

    @property
    def ready(self) -> bool:
        return self._model is not None

    # ── 加载 ────────────────────────────────────────────────────────────────
    def load(self):
        """加载模型并 warmup。"""
        import torch
        from sentence_transformers import SentenceTransformer

        t0 = time.time()

        # dtype
        if _USE_FP16 and DEVICE == "cuda" and torch.cuda.is_available():
            model_kwargs = {"torch_dtype": torch.float16}
            logger.info("FP16 推理已启用")
        else:
            model_kwargs = {}
            logger.info("FP32 推理")

        m = SentenceTransformer(
            MODEL_NAME,
            device=DEVICE,
            trust_remote_code=True,
            local_files_only=_LOCAL_ONLY,
            model_kwargs=model_kwargs,
        )
        try:
            m.max_seq_length = 512
        except Exception:
            pass

        self._dim = m.get_sentence_embedding_dimension()
        display = MODEL_NAME.split("/")[-1]

        # torch.compile (可选)
        if _USE_COMPILE and DEVICE == "cuda":
            logger.info("torch.compile 中 ...")
            try:
                fm = m._first_module()
                if hasattr(fm, "auto_model"):
                    fm.auto_model = torch.compile(
                        fm.auto_model, mode="reduce-overhead", dynamic=True
                    )
                    logger.info("torch.compile 已应用")
            except Exception as e:
                logger.warning("torch.compile 跳过: %s", e)

        # warmup
        logger.info("Warmup CUDA kernel ...")
        w0 = time.time()
        warmup_texts = ["warmup"] * 32
        with torch.inference_mode():
            for _ in range(3):
                m.encode(warmup_texts, normalize_embeddings=True,
                         show_progress_bar=False)
        logger.info("Warmup 完成 %.1fs", time.time() - w0)

        self._model = m
        logger.info("模型 %s dim=%d 加载完成 %.1fs", display, self._dim, time.time() - t0)

    # ── 推理 ────────────────────────────────────────────────────────────────
    def encode(self, texts: list[str]) -> np.ndarray:
        """编码文本列表 → (N, dim) numpy float16/32 数组。"""
        if not self.ready:
            raise RuntimeError("模型未加载，请先调用 load()")
        import torch
        with torch.inference_mode():
            vecs = self._model.encode(
                texts,
                normalize_embeddings=True,
                show_progress_bar=False,
                batch_size=_INFER_BATCH,
                convert_to_tensor=False,
                convert_to_numpy=True,
            )
        return vecs

    def encode_single(self, text: str, as_list: bool = False):
        """编码单条文本。"""
        vec = self.encode([text])[0]
        return vec.tolist() if as_list else vec

    # ── 清理 ────────────────────────────────────────────────────────────────
    def unload(self):
        self._model = None
        gc.collect()
        import torch
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
        logger.info("模型已释放")
