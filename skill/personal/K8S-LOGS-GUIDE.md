# K8s 日志查看与下载指南

> **优先使用 MCP Tool**（`kublog_status` / `kublog_logs` / `kublog_download`），MCP 不可用时降级为 `kublog` CLI。

## 触发方式

用户意图触发规则：

| 用户说 | 行为 |
|--------|------|
| "交互式查看日志" / "选 pod 看日志" / 不指定具体 pod | `kublog`（无参数进入交互模式） |
| "查看 *pod* 的日志" / "实时监听 *pod*" / "tail *pod*" | `kublog logs --ns <NS> --pod <POD>`（实时跟随） |
| "*pod* 最近 *N分钟* 的日志" | `kublog logs --ns <NS> --pod <POD> --since <N>m --no-follow` |
| "下载 *pod* 的日志" / "导出 *pod* 日志" | `kublog download --ns <NS> <POD>` |
| "*pod* 最近 *N小时* 的日志下载" | `kublog download --ns <NS> <POD> --since <N>h -o <文件>` |
| "查看 *pod* 重启前的日志" | `kublog logs --ns <NS> --pod <POD> --previous` |
| "列出 *命名空间* 下的 pod" | `kublog status pods --ns <NS>` |
| "*pod* 是什么状态" | `kublog status pods --ns <NS>` |
| "查看 *命名空间* 的 deployment / service" | `kublog status deploy --ns <NS>` / `kublog status svc --ns <NS>` |

> **原则**：涉及 K8s Pod 日志监听、历史日志查看、日志下载、Pod 状态查询，一律用 `kublog`，不要让用户去 Kuboard 网页操作，也不要手动用 `kubectl`（kublog 已解决 Kuboard 网页的日志断开问题）。

## 工具说明

`kublog` 是基于 Kuboard API 的 K8s 日志与状态 CLI 工具，已安装在系统 PATH（`~/.local/bin/kublog`）。

- 配置文件：`~/.kublog/config.toml`
- token 缓存：`~/.kublog/token.cache`

完整使用文档见：[/data/myProject/kub/docs/USAGE.md](/data/myProject/kub/docs/USAGE.md)

## 可用命令

| 用途 | 命令 |
|------|------|
| **交互模式（日常推荐）** | `kublog`（无参数，方向键选操作/Pod） |
| 登录并缓存 token（首次/token 过期时） | `kublog login` |
| 实时跟随日志（默认 tail 1000 后跟随） | `kublog logs --ns <NS> --pod <POD>` |
| 查看历史日志（不跟随） | `kublog logs --ns <NS> --pod <POD> --no-follow` |
| 下载日志到本地文件 | `kublog download --ns <NS> <POD>` |
| 查看 Pod 列表 | `kublog status pods --ns <NS>` |
| 查看 Deployment | `kublog status deploy --ns <NS>` |
| 查看 Service（含 Endpoints） | `kublog status svc --ns <NS>` |

## 交互模式（推荐日常使用）

无子命令直接运行 `kublog` 进入交互式向导：

```
kublog
```

流程：选操作 → 选命名空间 → 选 Pod →（多容器时选容器）→ 自动执行。

**Pod 选择特性**：

- 列表按 **Deployment 名去重**（去掉 hash 后缀），如 `archive-api-stable-64f56d658f-5f4ls` 与 `archive-api-stable-64f56d658f-hhzbb` 合并为一行 `archive-api-stable  [Running]`
- 方向键导航，回车确认
- 选中后传 Deployment 名给 `logs` 命令，`match_pods` 自动前缀匹配 → **同时聚合显示同 Deployment 的所有 Pod 日志**

**多 Pod 输出格式**：

| 模式 | 行首前缀 | 说明 |
|------|---------|------|
| 单 Pod | 无 | 纯日志内容，无任何前缀 |
| 多 Pod 聚合 | 无 | 纯日志内容，无任何前缀；按 Pod 名着色区分来源（`--no-color` 关闭） |

单 Pod 与多 Pod 显示结果完全一致，行首均不显示 Pod 标识。多 Pod 模式下通过颜色区分不同 Pod 的日志行（默认开启，`--no-color` 关闭）。错误/监控结束信息仍用完整 `ns/pod/container` 以便排查。

## 典型使用场景

### 1. 查找目标 Pod

当用户只给出服务名（如 "payment"、"订单服务"）而未给出完整 Pod 名时，先用 `status pods` 查找：

```bash
kublog status pods --ns newoffen
```

在输出中按服务名模糊匹配 Pod，再执行后续日志命令。

### 2. 实时监听 Pod 日志

最常用场景，类似 `tail -f`：

```bash
kublog logs --ns newoffen --pod pay-offen-payment-stable-667c7dd766-ddftg
```

特点（已通过 30 分钟长期测试）：
- 零被动断开（18s 主动 Close 1000 + 重连，抢在服务端 20s 超时之前）
- 零日志丢失（`last_ts` 续接 + 滑动窗口去重）
- 零 `LogWebSocket` 报错（payment 等应用感知不到异常关闭）

**退出方式**：按 `q` 或 `ESC` 优雅退出（进入 raw mode 监听单键，退出时恢复终端）。`Ctrl+C` 也可触发同样优雅退出。退出时打印 `[已退出实时日志]` 提示。

### 3. 查看最近一段时间的历史日志

```bash
# 最近 30 分钟
kublog logs --ns newoffen --pod pay-offen-payment-stable-667c7dd766-ddftg --since 30m --no-follow

# 最近 2 小时
kublog logs --ns newoffen --pod pay-offen-payment-stable-667c7dd766-ddftg --since 2h --no-follow
```

### 4. 下载日志到本地文件

```bash
# 全量下载（默认 <pod>.log）
kublog download --ns newoffen pay-offen-payment-stable-667c7dd766-ddftg

# 最近 1 小时，指定文件名
kublog download --ns newoffen pay-offen-payment-stable-667c7dd766-ddftg --since 1h -o payment_1h.log
```

下载完成后会显示文件大小与行数。

### 5. 查看容器重启前的日志

容器 crash 后想看上一次运行的日志：

```bash
kublog logs --ns newoffen --pod pay-offen-payment-stable-667c7dd766-ddftg --previous
```

### 6. 诊断 WS 连接问题

加 `-v` 查看详细日志：

```bash
kublog -v logs --ns newoffen --pod pay-offen-payment-stable-667c7dd766-ddftg --tail 5 2>&1 | grep -E "连接 WS|重连|ping|Error"
```

## 参数速查

### logs

| 参数 | 简写 | 说明 |
|------|------|------|
| `--ns` | `-n` | 命名空间（必填） |
| `--pod` | `-p` | Pod 名 |
| `--container` | `-c` | 容器名 |
| `--since` | | 时间起点（`30m` / `2h` / `10:00` / `2024-01-01 10:00`） |
| `--tail` | | 初始 tail 行数（默认 1000） |
| `--previous` | | 上一个容器实例 |
| `--no-follow` | | 不实时跟随 |
| `--no-color` | | 关闭着色 |

### download

| 参数 | 简写 | 说明 |
|------|------|------|
| `--ns` | `-n` | 命名空间（必填） |
| `<POD>` | | Pod 名（位置参数） |
| `--container` | `-c` | 容器名 |
| `--since` | | 时间起点 |
| `--limit-bytes` | | 最大字节数（默认 50MB） |
| `--out` | `-o` | 输出文件（默认 `<pod>.log`） |

### status

| 参数 | 简写 | 说明 |
|------|------|------|
| `<resource>` | | `pods` / `deploy` / `svc`（必填，位置参数） |
| `--ns` | `-n` | 命名空间 |
| `--output` | `-o` | `text` / `json` |

## 默认命名空间与集群

若 `~/.kublog/config.toml` 配置了 `default_namespace`，命令中可省略 `--ns`：

```bash
kublog logs --pod pay-offen-payment-stable-667c7dd766-ddftg
```

当前默认命名空间：`newoffen`（如已配置）。

## 错误排查

| 错误信息 | 原因 | 解决 |
|---------|------|------|
| `配置文件不存在` | 未创建 `~/.kublog/config.toml` | 参照 USAGE.md 创建 |
| `token 过期` | `~/.kublog/token.cache` 失效 | `kublog login` |
| `连接超时` | 网络不通（OpenVPN 未开启） | 提示用户检查 OpenVPN |
| `404 Pod not found` | Pod 名/命名空间错误 | 先 `kublog status pods` 确认 |

## 重要提示

- **实时跟随模式**是默认行为（`logs` 不加 `--no-follow`）。对于长时间监听需求，直接使用此模式，已通过 30 分钟稳定性测试。
- **退出实时日志**：按 `q` 或 `ESC` 优雅退出（raw mode 监听单键，恢复终端后打印退出提示）；`Ctrl+C` 同样优雅退出。不再需要强制杀进程。
- **下载模式**是阻塞式（非跟随），完成后立即返回。
- **历史日志查看**用 `--no-follow`，否则会持续等待新日志。
- **多容器 Pod** 必须用 `--container` 指定，否则默认取第一个容器。
- **交互模式选 Pod** 时，列表按 Deployment 名去重（如 `archive-api-stable` 只显示一行），选中后自动匹配所有同前缀 Pod。单 Pod 与多 Pod 显示结果完全一致，行首均不显示 Pod 标识；多 Pod 模式通过颜色区分不同 Pod（`--no-color` 关闭）。
- 如果用户只给了服务名而非完整 Pod 名，**先 `kublog status pods --ns <NS>` 查找**，再执行日志命令；或直接 `kublog` 进入交互模式用方向键选。
