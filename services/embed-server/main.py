"""
代码嵌入服务 - 接收文本返回向量
HTTP API:
  POST /embed          { text: "..." }            → { vector: [0.1,...] }
  POST /embed-batch    { texts: ["...","..."] }   → { vectors: [[0.1,...],...] }
  GET  /health                                    → { model: "...", dim: 768, status: "ok" }
"""

import os
import sys
import time
from contextlib import asynccontextmanager

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from sentence_transformers import SentenceTransformer

MODEL_NAME = os.environ.get("EMBED_MODEL", "BAAI/bge-base-zh-v1.5")
HOST = os.environ.get("EMBED_HOST", "0.0.0.0")
PORT = int(os.environ.get("EMBED_PORT", "8001"))
MAX_BATCH = int(os.environ.get("EMBED_MAX_BATCH", "512"))


model = None


class EmbedRequest(BaseModel):
    text: str


class EmbedBatchRequest(BaseModel):
    texts: list[str]


class EmbedResponse(BaseModel):
    vector: list[float]


class EmbedBatchResponse(BaseModel):
    vectors: list[list[float]]
    dim: int


@asynccontextmanager
async def lifespan(app: FastAPI):
    global model
    print(f"[加载] 模型 {MODEL_NAME} ...")
    t0 = time.time()
    model = SentenceTransformer(
        MODEL_NAME,
        device="cpu",
        trust_remote_code=True,
    )
    model.max_seq_length = 512
    dim = model.get_sentence_embedding_dimension()
    print(f"[就绪] {MODEL_NAME} dim={dim} 加载耗时 {time.time()-t0:.1f}s")
    yield
    model = None
    print("[卸载] 模型已释放")


app = FastAPI(title="Code Embed Server", version="1.0", lifespan=lifespan)


@app.get("/health")
async def health():
    if model is None:
        raise HTTPException(503, "模型未就绪")
    return {
        "status": "ok",
        "model": MODEL_NAME,
        "dim": model.get_sentence_embedding_dimension(),
    }


@app.post("/embed", response_model=EmbedResponse)
async def embed(req: EmbedRequest):
    if model is None:
        raise HTTPException(503, "模型未就绪")
    vec = model.encode([req.text], normalize_embeddings=True)[0]
    return EmbedResponse(vector=vec.tolist())


@app.post("/embed-batch", response_model=EmbedBatchResponse)
async def embed_batch(req: EmbedBatchRequest):
    if model is None:
        raise HTTPException(503, "模型未就绪")
    if not req.texts:
        return EmbedBatchResponse(vectors=[], dim=model.get_sentence_embedding_dimension())
    if len(req.texts) > MAX_BATCH:
        raise HTTPException(413, f"批次过大，上限 {MAX_BATCH}，实际 {len(req.texts)}")

    vecs = model.encode(req.texts, normalize_embeddings=True, show_progress_bar=False)
    return EmbedBatchResponse(
        vectors=[v.tolist() for v in vecs],
        dim=model.get_sentence_embedding_dimension(),
    )


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
