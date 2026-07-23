# Xinference 模型调用指南

> 服务地址: `http://localhost:9997` | 版本: Xinference 3.0.0 | 更新: 2026-07-23

---

## 目录

1. [调用方式总览](#1-调用方式总览)
2. [当前已部署模型](#2-当前已部署模型)
3. [LLM 对话 —— Chat Completions](#3-llm-对话--chat-completions)
4. [Embedding 向量化](#4-embedding-向量化)
5. [Rerank 重排序](#5-rerank-重排序)
6. [并发控制](#6-并发控制)
7. [常见问题](#7-常见问题)

---

## 1. 调用方式总览

Xinference 对外开放 **四种** 调用方式：

| 方式 | 接口 | 适用场景 |
|------|------|----------|
| **RESTful HTTP** | `POST /v1/chat/completions` 等 | 任何语言，最通用，OpenAI 兼容 |
| **Python Client** | `xinference.client.Client` | Python 项目深度集成，功能最全 |
| **CLI** | `xinference chat / launch / list` | 运维管理、脚本调用、快速测试 |
| **Web UI** | `http://localhost:9997` | 手动测试、模型管理、可视化操作 |

> ⚠️ Xinference 内部使用 gRPC 协议通信（Supervisor ↔ Worker 之间），但**不对外暴露 gRPC API**。用户只能通过以上四种方式调用。换成 OpenAI SDK 也只需改 `base_url`，零代码迁移。

---

## 2. 当前已部署模型

| 模型 ID | 类型 | 参数 | 量化 | 上下文 | 显存 |
|---------|------|------|------|--------|------|
| `qwen3` | LLM | 4B | Q4_K_M | 8192 | ~3.8 GB |
| `bge-m3` | Embedding | 0.56B | F32 | - | ~2.2 GB |
| `bge-reranker-base` | Rerank | 0.28B | F32 | - | ~0.6 GB |

**查询模型列表:**
```bash
curl http://localhost:9997/v1/models
```

---

## 3. LLM 对话 —— Chat Completions

### 3.1 端点

```
POST /v1/chat/completions
Content-Type: application/json
```

### 3.2 请求参数

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `model` | string | **必填** | 模型 ID，本站为 `"qwen3"` |
| `messages` | array | **必填** | 对话消息列表，见 [3.3](#33-消息格式) |
| `max_tokens` | int | 无限制 | 最大生成 token 数 |
| `temperature` | float | `0.8` | 随机性控制。0=确定性, 1=平衡, 2=最大随机 |
| `top_p` | float | `0.95` | 核采样概率阈值，0~1 |
| `top_k` | int | `40` | 只从概率最高的 K 个 token 中选择 |
| `repeat_penalty` | float | `1.1` | 重复惩罚。>1 抑制重复，<1 允许重复 |
| `frequency_penalty` | float | `0.0` | 基于出现频率惩罚，-2.0~2.0 |
| `presence_penalty` | float | `0.0` | 基于是否出现惩罚，-2.0~2.0 |
| `stop` | string/array | `[]` | 停止词，遇到即终止生成 |
| `stream` | bool | `false` | 是否流式返回（SSE） |
| `stream_options` | object | `{"include_usage": false}` | 流式选项 |
| `extra_body` | object | - | 扩展参数（见下方） |

#### extra_body 扩展参数

| 参数 | 类型 | 说明 |
|------|------|------|
| `enable_thinking` | bool | `false` 关闭 Qwen3 思考模式 |

### 3.3 消息格式

```json
{
  "role": "system",     // system / user / assistant / tool
  "content": "消息内容"
}
```

- `system`: 设定 AI 行为规则
- `user`: 用户输入
- `assistant`: AI 回复（用于多轮对话历史）
- `tool`: 工具调用结果

### 3.4 调用示例

#### cURL（非流式）

```bash
curl -s http://localhost:9997/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3",
    "messages": [
      {"role": "system", "content": "你是一个数学助手"},
      {"role": "user", "content": "1+1等于几？"}
    ],
    "max_tokens": 100,
    "temperature": 0.7,
    "extra_body": {"enable_thinking": false}
  }'
```

#### cURL（流式 SSE）

```bash
curl -N http://localhost:9997/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3",
    "messages": [{"role": "user", "content": "写一首诗"}],
    "stream": true,
    "extra_body": {"enable_thinking": false}
  }'
```

#### OpenAI Python SDK

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:9997/v1",
    api_key="not-needed"
)

# 非流式
response = client.chat.completions.create(
    model="qwen3",
    messages=[
        {"role": "system", "content": "你是一个数学助手"},
        {"role": "user", "content": "1+1等于几？"}
    ],
    max_tokens=200,
    temperature=0.7,
    extra_body={"enable_thinking": False}
)
print(response.choices[0].message.content)

# 流式
stream = client.chat.completions.create(
    model="qwen3",
    messages=[{"role": "user", "content": "写一首诗"}],
    stream=True,
    extra_body={"enable_thinking": False}
)
for chunk in stream:
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="", flush=True)
```

#### Xinference Python Client（非流式）

```python
from xinference.client import Client

client = Client("http://localhost:9997")
model = client.get_model("qwen3")

response = model.chat(
    messages=[
        {"role": "user", "content": "你好"}
    ],
    generate_config={
        "max_tokens": 200,
        "temperature": 0.7,
        "stream": False,
    }
)
print(response["choices"][0]["message"]["content"])
```

#### Xinference Python Client（流式）

```python
model = client.get_model("qwen3")

for chunk in model.chat(
    messages=[{"role": "user", "content": "写一首诗"}],
    generate_config={"stream": True}
):
    delta = chunk["choices"][0].get("delta", {})
    if "content" in delta:
        print(delta["content"], end="", flush=True)
```

#### CLI 命令行调用

```bash
# 对话
xinference chat --model-uid qwen3 --prompt "你好，用一句话介绍你自己"

# 非对话（补全模式）
xinference generate --model-uid qwen3 --prompt "1+1=" --max-tokens 50

# 查看模型列表
xinference list

# 终止模型
xinference terminate --model-uid qwen3

# 部署新模型
xinference launch \
  --model-name qwen3 \
  --model-engine llama.cpp \
  --model-format ggufv2 \
  --size-in-billions 4 \
  --quantization Q4_K_M
```

### 3.5 响应格式

#### 非流式响应

```json
{
  "id": "chatcmpl-xxx",
  "object": "chat.completion",
  "created": 1784776675,
  "model": "qwen3",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "1+1等于2"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 15,
    "completion_tokens": 5,
    "total_tokens": 20
  }
}
```

- `finish_reason`: `"stop"` (正常结束) / `"length"` (达到 max_tokens) / `"tool_calls"` (工具调用)
- `usage`: token 用量统计

#### 流式响应（SSE）

```
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"1"},"finish_reason":null}]}
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"+"},"finish_reason":null}]}
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"1"},"finish_reason":null}]}
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"="},"finish_reason":null}]}
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"2"},"finish_reason":"stop"}]}
data: [DONE]
```

### 3.6 参数速查

| 场景 | 推荐配置 |
|------|----------|
| 事实问答 / 代码 | `temperature=0.1, top_p=0.5` |
| 创意写作 | `temperature=1.2, top_p=0.95` |
| 通用对话 | `temperature=0.8, top_p=0.95` (默认) |
| 避免重复 | `repeat_penalty=1.3, frequency_penalty=0.5` |
| 关闭思考 | `extra_body={"enable_thinking": false}` |
| 长回答 | `max_tokens=4096` |
| 短回答 | `max_tokens=200` |

---

## 4. Embedding 向量化

### 4.1 端点

```
POST /v1/embeddings
Content-Type: application/json
```

### 4.2 请求参数

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `model` | string | **必填** | `"bge-m3"` |
| `input` | string / array | **必填** | 要编码的文本。支持单条或多条 |
| `encoding_format` | string | `"float"` | 编码格式 |

### 4.3 调用示例

#### 单条文本

```bash
curl -s http://localhost:9997/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{"model": "bge-m3", "input": "人工智能是计算机科学的一个分支"}'
```

#### 批量文本

```python
from openai import OpenAI

client = OpenAI(base_url="http://localhost:9997/v1", api_key="not-needed")

response = client.embeddings.create(
    model="bge-m3",
    input=[
        "人工智能是计算机科学的分支",
        "今天天气真好",
        "深度学习是AI的重要技术"
    ]
)

# 获取向量
vectors = [d.embedding for d in response.data]
print(f"维度: {len(vectors[0])}")  # 1024
print(f"向量数量: {len(vectors)}")  # 3
```

#### Xinference Python Client

```python
from xinference.client import Client

client = Client("http://localhost:9997")
model = client.get_model("bge-m3")

# 单条
result = model.create_embedding("人工智能是计算机科学的分支")
print(f"dim={len(result['data'][0]['embedding'])}")  # 1024

# 批量
result = model.create_embedding([
    "人工智能是计算机科学的分支",
    "深度学习是AI的重要技术"
])
```

#### CLI

```bash
# 向量化（通过 xinference chat 的 --embed 模式，或用 Python 脚本）
# 注：xinference CLI 不直接支持 embedding 命令，推荐用 Python Client
```

### 4.4 响应格式

```json
{
  "object": "list",
  "model": "bge-m3",
  "data": [
    {
      "index": 0,
      "object": "embedding",
      "embedding": [0.0123, -0.0456, ...]
    }
  ],
  "usage": {
    "prompt_tokens": 5,
    "total_tokens": 5
  }
}
```

### 4.5 模型规格

| 属性 | 值 |
|------|-----|
| 模型 | BAAI/bge-m3 |
| 维度 | **1024** |
| 支持语言 | 中文、英文 |
| 最大长度 | 8192 tokens |
| 用途 | 语义搜索、文档聚类、相似度计算 |

---

## 5. Rerank 重排序

### 5.1 端点

```
POST /v1/rerank
Content-Type: application/json
```

### 5.2 请求参数

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `model` | string | **必填** | `"bge-reranker-base"` |
| `query` | string | **必填** | 查询文本 |
| `documents` | string[] | **必填** | 候选文档列表 |
| `top_n` | int | 全部返回 | 只返回前 N 个结果 |
| `return_documents` | bool | `false` | 是否在结果中包含文档原文 |
| `max_chunks_per_doc` | int | - | 每个文档最大分块数 |

### 5.3 调用示例

#### cURL

```bash
curl -s http://localhost:9997/v1/rerank \
  -H "Content-Type: application/json" \
  -d '{
    "model": "bge-reranker-base",
    "query": "什么是大语言模型",
    "documents": [
      "大语言模型是一种能理解和生成人类语言的人工智能模型",
      "今天天气真好适合出去玩",
      "GPT和Qwen都是著名的大语言模型产品"
    ],
    "top_n": 2,
    "return_documents": true
  }'
```

#### Python

```python
import requests

response = requests.post("http://localhost:9997/v1/rerank", json={
    "model": "bge-reranker-base",
    "query": "什么是大语言模型",
    "documents": [
        "大语言模型是一种能理解和生成人类语言的人工智能模型",
        "今天天气真好适合出去玩",
        "GPT和Qwen都是著名的大语言模型产品"
    ],
    "top_n": 2
})

for r in response.json()["results"]:
    print(f"doc[{r['index']}] score={r['relevance_score']:.4f}")
```

#### Xinference Python Client

```python
from xinference.client import Client

client = Client("http://localhost:9997")
model = client.get_model("bge-reranker-base")

scores = model.rerank(
    documents=[
        "大语言模型是一种能理解和生成人类语言的人工智能模型",
        "今天天气真好适合出去玩",
        "GPT和Qwen都是著名的大语言模型产品"
    ],
    query="什么是大语言模型",
    top_n=2,
    return_documents=True
)
for r in scores["results"]:
    print(f"doc[{r['index']}] score={r['relevance_score']:.4f}")
```

### 5.4 响应格式

```json
{
  "id": "rerank-xxx",
  "results": [
    {
      "index": 0,
      "relevance_score": 0.9823,
      "document": {"text": "大语言模型是一种..."}
    },
    {
      "index": 2,
      "relevance_score": 0.7156,
      "document": null
    }
  ],
  "meta": {
    "tokens": {"input_tokens": 45, "output_tokens": 0}
  }
}
```

- `relevance_score`: 0~1，越高越相关
- `index`: 对应输入 `documents` 数组的原始索引

### 5.5 典型使用流程

```
用户查询 → Embedding 召回 Top-20 → Rerank 精排 Top-5 → 返回最终结果
    ↓              ↓                      ↓
  "什么是AI"   bge-m3 向量搜索    bge-reranker-base 打分
```

### 5.6 模型规格

| 属性 | 值 |
|------|-----|
| 模型 | BAAI/bge-reranker-base |
| 类型 | Cross-Encoder |
| 支持语言 | 中文、英文 |
| 最大长度 | 512 tokens |
| 用途 | 搜索精排、问答重排序 |

---

## 6. 并发控制

### 6.1 当前状态

三个模型均未设置 `request_limits`，理论无上限并发。

### 6.2 Auto-Batch 自动批处理

llama.cpp 引擎（qwen3）支持自动 batch：多个请求同时到达时，自动合并为一次 GPU 推理，显著提升吞吐。

```
3 个并发请求 → 自动合并 → 1 次 GPU 推理 → 3 个结果
```

### 6.3 设置并发上限

```python
from xinference.client import Client

# 重新启动模型时指定
client = Client("http://localhost:9997")
client.launch_model(
    model_name="qwen3",
    ...,
    request_limits=5,  # 最多同时处理 5 个请求
)
```

超出限制时返回 HTTP 503 `"Model is overloaded"`。

### 6.4 客户端限流

```python
import asyncio

# 信号量控制并发
sem = asyncio.Semaphore(5)

async def call_with_limit(prompt):
    async with sem:
        return await client.chat.completions.create(
            model="qwen3",
            messages=[{"role": "user", "content": prompt}],
        )
```

---

## 7. 常见问题

### Q: 如何让 qwen3 不输出思考过程？

在请求中添加 `"extra_body": {"enable_thinking": false}`。

### Q: BGE-M3 和 BGE-reranker-base 有什么区别？

- **BGE-M3**: Bi-Encoder，将文本编码为固定维度的向量，用于快速相似度搜索
- **BGE-reranker-base**: Cross-Encoder，同时输入 query 和 document 进行深度比对，更精确但更慢

典型用法：先用 BGE-M3 召回候选，再用 BGE-reranker-base 精排。

### Q: 上下文怎么控制？

部署时通过 `n_ctx` 参数设置。当前 qwen3 为 8192。如需调整：

```python
c.launch_model(model_name="qwen3", ..., n_ctx=16384)
```

### Q: 模型显存不够怎么办？

| 方案 | 操作 |
|------|------|
| 降量化 | Q4_K_M → Q3_K_M，显存减少约 25% |
| 降上下文 | n_ctx 8192 → 4096，减少约 0.5GB |
| 部分 GPU offload | `n_gpu_layers=20`，指定层数而非全部 |

### Q: 服务如何重启？

```bash
pkill -f xinference
XINFERENCE_AUTH_ADVANCED=false HF_ENDPOINT=https://hf-mirror.com \
  nohup xinference-local --host 0.0.0.0 --port 9997 > /tmp/xinference.log 2>&1 &
```

### Q: 如何用 Qwen3 的 Tool Calling？

```python
client.chat.completions.create(
    model="qwen3",
    messages=[{"role": "user", "content": "北京天气怎么样"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "获取城市天气",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "城市名"}
                },
                "required": ["city"]
            }
        }
    }]
)
```
