# Digital Twin

AI 辅助开发的持久化记忆层。Digital Twin 结合 **Neo4j 知识图谱**（结构化记忆：事件、决策、配置）和 **Qdrant 向量数据库**（语义代码搜索），让 AI 能够在多个会话之间保持上下文。

---

## 架构

```
┌─────────────────────────────────────────────────────────┐
│                    AI 助手 (OpenCode)                     │
│  AGENTS.md 触发规则: dt event / dt memorize / dt update  │
└──────────────────────┬──────────────────────────────────┘
                       │ dt CLI
┌──────────────────────▼──────────────────────────────────┐
│                    dt (Rust 命令行工具)                   │
│                                                         │
│  ┌──────────┐  ┌───────────┐  ┌────────┐  ┌─────────┐  │
│  │ dt event │  │ dt memorize│  │dt build│  │dt update│  │
│  │ dt remove│  │ dt search  │  │ index  │  │validate │  │
│  └─────┬────┘  └─────┬─────┘  └───┬────┘  └────┬────┘  │
│        │              │            │            │       │
│  ┌─────▼──────────────▼────────────▼────────────▼─────┐ │
│  │           tree-sitter（7 种语言解析器）              │ │
│  └───────────────────────┬───────────────────────────┘ │
└──────────────────────────┼─────────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────────┐
│   Neo4j      │  │   Qdrant     │  │   嵌入服务        │
│  知识图谱    │  │   向量数据库  │  │ (Python + BGE)   │
│ localhost    │  │  localhost   │  │  localhost:8001   │
│   :7474      │  │   :6333      │  └──────────────────┘
└──────────────┘  └──────────────┘
```

### 组件

| 组件 | 语言 | 用途 |
|------|------|------|
| `engine-rust/` | Rust | 核心 CLI（`dt`）：索引、搜索、事件/记忆管理 |
| `services/embed-server/` | Python + sentence-transformers | 文本向量化推理（BGE-base-zh-v1.5） |
| `services/search-web/` | Python + Flask | Web 搜索界面 |
| `config.yaml` | YAML | 中心配置 |

---

## 环境要求

### 运行时

| 服务 | 版本 | 用途 |
|------|------|------|
| [Neo4j](https://neo4j.com/download/) | 5.x | 知识图谱存储 |
| [Qdrant](https://qdrant.tech/documentation/quick-start/) | 1.x | 向量数据库，用于语义搜索 |
| Python | 3.10+ | 嵌入服务 |
| Rust | 1.75+ | 编译 `dt` CLI |

### 系统依赖

```bash
# tree-sitter 编译依赖
sudo apt install build-essential cmake pkg-config

# Python 依赖
sudo apt install python3 python3-pip python3-venv
```

---

## 安装

### 1. 启动所需服务

**Neo4j：**
```bash
# 原生安装（systemd）
sudo systemctl start neo4j

# 或从 https://neo4j.com/download/ 下载手动安装
```

**Qdrant：**
```bash
# 原生安装
curl -L https://github.com/qdrant/qdrant/releases/latest/download/qdrant-x86_64-unknown-linux-gnu.tar.gz | tar xz
./qdrant &
```

### 2. 嵌入服务

```bash
cd services/embed-server
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
python3 main.py
# 启动在 http://localhost:8001
```

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
dt update --path /path/to/project --name my-project --file src/main.py

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

# 跨项目搜索
dt search "支付超时" --all --limit 20

# JSON 格式输出
dt search "退款逻辑" --project order-center --json

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
  │   └─ 缓存中有但磁盘已删除 → 从 Neo4j + Qdrant 删除
  │
  ├─ 每个变更文件：
  │   1. tree-sitter 解析 → 提取方法/类
  │   2. HTTP 调用嵌入服务 → 生成 768 维向量
  │   3. 写入 Qdrant（向量 + 负载数据）
  │   4. 写入 Neo4j（Method 节点 + Class + CONTAINS 关系）
  │   5. 更新 SQLite 哈希缓存
  │
  └─ 重建 Neo4j 中的 CALLS 关系
```

---

## AI 集成（OpenCode）

将 `AGENTS.md` 软链到 `~/AGENTS.md`（或复制），AI 助手会自动遵循以下规则：

| 触发操作 | 命令 |
|---------|------|
| 安装软件 | `dt event --type SoftwareInstalled --entity-type Software ...` |
| 配置变更 | `dt event --type ConfigChange --entity-type NacosConfig ...` |
| 架构决策 | `dt memorize --type Decision --entity-type ArchitectureDecision ...` |
| 生产部署 | `dt event --type Deploy --entity-type JenkinsJob ...` |
| 源码修改 | `dt update --path <根目录> --name <项目名> --file <路径>` |
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
digital-twin/
├── README.md                    # 英文文档
├── README.zh.md                 # 中文文档
├── AGENTS.md                    # AI 集成规则
├── SKILL.md                     # OpenCode Skill
├── config.yaml.example          # 配置模板
├── setup.sh                     # 一键部署脚本
├── dt-sync                      # 增量同步编排脚本
│
├── engine-rust/                 # Rust CLI (dt)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs              # CLI 入口（clap）
│       ├── config.rs            # 配置读取（YAML + 环境变量）
│       ├── neo4j.rs             # Neo4j HTTP 客户端
│       ├── qdrant.rs            # Qdrant HTTP 客户端
│       ├── embed.rs             # 嵌入服务 HTTP 客户端
│       ├── scanner.rs           # 文件扫描器
│       ├── parser.rs            # tree-sitter 解析器
│       ├── models.rs            # 数据模型
│       ├── build.rs             # 索引/构建/更新/验证逻辑
│       ├── event.rs             # Event 节点写入
│       ├── knowledge.rs         # Knowledge 节点写入
│       ├── remove.rs            # 代码实体删除
│       └── search.rs            # 语义搜索
│
├── services/
│   ├── embed-server/            # 向量嵌入服务（Python）
│   │   ├── main.py
│   │   └── requirements.txt
│   └── search-web/              # Web 搜索界面（Python）
│       ├── app.py
│       └── templates/
│
└──（运行时数据目录）             # SQLite 缓存、Neo4j 数据等
```

---

## 配置参考

`config.yaml` 字段说明：

| 键 | 默认值 | 说明 |
|-----|--------|------|
| `services.neo4j.url` | `http://localhost:7474` | Neo4j REST API 地址 |
| `services.neo4j.user` | `neo4j` | Neo4j 用户名 |
| `services.neo4j.password` | `neo4j` | Neo4j 密码 |
| `services.qdrant.url` | `http://localhost:6333` | Qdrant REST API 地址 |
| `services.embed_server.url` | `http://localhost:8001` | 嵌入服务地址 |
| `services.embed_server.dim` | `768` | 向量维度 |
| `services.embed_server.model` | `BAAI/bge-base-zh-v1.5` | 嵌入模型名称 |
| `snapshot_dir` | `/var/lib/digital-twin/snapshots` | 快照目录 |

环境变量 `DT_CONFIG` 可覆盖配置文件路径。

---

## 数据存储

| 数据 | 位置 | 技术 |
|------|------|------|
| 知识图谱 | Neo4j（`localhost:7474`） | 节点与关系 |
| 代码向量 | Qdrant（`localhost:6333`） | 按项目的集合 |
| 文件哈希缓存 | `/var/lib/digital-twin/lazy.db` | SQLite |
| 文本嵌入 | 内存 / CPU | BGE-base-zh-v1.5 |

---

## 许可

MIT
