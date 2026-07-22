# dt-inference-server 架构重构设计

> 状态：设计阶段 | 日期：2026-07-22

## 一、现状诊断

当前 `server.py` 是单文件 847 行的巨石，存在以下架构问题：

### 1.1 推理阻塞事件循环（致命）

```python
async def _process_one(self, task):
    result = self._dispatch(task)  # ← 同步调用，阻塞整个事件循环
```

`_dispatch()` 内部 `model.encode()` / `model.generate()` 是同步 GPU 操作。在 async 协程中直接调用会阻塞事件循环——一个 HIGH 任务推理时，所有后续任务排队等待。

### 1.2 gRPC ↔ asyncio 桥接崩溃级开销

```python
def Embed(self, request, context):
    result = asyncio.run(self.router.submit(...))  # 每次创建/销毁 event loop
```

### 1.3 模型硬编码不可扩展

```python
def _dispatch(self, task):
    if task.task_type == "embed":     ...
    elif task.task_type == "rerank":  ...
    elif task.task_type == "chat":    ...
    else: raise ValueError(...)       # 加 HanLP = 改 3 处
```

### 1.4 缺少批处理、监控、优雅卸载

---

## 二、目标架构

```
services/inference-server/src/
├── server.py                  # 入口：解析参数、组装、启动 (< 60行)
├── config.py                  # 配置管理 (env vars → typed config)
│
├── models/
│   ├── __init__.py
│   ├── registry.py            # ModelRegistry (插件注册 + 生命周期)
│   ├── spec.py                # ModelSpec 数据类
│   ├── loader.py              # _ensure_downloaded (aria2c 下载)
│   ├── embed.py               # BGE-M3 加载
│   ├── reranker.py            # BGE-reranker 加载
│   └── llm.py                 # Qwen3-4B 加载
│
├── queue/
│   ├── __init__.py
│   ├── router.py              # TaskRouter + Priority
│   ├── worker.py              # 真正异步的推理 Worker
│   └── task.py                # InferenceTask 数据类
│
├── api/
│   ├── __init__.py
│   ├── grpc_server.py         # gRPC 服务实现
│   └── rest_server.py         # REST handlers
│
└── metrics.py                 # Prometheus 指标 + structlog
```

### 2.1 关注点分离

| 模块 | 职责 |
|------|------|
| `server.py` | 参数解析、组装 Registry/Router/Worker/Servers、启动 |
| `config.py` | 从环境变量加载配置，转为强类型 dataclass |
| `models/` | 模型加载、下载、缓存、懒加载、idle卸载 |
| `queue/` | 优先级队列、攒批、异步推理调度 |
| `api/` | gRPC + REST 协议处理，薄层 |
| `metrics.py` | Prometheus 指标暴露 + structlog 结构化日志 |

---

## 三、核心改进

### 3.1 真正异步推理（线程池隔离）

```python
# queue/worker.py

class InferenceWorker:
    def __init__(self, router, executor: ThreadPoolExecutor):
        self.router = router
        self.executor = executor  # GPU 推理专用线程池 (max_workers=2)

    async def run(self):
        low_batch: list[InferenceTask] = []
        while self.running:
            task = await self._dequeue_with_timeout()

            if task.priority == Priority.LOW:
                low_batch.append(task)
                if len(low_batch) >= LOW_BATCH_SIZE:
                    await self._batch_infer(low_batch)
                    low_batch.clear()
            else:
                # HIGH/NORMAL: run_in_executor → 事件循环不阻塞
                loop = asyncio.get_event_loop()
                result = await loop.run_in_executor(
                    self.executor, self._infer_sync, task
                )
                self._resolve(task, result)

    def _infer_sync(self, task: InferenceTask) -> dict:
        """在线程池中运行的同步推理（不阻塞事件循环）"""
        return self.router.dispatch(task)
```

### 3.2 共享 Event Loop（修复 gRPC 桥接）

```python
# server.py

class SharedEventLoop:
    """让 gRPC 线程与 asyncio 共享同一个 event loop"""
    
    def __init__(self):
        self.loop: Optional[asyncio.AbstractEventLoop] = None
        self._ready = threading.Event()
    
    def run_coro_from_thread(self, coro, timeout=30):
        """从 gRPC 线程安全调度协程，不再创建新 loop"""
        self._ready.wait()
        future = asyncio.run_coroutine_threadsafe(coro, self.loop)
        return future.result(timeout=timeout)
```

gRPC handler 改为:

```python
class LegacyEmbedServiceImpl:
    def Embed(self, request, context):
        coro = self.router.submit("embed", {"texts": list(request.texts)})
        result = self.shared_loop.run_coro_from_thread(coro)
        # ... build proto response
```

### 3.3 模型插件化（ModelSpec 注册制）

```python
# models/spec.py
@dataclass
class ModelSpec:
    name: str                     # "BAAI/bge-m3"
    model_type: str               # "embed" | "rerank" | "llm" | "nlp"
    loader: Callable              # async function → returns (model, load_ms)
    device: str = "cpu"
    idle_ttl_sec: int = 0         # 0 = 永驻
    batch_capable: bool = False

# models/registry.py
class ModelRegistry:
    def __init__(self):
        self._specs: dict[str, ModelSpec] = {}
        self._loaded: dict[str, LoadedModel] = {}
    
    def register(self, spec: ModelSpec):
        self._specs[spec.name] = spec
    
    def get(self, name: str) -> LoadedModel:
        spec = self._specs[name]
        if name in self._loaded:
            self._loaded[name].touch()  # 重置 idle timer
            return self._loaded[name]
        loaded = LoadedModel(spec, spec.loader())
        self._loaded[name] = loaded
        return loaded
    
    async def evict_idle(self):
        """定期清理超过 idle_ttl 的模型"""
        now = time.time()
        for name, loaded in list(self._loaded.items()):
            if loaded.spec.idle_ttl_sec > 0:
                if now - loaded.last_used > loaded.spec.idle_ttl_sec:
                    del self._loaded[name]
                    torch.cuda.empty_cache()
                    logger.info("evicted idle model", model=name)
```

### 3.4 双模式批处理

```python
# queue/worker.py

class InferenceWorker:
    def __init__(self):
        self._normal_embed_batch: list[InferenceTask] = []
        self._normal_embed_max = 8        # NORMAL embed 小攒批
        self._normal_embed_timeout = 0.1  # 100ms 超时

    async def _dequeue_with_timeout(self) -> InferenceTask:
        while True:
            try:
                task = await asyncio.wait_for(
                    self.router.queue.get(),
                    timeout=FLUSH_SEC
                )
            except asyncio.TimeoutError:
                # flush 积攒的 batch
                if self._normal_embed_batch:
                    return self._flush_normal_embed_batch()
                if self._low_batch:
                    await self._batch_infer(self._low_batch)
                    self._low_batch.clear()
                continue

            # HIGH 优先级跳过所有攒批
            if task.priority == Priority.HIGH:
                if self._normal_embed_batch:
                    self._flush_normal_embed_batch_background()
                return task

            # NORMAL embed → 小批次攒批
            if task.priority == Priority.NORMAL and task.task_type == "embed":
                self._normal_embed_batch.append(task)
                if len(self._normal_embed_batch) >= self._normal_embed_max:
                    return self._flush_normal_embed_batch()
                continue

            # LOW → 大批次攒批 (64条/0.5s)
            if task.priority == Priority.LOW:
                self._low_batch.append(task)
                if len(self._low_batch) >= LOW_BATCH_SIZE:
                    return self._flush_batch_task()
                continue

            return task
```

### 3.5 指标与监控

```python
# metrics.py
from prometheus_client import Counter, Histogram, Gauge, generate_latest

inference_total = Counter(
    "dt_inference_total", "Total requests",
    ["model_type", "priority", "status"]
)
inference_latency = Histogram(
    "dt_inference_latency_seconds", "Request latency",
    ["model_type", "priority"]
)
queue_depth = Gauge(
    "dt_inference_queue_depth", "Current queue depth"
)
gpu_memory = Gauge(
    "dt_inference_gpu_memory_bytes", "GPU VRAM used"
)
model_load_time = Gauge(
    "dt_inference_model_load_seconds", "Model load time",
    ["model_name"]
)

# 在 REST 中暴露 /metrics 端点
async def handle_metrics(request):
    return web.Response(body=generate_latest(), content_type="text/plain")
```

### 3.6 结构化日志

```python
import structlog

logger = structlog.get_logger()

# Worker 中
log = logger.bind(task_id=task.task_id, model_type=task.task_type)
log.info("inference_start")
result = await loop.run_in_executor(executor, infer_sync, task)
log.info("inference_done", elapsed_ms=result.elapsed_ms)
```

---

## 四、模块接口规范

### 4.1 ModelRegistry

```python
registry = ModelRegistry()

# 注册
registry.register(ModelSpec(
    name="BAAI/bge-m3", model_type="embed",
    loader=load_bge_m3, device="cuda",
    batch_capable=True,
))

# 获取（懒加载）
model = registry.get("BAAI/bge-m3")

# 状态
status = registry.status()
# → {"BAAI/bge-m3": {"loaded": True, "load_ms": 1234, "error": ""}, ...}

# 卸载空闲
await registry.evict_idle()
```

### 4.2 TaskRouter

```python
router = TaskRouter(registry, max_queue_size=200)

# 提交任务
result = await router.submit(
    task_type="embed",
    payload={"texts": ["hello", "world"]},
    priority=Priority.NORMAL,
    sync=True,
)

# 异步（fire-and-forget）
task_id = await router.submit(
    task_type="chat",
    payload={"messages": [...]},
    priority=Priority.LOW,
    sync=False,
)

# 调度（由 Worker 调用）
result = router.dispatch(task)  # 同步，在 executor 线程中运行
```

### 4.3 REST API

```
POST /v1/chat/completions     OpenAI 兼容 Chat API
POST /v1/embeddings            Embed (sync + async)
POST /v1/rerank                Rerank
GET  /v1/models                模型列表 + 状态
GET  /health                   健康检查
GET  /metrics                  Prometheus 指标
```

### 4.4 gRPC API (legacy 兼容)

```
EmbedService.Embed(texts) → embeddings
RerankerService.Rerank(query, texts) → scores
```

---

## 五、文件依赖关系

```
server.py
  ├── config.py            (无依赖)
  ├── models/
  │   ├── spec.py          (无依赖)
  │   ├── loader.py        → huggingface_hub, aria2c
  │   ├── embed.py         → loader, sentence_transformers
  │   ├── reranker.py      → loader, FlagEmbedding
  │   ├── llm.py           → loader, transformers, torch
  │   └── registry.py      → spec
  ├── queue/
  │   ├── task.py          (无依赖)
  │   ├── router.py        → registry, task
  │   └── worker.py        → router, task
  ├── api/
  │   ├── grpc_server.py   → router, shared_loop, protos
  │   └── rest_server.py   → router, registry
  └── metrics.py           → prometheus_client
```

---

## 六、迁移策略

从现有单文件 `server.py` 到模块化架构，采用**渐进式拆分**：

### 阶段 1：拆分 models/ 模块
- 提取 `loader.py` (下载逻辑)
- 提取 `embed.py`, `reranker.py`, `llm.py` (模型加载)
- 提取 `registry.py` + `spec.py`
- 旧 `server.py` import 新模块，功能不变

### 阶段 2：拆分 queue/ 模块
- 提取 `task.py` (InferenceTask, Priority)
- 提取 `router.py` (TaskRouter)
- 提取 `worker.py` (推理 Worker → 修复阻塞问题)

### 阶段 3：拆分 api/ 模块
- 提取 `grpc_server.py`
- 提取 `rest_server.py`
- 修复 gRPC-asyncio 桥接

### 阶段 4：监控与收尾
- 添加 `metrics.py`
- 添加 structlog
- 精简 `server.py` 入口
- 清理旧代码
- 更新 README
