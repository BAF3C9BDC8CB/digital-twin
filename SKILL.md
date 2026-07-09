---
name: digital-twin
description: 知识图谱查询 + Qdrant 语义代码搜索 + 事件写入规则
---

# digital-twin 技能

> **优先使用 MCP Tool**（`digital-twin_*`），MCP 不可用时降级为 CLI 命令。MCP Tool 列表见 [DT-CLI-REFERENCE.md](./guides/DT-CLI-REFERENCE.md)。

凡是需要**定位代码、找函数、找类、找文件、理解项目逻辑**，一律走语义搜索流程。
**禁止直接用 grep / glob / find 扫代码。** 遇到代码搜索需求时，先读 [CODE-SEARCH.md](./guides/CODE-SEARCH.md)。

---

## ⚠️ 打开新工作空间：项目发现

进入新目录时，**必须执行项目发现流程**：检查当前目录及其子目录是否有源码项目尚未加入向量库。详细规则见 [PROJECT-DISCOVERY.md](./guides/PROJECT-DISCOVERY.md)。

核心要点：
- 项目配置在 `config.yaml` 的 `projects` 段，按 `- base:` 分组列表
- 完整路径 = base + 项目名（或指定的相对路径 `name: rel_path`）
- 先用 `dt list --all` 快速查看已有项目状态（磁盘、向量数、方法数）
- 递归扫描子目录，**有源码文件即为项目候选**（不依赖 pom.xml / package.json）
- 对比 `config.yaml` 去重，检查 `ignored_dirs.yaml` 跳过
- 发现候选时**必须**用 `question` 工具提示用户，不得静默操作

---

遇到以下场景，按对应文档执行：

| 场景 | 文档 |
|------|------|
| 🎯 **Leader 工作流（高效并行执行）** | [EXECUTION-LEADER.md](./guides/EXECUTION-LEADER.md) |
| 📖 **dt CLI 全部命令参考** | [DT-CLI-REFERENCE.md](./guides/DT-CLI-REFERENCE.md) |
| 🔍 **搜索代码逻辑、方法定位、文件查找** | [CODE-SEARCH.md](./guides/CODE-SEARCH.md) |
| 🧠 **查知识图谱（任何任务的第一个动作）** | [KG-QUERY.md](./guides/KG-QUERY.md) |
| 🔄 **KG 节点同步到向量库** | MCP: `dt_kg_sync` / CLI: `dt kg-sync --incremental` |
| AI 操作后必须触发的写入（代码修改/部署/配置变更等） | [TRIGGER-RULES.md](./guides/TRIGGER-RULES.md) |
| 写入事件/知识/记忆，或结束会话 | [WRITE-EVENTS.md](./guides/WRITE-EVENTS.md) |
| 长任务全流程：Brainstorming → 计划 → 子 agent → 审查 → 验收 | [LONG-TASK-WORKFLOW.md](./guides/LONG-TASK-WORKFLOW.md) |
| 🚀 **发布服务（正式/测试环境）** | [JCLI-GUIDE.md](./guides/JCLI-GUIDE.md) |
| ⚙️ **管理本地服务（启停/状态/日志）** | [SVC-GUIDE.md](./guides/SVC-GUIDE.md) |
| 📦 **监听/下载 K8s Pod 日志，查看 Pod/Deployment/Service 状态** | [K8S-LOGS-GUIDE.md](./guides/K8S-LOGS-GUIDE.md) |
| 📝 **Git 提交规范** | [COMMIT-GUIDE.md](./guides/COMMIT-GUIDE.md) |
| 🆕 **打开新工作空间 → 发现未索引项目** | [PROJECT-DISCOVERY.md](./guides/PROJECT-DISCOVERY.md) |

---

## 禁止

- ❌ 不要问用户"要不要查知识图谱"——静默查询
- ❌ 不要每次对话全量扫代码
- ❌ 不要询问用户已知存在于知识图谱中的信息
- ❌ **禁止直接用 grep / glob / find 搜索代码——先用 MCP Tool `dt_search_expand` 语义搜索**
- ❌ **禁止用 `ls` + `read` 浏览目录来替代语义搜索——查找代码逻辑第一步永远是 `dt_search_expand`**
- ❌ **禁止让用户去 Kuboard 网页查看 K8s Pod 日志——一律用 MCP Tool `kublog_*`（已解决网页日志断开问题）**
- ❌ **禁止用 `kubectl logs` 替代 `kublog`**——kublog 已封装 Kuboard 鉴权与 WS 稳定性
