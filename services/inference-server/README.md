# dt-inference-server

统一模型推理服务，提供 **Embed / Rerank / LLM Chat** 三种能力，内置优先级异步队列。

## 架构

```
                     ┌──────────────┐
   gRPC :50051 ─────►│  EmbedService│──┐
                      │  (legacy)    │  │
                      └──────────────┘  │     ┌─────────────┐
                                         ├────►│ TaskRouter  │
                      ┌──────────────┐  │     │             │
   REST :50052 ──────►│  /v1/embed   │──┤     │ asyncio     │
                      │  /v1/rerank  │──┼────►│ .Queue      │──► ModelRegistry
                      │  /v1/chat    │──┘     │             │     ├─ BGE-M3
                      │  /health     │        │ HIGH/NORMAL │     ├─ BGE-reranker
                      └──────────────┘        │ /LOW lanes  │     └─ Qwen3-4B
                                              └─────────────┘
```

## 启动

```bash
cd services/inference-server/src
python3 server.py --port 50051 --llm-port 50052
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--port` | 50051 | gRPC embed 服务端口 |
| `--llm-port` | 50052 | REST API 端口 (chat/rerank/health) |
| `--workers` | 4 | gRPC 线程池大小 |
| `--device` | cpu | embed/reranker 设备 (cuda/cpu) |

模型首次请求时懒加载，启动不占显存。

---

## API

### Embedding

**gRPC** (旧 proto 兼容，Rust dt-daemon 直连)：

```python
stub = EmbedServiceStub(channel)
resp = stub.Embed(EmbedRequest(texts=["hello", "world"]))
# resp.embeddings[0].vector  →  [0.12, -0.34, ...]  (1024 维)
```

**REST**（同步 / 异步）：

```bash
# 同步
curl -X POST localhost:50052/v1/embeddings \
  -d '{"input": ["hello", "world"]}'
# → {"data": [{"embedding": [...], "index": 0}, ...], "model": "BAAI/bge-m3"}

# 异步 (fire-and-forget, 用于后台索引)
curl -X POST localhost:50052/v1/embeddings \
  -d '{"input": ["hello"], "async": true}'
# → {"task_id": "a1b2c3d4e5f6", "status": "queued"}
```

### Rerank

```bash
curl -X POST localhost:50052/v1/rerank \
  -d '{"query": "AI 是什么？", "texts": ["AI 是人工智能", "我喜欢披萨", "机器学习很有趣"]}'
# → {"data": [{"index": 0, "score": 6.48}, ...]}
```

### LLM Chat

OpenAI 兼容格式：

```bash
curl -X POST localhost:50052/v1/chat/completions \
  -d '{
    "messages": [{"role": "user", "content": "你好"}],
    "max_tokens": 100,
    "temperature": 0.7
  }'
# → {"choices": [{"message": {"content": "..."}}], "model": "Qwen/Qwen3-4B"}
```

### Health

```bash
curl localhost:50052/health
# → {"status": "healthy", "models": {"BAAI/bge-m3": {"loaded": true, ...}, ...}}
```

---

## 队列与优先级

所有推理请求通过 `TaskRouter` 入队，三级优先级调度：

| 优先级 | 值 | 行为 | 典型场景 |
|--------|-----|------|----------|
| **HIGH** | 2 | 立即处理，不攒批 | 用户搜索、实时查询 |
| **NORMAL** | 1 | 立即处理，不攒批 | 代码索引 (dt build) |
| **LOW** | 0 | 攒批 64 条或 0.5s 超时后批量执行 | 后台同步、fire-and-forget |

### 队列参数

通过环境变量配置：

```bash
export INFERENCE_QUEUE_SIZE=200       # 队列最大长度 (默认 200)
export INFERENCE_LOW_BATCH=64         # LOW 优先级攒批大小 (默认 64)
export INFERENCE_LOW_FLUSH_SEC=0.5    # LOW 批次超时秒数 (默认 0.5)
```

### 同步 vs 异步

| 模式 | API 行为 | 队列行为 |
|------|---------|---------|
| **同步** (`sync=True`) | 阻塞等待结果返回 | `Future` 等待 worker 处理完 |
| **异步** (`async: true`) | 立即返回 `task_id` | fire-and-forget, 不等待结果 |

gRPC embed/rerank 始终走同步。REST 端点通过 `"async": true` 切换异步。

---

## 添加新模型

只需两步：

**1. 注册模型名**（顶部环境变量或直接改）：

```python
DEFAULT_NEW_MODEL = os.environ.get("INFERENCE_NEW_MODEL", "org/model-name")
```

**2. 在 ModelRegistry 加 loader**：

```python
def _load_newmodel(self):
    try:
        t0 = time.time()
        try:
            model = YourModel.from_pretrained(
                DEFAULT_NEW_MODEL, local_files_only=True
            )
        except Exception:
            self._ensure_downloaded(DEFAULT_NEW_MODEL)  # aria2c 自动下载
            model = YourModel.from_pretrained(DEFAULT_NEW_MODEL)
        elapsed = int((time.time() - t0) * 1000)
        logger.info("NewModel loaded in %dms", elapsed)
        return model, elapsed, None
    except Exception as e:
        return None, 0, str(e)
```

下载机制 `_ensure_downloaded(model_name)` 已通用化：
- 调 HuggingFace API 获取文件列表
- 用 **aria2c** 16 连接并行下载到 HF cache
- 自动跳过已缓存的文件
- 无 aria2c 时回退 `huggingface_hub.snapshot_download`

---

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `INFERENCE_EMBED_MODEL` | `BAAI/bge-m3` | 嵌入模型 |
| `INFERENCE_RERANKER_MODEL` | `BAAI/bge-reranker-large` | 重排序模型 |
| `INFERENCE_LLM_MODEL` | `Qwen/Qwen3-4B` | LLM 模型 |
| `INFERENCE_DEVICE` | `cpu` | embed/reranker 运行设备 |
| `INFERENCE_LLM_DEVICE` | `cuda` | LLM 运行设备 |
| `INFERENCE_QUEUE_SIZE` | `200` | 队列最大长度 |
| `INFERENCE_LOW_BATCH` | `64` | LOW 优先级攒批大小 |
| `INFERENCE_LOW_FLUSH_SEC` | `0.5` | LOW 批次超时秒数 |
