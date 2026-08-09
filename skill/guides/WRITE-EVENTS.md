# 写入事件/知识/记忆到知识图谱

> **优先使用 MCP Tool**（`dt_memorize` / `dt_event` / `dt_build` 等），MCP 不可用时降级为 CLI。

## 触发写入

| 触发操作 | 条件 | MCP Tool（首选） |
|---------|------|-----------------|
| 用户说"记忆/记一下/记住这个/记下来/记住" | 总是 | `dt_memorize(type="KnowledgeAdded", entity_id="<标识>", entity_type="<实体类型>", details="<内容>", project="<项目>")` |
| 安装软件（apt/pip/npm/brew 等） | 总是 | `dt_event(type="SoftwareInstalled", entity_id="<包名>", entity_type="Software", details="version: <版本>, method: <安装方式>", project="<项目>")` |
| 修改 Nacos/Apollo/Consul 等外部配置 | 总是 | `dt_event(type="ConfigChange", entity_id="<data_id>", entity_type="NacosConfig", details="<改动摘要>", project="<项目>")` |
| 同步 Nacos 配置 | AI 判断必要时 | `nacos_sync(env="test")` 或 `nacos_sync(env="prod")` |
| 做出架构/技术决策 | 总是 | `dt_memorize(type="Decision", entity_id="<决策标识>", entity_type="ArchitectureDecision", details="decision: <决策>; reason: <原因>", project="<项目>")` |
| Jenkins 部署 | **所有环境** | `dt_event(type="Deployment", entity_id="<job_name>", entity_type="JenkinsJob", details="job: <job_name>; env: <环境>; build_number: <构建号>; branch: <分支>; version: <版本>", project="<项目>")` |

**CLI 降级**（MCP 不可用时，参数一一对应）：

```bash
dt memorize --type KnowledgeAdded --entity-id "<标识>" --entity-type "<实体类型>" --details "<内容>" --project "<项目>"
dt event --type SoftwareInstalled --entity-id "<包名>" --entity-type Software --details "version: <版本>" --project "<项目>"
dt nacos-sync test
```

## 代码实体同步

OpenCode after-edit Hook 脚本为：

```text
scripts/opencode-after-edit.sh
```

它调用真正的单文件增量构建：

```bash
cargo run --quiet --manifest-path <项目根>/Cargo.toml -- \
  build --path <项目根> --file <相对或绝对文件路径>
```

Hook 已在 `/home/luis/opencode.json` 配置；当前已验证脚本级触发，真实 OpenCode 会话需在 OpenCode CLI 可用时验证。

| 触发操作 | 条件 | 执行方式 |
|---------|------|---------|
| 源码修改 | OpenCode Hook | 自动调用脚本中的 `cargo run ... build --path <项目根> --file <文件>` |
| Hook 失败或批量修改 | 手动/定时兜底 | `dt build --path <项目根>` |
| 删除文件 | 单文件删除需清理旧数据 | 优先项目级增量；无法确认快照时使用 `dt build --path <项目根> --full` |
| 批量同步 / 项目首次索引 | 项目维度 | MCP `dt_build`；CLI `dt build --path <路径> --name <项目名>` |

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
2. 执行：`dt_event(type="Conversation", entity_id="<会话日期>", entity_type="Session", project="<项目>", details="<关键发现摘要>")`
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
