# Digital Twin v0.1.3 完整配置文档

适用版本: v0.1.3 (2026-08-13 发布)
平台: Linux / macOS / Windows (原生二进制 + 交叉编译)

本仓库实现「数字孪生知识图谱」服务: 扫描项目代码/配置/文档, 用 Memgraph 存实体关系图、
Qdrant 存向量、SQLite 存快照, 并提供 `dt` CLI 与 `dt-mcp` (MCP server) 供 AI Agent 调用。

---

## 1. 包内容与目录布局

```
dt-release-v0.1.3/
├── bin/
│   ├── dt                     # Linux/macOS CLI
│   ├── dt-mcp                 # Linux/macOS MCP server
│   └── windows/
│       ├── dt.exe             # Windows CLI (x86_64-pc-windows-gnu 交叉编译)
│       └── dt-mcp.exe         # Windows MCP server
├── config/
│   ├── config.yaml.example        # 主配置模板
│   ├── pipeline.yaml.example      # 构建管线配置模板 (api_key 已脱敏为占位符)
│   ├── event-hooks.yaml.example   # 事件 Hook 模板
│   └── prompts/
│       ├── code_analysis.yaml     # 代码分析系统提示词
│       ├── code_with_ast.yaml
│       ├── document_with_nlp.yaml
│       ├── nacos_config.yaml
│       └── raw_text.yaml
├── docs/
│   └── CONFIG.md              # 本文档
└── README.md                  # 快速上手
```

## 2. 配置文件加载位置 (固定约定)

| 文件 | Linux/macOS | Windows |
|---|---|---|
| 主配置 config.yaml | `~/.config/digital-twin/config.yaml` | `%USERPROFILE%\.config\digital-twin\config.yaml` |
| 管线配置 pipeline.yaml | `~/.config/digital-twin/pipeline.yaml` | 同上目录 |
| 事件 Hook event-hooks.yaml | `~/.config/digital-twin/event-hooks.yaml` | 同上目录 |
| 提示词 prompts/*.yaml | `~/.config/digital-twin/prompts/` | 同上目录 |
| 快照库 snapshots.db | `~/.config/digital-twin/snapshots/`(运行时确定) | 同上 |

> v0.1.3 起代码统一通过 `shared::home_dir()` 解析用户目录:
> Unix 读 `HOME`, Windows 读 `USERPROFILE`(次选 `HOMEDRIVE+HOMEPATH`)。
> Windows 上不需要手动设置 `HOME` 环境变量。

提示词搜索顺序(优先级从高到低):
1. 环境变量 `DT_PROMPTS_DIR` 指定目录
2. 当前工作目录 `config/prompts/`
3. 用户级固定路径 `~/.config/digital-twin/prompts/`
4. 可执行文件所在目录的 `config/prompts/`

---

## 3. config.yaml 配置项详解

```yaml
server:
  hostname: my-server          # 本机标识名, 写入节点元数据

services:
  # ── 推理服务提供商(可配多个, 通过 services.embed 路由) ──
  siliconflow:                 # 在线 API(硅基流动)
    url: https://api.siliconflow.cn/v1
    api_key: "REPLACE_WITH_YOUR_KEY"   # 真实 API Key
    model_embed: BAAI/bge-m3               # embedding 模型
    model_reranker: BAAI/bge-reranker-v2-m3 # rerank 模型
    model_llm: Qwen/Qwen3.5-9B             # LLM 模型

  xinference:                  # 本地推理服务(Xinference, 默认 9997 端口)
    url: http://localhost:9997/v1
    api_key: ""                # 本地服务通常无需 key
    model_embed: BAAI/bge-m3
    model_reranker: BAAI/bge-reranker-v2-m3
    model_llm: ""              # 留空 = 该 provider 不提供 LLM

  openai_compatible:           # 任意 OpenAI 兼容网关
    url: https://glmcoding.cn
    api_key: ""
    model_llm: deepseek-v4-flash

  # ── 能力路由: 每种能力指定用哪个 provider ──
  embed:
    embed_provider: siliconflow     # 文本 embedding
    rerank_provider: siliconflow    # 重排
    llm_provider: siliconflow       # LLM 对话

  graph:                       # Memgraph 图数据库
    url: bolt://localhost:7688 # Bolt 协议; Docker 默认 7687
    user: memgraph
    password: ""               # 无密码留空

  qdrant:                      # Qdrant 向量库
    url: http://localhost:6333 # REST; gRPC 为 6334

security:
  tls:                         # 未启用 TLS 时整段可注释
    ca_cert: "/etc/dt/ca.pem"
    server_cert: "/etc/dt/server.pem"
    server_key: "/etc/dt/server.key"
  jwt_secret: "env:DT_JWT_SECRET"   # 支持 env: 前缀从环境变量读取
  allow_plain_passwords: false

snapshot_dir: /var/lib/digital-twin/snapshots   # SQLite 快照目录; Windows 用 D:\dt\snapshots

# 批量构建参数(可选, 默认值如下)
# batch:
#   unwind: 200          # 每批展开的实体数
#   embed: 512           # 每批 embedding 数
#   upsert: 1000         # 每批向量写入数
#   embed_concurrency: 3 # embedding 并发数

# 注册项目(可选; dt build 无参数时按此全量扫描)
# projects:
#   - name: my-project
#     path: /path/to/project
```

## 4. pipeline.yaml 配置项详解

```yaml
enabled: true                 # 总开关: 构建管线是否启用

providers:                    # 与 config.yaml services 对应, 此处可覆盖
  embed_provider: siliconflow     # 构建时实际使用的 provider
  rerank_provider: siliconflow
  llm_provider: openai_compatible

  siliconflow:
    max_concurrent: 8         # 并发上限
    url: https://api.siliconflow.cn/v1
    api_key: "REPLACE_WITH_YOUR_KEY"
    model_embed: BAAI/bge-m3
    model_reranker: BAAI/bge-reranker-v2-m3
    model_llm: deepseek-ai/DeepSeek-V3.2
    max_tokens: 512           # 单次回复上限(默认 512)

  xinference:
    url: http://localhost:9997/v1
    api_key: ''
    model_embed: bge-m3
    model_reranker: bge-reranker-v2-m3
    model_llm: qwen3.5
    max_tokens: 512

  openai_compatible:
    url: https://opencode.ai/zen/go
    api_key: "REPLACE_WITH_YOUR_KEY"
    protocol: openai
    model_llm: deepseek-v4-flash
    max_concurrent: 24
    max_tokens: 512

processors:                   # 构建管线各阶段开关
  tree_sitter: true           # AST 解析
  llm: true                   # LLM 描述生成
  chunk: true                 # 文本分块
  extract_text: true          # 文档文本提取
  ocr: false                  # OCR(未内置引擎, 需外部配合)
  store: true                 # 实体入库
  embed: true                 # 向量化

llm:
  temperature: 0.1
  max_tokens: 4096
```

> ⚠️ LLM 失败重试: 每次调用失败后自动重试(v0.1.2 起); 构建期间不要并行跑多个 dt 命令。
> 全量重建 `dt build --full` 耗时较长(实测 4-5 小时/66 项目), 增量构建为默认。

## 5. event-hooks.yaml 说明

事件 Hook 机制: `dt event --type <名称> --context <JSON>` 触发具名 Hook,
Hook 按配置执行动作(如调用外部脚本/记录日志)。模板含全部事件名与动作示例,
按需复制到用户目录后修改。Hook 仅负责「触发与分发」, 不内置 shell 执行。

## 6. 运行环境与依赖(完整清单)

### 6.1 环境依赖总览

| 组件 | 用途 | 必需性 | 协议/端口 | 版本要求 | 资源建议 |
|---|---|---|---|---|---|
| Memgraph | 知识图谱图数据库(实体/关系) | **必需** | Bolt TCP 7687(模板示例 7688)、Lab 7444 | 任意现代版本(bolt 协议, 客户端 neo4rs 0.8) | 内存 ≥ 4GB(推荐 8GB)、磁盘按图规模(1GB 起) |
| Qdrant | 向量数据库(embedding 检索) | **必需** | REST 6333、gRPC 6334 | 1.x(客户端 qdrant-client 1.x) | 内存 ≥ 2GB、磁盘按向量规模 |
| SQLite | 快照/任务状态 | 内置 | 文件(无网络) | rusqlite 0.32 bundled, 随二进制内置 | 忽略(磁盘 KB 级) |
| Embedding 服务 | 文本向量化(build 必需) | 构建时必需 | HTTPS/HTTP 自定义 | OpenAI 兼容 API | 在线 API 免资源; 本地模型需 GPU/内存 |
| Rerank 服务 | 搜索重排 | 构建/搜索可用 | 同上 | OpenAI 兼容 API | 同上 |
| LLM 服务 | 描述生成/知识提炼 | 构建时必需 | 同上 | OpenAI 兼容 API | 同上 |
| Docker | 后端容器化部署 | 推荐(非源码构建时必须) | - | 20.10+ | 随宿主机 |
| Rust 工具链 | 源码自行构建 | 仅源码构建需要 | - | stable | - |

> 说明: 发布包二进制无需 Rust 工具链; 只有从源码 `cargo build` 才需要安装 Rust。
> SQLite、tree-sitter(各语言解析器)均编译进二进制, 无需外部安装。

### 6.2 Memgraph(知识图谱图数据库)

**部署方式**

| 平台 | 方式 | 命令 |
|---|---|---|
| Linux | Docker | `docker run -d -p 7687:7687 -p 7444:7444 --name memgraph memgraph/memgraph-platform` |
| Linux | 原生二进制 | 官网下载 .deb/.rpm 安装, `systemctl start memgraph` |
| macOS | Docker | 同 Linux Docker 命令 |
| Windows | Docker Desktop | 同 Linux Docker 命令 (WSL2 backend) |
| Windows | WSL2 内 | 在 WSL2 发行版内安装 Linux 版 |

**端口与验证**
- Bolt: 7687(Docker 默认)/ 7688(模板示例), 以 `config.yaml → services.graph.url` 实际配置为准
- Memgraph Lab 控制台: 7444, 浏览器打开 `http://localhost:7444` 可查图
- 验证连通: `dt health` 输出 graph: ok; 或
  ```bash
  # Linux
  docker exec memgraph mgconsole --host localhost --port 7687
  # Windows (Docker Desktop)
  docker exec memgraph mgconsole --host localhost --port 7687
  ```

**配置项**(`config.yaml`)
```yaml
services:
  graph:
    url: bolt://localhost:7687
    user: memgraph
    password: ""      # 默认无密码; 设置了则必须一致
```

### 6.3 Qdrant(向量数据库)

**部署方式**

| 平台 | 方式 | 命令 |
|---|---|---|
| Linux | Docker | `docker run -d -p 6333:6333 -p 6334:6334 --name qdrant qdrant/qdrant` |
| Linux | 原生 | 下载 GitHub Releases 的 `qdrant-x86_64-unknown-linux-gnu.tar.gz`, 解压运行 `./qdrant` |
| macOS | Docker | 同 Linux Docker 命令 |
| Windows | 原生 | GitHub Releases 下载 `qdrant-x86_64-pc-windows-msvc.zip`, 解压运行 `qdrant.exe` |
| Windows | Docker Desktop | 同 Linux Docker 命令 |

**端口与验证**
- REST API: 6333; gRPC: 6334(客户端走 gRPC)
- 模板示例用 6333, 代码默认 6334; 以 `config.yaml → services.qdrant.url` 为准
- 验证: `dt health` 输出 vector: ok; 或浏览器打开 `http://localhost:6333/dashboard`
  ```bash
  curl http://localhost:6333/collections
  ```

**配置项**
```yaml
services:
  qdrant:
    url: http://localhost:6333
```

### 6.4 推理服务(Embedding / Rerank / LLM)

构建管线三个能力都要可用: embedding(文本向量化)、rerank(搜索重排)、LLM(描述/提炼)。
任一 provider 均可同时承载三种能力(如硅基流动)。

**可选形态**

1. **在线 API(推荐, 免运维)**: 硅基流动 SiliconFlow 等 OpenAI 兼容服务
   - 配置 `services.siliconflow.api_key` 填真实 key
   - 模型: `BAAI/bge-m3`(embed)、`BAAI/bge-reranker-v2-m3`(rerank)、任意 LLM
   - 网络: 需能访问外网 API 域名
2. **本地 Xinference(内网/离线)**: 本地部署, 默认 `http://localhost:9997/v1`
   - `api_key` 留空
   - 需先启动 Xinference 并加载模型(embed/rerank/llm 各一)
3. **自建 OpenAI 兼容网关**: 任意兼容服务填 url + key + 模型名

**路由配置**(`config.yaml` + `pipeline.yaml` 双处, 以 pipeline 为构建实际路由)
```yaml
services:
  embed:
    embed_provider: siliconflow      # embedding 用哪个 provider
    rerank_provider: siliconflow     # rerank 用哪个 provider
    llm_provider: siliconflow        # LLM 用哪个 provider
```

**纯搜索/只查询场景**: 不跑 `dt build` 时可只配 graph+qdrant, pipeline.yaml 的
`processors.llm/embed` 设 false, 或仅保留 search/memorize 等命令。

### 6.5 SQLite(快照, 内置)

- 随二进制内置(rusqlite bundled), **无需任何安装**
- 存放增量快照与任务状态: 默认 `snapshot_dir`(config.yaml 可改, 建议磁盘充裕路径)
- 验证: `dt health` 输出 sqlite: ok

### 6.6 Docker 一键部署后端(推荐)

```bash
# 全部后端一条命令拉起(Linux / macOS / Windows Docker Desktop)
docker network create dt-net 2>/dev/null
docker run -d --network dt-net -p 7687:7687 -p 7444:7444 --name memgraph memgraph/memgraph-platform
docker run -d --network dt-net -p 6333:6333 -p 6334:6334 --name qdrant qdrant/qdrant
```

或写 `docker-compose.yml`:
```yaml
services:
  memgraph:
    image: memgraph/memgraph-platform
    ports: ["7687:7687", "7444:7444"]
    restart: unless-stopped
  qdrant:
    image: qdrant/qdrant
    ports: ["6333:6333", "6334:6334"]
    restart: unless-stopped
```
`docker compose up -d` 即可。

### 6.7 环境变量清单

| 变量 | 作用 | 默认 |
|---|---|---|
| `DT_PROMPTS_DIR` | 提示词目录(优先级最高) | 见第 2 章搜索顺序 |
| `DT_LOG_DIR` | 日志输出目录 | 运行时默认 |
| `DT_LOG_LEVEL` | 日志级别(info/debug/warn/error) | info |
| `DT_LOG_STDERR` | 日志写 stderr(供服务化/容器) | 关 |
| `DT_SEARCH_MIN_SCORE` | 搜索最低相关分阈值 | 0 |
| `DT_KG_RERANK_TOP_N` | 知识图谱 rerank 返回条数 | - |
| `DT_NACOS_MAX_CHUNKS` / `DT_NACOS_TARGET_CHUNKS` | Nacos 配置分块参数 | - |
| `DT_JWT_SECRET` | config.yaml `jwt_secret: env:DT_JWT_SECRET` 引用 | - |

### 6.8 资源与系统要求

- **最低配置**: 4GB 内存、20GB 磁盘(后端全 Docker 时)
- **推荐配置**: 8GB 内存、50GB+ 磁盘
- **OS**: Linux / macOS / Windows 10+ (v0.1.3 起原生支持)
- **端口占用清单**: 7687(Memgraph Bolt)、7444(Memgraph Lab)、6333/6334(Qdrant)、9997(本地 Xinference, 可选)、任意(推理 API, 出站)
- 若端口冲突, 改 Docker `-p` 映射后同步改 `config.yaml` 连接地址

### 6.9 部署验证清单

```bash
dt --version        # 1) 二进制可运行
dt health           # 2) 三个后端全部 ok (graph / vector / sqlite)
dt sense            # 3) 环境感知正常输出
dt build --file <某项目文件> --project <项目名>   # 4) 单文件构建成功(验证 embed+LLM 管线)
dt search "关键词"   # 5) 搜索返回结果
```

任何一步失败: 按组件排查——Memgraph/Qdrant 用 `docker ps` 看容器; embed/LLM 看
pipeline.yaml 的 api_key 与 url 可达性(`curl -v <url>`)。

---

## 7. Windows 配置专章

### 7.1 两种运行方式(推荐顺序)

**方式 A: 原生 Windows 二进制(包内 bin/windows/)+ 后端跑 Docker Desktop**

1. 安装 Docker Desktop for Windows(设置里启用 WSL2 backend)
2. 启动 Memgraph + Qdrant:
   ```bat
   docker run -d -p 7687:7687 -p 7444:7444 --name memgraph memgraph/memgraph-platform
   docker run -d -p 6333:6333 -p 6334:6334 --name qdrant qdrant/qdrant
   ```
3. 解压发布包到 `C:\dt\`, 把 `C:\dt\bin\windows\` 加入 PATH:
   ```bat
   setx PATH "%PATH%;C:\dt\bin\windows"
   ```
4. 放置配置(新建目录):
   ```bat
   mkdir %USERPROFILE%\.config\digital-twin\prompts
   copy C:\dt\config\config.yaml.example   %USERPROFILE%\.config\digital-twin\config.yaml
   copy C:\dt\config\pipeline.yaml.example %USERPROFILE%\.config\digital-twin\pipeline.yaml
   copy C:\dt\config\event-hooks.yaml.example %USERPROFILE%\.config\digital-twin\event-hooks.yaml
   copy C:\dt\config\prompts\*.yaml %USERPROFILE%\.config\digital-twin\prompts\
   ```
5. 编辑 `%USERPROFILE%\.config\digital-twin\config.yaml`:
   - `services.graph.url` 改 `bolt://localhost:7687`(若 Memgraph 是 Docker 默认端口)
   - `services.qdrant.url` 改 `http://localhost:6333`
   - 填 `api_key` 与模型路由
6. 验证(新开终端):
   ```bat
   dt --version
   dt health
   ```

**方式 B: WSL2 内运行(后端 + 二进制全在 Linux, 兼容性最好)**

```bash
# 在 WSL2 (Ubuntu) 内
sudo apt install -y docker.io
sudo service docker start
docker run -d -p 7687:7687 -p 7444:7444 --name memgraph memgraph/memgraph-platform
docker run -d -p 6333:6333 -p 6334:6334 --name qdrant qdrant/qdrant
# 把发布包 bin/dt 放到 /usr/local/bin/ (或任意 PATH 目录)
chmod +x /usr/local/bin/dt
mkdir -p ~/.config/digital-twin/prompts
cp <包>/config/*.example ~/.config/digital-twin/ && cp <包>/config/prompts/*.yaml ~/.config/digital-twin/prompts/
dt --version && dt health
```

> 从 Windows 访问 WSL2 服务: localhost 直通, 无需额外配置(WSL2 自动端口转发)。
> 从 WSL2 访问 Windows 上的服务: 用 `$(hostname).local` 或 WSL 网关 IP。

### 7.2 Windows 路径差异速查

| 项 | Linux | Windows |
|---|---|---|
| 用户配置目录 | `~/.config/digital-twin/` | `%USERPROFILE%\.config\digital-twin\` |
| 临时文件 | `/tmp/` | `%TEMP%`(自动识别) |
| 快照目录 | `/var/lib/digital-twin/snapshots` | 建议 `D:\dt\snapshots`(config.yaml 指定) |
| 路径分隔符 | `/` | `\`(YAML 中写字符串路径可用 `/`, 兼容) |
| 项目路径 | `/data/myProject/xxx` | `D:\projects\xxx`(注册 projects 时写实际路径) |

### 7.3 配置 MCP 客户端(Claude Code / OpenCode / Hermes)

Claude Code 的 `.mcp.json`(Windows):

```json
{
  "mcpServers": {
    "digital-twin": {
      "command": "C:\\dt\\bin\\windows\\dt-mcp.exe",
      "args": []
    }
  }
}
```

OpenCode 的 `opencode.json`:

```json
{
  "mcp": {
    "digital-twin": {
      "type": "stdio",
      "command": "C:\\dt\\bin\\windows\\dt-mcp.exe",
      "args": [],
      "enabled": true
    }
  }
}
```

MCP 暴露 10 个工具: `dt_search_kg / dt_search / dt_sense / dt_memorize / dt_event /
dt_learn / dt_build / dt_kg_sync / dt_health / dt_backup`(工具名与 dt CLI 命令对应)。

### 7.4 Windows 已知注意事项

- **防火墙**: 若后端( Memgraph/Qdrant )跑在另一台机器, 确保 7687/6333/6334 端口可达;
  本机 Docker Desktop 默认端口已放行。
- **杀毒软件**: 首次运行 `dt.exe` 可能触发 SmartScreen, 选择「仍要运行」; 包内二进制
  由官方工具链交叉编译, 无签名。
- **路径含空格/中文**: 配置 YAML 中路径用双引号包裹。
- **构建性能**: 大量小文件扫描在 Windows 上略慢于 Linux(文件系统差异), 属正常。
- **`dt backup` 的 Memgraph 容器备份**依赖 `docker` CLI 可用; 无 Docker 时用
  `dt backup --help` 查看 SQLite/Qdrant 备份方式。
- 中文输出: 终端请用 UTF-8 代码页(`chcp 65001`), 避免乱码。

---

## 8. 快速验证

```bash
dt --version        # dt 0.1.0 (Cargo.toml 版本; 发布包版本看包名 v0.1.3)
dt health           # 检查 Memgraph / Qdrant / SQLite 状态
dt sense            # 当前目录环境感知(定位项目+索引状态)
dt build --file path/to/file.py --project my-project   # 单文件增量构建
dt search "关键词" --project my-project
dt memorize --type Concept --entity-id xxx --entity-type Class --project my-project --details "..."   # 写入知识
```

## 9. 常见问题

**Q: dt health 显示 Memgraph 离线?**
A: 确认 Docker 容器在跑: `docker ps | grep memgraph`; 检查 config.yaml 端口
(7687 vs 7688)。

**Q: Windows 上找不到配置文件?**
A: v0.1.3 起自动读 `%USERPROFILE%\.config\digital-twin\`。旧版本只认 `HOME` 变量,
Windows 需 `set HOME=%USERPROFILE%` 或升级。

**Q: build 报 embedding 失败?**
A: 检查 pipeline.yaml 的 `api_key` 与 `model_embed`, 以及 `embed_provider` 路由;
在线 API 需网络可达。

**Q: 为什么 dt --version 显示 0.1.0 而包名是 v0.1.3?**
A: Cargo.toml 版本固定 0.1.0, 发布包用目录名区分版本(v0.1.1→v0.1.3)。

**Q: 构建期间能并行跑其他 dt 命令吗?**
A: 不建议。增量构建有状态锁, 并行可能导致快照不一致; 单文件构建可并行搜索。
