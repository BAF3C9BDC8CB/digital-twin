# 工具参考：MCP 优先，CLI 降级

> **首选 MCP Tool**（服务前缀 `digital-twin_`），MCP 不可用时才降级为 `dt` CLI 命令。
> 不是命令手册，是**决策指南**——AI 根据用户意图选择正确的工具。
> MCP 工具返回结构化 JSON，比解析 CLI 文本输出更省上下文、更可靠。

---

## 🔍 搜索：用户想找东西

| 用户说 | MCP Tool（首选） | 说明 |
|--------|-----------------|------|
| "XX 服务的密码/账号/地址/配置" | `dt_search_kg(query="XX", limit=10)` → memgraph `run_cypher_query` 取属性 | 基础设施/凭证类走 KG GraphRAG |
| "XX 服务的端口/URL" | 同上 | 同上 |
| "项目有哪些数据库/中间件" | `dt_search_kg(query="数据库 中间件 基础设施", limit=20)` | 全景查询 |
| "XX 功能的代码在哪里/怎么实现的" | `dt_search(query="XX", world="code", project="<项目名>")` | 代码语义搜索，命中含 llm_analysis + 文件行号 |
| "XX 的逻辑是什么"（模糊） | `dt_search(query="XX")`（默认 world=all） | 代码+知识+文档 RRF 融合 |
| "查文档/手册里的说明" | `dt_search(query="XX", world="doc")` | 文档块检索，含原文 |
| "之前做过类似的事吗" | `dt_search(query="XX", world="memory")` | 事件/历史任务检索（Memgraph 事件标签关键词） |
| "代码仓库/GitLab/ELK/K8s 地址" | `dt_search_kg(query="关键字", limit=5)` | 服务 URL 类走 KG |
| Jenkins Job 信息/构建历史/参数 | `jcli_list` / `jcli_params` / `jcli_history` | Jenkins 类走 jcli 工具 |
| "微服务状态/日志" | `svc_list` / `svc_status` / `svc_logs` | 本地服务走 svc 工具 |
| "K8s Pod 日志/状态" | `kublog_logs` / `kublog_status` | K8s 类走 kublog 工具 |

**CLI 降级**：

```bash
dt search "XX" --world code --project <项目名> --limit 10   # 代码
dt search "XX" --world knowledge --limit 10                # KG（dt search-kg 已移除）
dt search "XX" --limit 10                                  # all 世界
```

---

## ✍️ 写入：用户做了变更操作

| 用户做了 | MCP Tool（首选） | 说明 |
|---------|-----------------|------|
| 修改了代码文件（.java/.py/.ts等） | OpenCode after-edit Hook | 脚本调用 `cargo run ... build --path <根目录> --file <文件>`；失败查看 Hook 日志 |
| 批量同步 / 首次索引 | `dt_build(path="<根目录>", name="<项目名>")` | 手动触发 |
| 删除了文件 | `dt_build(path="<根目录>", name="<项目名>", full=true)` | 全量重建，不再支持单文件删除 |
| 修改了 Nacos/Apollo 配置 | `nacos_sync(env="test")`（测试）或 `nacos_sync(env="prod")`（生产） | 同步到 KG |
| 安装了软件（apt/pip/npm等） | `dt_event(type="SoftwareInstalled", entity_id="<包名>", entity_type="Software", details="version: X", project="<项目>")` | 记录事件 |
| 做了架构/技术决策 | `dt_memorize(type="Decision", entity_id="<标识>", entity_type="ArchitectureDecision", details="decision: X; reason: Y", project="<项目>")` | 记录决策 |
| 部署了生产环境 | `dt_event(type="Deployment", entity_id="<Job名>", entity_type="JenkinsJob", details="branch: X, env: prod", project="<项目>")` | **仅生产**部署 |
| 说"记一下/记住这个/记下来" | `dt_memorize(type="KnowledgeAdded", entity_id="<标识>", details="<内容>", project="<项目>")` | 用户命令 |
| 任务完成沉淀经验 | `dt_learn(task="<任务>", pattern="...", pitfalls="...", decisions="...", project="<项目>")` | 结构化知识沉淀 |

---

## 🔄 同步：保持数据一致

| 场景 | MCP Tool（首选） | CLI 降级 |
|------|-----------------|---------|
| KG 节点增加了新的基础设施/服务 | `dt_kg_sync()` | `dt build --source knowledge --full` |
| KG 节点有少量变更 | `dt_kg_sync()` | `dt build --source knowledge`（默认即增量） |
| Nacos 配置有更新 | `nacos_sync(env="test")` | `dt nacos-sync test`（位置参数） |
| K8s 资源有变化 | （无 MCP 等价物） | `dt k8s-sync` |
| ~~Jenkins Views/Jobs/Builds~~ | （无） | ~~`dt jc-sync`~~ 已移除(2026-08-12) |

---

## 🩺 维护：诊断和验证

| 场景 | 应执行 | 说明 |
|------|--------|------|
| 搜索失败/报错/行为异常 | `dt_health`（MCP）/ `dt health`（CLI） | 检查 Memgraph、Qdrant、SQLite 和 Embed/SiliconFlow；不代表当前 LLM provider 健康 |
| "dt_search 返回空" | 先 `dt_health`，再检查项目是否已索引 | Embed 或 Qdrant 可能挂了 |
| "knowledge 世界报错" | `dt_health`，看 KG Bridge 检查项 | kg_nodes 集合可能不存在，需 `dt_kg_sync` |
| `llm_analysis` 为空 | `tail -n 100 /var/log/digital-twin/dt.log` | 确认 provider、429/401/JSON、Qdrant upsert 和 progress 状态后再增量构建 |
| 验证新项目解析是否正常 | `dt build --path <路径> --name <项目名>` 后用 `dt_health` 确认 | 实测索引（无独立 validate 命令） |
| 数据备份/清空 | `dt_backup`（MCP）/ `dt clean`（CLI） | 运维类工具 |

---

## 🏗️ 项目首次接入

| 步骤 | 执行 | 说明 |
|------|------|------|
| 1 | `dt_build(path="<项目根>", name="<项目名>", full=true)` | 全量重建索引 |
| 2 | `dt_kg_sync()` | 同步 KG 节点到向量库 |
| 3 | `dt_health()` | 确认全部就绪 |

---

## 决策速查：MCP Tool vs CLI

| 操作 | 优先用 | CLI 降级 |
|------|--------|---------|
| 统一搜索（代码/知识/文档/配置/事件） | `dt_search` | `dt search` |
| 搜索 KG（GraphRAG） | `dt_search_kg` | `dt search --world knowledge` |
| 写知识/事件 | `dt_memorize` / `dt_event` | `dt memorize` / `dt event` |
| 任务经验沉淀 | `dt_learn` | `dt learn` |
| 索引代码 | `dt_build` | `dt build` |
| 同步 KG | `dt_kg_sync` | `dt build --source knowledge` |
| 同步 Nacos | `nacos_sync` | `dt nacos-sync [test|prod]` |
| 健康检查 | `dt_health` | `dt health` |
| 查 Jenkins | `jcli_*`（外部二进制） | ~~`dt jcli`~~ 已移除(2026-08-12)，用外部 `jcli` |
| 管微服务 | `svc_*` | （MCP 专属，无 CLI） |
| 查 K8s 日志 | `kublog_*` | `dt kub` |
| 项目注册表/项目发现 | （读 `~/.config/digital-twin/config.yaml`） | `dt build`（无参数=构建所有项目） |

> 有 MCP Tool 的操作一律优先调 Tool，不要手写 bash。Tool 失败时才回退 CLI；
> CLI 也失败（如 `dt` 不在 PATH）才考虑直接操作 Qdrant/Memgraph。
