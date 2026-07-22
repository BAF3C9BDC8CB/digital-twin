# 非结构化数据处理管线设计

> 状态：设计阶段 | 日期：2026-07-22

## 一、背景与目标

### 现状

当前 `dt build` 的数据处理管线是硬编码的：

- 代码文件 → tree-sitter AST 解析 → Memgraph
- Markdown → `chunk_markdown_by_headings` → Qdrant
- YAML → `chunk_config_by_sections` → Qdrant
- Properties → `chunk_properties_adaptive` → Qdrant

### 问题

1. **无扩展性**：新增文件类型（PDF、XML、Word）需要新增 Rust 代码
2. **无语义理解**：所有文本只做了向量化，没有实体抽取、关系提取、摘要生成
3. **无跨文件关联**：无法从项目级别的文档和代码中构建调用链、知识图谱
4. **无法利用本地 GPU**：已有 RTX 3060+ 级别的 GPU 资源，但未被管线使用

### 目标

构建一个**自动编排、插件化的非结构化数据处理引擎**，满足：

- 根据文件类型自动匹配处理器，无需手工配置 pipeline
- 三层分析：文件级 → 项目级 → 生态级
- 处理器可按需启停（tree-sitter / HanLP / LLM / OCR 等）
- 输出实体、关系、摘要、标签到 Memgraph + Qdrant

### 非目标

- 不替换现有 `dt build` 的核心流程（hash 检测、增量构建）
- 不引入外部云 API 依赖（全部本地执行）
- 不在这一步做 Pipeline 配置的 UI 界面

---

## 二、总体架构

```
                          dt build / dt analyze
                               │
                               │  文件列表 + hash 增量检测
                               ▼
                    ┌─────────────────────┐
                    │   Processor Engine  │  ← Rust (CPU only)
                    │   (自动编排引擎)     │
                    └─────────┬───────────┘
                              │
          ┌───────────────────┼───────────────────┐
          │  Rust 本地 (CPU)  │  HTTP 客户端       │
          │                   │                   │
          ▼                   ▼                   ▼
   ┌─────────────┐    ┌─────────────┐    ┌─────────────────────────┐
   │ tree-sitter │    │   chunk     │    │  dt-inference-server    │
   │  (内置AST)   │    │  (内置分片)  │    │  :50052 (REST)          │
   └──────┬──────┘    └──────┬──────┘    │                         │
          │                   │           │  ┌───────────────────┐  │
          │         ┌─────────┘           │  │   TaskRouter       │  │
          │         │                     │  │   优先级队列+攒批   │  │
          │         │                     │  │   HIGH/NORMAL/LOW  │  │
          │         │                     │  └────────┬──────────┘  │
          │         │                     │           │             │
          │         │                     │  ┌────────▼──────────┐  │
          │         │                     │  │  ModelRegistry     │  │
          │         │                     │  │  ├─ BGE-M3 (embed) │  │
          │         │                     │  │  ├─ BGE-reranker   │  │
          │         │                     │  │  ├─ Qwen3-4B (LLM) │  │
          │         │                     │  │  └─ HanLP (future) │  │
          │         │                     │  └────────────────────┘  │
          │         │                     └─────────────────────────┘
          │         │                               │
          │         │                      HTTP 响应 (JSON)
          │         │                               │
          └─────────┼───────────────────────────────┘
                    │
                    ▼
          ┌─────────────────────┐
          │    Store Writer     │
          │  Memgraph + Qdrant  │
          └─────────────────────┘
```

**关键边界：**

| 组件 | 职责 | 不负责 |
|------|------|--------|
| **Processor Engine (Rust)** | 文件扫描、编排决策、CPU 处理器（tree-sitter/chunk）、调用 inference-server API、结果汇总入库 | GPU 管理、模型加载、推理队列 |
| **dt-inference-server (Python)** | GPU 模型管理、优先级队列、批量推理、Embed/Rerank/LLM/HanLP（未来） | 文件处理、知识图谱写入 |

### 设计原则

1. **自动编排**：引擎根据文件类型和处理器能力卡片自动决定执行链，用户无需手工配置 stages 顺序
2. **开放输出**：每个处理器输出写入统一的 `PipelineContext`，下游处理器可任意引用上游输出
3. **处理器自治**：每个处理器自描述其能力（支持的文件类型、产生的输出、依赖关系），引擎据此编排
4. **关注点分离**：Rust 负责编排和确定性处理（tree-sitter/chunk），GPU 推理全部委托 inference-server
5. **优雅降级**：任一处理器或 inference-server 失败不影响其他处理器

---

## 三、三层分析模型

```
Level 0: file_stages      每个文件独立分析
Level 1: project_stages   单个项目内部汇总
Level 2: ecosystem_stages 跨项目（微服务集群）全局拓扑
```

### Level 0：文件级

对每个变更文件独立执行，可并行。输出写入 `PipelineContext`，键为处理器名。

### Level 1：项目级

所有文件处理完后，汇总 Level 0 的所有输出，执行一次项目级分析。例如：

- 从所有 Java 文件的 import/Feign 声明中构建服务调用图
- 从 docs/ 目录的所有 Markdown 中构建文档知识图谱

### Level 2：生态级

跨多个项目的汇总分析，解决微服务集群场景。例如：

- 从各服务的 Nacos 配置 + API 路由 + Feign 声明中构建服务拓扑
- 识别分布式事务链路、共享基础设施

---

## 四、处理器系统

### 4.1 处理器注册表

每个处理器一个 YAML 文件，放在 `config/processors/` 下。引擎启动时加载所有 `.yaml` 文件。

#### tree-sitter — 代码 AST 解析

```yaml
name: tree_sitter
priority: 100
enabled: true

match:
  file_extensions: [.java, .py, .rs, .go, .ts, .tsx, .js, .jsx,
                    .c, .cpp, .h, .hpp, .cs, .rb, .php, .swift]

languages:
  java:
    extract: [classes, methods, fields, annotations, imports]
    patterns:
      - name: feign_clients
        regex: '@FeignClient\s*\(\s*(?:name\s*=\s*)?["\']([^"\']+)["\']'
      - name: rest_mappings
        regex: '@(?:GetMapping|PostMapping|PutMapping|DeleteMapping|RequestMapping)\s*\(\s*["\']([^"\']+)["\']'
      - name: autowired
        regex: '@Autowired'
  python:
    extract: [classes, functions, imports, decorators]
    patterns:
      - name: routes
        regex: '@app\.route\s*\(\s*["\']([^"\']+)["\']'
  rust:
    extract: [structs, enums, functions, traits, impls, macros]
```

#### hanlp — 中文 NLP 前置分析

```yaml
name: hanlp
priority: 80
enabled: true

match:
  file_extensions: [.md, .txt, .yaml, .yml, .properties]
  languages: [zh]

# 通过 inference-server 调用（模型管理、队列、GPU 由 server 负责）
server: http://localhost:50052
endpoint: /v1/nlp/hanlp       # 未来扩展端点（当前可先直连 HanLP 服务）
timeout_sec: 30

tasks: [tok, pos, ner, srl]

custom_ner:
  source: memgraph
  query: "MATCH (s:Service) RETURN s.name"
  entity_tag: SERVICE_NAME

  static:
    SERVICE_NAME: []
    TECH_COMPONENT: [Redis, Kafka, MySQL, Nacos, Docker, Kubernetes]
    BUSINESS_ENTITY: []
```

> **设计决策**：HanLP 最终也应接入 inference-server，统一 GPU 调度。过渡期可直连。Rust 侧只负责构造请求 payload、调用 HTTP API、解析 JSON 响应。

#### llm — 语义推理

```yaml
name: llm
priority: 60
enabled: true

match:
  file_extensions: [.java, .py, .rs, .go, .ts, .md, .txt, .yaml, .yml]
  min_chars: 50

# 通过 inference-server 调用（OpenAI 兼容 API）
server: http://localhost:50052
endpoint: /v1/chat/completions
temperature: 0.1
max_tokens: 4096

# 并发控制（Rust 侧只需限制并发请求数，队列/攒批由 server 负责）
max_concurrent: 16

# ── 按输入场景选择 Prompt ──
prompts:
  code:
    when:
      has_prefix: tree_sitter
    file: code_with_ast.yaml

  document_zh:
    when:
      has_prefix: hanlp
    file: document_with_nlp.yaml

  raw:
    when:
      none_of: [tree_sitter, hanlp]
    file: raw_text.yaml
```

> **设计决策**：Rust 不管理 LLM 队列，不关心 GPU 状态。只需限制并发请求数（`max_concurrent=16`），其余（队列优先级、攒批、模型切换）全部由 inference-server 的 TaskRouter 处理。调用方式为标准 OpenAI Chat Completions API。

#### chunk — 文本分片

```yaml
name: chunk
priority: 90
enabled: true

match:
  file_extensions: [.md, .txt, .yaml, .yml, .properties]

chunk_size: 512
chunk_overlap: 64
```

#### extract_text — 二进制文本提取

```yaml
name: extract_text
priority: 95
enabled: true

match:
  file_extensions: [.pdf, .docx, .doc]
```

#### store — 统一写入

```yaml
name: store
priority: 10
enabled: true

write:
  memgraph:
    entities:
      - source: tree_sitter.entities
        labels: [Class, Method, Field]
      - source: hanlp.entities
        labels: [NamedEntity]
      - source: llm.entities
        labels: [SemanticEntity, Service, Component]

    relations:
      - source: tree_sitter.patterns.feign_clients
        type: CALLS_SERVICE
      - source: llm.relations
        type: DEPENDS_ON

  qdrant:
    text_source: chunk.text
    payload_fields:
      - llm.summary
      - llm.tags
      - hanlp.keywords
```

### 4.2 优先级规则

引擎按 `priority` 降序执行处理器。优先级设计逻辑：

| 优先级 | 处理器 | 理由 |
|--------|--------|------|
| 100 | tree_sitter | 最底层结构分析，必须最先执行 |
| 95 | extract_text | 二进制 → 文本，否则后续无法处理 |
| 90 | chunk | 文本分片，HanLP/LLM 的输入单元 |
| 80 | hanlp | NLP 前置，为 LLM 提供结构化线索 |
| 60 | llm | 语义推理，使用前面所有处理器的输出 |
| 10 | store | 终结点，汇总所有结果入库 |

### 4.3 处理器实现接口

```rust
#[async_trait]
pub trait Processor: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;
    fn matches(&self, file: &FileInfo) -> bool;
    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, Error>;
}
```

各处理器的实现策略：

| 处理器 | 运行位置 | 实现方式 |
|--------|---------|---------|
| tree_sitter | Rust 内联 | 复用现有 tree-sitter 解析逻辑，CPU 并行 |
| chunk | Rust 内联 | 复用现有 `chunker.rs`，CPU 并行 |
| extract_text | Rust 调外部工具 | `pdf-extract` / `python-docx`，输出纯文本 |
| **hanlp** | **→ inference-server** | HTTP POST 到 `:50052/v1/nlp/hanlp`，返回 JSON |
| **llm** | **→ inference-server** | HTTP POST 到 `:50052/v1/chat/completions`，OpenAI 兼容格式 |
| store | Rust 内联 | 复用现有 Memgraph + Qdrant 写入逻辑 |

**Rust 侧不负责**：GPU 管理、模型加载/卸载、推理队列管理、请求攒批——全部由 inference-server 的 TaskRouter + ModelRegistry 处理。

---

## 五、数据流与上下文传递

### 5.1 PipelineContext

`PipelineContext` 是处理器的共享数据容器：

```
PipelineContext
├── raw                          # 原始文件
│   ├── path: &str               # /path/to/OrderController.java
│   ├── text: &str               # 文件纯文本
│   └── bytes: &[u8]             # 原始字节
│
├── outputs: HashMap<processor_name, ProcessorOutput>
│   ├── "tree_sitter" → {entities, annotations, patterns, imports}
│   ├── "hanlp"       → {entities, keywords, dependencies}
│   ├── "chunk"       → {chunks: [DocumentChunk]}
│   └── "llm"         → {entities, relations, summary, tags, ...}
│
└── project                       # 项目元信息（仅 project_stages 可用）
    ├── file_tree: Vec<FileEntry>
    ├── all_outputs: Vec<PipelineContext>  # 所有文件的上下文
    └── deps: Vec<DependencyEdge>
```

### 5.2 变量引用语法

Prompt 模板中通过 `${prefix.field.subfield}` 引用上下文数据：

```
${raw.text}                       → 原文
${tree_sitter.entities}           → tree-sitter 提取的实体列表
${tree_sitter.patterns.feign_clients} → Feign 客户端列表
${hanlp.keywords}                 → HanLP 提取的关键词
${project.file_tree}              → 项目目录树
```

支持简单迭代：

```
${for entity in tree_sitter.entities}
  - ${entity.name}: ${entity.kind}
${end}
```

---

## 六、Prompt 模板系统

### 6.1 模板文件位置

```
config/prompts/
├── code_with_ast.yaml          # 有 AST 输出的代码分析
├── document_with_nlp.yaml      # 有 HanLP 输出的文档分析
├── raw_text.yaml               # 纯文本分析
├── project_architecture.yaml   # 项目架构汇总
└── service_mesh_topology.yaml  # 微服务拓扑分析
```

### 6.2 模板格式

```yaml
name: code_with_ast
description: "结合 AST 分析结果的代码语义理解"

system: |
  你是代码分析助手。基于AST语法解析结果和源码，分析代码的业务语义。
  严格按JSON格式返回，不要额外文本。

prompt: |
  文件: ${raw.path}

  ## AST解析结果
  类: ${tree_sitter.entities}
  注解: ${tree_sitter.annotations}
  Feign调用: ${tree_sitter.patterns.feign_clients}
  REST路由: ${tree_sitter.patterns.rest_mappings}

  ## 源码
  ${raw.text}

  请分析并输出JSON：
  {
    "entities": [{"name": "...", "type": "Service|Controller|Component|..."}],
    "relations": [{"from": "...", "rel": "DEPENDS_ON|CALLS|PRODUCES|CONSUMES", "to": "..."}],
    "summary": "这个类的功能摘要（50字内）",
    "service_role": "Controller|Service|Repository|Client|Config",
    "external_calls": [{"service_name": "...", "protocol": "HTTP|RPC|MQ|DB", "endpoints": ["..."]}]
  }

output_schema:
  type: object
  required: [entities, relations, summary, service_role]
  properties:
    entities: {type: array, items: {$ref: "#/$defs/entity"}}
    relations: {type: array, items: {$ref: "#/$defs/relation"}}
    summary: {type: string, maxLength: 100}
    service_role: {type: string}
    external_calls: {type: array, items: {$ref: "#/$defs/external_call"}}
  $defs:
    entity: {type: object, properties: {name: {type: string}, type: {type: string}}}
    relation: {type: object, properties: {from: {type: string}, rel: {type: string}, to: {type: string}}}
    external_call: {type: object, properties: {service_name: {type: string}, protocol: {type: string}, endpoints: {type: array, items: {type: string}}}}
```

### 6.3 Prompt 选择逻辑

LLM 处理器根据 `PipelineContext` 中已有的输出自动选择 Prompt：

```
有 tree_sitter 输出 → code_with_ast.yaml
有 hanlp 输出       → document_with_nlp.yaml
都没有              → raw_text.yaml
```

---

## 七、执行流程

### 7.1 引擎核心

```rust
use tokio::sync::Semaphore;

pub async fn analyze(&self, file: &FileInfo) -> Result<AnalysisResult> {
    let mut context = PipelineContext::new(file);

    // 1. 收集匹配且启用的处理器，按优先级排序
    let processors: Vec<&dyn Processor> = self.registry
        .iter()
        .filter(|p| p.enabled() && p.matches(file))
        .sorted_by_key(|p| Reverse(p.priority()))
        .collect();

    // 2. 顺序执行（CPU处理器 + HTTP调用 inference-server）
    for processor in processors {
        match processor.execute(&context).await {
            Ok(output) => { context.add(processor.name(), output); }
            Err(e) => {
                log::warn!("Processor {} failed for {}: {}", processor.name(), file.path, e);
                // 继续执行后续处理器（优雅降级）
            }
        }
    }

    Ok(context.into_result())
}

/// 批量并行处理多个文件（阶段批量模式）
pub async fn analyze_batch(&self, files: &[FileInfo]) -> Vec<Result<AnalysisResult>> {
    // Phase 1-2: CPU 密集型 — 全并行
    let contexts: Vec<_> = stream::iter(files)
        .map(|f| async { self.run_cpu_processors(f).await })
        .buffer_unordered(num_cpus::get())
        .collect().await;

    // Phase 3-4: GPU 委托 — 信号量控制并发 HTTP 请求
    let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent));
    let results: Vec<_> = stream::iter(contexts)
        .map(|ctx| {
            let sem = semaphore.clone();
            async { self.run_gpu_processors(ctx, &sem).await }
        })
        .buffer_unordered(self.config.max_concurrent)
        .collect().await;

    results
}
```

Rust 引擎的并发控制只有一点：**信号量限制对 inference-server 的并发 HTTP 请求数**。其余全部委托。

### 7.2 完整链路示例（Spring Cloud 微服务）

以 `order-service/src/main/java/com/xxx/OrderController.java` 为例：

```
Phase 1: tree_sitter (priority=100)
─────────────────────────────────────────
output:
  entities: [OrderController(Class), create(Method), PayServiceClient(Interface)]
  annotations: [@RestController, @RequestMapping("/api/orders"),
                @FeignClient(name="pay-service"), @PostMapping("/create"), @Autowired]
  patterns:
    feign_clients: ["pay-service"]
    rest_mappings: ["/api/orders", "/pay/create", "/create"]

Phase 2: hanlp (priority=80)  — 仅分析 JavaDoc 和注释
─────────────────────────────────────────
input:  "创建订单并调用支付服务完成支付\n支付成功后通知物流服务发货"

output:
  entities: [{text: "支付服务", tag: SERVICE_NAME},
             {text: "物流服务", tag: SERVICE_NAME},
             {text: "订单", tag: BUSINESS_ENTITY}]
  keywords: [创建订单, 支付, 物流, 发货]

Phase 3: llm (priority=60)  — 语义推理
─────────────────────────────────────────
input: tree_sitter 输出 + hanlp 输出 + 源码

output:
  entities:
    - {name: "order-service", type: Service}        ← 本服务
    - {name: "pay-service", type: ExternalService}  ← Feign 调用目标
    - {name: "user-service", type: ExternalService} ← LLM 从注释推断
    - {name: "logistics-service", type: ExternalService}
  relations:
    - {from: "order-service", rel: CALLS, to: "pay-service", protocol: HTTP}
    - {from: "order-service", rel: CALLS, to: "user-service", protocol: RPC}
    - {from: "pay-service", rel: NOTIFIES, to: "logistics-service", protocol: MQ}
  summary: "订单创建控制器，负责创建订单并协调支付服务和物流服务"
  service_role: Controller

Phase 4: store (priority=10)
─────────────────────────────────────────
Memgraph:
  (:Service {name: "order-service"})
  (:Service {name: "pay-service"})
  (:Service {name: "user-service"})
  (:Service {name: "logistics-service"})
  (:Service)-[:CALLS {protocol: "HTTP"}]->(:Service)
  (:Service)-[:CALLS {protocol: "RPC"}]->(:Service)
  (:Service)-[:NOTIFIES {protocol: "MQ"}]->(:Service)

Qdrant:
  payload: {
    summary: "订单创建控制器...",
    tags: ["订单", "支付", "物流"],
    keywords: ["创建订单", "支付", "物流"],
    source_type: "code",
    project: "order-service",
    service_role: "Controller"
  }
```

### 7.3 三层完整时序

```
dt build --project my-microservices

┌─ Phase A: CPU 阶段 (Rust 并行) ────────────────────────────────────┐
│                                                                    │
│  gateway/**/*.java     → tree_sitter (并行) → chunk (并行)        │
│  user-service/**/*.java → tree_sitter → chunk                     │
│  order-service/**/*.java→ tree_sitter → chunk                     │
│  pay-service/**/*.java  → tree_sitter → chunk                     │
│  docs/**/*.md           → chunk                                   │
│  *.yml                  → chunk(配置策略)                          │
│                                                                    │
└────────────────────────────┬───────────────────────────────────────┘
                             │
┌─ Phase B: GPU 委托 (→ inference-server) ──────────────────────────┐
│                                                                    │
│  所有文件 ──HTTP──► inference-server :50052                       │
│                      ├─ hanlp (NLP分析, LOW优先级 → 自动攒批)     │
│                      └─ llm  (语义推理, NORMAL优先级)             │
│                                                                    │
│  Rust 侧仅控制并发 HTTP 请求数 (信号量=16)                         │
│                                                                    │
└────────────────────────────┬───────────────────────────────────────┘
                             │
┌─ Phase C: 项目/生态级汇总 + 入库 ──────────────────────────────────┐
│                                                                    │
│  Rust: project_stages (llm汇总各服务内所有文件)                    │
│  Rust: ecosystem_stages (llm构建服务拓扑、分布式事务链路)          │
│  Rust: store → Memgraph + Qdrant                                  │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## 八、性能架构：Rust 并发 + inference-server 委托

### 8.1 关注点分离

Rust 引擎和 inference-server 各自负责擅长的部分：

```
Rust Processor Engine (CPU)              dt-inference-server (GPU)
─────────────────────────────            ─────────────────────────
├─ 文件扫描 + hash 检测                  ├─ 模型加载/卸载 (懒加载)
├─ tree-sitter AST 解析 (并行)           ├─ 三级优先级队列 (HIGH/NORMAL/LOW)
├─ chunk 文本分片 (并行)                 ├─ LOW 优先级自动攒批 (64条/0.5s)
├─ 构造推理请求 payload                  ├─ BGE-M3 embed (gRPC :50051)
├─ HTTP 客户端 → 调用推理 API            ├─ BGE-reranker 重排序
├─ 限制并发数 (信号量)                   ├─ Qwen3-4B LLM (OpenAI API)
├─ 解析 API 响应 JSON                    └─ 自动下载缺失模型 (aria2c)
└─ 写入 Memgraph + Qdrant
```

### 8.2 Rust 侧并发模型

```rust
use tokio::sync::Semaphore;
use std::sync::Arc;

// 阶段 1: CPU 密集型 — 无限制并行
async fn phase_tree_sitter(files: Vec<FileInfo>) -> Vec<Output> {
    let tasks: Vec<_> = files.into_iter().map(|f| {
        tokio::task::spawn_blocking(move || tree_sitter_parse(f))
    }).collect();
    futures::future::join_all(tasks).await
}

// 阶段 2: CPU 密集型 — 同上
async fn phase_chunk(files: Vec<FileInfo>) -> Vec<Output> { /* 同上模式 */ }

// 阶段 3: GPU 委托 — 信号量限制并发请求数
async fn phase_llm(contexts: Vec<PipelineContext>, config: &LlmConfig) -> Vec<Output> {
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent)); // 默认 16

    let tasks: Vec<_> = contexts.into_iter().map(|ctx| {
        let sem = semaphore.clone();
        let client = reqwest::Client::new();
        async move {
            let _permit = sem.acquire().await.unwrap();
            // inference-server 内部处理队列、优先级、攒批
            let resp = client
                .post("http://localhost:50052/v1/chat/completions")
                .json(&build_chat_request(&ctx))
                .send().await?
                .json::<ChatResponse>().await?;
            parse_llm_output(resp)
        }
    }).collect();

    futures::future::join_all(tasks).await
}
```

**Rust 不需要**：
- 消息队列（inference-server 内置）
- 攒批逻辑（inference-server LOW 优先级自动攒批 64 条）
- GPU 显存管理（inference-server 懒加载 + 自动 GC）
- 模型切换协调（inference-server 统一调度）

**Rust 只需要**：
- 信号量控制并发 HTTP 请求数，避免打爆 inference-server
- 构造符合 OpenAI Chat API 格式的请求体
- 解析 JSON 响应

### 8.3 请求优先级使用

| Rust 调用场景 | 对应 Priority | 说明 |
|--------------|---------------|------|
| `dt build` / `dt analyze` 批量处理 | NORMAL | 代码索引，不攒批，单条立即处理 |
| 后台文档同步 | LOW | 触发攒批（64条/0.5s），提高 GPU 吞吐 |
| 用户实时搜索 | HIGH | 最高优先，立即处理（不经过本管线） |

> `dt build` 期间调用 llm 走 NORMAL 优先级；HanLP NLP 分析走 LOW 优先级（可攒批）。

### 8.4 增量处理与缓存

```
增量策略                   缓存策略
─────────                  ─────────
1. hash 检测（现有）       1. 处理器输出缓存
   SHA256 比对 → 仅处理      key = file_hash + processor_name + config_hash
   变更文件                   value = ProcessorOutput (JSON)
                           存储 = SQLite（现有快照表）

2. 阶段级跳过              2. Prompt 模板缓存
   如果 tree_sitter 输出      模板预编译，变量替换用模板引擎
   未变 → 跳过 Phase 1
                           3. LLM 输出缓存
3. 跨文件去重                 相同代码模式（如标准 CRUD Controller）
   相同签名的类/方法           → 缓存 LLM 结果，直接复用
   共享分析结果
```

### 8.5 性能预估

基于 RTX 3060 (12GB)，处理 200 Java + 50 文档：

| 阶段 | 运行位置 | 并行方式 | 耗时 |
|------|---------|---------|------|
| Phase 1: tree_sitter | Rust CPU | 8核 `spawn_blocking` | < 3s |
| Phase 2: chunk | Rust CPU | 8核并行 | < 1s |
| Phase 3: hanlp | → inference-server | HTTP + 信号量(16) | ~30s |
| Phase 4: llm | → inference-server | HTTP + 信号量(16), NORMAL优先级 | ~5min |
| Phase 5: store | Rust CPU | 连接池并行写入 | < 10s |
| **总计** | | | **~6 分钟** |

> inference-server 内部的 TaskRouter 确保 GPU 在 Phase 3+4 期间持续满载，无需 Rust 侧额外编排。

---

## 九、HanLP 自定义 NER 与微服务调用链

### 9.1 两层互补

| | tree-sitter + 正则 | HanLP 自定义 NER |
|---|---|---|
| 数据来源 | 注解、import 语句 | JavaDoc、注释、README |
| 提取内容 | 硬依赖（Feign 声明、Controller 路由） | 软依赖（注释中提到的服务名、业务实体） |
| 精度 | 100%（结构化代码，无歧义） | 依赖词典质量 |
| 适用语言 | 所有支持 tree-sitter 的语言 | 仅中文（当前） |

### 8.2 自定义 NER 词典来源

```yaml
custom_ner:
  # 动态来源：从知识图谱中已有的 Service 节点加载
  source: memgraph
  query: "MATCH (s:Service) RETURN s.name"
  entity_tag: SERVICE_NAME

  # 静态补充：项目特有的技术组件名
  static:
    SERVICE_NAME:
      - 用户服务
      - 订单服务
      - 支付服务
      - 网关服务
      - 物流服务
    TECH_COMPONENT:
      - Redis
      - Kafka
      - MySQL
      - Nacos
      - Docker
      - Seata
```

> **首次运行注意**：知识图谱中尚无 Service 节点时，仅使用 static 词典。后续随着 `dt build` 和 `nacos-sync` 不断写入服务信息，动态词典自动丰富。这是个自增强的正循环。


### 8.3 完整调用链提取流程

```
Java 源码
    │
    ├──────────────────┬─────────────────────────┐
    │                  │                         │
    ▼                  ▼                         ▼
 注解+import       JavaDoc+注释              方法体代码
    │                  │                         │
    ▼                  ▼                         ▼
 tree-sitter        HanLP NER                  LLM
 +正则提取          识别服务名                 语义推理
    │                  │                         │
    ▼                  ▼                         ▼
 @FeignClient     "调用支付服务"            "创建订单后发起支付，
 (name="pay")      → 实体: 支付服务          协调用户服务校验"
    │                                        → 补充调用链
    ▼
 pay-service

        三者合并 → 完整调用链:
          order-service → pay-service (HTTP, Feign)
          order-service → user-service (RPC, 注释推断)
          pay-service → logistics-service (MQ, 注释推断)
```

---

## 十、配置全景

用户可见的配置结构：

```
config/
├── config.yaml                       # 现有：项目路径配置
├── pipeline.yaml                     # 新：管线全局配置
├── processors/                       # 新：处理器注册表
│   ├── tree_sitter.yaml
│   ├── hanlp.yaml
│   ├── llm.yaml
│   ├── chunk.yaml
│   ├── extract_text.yaml
│   ├── ocr.yaml
│   └── store.yaml
└── prompts/                          # 新：Prompt 模板库
    ├── code_with_ast.yaml
    ├── document_with_nlp.yaml
    ├── raw_text.yaml
    ├── project_architecture.yaml
    └── service_mesh_topology.yaml
```

### pipeline.yaml 最小配置

```yaml
# config/pipeline.yaml
pipeline:
  enabled: true                       # 总开关

  # inference-server 连接
  inference_server:
    url: http://localhost:50052       # REST API
    grpc_url: http://localhost:50051  # gRPC (legacy embed)
    max_concurrent: 16                # Rust 侧并发 HTTP 请求上限

  # 启用的处理器
  processors:
    tree_sitter: true
    hanlp: true
    llm: true
    chunk: true
    extract_text: true
    ocr: false
    store: true

  # LLM 配置（透传给 inference-server）
  llm:
    temperature: 0.1
    max_tokens: 4096

  # 项目级/生态级分析
  ecosystem:
    enabled: true
    projects:
      - my-microservices
```

---

## 十一、代码结构

### 新增模块

```
src/pipeline/                         # 新增模块
├── mod.rs                            # 模块入口
├── engine.rs                         # ProcessorEngine - 阶段批量执行
│   pub async fn analyze_batch(files, config) -> Vec<Result>
├── context.rs                        # PipelineContext - 数据容器
│   pub struct PipelineContext { raw, outputs, project }
│   pub fn add(name, output)
│   pub fn get<T>(name) -> Option<&T>
├── registry.rs                       # 处理器注册表（从 config/processors/ 加载）
├── processor.rs                      # Processor trait 定义
│   pub trait Processor { name, priority, matches, execute }
├── output.rs                         # ProcessorOutput 通用输出类型
├── processors/
│   ├── mod.rs
│   ├── tree_sitter.rs                # 封装现有 tree-sitter（CPU 内联）
│   ├── chunk.rs                      # 封装现有 chunker（CPU 内联）
│   ├── extract_text.rs              # PDF/DOCX 文本提取（调外部工具）
│   ├── hanlp_client.rs              # HanLP HTTP 客户端 → inference-server
│   ├── llm_client.rs                # LLM HTTP 客户端 → inference-server
│   └── store.rs                      # Memgraph + Qdrant 写入
├── prompt.rs                         # Prompt 加载 + 变量替换
└── infer_client.rs                   # 共享：inference-server HTTP 客户端
    pub struct InferClient { base_url, client, semaphore }
    pub async fn chat(messages, config) -> ChatResponse
    pub async fn hanlp_nlp(text, tasks) -> NlpResponse
```

### 与现有代码的集成点

```rust
// src/interfaces/cli/build.rs
pub async fn execute(config: BuildConfig) -> Result<()> {
    // 现有逻辑：hash 检测、增量判断 ...

    let pipeline = if config.pipeline_enabled {
        Some(ProcessorEngine::from_config(&config.pipeline_config)?)
    } else {
        None
    };

    // ── 阶段批量执行 ──
    if let Some(engine) = &pipeline {
        // Phase A: CPU 密集阶段（Rust 并行）
        let cpu_results = engine.run_cpu_stages(&changed_files).await?;

        // Phase B: GPU 委托阶段（HTTP → inference-server）
        let gpu_results = engine.run_gpu_stages(&cpu_results).await?;

        // Phase C: 项目/生态级汇总 + 入库
        engine.run_project_stages(&gpu_results).await?;
        engine.run_ecosystem_stages(&gpu_results).await?;
        engine.store_all(&gpu_results).await?;
    }

    Ok(())
}
```

---

## 十二、降级与容错

### 优雅降级策略

```
任一处理器执行失败 → 记录 warn 日志 → 继续执行后续处理器 → 最终入库可用数据

例如：
  tree_sitter 成功 → entities, annotations ✓
  hanlp 失败     → 记录 warn，继续（LLM 仍可基于原文推理）
  llm 失败       → 记录 warn，继续（至少 tree_sitter 的结构数据已入库）
  store 成功     → 数据入库
```

### inference-server 异常处理

```
场景                           Rust 行为
─────────────────────────────────────────────────────
inference-server 未启动         启动时 check /health → 禁用 GPU 处理器
                                只运行 CPU 阶段（tree_sitter + chunk + store）

请求超时 (30s)                  重试 1 次 → 仍失败则跳过该文件
                                其他文件不受影响

inference-server 返回 5xx       记录错误 + 跳过 → 下一个文件继续

并发过高触发 server 背压         Rust 信号量自动限流（max_concurrent=16）
```

### 失败重试

- LLM 推理超时：重试 1 次，仍失败则跳过该文件
- HanLP NLP 超时：重试 1 次，仍失败则跳过，LLM 仍可用原文推理
- inference-server 整体不可用：禁用所有 GPU 处理器，仅运行 CPU 管线

---

## 十三、实施计划

### 第一阶段：核心引擎 + CPU 管线（2-3周）

- [ ] `ProcessorEngine` + `PipelineContext` + `Processor` trait
- [ ] 处理器注册表加载（从 YAML）
- [ ] **HTTP 客户端**：封装 inference-server 调用（InferClient）
- [ ] tree_sitter 处理器（封装现有逻辑）
- [ ] chunk 处理器（封装现有逻辑）
- [ ] store 处理器（封装现有写入逻辑）
- [ ] **增量缓存**：SHA256 hash 检测 + 处理器输出缓存
- [ ] 验证：Java 项目正确入库，inference-server 联通

### 第二阶段：HanLP + LLM 集成（2-3周）

- [ ] HanLP 接入 inference-server（或直连过渡）
- [ ] LLM chat 客户端（OpenAI 兼容 → inference-server）
- [ ] Prompt 模板加载与变量替换
- [ ] HanLP 自定义 NER（从 Memgraph 加载服务词典）
- [ ] 验证：中文文档的实体+关系+摘要正确生成

### 第三阶段：项目级/生态级分析（1-2周）

- [ ] project_stages 汇总逻辑
- [ ] ecosystem_stages 跨项目分析
- [ ] 验证：Spring Cloud 项目的服务调用图正确

### 第四阶段：二进制文件支持（1周）

- [ ] extract_text 处理器（PDF/DOCX）
- [ ] OCR 处理器（可选）

---

## 十四、风险与缓解

| 风险 | 缓解 |
|------|------|
| qwen3-4b 实体抽取质量不如预期 | 保留 tree-sitter 确定性输出作为兜底；Prompt 可迭代优化 |
| HanLP 仅支持中文，多语言文档无法处理 | 多语言文档回退到纯 LLM 推理（raw_text prompt） |
| inference-server 不可用 | 启动 /health 检测 → 降级为纯 CPU 管线（tree-sitter + chunk + store） |
| 大量文件时 LLM 耗时过长 | 增量（仅处理变更文件）+ 信号量限流 + NORMAL 优先级避免阻塞 |
| Prompt 质量依赖人工调优 | 提供默认 Prompt，输出带 schema 校验，不合格自动重试 |
| inference-server 与 embed 服务争抢 GPU | server 内置 TaskRouter 优先级调度（HIGH > NORMAL > LOW） |
| 增量缓存失效（配置变更导致） | 缓存 key 包含 processor config hash，配置变更自动失效 |
