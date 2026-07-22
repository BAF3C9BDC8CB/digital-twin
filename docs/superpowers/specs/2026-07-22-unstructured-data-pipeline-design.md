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
                          dt build (现有)
                               │
                               │  文件列表 + hash 增量检测
                               ▼
                    ┌─────────────────────┐
                    │   Processor Engine  │  ← 新增
                    │   (自动编排引擎)     │
                    └─────────┬───────────┘
                              │
          ┌───────────────────┼───────────────────┐
          │                   │                   │
          ▼                   ▼                   ▼
   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
   │ tree-sitter │    │   HanLP     │    │    LLM      │
   │  (确定性)    │    │ (中文NLP)    │    │  (语义推理)  │
   │  代码AST解析  │    │ 分词/NER/SRL │    │  关系/摘要   │
   └──────┬──────┘    └──────┬──────┘    └──────┬──────┘
          │                   │                   │
          │         ┌─────────┴─────────┐         │
          │         ▼                   ▼         │
          │  ┌─────────────┐    ┌─────────────┐   │
          │  │   chunk     │    │ extract_text│   │
          │  │  (文档分片)  │    │ (PDF/DOCX)  │   │
          │  └─────────────┘    └─────────────┘   │
          │                                       │
          └───────────────────┬───────────────────┘
                              │
                              ▼
                    ┌─────────────────────┐
                    │    Store Writer     │
                    │  Memgraph + Qdrant  │
                    └─────────────────────┘
```

### 设计原则

1. **自动编排**：引擎根据文件类型和处理器能力卡片自动决定执行链，用户无需手工配置 stages 顺序
2. **开放输出**：每个处理器输出写入统一的 `PipelineContext`，下游处理器可任意引用上游输出
3. **处理器自治**：每个处理器自描述其能力（支持的文件类型、产生的输出、依赖关系），引擎据此编排
4. **优雅降级**：任一处理器失败不影响其他处理器，tree-sitter 挂了仍然可以 chunk+store

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

model: transformer
device: cuda

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

#### llm — 语义推理

```yaml
name: llm
priority: 60
enabled: true

match:
  file_extensions: [.java, .py, .rs, .go, .ts, .md, .txt, .yaml, .yml]
  min_chars: 50

model: qwen3-4b
api: http://localhost:11434
temperature: 0.1
max_tokens: 4096
batch_size: 8

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

每个处理器通过 trait 实现：

```rust
#[async_trait]
pub trait Processor: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;

    /// 判断此处理器是否适用于该文件
    fn matches(&self, file: &FileInfo) -> bool;

    /// 执行处理，可从 context 中读取上游处理器的输出
    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, Error>;
}
```

各处理器的实现：

- **tree_sitter**：Rust 内直接集成，复用现有 tree-sitter 解析逻辑
- **chunk**：Rust 内直接集成，复用现有 `chunker.rs`
- **hanlp**：通过 HTTP 调 Python HanLP 服务（hanlp serve），或直接集成 `hanlp-rs`
- **llm**：通过 HTTP 调 ollama API
- **extract_text**：调外部工具（pdf-extract / python-docx），输出纯文本到 context
- **store**：复用现有 Memgraph + Qdrant 写入逻辑

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
pub async fn analyze(&self, file: &FileInfo) -> Result<AnalysisResult> {
    let mut context = PipelineContext::new(file);

    // 1. 收集匹配且启用的处理器，按优先级排序
    let processors: Vec<&dyn Processor> = self.registry
        .iter()
        .filter(|p| p.enabled() && p.matches(file))
        .sorted_by_key(|p| Reverse(p.priority()))
        .collect();

    // 2. 顺序执行
    for processor in processors {
        match processor.execute(&context).await {
            Ok(output) => { context.add(processor.name(), output); }
            Err(e) => {
                log::warn!("Processor {} failed for {}: {}", processor.name(), file.path, e);
                // 继续执行后续处理器（优雅降级）
            }
        }
    }

    // 3. 汇总
    Ok(context.into_result())
}
```

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

┌─ Phase A: 所有文件的 file_stages (并行) ──────────────────────────┐
│                                                                    │
│  gateway/**/*.java     → tree_sitter → hanlp → llm → store       │
│  user-service/**/*.java → tree_sitter → hanlp → llm → store      │
│  order-service/**/*.java→ tree_sitter → hanlp → llm → store      │
│  pay-service/**/*.java  → tree_sitter → hanlp → llm → store      │
│  docs/**/*.md           → chunk → hanlp → llm → store            │
│  *.yml                  → chunk(配置策略) → hanlp → store         │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
                                 │
┌─ Phase B: 每个项目的 project_stages (服务间并行) ──────────────────┐
│                                                                    │
│  gateway       → llm(project_architecture) 汇总本服务内所有文件   │
│  user-service  → llm(project_architecture)                       │
│  order-service → llm(project_architecture)                       │
│  pay-service   → llm(project_architecture)                       │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
                                 │
┌─ Phase C: ecosystem_stages (全局汇总) ─────────────────────────────┐
│                                                                    │
│  llm(service_mesh_topology)       — 构建服务拓扑                  │
│  llm(cross_service_transactions)  — 分布式事务链路                │
│  llm(architecture_health_check)   — 架构风险识别                  │
│  store → 写入生态级实体和关系                                     │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## 八、HanLP 自定义 NER 与微服务调用链

### 8.1 两层互补

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

## 九、配置全景

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

  # 启用的处理器
  processors:
    tree_sitter: true
    hanlp: true
    llm: true
    chunk: true
    extract_text: true
    ocr: false                          # 不需要 OCR 时关闭
    store: true

  # LLM 配置
  llm:
    model: qwen3-4b
    api: http://localhost:11434
    temperature: 0.1

  # HanLP 配置
  hanlp:
    model: transformer
    device: cuda

  # 项目级/生态级分析
  ecosystem:
    enabled: true
    projects:
      - my-microservices
```

---

## 十、代码结构

### 新增模块

```
src/pipeline/                         # 新增模块
├── mod.rs                            # 模块入口
├── engine.rs                         # ProcessorEngine - 自动编排核心
│   pub struct ProcessorEngine { registry }
│   pub async fn analyze(&self, file) -> AnalysisResult
│   pub async fn analyze_project(&self, project) -> ProjectResult
│   pub async fn analyze_ecosystem(&self, ecosystem) -> EcosystemResult
├── context.rs                        # PipelineContext - 数据容器
│   pub struct PipelineContext { raw, outputs, project }
│   pub fn add(name, output)
│   pub fn get<T>(name) -> Option<&T>
│   pub fn resolve_variables(template, ctx) -> String
├── registry.rs                       # 处理器注册表
│   pub fn load(path: &Path) -> ProcessorRegistry
│   pub fn register(processor)
│   pub fn matching(file) -> Vec<&Processor>
├── processor.rs                      # Processor trait 定义
│   pub trait Processor { name, priority, matches, execute }
├── output.rs                         # ProcessorOutput 通用输出类型
│   pub struct ProcessorOutput(HashMap<String, JsonValue>)
├── processors/
│   ├── mod.rs
│   ├── tree_sitter.rs                # 封装现有 tree-sitter
│   ├── hanlp.rs                      # HanLP HTTP 客户端
│   ├── llm.rs                        # Ollama HTTP 客户端
│   ├── chunk.rs                      # 封装现有 chunker
│   ├── extract_text.rs              # PDF/DOCX 文本提取
│   └── store.rs                      # Memgraph + Qdrant 写入
└── prompt.rs                         # Prompt 加载 + 变量替换
    pub fn load(name) -> Prompt
    pub fn render(prompt, ctx) -> String
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

    for file in changed_files {
        // 现有：tree-sitter (代码) + chunker (文档)
        let existing_entities = extract_entities(&file);

        // 新：自动编排管线
        let pipeline_result = if let Some(engine) = &pipeline {
            engine.analyze(&file).await?
        } else {
            PipelineResult::empty()
        };

        // 合并写入
        store_writer.write_all(merge(existing_entities, pipeline_result)).await?;
    }

    // 项目级分析（可选）
    if let Some(engine) = &pipeline {
        engine.analyze_project(&config.project).await?;
    }

    Ok(())
}
```

---

## 十一、降级与容错

### 优雅降级策略

```
任一处理器执行失败 → 记录 warn 日志 → 继续执行后续处理器 → 最终入库可用数据

例如：
  tree_sitter 成功 → entities, annotations ✓
  hanlp 失败     → 记录 warn，继续
  llm 失败       → 记录 warn，继续
  store 成功     → 至少 tree_sitter 的结构数据已入库
```

### 处理器依赖声明

处理器可声明最小依赖要求：

```yaml
# llm 可以不依赖任何前置处理器
dependencies: []
  # 但如果有 tree_sitter 输出，启用 code_with_ast prompt
  # 如果有 hanlp 输出，启用 document_with_nlp prompt
  # 都没有，启用 raw_text prompt（纯文本推理）
```

### 失败重试

- LLM 推理超时：重试 1 次，仍失败则跳过
- HanLP 服务不可用：重试 1 次，仍失败则跳过，但不阻塞 LLM 处理器

---

## 十二、性能预估

基于 RTX 3060 (12GB VRAM)，处理一个中等规模 Spring Cloud 项目（~200 个 Java 文件 + 50 个文档）：

| 阶段 | 处理器 | 单文件耗时 | 200文件并行 |
|------|--------|-----------|------------|
| tree_sitter | Rust 直接调用 | < 10ms | < 2s |
| chunk | Rust 直接调用 | < 5ms | < 1s |
| HanLP | GPU 推理 | ~200ms/文件 | ~40s (串行) |
| LLM (qwen3-4b + INT4) | GPU 推理 | ~3-5s/文件 | ~10min (batch=8) |
| Store | Memgraph + Qdrant | < 50ms | < 10s |

**总计：约 10-15 分钟处理一个完整微服务项目**（主要是 LLM 推理时间，可通过增大 batch_size 优化）。

---

## 十三、实施计划

### 第一阶段：核心引擎 + 代码管线（2-3周）

- [ ] `ProcessorEngine` + `PipelineContext` + `Processor` trait
- [ ] 处理器注册表加载（从 YAML）
- [ ] tree_sitter 处理器（封装现有逻辑）
- [ ] store 处理器（封装现有写入逻辑）
- [ ] 验证：Java 项目的语法实体正确入库

### 第二阶段：HanLP + LLM 集成（2-3周）

- [ ] HanLP Python 服务 / hanlp-rs 集成
- [ ] LLM ollama 客户端
- [ ] Prompt 模板加载与变量替换
- [ ] HanLP 自定义 NER（从 Memgraph 加载服务词典）
- [ ] 验证：中文文档的实体+关系+摘要正确生成

### 第三阶段：项目级/生态级分析（1-2周）

- [ ] project_stages 汇总逻辑
- [ ] ecosystem_stages 跨项目分析
- [ ] 微服务拓扑 Prompt 模板
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
| GPU 显存不足同时加载 HanLP + LLM | 串行加载：HanLP 跑完后释放，再加载 LLM |
| 大量文件时 LLM 推理耗时长 | 批处理 + 增量（仅处理变更文件）+ batch_size 调优 |
| Prompt 质量依赖人工调优 | 提供默认 Prompt，输出带 schema 校验，不合格自动重试 |
