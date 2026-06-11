# 写入事件/知识/记忆到知识图谱

## 触发写入

| 触发操作 | 条件 | 命令 |
|---------|------|------|
| 用户说"记忆/记一下/记住这个/记下来/记住" | 总是 | `dt memorize --type KnowledgeAdded --entity-id "<标识>" --entity-type "<实体类型>" --details "<内容>" --project "<项目>"` |
| 安装软件（apt/pip/npm/brew 等） | 总是 | `dt event --type SoftwareInstalled --entity-id "<包名>" --entity-type Software --details "version: <版本>, method: <安装方式>" --project "<项目>"` |
| 修改 Nacos/Apollo/Consul 等外部配置 | 总是 | `dt event --type ConfigChange --entity-id "<data_id>" --entity-type NacosConfig --details "<改动摘要>" --project "<项目>"` |
| 同步 Nacos 配置 | AI 判断必要时 | `dt nacos-sync --env test` 或 `dt nacos-sync --env prod` |
| 做出架构/技术决策 | 总是 | `dt memorize --type Decision --entity-id "<决策标识>" --entity-type ArchitectureDecision --details "decision: <决策>; reason: <原因>" --project "<项目>"` |
| Jenkins 部署 | 仅生产/stable 环境 | `dt event --type Deploy --entity-id "<job_name>" --entity-type JenkinsJob --details "branch: <分支>, env: <环境>" --project "<项目>"` |

## 不写 Event/Knowledge 但同步代码实体

`dt update` / `dt build` 同步 Method/Class/CALLS 到 Neo4j + Qdrant。

| 触发操作 | 条件 | 命令 |
|---------|------|------|
| 源码修改（创建/编辑 .py/.java/.ts 等） | 文件维度 | `dt update --path <路径> --name <项目名> --file <相对路径>` |
| 批量同步 / 项目首次索引 | 项目维度 | `dt build --path <路径> --name <项目名>` |
| 删除文件 | 文件已删除 | `dt remove --project <项目名> --file <原相对路径>` |

## 完全不操作

| 操作 | 原因 |
|------|------|
| Bug 修复 | 信息已 inline 在代码中 |
| 开发/测试环境的临时部署构建 | 非生产发布，无回溯价值 |
| 一般的 API 请求（查询类 GET） | 读操作不产生变更 |
| 一次性对话、临时调试、常规编辑 | 无长期价值 |

## 执行规则

- 写入后回复：`📝 已将 [XXX] 记录到知识图谱`
- 写入时优先关联已有实体，禁止创建孤立节点

## Session-end Protocol

用户说 "done" / "结束" 时：
1. 列出关键发现
2. 执行：`dt event --type Conversation --entity-id "<会话日期>" --entity-type Session --project "<项目>" --details "<关键发现摘要>"`
3. 回复：`📝 已将 [本次会话摘要] 记录到知识图谱`

## Event 架构

```
(:Event)-[:INDEXED_METHOD]->(:Method)       # 方法被索引
(:Event)-[:INDEXED_PROJECT]->(:Project)     # 项目被索引
(:Event)-[:INSTALLED_SOFTWARE]->(:Software)  # 软件被安装
(:Event)-[:INDEXED_DOC]->(:Document)        # 文档被索引
(:Event)-[:SYNCED_NAMESPACE]->(:NacosNamespace) # Nacos 同步
(:Event)-[:SCANNED_SERVER]->(:Server)       # 服务器扫描
(:Event)-[:DEPLOYED_JOB]->(:JenkinsJob)     # Jenkins 部署
```

Event 通过 `event_id` 唯一约束去重（SHA256 哈希）。
