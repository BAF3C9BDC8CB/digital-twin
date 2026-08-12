# Digital Twin V2

Rust 实现的数字孪生服务，为项目代码、配置和知识提供索引、图谱存储、向量检索及 AI Agent 集成能力。

> 本文只记录当前仓库中已实现并可验证的用法。详细设计请参阅 [`docs/`](docs/)。

## 当前能力

- 扫描项目文件并构建增量索引
- 使用 Memgraph 保存实体及关系
- 使用 Qdrant 保存向量并执行语义检索
- 使用 SQLite 保存快照和任务状态
- 写入知识、事件及任务经验
- 提供统一搜索、环境感知和健康检查
- 提供 Jenkins 操作与同步能力
- 通过 MCP 为 OpenCode、Claude Code 等 AI 工具提供结构化调用
- 通过 gRPC daemon 提供服务端入口

Nacos、K8s 和 HanLP 相关内容在仓库中仍有部分配置或设计痕迹，但当前不应视为已接入的独立 CLI 能力；使用前请以 `dt --help` 和实际配置为准。

## 架构概览

```text
CLI / gRPC daemon
        │
        ├── Memgraph       图数据、实体和关系
        ├── Qdrant         向量存储与检索
        ├── SQLite         增量快照和任务状态
        └── 外部推理服务   embedding、rerank、LLM（按配置启用）
```

代码按领域层、基础设施层、应用层、接口层和共享模块组织，具体实现以 `src/` 为准。

## 前置依赖

- Rust stable 和 Cargo
- Memgraph：连接地址由配置文件决定；代码默认使用 `bolt://localhost:7687`，示例模板使用 `bolt://localhost:7688`，请以实际部署端口为准
- Qdrant：连接地址由配置文件决定；常见 REST 端口为 `6333`、gRPC 端口为 `6334`，当前模板和客户端配置可能不同，部署时必须以实际连接方式验证
- 可选的 OpenAI-compatible 推理服务，用于 embedding、rerank 或 LLM
- 推理 provider、模型和管线阶段以 `config/pipeline.yaml` 为准；不要在 README 中假定固定 provider 或模型
- HanLP 当前不是已验证的默认管线处理器，不应按必需依赖安装

Memgraph、Qdrant、推理服务和相关凭证不会由本项目自动安装。请先准备服务，再配置连接信息。

## 构建

```bash
cargo build --release
./target/release/dt --help
```

开发期间也可以直接运行：

```bash
cargo run -- --help
```

## 配置

1. 复制配置模板：

   ```bash
   mkdir -p ~/.config/digital-twin
   cp config/config.yaml.example ~/.config/digital-twin/config.yaml
   ```

2. 根据环境修改 `~/.config/digital-twin/config.yaml`。
3. 根据管线需求修改 `config/pipeline.yaml`，并检查其中的 provider、模型和 URL。
4. 事件 Hook 模板位于 `config/event-hooks.yaml`；运行时从 `~/.config/digital-twin/event-hooks.yaml` 加载，请按部署方式复制并配置。

如果需要使用仓库内的 Hook 模板，可执行：

```bash
cp config/event-hooks.yaml ~/.config/digital-twin/event-hooks.yaml
```

如果使用 `dt build` 的无参数模式，还需要在主配置中注册项目：

```yaml
projects:
  - name: my-project
    path: /path/to/project
```

配置模板中的默认项包括 Memgraph、Qdrant、SiliconFlow、XInference 和 GLM Coding 等 provider。实际使用哪个 provider 由配置决定；不要把 API 密钥提交到仓库，建议使用环境变量或密钥管理系统。

## 常用命令

所有命令都可以使用 `dt <command> --help` 查看当前参数。以下示例以已正确配置后端服务为前提。

Cargo package 名称为 `dt-daemon`，构建出的可执行文件名称为 `dt`。

最小启动流程：

```bash
# 确认 CLI 可用
cargo run -- --help

# 初始化 Schema 并检查后端
./target/release/dt schema init
./target/release/dt health

# 索引指定项目并搜索
./target/release/dt build --path /path/to/project --name my-project
./target/release/dt search "关键词" --project my-project
```

### 构建和搜索

```bash
# 构建配置中注册的项目
 dt build

# 构建指定项目
 dt build --path /path/to/project --name my-project

# 单文件增量更新
 dt build --file /path/to/project/src/main.rs

# 绕过增量快照进行全量构建
 dt build --path /path/to/project --full

# 跨世界搜索
 dt search "关键词"
 dt search "关键词" --world code --limit 10
 dt search "关键词" --world knowledge --project my-project

# 感知当前目录所属项目及索引状态
 dt sense
```

`--source knowledge` 用于执行知识节点同步；`dt kg-sync` 仍可使用但已弃用，建议改用前者。

### 知识和事件

```bash
# 写入知识
 dt memorize Decision architecture "decision: use Memgraph; reason: graph relationships" --project my-project

# 触发配置文件中定义的 Hook
 dt event code_modified '{"project":"my-project","file_path":"src/main.rs"}'

# 从任务中沉淀经验
 dt learn "迁移支付服务" --pattern "分阶段切换" --pitfalls "兼容旧配置" --success true
```

`dt event` 接收 Hook 名称和 JSON 上下文，实际字段及副作用以 `config/event-hooks.yaml` 为准。

### 运维和同步

```bash
# 检查 Memgraph、Qdrant 和 SQLite
 dt health

# 初始化图数据库 Schema
 dt schema init

# 启动或查看 gRPC daemon
 dt daemon start
 dt daemon status

# 将 KG 节点同步到 Qdrant（推荐）
dt build --source knowledge
dt build --source knowledge --full

# Jenkins 操作与同步
 dt jcli list
 dt jc-sync

# 预览清理内容；真正清理必须显式确认
 dt clean --dry-run
 dt clean --confirm

# 备份相关操作
 dt backup
 dt backup list
```

`clean` 是破坏性命令，执行前请确认目标和备份状态。备份恢复、校验及其他参数请查看 `dt backup --help`。

当前 CLI 的顶层命令以 `dt --help` 输出为准；不要根据旧文档使用不存在的 `dt nacos-sync` 或 `dt k8s-sync` 命令。Nacos、K8s 等相关能力应以当前同步模块、插件和配置为准。

## MCP 集成

MCP 服务入口为 [`mcp/mcp-server.py`](mcp/mcp-server.py)。当前 MCP server 主要通过子进程调用 `dt` CLI，提供搜索、构建、健康检查、知识写入以及 Jenkins、服务和 K8s 相关工具；实际工具列表和参数以该文件及 MCP 服务输出为准。gRPC 集成属于独立的 daemon 入口，不应假定 MCP 已通过 gRPC 通信。

在 AI 工具中，优先使用 MCP 返回的结构化结果；MCP 不可用时再使用 `dt` CLI。有关操作建议可参考 [`skills/digital-twin-ops`](skills/digital-twin-ops/SKILL.md)（Hermes 集成的操作技能，含 references 知识点与 agent-workflow 工作流指南）。

## 测试

```bash
cargo test
```

单元测试位于 `src/` 内（`#[cfg(test)]` 模块）。需要 Memgraph、Qdrant 或推理服务的测试，应先准备对应服务和配置。

## 当前边界

以下内容目前不能仅凭 README 视为已实现的独立能力：

- `dt nacos-sync`、`dt k8s-sync`、`dt kub` 等旧文档中的命令；`dt kg-sync` 虽仍可运行，但已标记为弃用；
- HanLP 处理器及其固定管线阶段；
- 源码编辑、K8s 事件、Nacos 变更等自动 Hook；部分 Hook 仍需手动触发或属于预留配置；
- MCP 直接通过 gRPC 通信。

请以 `dt --help`、`dt <command> --help`、`config/` 模板和当前源码为准。

## 项目结构

```text
src/        Rust 主代码（领域、基础设施、应用和接口层）
config/     配置、管线参数、Prompt 和事件 Hook 模板
mcp/        MCP 服务
proto/      Protobuf 定义
docs/       设计规格和实施文档
skills/     Hermes 集成的操作技能（digital-twin-ops：SKILL.md + references + agent-workflow）
```

项目的实际目录和文件以当前分支内容为准，不在 README 中维护文件数量或模块数量。`scripts/setup.sh` 仍包含旧目录引用，不作为当前安装入口；请使用上面的 Cargo 构建和手动配置流程。

## 相关文档

- [`docs/`](docs/)：设计规格和实施计划
- [`config/config.yaml.example`](config/config.yaml.example)：配置模板
- [`skills/digital-twin-ops/references/agent-workflow/JCLI-GUIDE.md`](skills/digital-twin-ops/references/agent-workflow/JCLI-GUIDE.md)：Jenkins 部署指南
- [`skills/digital-twin-ops/references/agent-workflow/WRITE-EVENTS.md`](skills/digital-twin-ops/references/agent-workflow/WRITE-EVENTS.md)：事件写入指南

## 许可证

当前 README 原先声明为 MIT。使用或发布前请确认仓库中的许可证文件和项目授权信息与该声明一致。

> 本文档中的命令、默认值和功能说明应随代码与配置同步更新；遇到差异时，以 `dt --help`、配置模板和实际源码为准。
