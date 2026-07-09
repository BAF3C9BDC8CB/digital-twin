# 触发规则：AI 操作后必须执行

> **优先使用 MCP Tool**，MCP 不可用时降级为 CLI 命令。

以下所有操作完成后，AI **必须无条件**执行对应的写入命令，不可跳过。

---

## 一、用户命令类

| 用户说的话 | 必须执行 |
|-----------|---------|
| "记忆" / "记一下" / "记住这个" / "记下来" / "记住" | `dt memorize --type KnowledgeAdded --entity-id "<唯一标识>" --entity-type "<实体类型>" --details "<内容>" --project "<项目>"` |

## 二、代码修改类

> **dt build 已由 OpenCode 插件自动触发**（`tool.execute.after` 钩子），**AI 无需手动执行**。仅在以下情况手动操作：

| 操作 | 必须执行 |
|------|---------|
| 修改文件（任意数量） | ✅ 插件自动触发 `dt build`，AI 无需操作 |
| 删除文件 | `dt remove --file <被删文件的绝对路径>` |
| 批量同步 / 首次索引 | `dt build --path <项目路径> --name <项目名>`（手动） |

> **自动解析**：`dt build/remove --file` 会根据 `config.yaml` 的 `projects` 段自动解析项目名和路径。

## 三、环境与配置变更类

| 操作 | 必须执行 |
|------|---------|
| 修改 Nacos 配置（增/删/改 data_id） | `dt event --type ConfigChange --entity-id "<data_id>" --entity-type NacosConfig --details "<改动摘要>" --project "<项目>"` |
| 同步 Nacos 配置到 KG | `dt nacos-sync --env test` 或 `dt nacos-sync --env prod` |

## 四、安装与部署类

| 操作 | 必须执行 |
|------|---------|
| 安装软件（apt/pip/npm/brew 等） | `dt event --type SoftwareInstalled --entity-id "<包名>" --entity-type Software --details "version: <版本>, method: <安装方式>" --project "<项目>"` |
| 生产/stable 环境 Jenkins 部署 | `dt event --type Deploy --entity-id "<job_name>" --entity-type JenkinsJob --details "branch: <分支>, env: <环境>, params: <参数>" --project "<项目>"` |

## 五、架构与决策类

| 操作 | 必须执行 |
|------|---------|
| 做出架构选型、技术方案、迁移决策 | `dt memorize --type Decision --entity-id "<决策标识>" --entity-type ArchitectureDecision --details "decision: <决策>; reason: <原因>; scope: <影响范围>" --project "<项目>"` |

## 六、会话结束

| 用户说的话 | 必须执行 |
|-----------|---------|
| "done" / "结束" | 见 [WRITE-EVENTS.md](./WRITE-EVENTS.md) 的 Session-end Protocol |

## 七、关于文件同步机制

- `dt build`：由 OpenCode 插件自动触发（`edit`/`write` 工具后 3 秒防抖），无需 AI 手动执行
- 插件会同时更新 **Neo4j（代码实体） + Qdrant（向量）**，始终保持一致
