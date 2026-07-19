# 触发规则（已自动化）

事件写入已由 Hook 系统自动处理，AI 不再需要手动调用 `dt event`：

| 操作 | 自动触发的 Hook | 写入标签 |
|------|----------------|---------|
| 代码修改 | `code_modified`（dt build 插件） | `:Modification` |
| Jenkins 部署 | `jenkins_deploy_completed`（jcli_build） | `:Deployment` + 更新 JenkinsJob/Build/ServiceInstance |
| Nacos 配置变更 | `config_changed` | `:ConfigChange` |
| 架构决策 | `decision_made`（dt memorize） | `:Decision` |
| Bug 修复 | `bug_fix_recorded` | `:BugFix` |
| 会话结束 | `session_ended` | `:Conversation` |
| K8s Pod 异常 | `pod_event_occurred` | `:PodEvent` |
| K8s 同步完成 | `k8s_synced` | `:K8sSyncEvent` |

AI 只需要：
- 执行正常的操作（修改代码、部署、变更配置等），Hook 会自动完成事件记录
- 无需手动调用 `dt event` 或记忆命令
