# Digital Twin

AI 辅助开发的持久化记忆层。Digital Twin 结合 **Memgraph 知识图谱**（结构化记忆：事件、决策、配置）和 **Qdrant 向量数据库**（语义代码搜索），让 AI 能够在多个会话之间保持上下文。

---

## 架构

```
┌─────────────────────────────────────────────────────────┐
│                    AI 助手 (OpenCode)                     │
│  AGENTS.md 触发规则: dt event / dt memorize / dt build   │
└──────────────────────┬──────────────────────────────────┘
                       │ dt CLI
┌──────────────────────▼──────────────────────────────────┐
│                    dt (Rust 命令行工具)                   │
│                                                         │
│  ┌──────────┐  ┌───────────┐  ┌────────┐  ┌─────────┐  │
│  │ dt event │  │ dt memorize│  │dt build│  │dt search │  │
│  │ dt remove│  │ dt search  │  │ index  │  │validate │  │
│  └─────┬────┘  └─────┬─────┘  └───┬────┘  └────┬────┘  │
│        │              │            │            │       │
│  ┌─────▼──────────────▼────────────▼────────────▼─────┐ │
│  │           tree-sitter（7 种语言解析器）              │ │
│  └───────────────────────┬───────────────────────────┘ │
│                          │ 子进程                      │
│  ┌───────────────────────▼───────────────────────────┐ │
│  │    dt-inference-server (BGE-M3 / reranker / Qwen) │ │
│  └───────────────────────────────────────────────────┘ │
└──────────────────────────┬──────────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  
┌──────────────┐  ┌──────────────┐  
│   Memgraph      │  │   Qdrant     │  
│  知识图谱    │  │   向量数据库  │  
│ localhost    │  │  localhost   │  
│   :7474      │  │   :6333      │  
└──────────────┘  └──────────────┘  
```

### 组件

| 组件 | 语言 | 用途 |
|------|------|------|
| `engine-rust/` | Rust | 核心 CLI（`dt`）：索引、搜索、事件/记忆管理 |
| `services/inference-server/` | Python + transformers | 统一推理服务：Embed (BGE-M3)、Rerank (BGE-reranker)、LLM Chat (Qwen3-4B) |
| `services/search-web/` | Python + Flask | Web 搜索界面 |
| `config.yaml` | YAML | 中心配置 |

---

## 环境要求

### 运行时

| 服务 | 版本 | 用途 |
|------|------|------|
| [Memgraph](https://memgraph.com/download/) | 5.x | 知识图谱存储 |
| [Qdrant](https://qdrant.tech/documentation/quick-start/) | 1.x | 向量数据库，用于语义搜索 |
| Python | 3.10+ | dt-embed CLI |
| Rust | 1.75+ | 编译 `dt` CLI |

### 系统依赖

```bash
# tree-sitter 编译依赖
sudo apt install build-essential cmake pkg-config

# Python 依赖（dt-embed CLI）
sudo apt install python3 python3-pip
```

---

## 安装

### 1. 启动所需服务

**Memgraph：**
```bash
# 原生安装（systemd）
sudo systemctl start memgraph

# 或从 https://memgraph.com/download/ 下载手动安装
```

**Qdrant：**
```bash
# 原生安装
curl -L https://github.com/qdrant/qdrant/releases/latest/download/qdrant-x86_64-unknown-linux-gnu.tar.gz | tar xz
./qdrant &
```

### 2. dt-inference-server

统一模型推理服务，提供 Embed (BGE-M3)、Rerank (BGE-reranker) 和 LLM Chat (Qwen3-4B) 三种能力。

```bash
cd services/inference-server/src
python3 server.py --port 50051 --llm-port 50052
```

- **`:50051`** — gRPC Embed 服务 (旧 `dt-embed` 兼容，`dt build` 直连)
- **`:50052`** — REST API (chat/rerank/embed/health/metrics/nlp)
- 模型首次请求时懒加载，启动不占显存
- 内置 `TaskRouter` 三级优先级队列 (HIGH/NORMAL/LOW)，LOW 支持自动攒批

### 3. 编译并安装 `dt` CLI

```bash
cd engine-rust
cargo build --release
sudo cp target/release/dt /usr/local/bin/dt

# 验证
dt --help
```

### 4. 配置

```bash
cp config.yaml.example config.yaml
```

编辑 `config.yaml` 匹配你的环境。

`dt` CLI 会自动从项目根目录读取 `config.yaml`。可通过 `DT_CONFIG` 环境变量覆盖路径。

新增 `config/pipeline.yaml` 配置 Pipeline 引擎参数（推理服务地址、处理器开关等），以及 `config/prompts/*.yaml` 管理 LLM 提示词模板。

### 5. 安装 OpenCode Skill（可选）

```bash
# Skill 告诉 AI 助手自动查询知识图谱
mkdir -p ~/.opencode/skills/digital-twin
cp SKILL.md ~/.opencode/skills/digital-twin/SKILL.md

# 软链 AGENTS.md 作为 AI 行为规则
ln -sf "$(pwd)/AGENTS.md" ~/AGENTS.md
```

或者一键运行安装脚本：

```bash
bash setup.sh
```

---

## 使用方法

### 代码索引

```bash
# 全量重建
dt index --path /path/to/project --name my-project

# 增量构建（基于 SQLite 哈希缓存）
dt build --path /path/to/project --name my-project

# 单文件增量更新（编辑文件后使用）
dt build --file /path/to/project/src/main.py

# 删除文件
dt remove --project my-project --file src/old.py

# 删除整个项目
dt remove --project my-project --all
```

### 知识图谱操作

```bash
# 记录事件（如部署、配置变更）
dt event --type Deploy \
  --entity-id "user-center" \
  --entity-type JenkinsJob \
  --project "user-center" \
  --details "branch: main, env: production"

# 记录知识条目（如架构决策）
dt memorize --type Decision \
  --entity-id "REST-to-gRPC" \
  --entity-type ArchitectureDecision \
  --project "user-center" \
  --details "decision: 迁移到 gRPC; reason: 延迟降低 10 倍; scope: user-service"
```

### 语义代码搜索

```bash
# 项目内搜索
dt search "用户登录流程" --project user-center

# 查询扩展（多变体合并，低级模型推荐）
dt search "支付超时" --project order-center --expand

# 跨项目搜索
dt search "支付超时" --all --limit 20

# JSON 格式输出
dt search "退款逻辑" --project order-center --json

# 知识图谱向量搜索（无需写 Cypher）
dt search-kg "MySQL 正式环境 账号 密码" --limit 10

# 重建调用关系图
dt build-call-graph --name user-center
```

### Nacos 配置同步

```bash
# 同步测试环境 (nacos.newoffen.net)
dt nacos-sync --env test

# 同步生产环境 (nacos.newoffen.com)
dt nacos-sync --env prod

# 同步全部
dt nacos-sync --env all
```

### 工具

```bash
# 验证解析质量（干运行，不写入数据库）
dt validate --path /path/to/project --name my-project

# 解析单个文件并输出 JSON
dt parse --file src/main.py --project my-project --root /path/to/project

# 同步 KG 节点到向量库（KG→Qdrant 桥接）
dt kg-sync                  # 全量同步
dt kg-sync --incremental    # 增量同步
```

---

## 增量构建原理

```
dt build --path /proj --name myapp
  │
  ├─ 扫描项目目录（忽略 node_modules, .git 等）
  │
  ├─ 计算每个文件的 SHA1 哈希
  │
  ├─ 与 SQLite 缓存比较（/var/lib/digital-twin/lazy.db）
  │   ├─ 哈希一致 → 跳过（未变更）
  │   ├─ 哈希不同 → 重新索引
  │   └─ 缓存中有但磁盘已删除 → 从 Memgraph + Qdrant 删除
  │
  ├─ 每个变更文件：
  │   1. tree-sitter 解析 → 提取方法/类
   │   2. dt-inference-server 调用 → 生成 1024 维向量 (BGE-M3)
  │   3. 写入 Qdrant（向量 + 负载数据）
  │   4. 写入 Memgraph（Method 节点 + Class + CONTAINS 关系）
  │   5. 更新 SQLite 哈希缓存
  │
  └─ 重建 Memgraph 中的 CALLS 关系
```

---

## dt-inference-server

统一模型推理服务，合并旧 `dt-embed` (Python + BGE-M3) 为单一服务，新增 Rerank 和 LLM Chat 能力。

**端口：**
- `:50051` — gRPC Embed 服务（旧版兼容，Rust dt-daemon 直连）
- `:50052` — REST API (chat/rerank/embed/health/metrics/nlp)

**核心组件：**
- **TaskRouter** — 三级优先级队列 (HIGH/NORMAL/LOW)，LOW 支持自动攒批 (64条 或 0.5s)
- **ModelRegistry** — 模型懒加载 + 空闲自动卸载，支持 BGE-M3 / BGE-reranker-large / Qwen3-4B
- **gRPC endpoint** — 兼容旧 `dt-embed` 协议，`dt build` 无需修改

详见 [services/inference-server/README.md](services/inference-server/README.md)。

## Pipeline Engine（管线引擎）

处理器编排框架，将非结构化文件（代码/文档/文本）通过多阶段处理管道转换为结构化知识。

**处理流程：**
```
File → TreeSitterProcessor → ChunkProcessor → HanlpClientProcessor → LlmClientProcessor → StoreProcessor → KG+Qdrant
```

**核心组件：**
- **Processor trait** — 通用处理器接口，按 `priority()` 自动排序编排
- **ProcessorEngine** — `analyze_file()` / `analyze_batch()` 入口，CPU/GPU 阶段分离
- **InferClient** — HTTP 客户端连接 dt-inference-server (embed/rerank/chat)
- **PromptRegistry** — YAML 模板管理 (`config/prompts/*.yaml`)
- **PipelineConfig** — `config/pipeline.yaml` 配置

**内置处理器：**

| 处理器 | 优先级 | 阶段 | 功能 |
|--------|--------|------|------|
| TreeSitterProcessor | 0 | CPU | tree-sitter AST 解析 |
| ChunkProcessor | 1 | CPU | 文档分块 + 边界检测 |
| HanlpClientProcessor | 2 | GPU | HanLP NLP 标注（分词/词性/命名实体） |
| LlmClientProcessor | 3 | GPU | LLM 摘要生成 + 标签提取 |
| StoreProcessor | 4 | CPU | 结果写入 Memgraph + Qdrant |

**测试命令：**
- `dt build --test` — 对真实项目运行 BuildCommand，验证 KG+Qdrant 写入
- `dt clean --test` — 删除所有 `test-` 前缀的测试数据

---

## AI 集成（OpenCode）

将 `AGENTS.md` 软链到 `~/AGENTS.md`（或复制），AI 助手会自动遵循以下规则：

| 触发操作 | 命令 |
|---------|------|
| 安装软件 | `dt event --type SoftwareInstalled --entity-type Software ...` |
| 配置变更 | `dt event --type ConfigChange --entity-type NacosConfig ...` |
| 架构决策 | `dt memorize --type Decision --entity-type ArchitectureDecision ...` |
| 生产部署 | `dt event --type Deploy --entity-type JenkinsJob ...` |
| 源码修改 | `dt build --path <根目录> --name <项目名>` 或 `dt build --file <绝对路径>` |
| 文件删除 | `dt remove --project <项目名> --file <路径>` |

---

## 支持的语言

| 语言 | 文件扩展名 | 解析器 |
|------|-----------|--------|
| Java | `.java` | tree-sitter-java |
| TypeScript | `.ts`, `.tsx` | tree-sitter-typescript |
| Python | `.py` | tree-sitter-python |
| Go | `.go` | tree-sitter-go |
| Rust | `.rs` | tree-sitter-rust |
| PHP | `.php` | tree-sitter-php |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter-javascript |

---

## 项目结构

```
digital-twin-v2/
├── README.md                    # 英文文档
├── README.zh.md                 # 中文文档
├── AGENTS.md                    # AI 集成规则
├── config.yaml                  # 集中配置
├── config/
│   ├── pipeline.yaml            # Pipeline 引擎配置
│   └── prompts/                 # LLM prompt 模板
│       ├── code_with_ast.yaml
│       ├── document_with_nlp.yaml
│       └── raw_text.yaml
│
├── src/                         # Rust 核心 CLI (dt)
│   ├── main.rs                  # CLI 入口（clap）+ gRPC 服务启动
│   ├── lib.rs
│   ├── domain/                  # 领域层
│   │   ├── config.rs
│   │   ├── error.rs
│   │   ├── id.rs
│   │   ├── traits.rs
│   │   └── types.rs
│   ├── infrastructure/          # 基础设施层
│   │   ├── embedder.rs          # dt-inference-server gRPC 客户端
│   │   ├── memgraph.rs          # Memgraph 客户端
│   │   ├── parser/              # tree-sitter 解析器
│   │   ├── qdrant/              # Qdrant gRPC 客户端
│   │   ├── scanner.rs           # 文件扫描
│   │   └── sqlite/              # SQLite 哈希缓存
│   ├── application/             # 应用层
│   │   ├── build/               # dt build / update / watch
│   │   ├── context/             # 六世界上下文构建
│   │   ├── knowledge/           # memorize / learn / event
│   │   ├── pipeline/            # Pipeline Engine 处理器编排
│   │   │   ├── engine.rs        # ProcessorEngine (analyze_file/batch)
│   │   │   ├── registry.rs      # 优先级排序 + 注册中心
│   │   │   ├── processor.rs     # Processor trait
│   │   │   ├── config.rs        # PipelineConfig
│   │   │   ├── context.rs       # 共享 PipelineContext
│   │   │   ├── infer_client.rs  # HTTP → inference-server
│   │   │   ├── prompt.rs        # YAML 提示词注册表
│   │   │   ├── output.rs        # 输出类型
│   │   │   ├── processors/      # 内置处理器
│   │   │   │   ├── tree_sitter.rs    # AST 解析
│   │   │   │   ├── chunk.rs          # 文档分块
│   │   │   │   ├── hanlp_client.rs   # HanLP NLP 标注
│   │   │   │   ├── llm_client.rs     # LLM 摘要/标签
│   │   │   │   └── store.rs          # KG+Qdrant 写入
│   │   │   └── test/            # 集成测试
│   │   │       ├── runner.rs    # 测试执行器
│   │   │       ├── cleanup.rs   # 测试数据清理
│   │   │       └── report.rs    # 测试报告
│   │   ├── plugins/             # Plugin 插件系统
│   │   └── sync/                # nacos/k8s/kg 同步
│   ├── interfaces/              # 接口层
│   │   ├── cli/                 # CLI 辅助模块
│   │   └── grpc/                # gRPC 服务
│   └── shared/                  # 横切关注点
│       ├── chunker.rs
│       ├── coordinator.rs
│       ├── logging/
│       └── vectorizer.rs
│
├── services/
│   ├── inference-server/        # dt-inference-server（Python）
│   │   ├── src/
│   │   │   └── server.py        # 统一推理服务入口
│   │   └── README.md
│   └── search-web/              # Web 搜索界面（Python）
│       ├── app.py
│       └── templates/
│
├── docs/                        # 架构设计文档
│   ├── architecture-v3-single-crate-layered.md
│   ├── architecture-v2-six-worlds.md
│   ├── architecture-v2-data-schema.md
│   ├── architecture-v2-data-pipeline.md
│   ├── architecture-v2-pipeline-impl.md
│   ├── architecture-v2-project-structure.md
│   ├── architecture-v2-mcp-api-spec.md
│   └── superpowers/specs/
│       ├── 2026-07-22-inference-server-refactor-design.md
│       ├── 2026-07-22-unstructured-data-pipeline-design.md
│       └── 2026-07-22-build-test-design.md
│
├── test/
│   └── fixtures/                # 测试夹具
│       ├── java/OrderController.java
│       ├── python/payment.py
│       ├── markdown/architecture.md
│       └── yaml/config.yaml
│
└──（运行时数据目录）             # SQLite 缓存、Memgraph 数据等
```

---

## 配置参考

`config.yaml` 字段说明：

| 键 | 默认值 | 说明 |
|-----|--------|------|
| `services.memgraph.url` | `http://localhost:7474` | Memgraph REST API 地址 |
| `services.memgraph.user` | `memgraph` | Memgraph 用户名 |
| `services.memgraph.password` | `memgraph` | Memgraph 密码 |
| `services.qdrant.url` | `http://localhost:6333` | Qdrant REST API 地址 |
| `services.embed_server.url` | `http://localhost:8001` | （已废弃）dt 直接调 dt-embed CLI |
| `services.embed_server.dim` | `1024` | 向量维度 |
| `services.embed_server.model` | `BAAI/bge-m3` | 嵌入模型名称 |
| `snapshot_dir` | `/var/lib/digital-twin/snapshots` | 快照目录 |

环境变量 `DT_CONFIG` 可覆盖配置文件路径。

---

## 数据存储

| 数据 | 位置 | 技术 |
|------|------|------|
| 知识图谱 | Memgraph（`localhost:7474`） | 节点与关系 |
| 代码向量 | Qdrant（`localhost:6333`） | 按项目的集合 |
| 文件哈希缓存 | `/var/lib/digital-twin/lazy.db` | SQLite |
| 文本嵌入 | 内存 / CPU | bge-m3 |

---

## 许可

MIT
