# dt CLI 命令参考：按场景触发

> **优先使用 MCP Tool**（`digital-twin_*`），MCP 不可用时降级为 CLI 命令。
> 不是命令手册，是**决策指南**——AI 根据用户意图选择正确的工具。

---

## 🔍 搜索：用户想找东西

| 用户说 | 应执行 | 说明 |
|--------|--------|------|
| "XX 服务的密码/账号/地址/配置" | `dt search-kg "XX" --limit 10` → `memgraph_read_cypher` 取属性 | 基础设施/凭证类走 KG 向量搜索 |
| "XX 服务的端口/URL" | 同上 | 同上 |
| "项目有哪些数据库/中间件" | `dt search-kg "数据库 中间件 基础设施" --limit 20` | 全景查询 |
| "XX 功能的代码在哪里/怎么实现的" | `dt search "XX" --project <项目名>` | 代码语义搜索 |
| "XX 的逻辑是什么"（模糊） | `dt search "XX" --project <项目名> --expand` | 扩展搜索，多变体合并 |
| "代码仓库/GitLab/ELK/K8s 地址" | `dt search-kg "关键字" --limit 5` | 服务 URL 类走 KG |
| Jenkins Job 信息/构建历史/参数 | 用 `jcli_list` / `jcli_params` / `jcli_history` | Jenkins 类走 jcli 工具 |
| "微服务状态/日志" | 用 `svc_list` / `svc_status` / `svc_logs` | 本地服务走 svc 工具 |
| "K8s Pod 日志/状态" | 用 `kublog_logs` / `kublog_status` | K8s 类走 kublog 工具 |

---

## ✍️ 写入：用户做了变更操作

| 用户做了 | 应执行 | 说明 |
|---------|--------|------|
| 修改了代码文件（.java/.py/.ts等） | ✅ 插件自动触发，AI 无需操作 | 自动增量索引 |
| 批量同步 / 首次索引 | `dt build --path <根目录> --name <项目名>` | 手动触发 |
| 删除了文件 | `dt remove --project <项目名> --file <路径>` | 清理索引 |
| 修改了 Nacos/Apollo 配置 | `dt nacos-sync --env test`（测试）或 `--env prod`（生产） | 同步到 KG |
| 安装了软件（apt/pip/npm等） | `dt event --type SoftwareInstalled --entity-id <包名> --details "version: X"` | 记录事件 |
| 做了架构/技术决策 | `dt memorize --type Decision --entity-id <标识> --project <项目> --details "decision: X; reason: Y"` | 记录决策 |
| 部署了生产环境 | `dt event --type Deploy --entity-id <Job名> --details "branch: X, env: prod"` | **仅生产**部署 |
| 说"记一下/记住这个/记下来" | `dt memorize --type KnowledgeAdded --entity-id <标识> --details "<内容>" --project <项目>` | 用户命令 |

---

## 🔄 同步：保持数据一致

| 场景 | 应执行 | 说明 |
|------|--------|------|
| KG 节点增加了新的基础设施/服务 | `dt kg-sync` | 全量同步到 Qdrant |
| KG 节点有少量变更 | `dt kg-sync --incremental` | 增量同步，仅新节点 |
| Nacos 配置有更新 | `dt nacos-sync --env test` | 按环境同步 |
| K8s 资源有变化 | `dt k8s-sync` | 同步 K8s 到 KG |

---

## 🩺 维护：诊断和验证

| 场景 | 应执行 | 说明 |
|------|--------|------|
| 搜索失败/报错/行为异常 | `dt health` | 5 项检查：Memgraph、Embed、Qdrant、KG Bridge、全文索引 |
| "dt search 返回空" | 先 `dt health`，再检查项目是否已索引 | Embed 或 Qdrant 可能挂了 |
| "search-kg 报错" | `dt health`，看 [4/5] 是否通过 | kg_nodes 集合可能不存在，需 `dt kg-sync` |
| 验证新项目解析是否正常 | `dt validate --path <路径> --name <项目名>` | 干跑，不写数据库 |

---

## 🏗️ 项目首次接入

| 步骤 | 命令 | 说明 |
|------|------|------|
| 1 | `dt index --path <项目根> --name <项目名>` | 全量重建索引 |
| 2 | `dt kg-sync` | 同步 KG 节点到向量库 |
| 3 | `dt health` | 确认全部就绪 |

---

## 决策速查：选 dt 还是 MCP Tool

| 操作 | 优先用 |
|------|--------|
| 搜索 KG | `dt_search_kg` (MCP Tool) |
| 搜索代码 | `dt_search_expand` (MCP Tool) |
| 写知识/事件 | `dt_memorize` / `dt_event` (MCP Tool) |
| 索引代码 | `dt_build` (MCP Tool) |
| 同步 KG | `dt_kg_sync` (MCP Tool) |
| 健康检查 | `dt_health` (MCP Tool) |
| 查 Jenkins | `jcli_*` (MCP Tool) |
| 管微服务 | `svc_*` (MCP Tool) |
| 查 K8s | `kublog_*` (MCP Tool) |

> 以上操作都有对应的 MCP Tool（`digital-twin_*`），AI 应优先调用 Tool 而非手写 bash。Tool 失败时才回退 shell。
