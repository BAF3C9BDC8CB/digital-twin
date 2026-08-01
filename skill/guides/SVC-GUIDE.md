# 本地服务管理指南

> **优先使用 MCP Tool**（`svc_list` / `svc_start` / `svc_stop` / `svc_restart` / `svc_status` / `svc_logs`），MCP 不可用时降级为 `svc` CLI。

## 触发方式

用户意图触发规则：

| 用户说 | 行为 |
|--------|------|
| "重启 *服务*" | 使用 `svc restart <NAME>` |
| "启动 *服务*" | 使用 `svc start <NAME>` |
| "停止 *服务*" | 使用 `svc stop <NAME>` |
| "*服务* 状态" 或 "查看 *服务*" | 使用 `svc status <NAME>` 或 `svc list` |
| "*服务* 日志" | 使用 `svc logs <NAME>` |

> **原则**：涉及本地服务的启停/状态/日志一律用 `svc`，不要用 systemctl 或手动操作。

## 降级策略

如果 `svc list` 中不包含目标服务，降级使用以下方案之一：

1. `systemctl <start|stop|restart|status> <服务名>`
2. `service <服务名> <start|stop|restart|status>`
3. 直接操作进程（`kill`、`nohup` 等）

## 配置

`svc` 自动扫描指定目录下的微服务项目，无需额外配置。

## 可用命令

| 用途 | 命令 |
|------|------|
| 列出所有服务及运行状态 | `svc list` |
| 查看服务详细状态 | `svc status [NAME]` |
| 启动服务（编译+启动） | `svc start <NAME> [PROFILE]` |
| 停止服务 | `svc stop <NAME>` |
| 重启服务 | `svc restart <NAME> [PROFILE]` |
| 查看日志 | `svc logs [-f] <NAME>` |

## 典型使用场景

### 列出所有本地微服务

```bash
svc list
```

输出会显示运行中（绿色）和已停止（红色）的服务。

### 查看单个服务状态

```bash
svc status user
```

### 启动服务

```bash
svc start user          # 默认 profile
svc start user dev      # 指定 dev profile
```

> 修改代码后启动服务会自动编译。

### 停止服务

```bash
svc stop user
```

### 重启服务

```bash
svc restart user        # 默认 profile
svc restart user prod   # 指定 prod profile
```

### 查看服务日志

```bash
svc logs user           # 查看日志
svc logs -f user        # 实时跟踪日志
```

## 注意事项

- `svc list` 中的服务名是简写名（如 `user`、`order`、`gateway`），不是完整项目名
- 修改代码后启动服务会自动编译
- 不同 profile（`dev` / `prod`）加载不同配置文件
- 如果 `svc list` 中不包含目标服务，降级使用 `systemctl` 或 `service` 命令
- 对应的 Jenkins Job 名通常为 `DEV-uvp-<name>-center` 或 `JAVA-uvp-<name>-center` 格式
