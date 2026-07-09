# Jenkins 部署指南

> **优先使用 MCP Tool**（`jcli_list` / `jcli_params` / `jcli_history` / `jcli_build` / `jcli_build_log`），MCP 不可用时降级为 `jcli` CLI。

## 触发方式

用户意图触发规则：

| 用户说 | 行为 |
|--------|------|
| "发布 **正式** / 生产" | 使用 `jcli build` 触发生产环境 Jenkins 构建 |
| "发布 **测试** / 开发环境" 或 仅说"发布" | 使用 `jcli build` 触发测试环境 Jenkins 构建 |
| 明确说 "发布 X 服务" | 默认按测试环境处理，除非包含"正式/生产" |

> **原则**：涉及"发布"一律用 `jcli`（远程 Jenkins 部署），不要用其他工具。

## 配置

配置文件位于 `~/.jcli.toml`，已包含服务器地址和认证信息，无需额外传参。

## 可用命令

| 用途 | 命令 |
|------|------|
| 列出所有 Job | `jcli jobs` |
| 查看 Job 参数定义 | `jcli params <JOB>` |
| 查看构建历史（含参数） | `jcli history <JOB> [-n 数量]` |
| 触发构建 | `jcli build <JOB> [-p KEY=VALUE...] [-w] [-s]` |
| 查看构建日志 | `jcli log <JOB> [BUILD_NUMBER]` |

## Job 命名规范

Jenkins Job 按环境和技术栈划分：

| 环境前缀 | 用途 | 示例 |
|---------|------|------|
| `DEV-*` | 开发环境部署 | `DEV-uvp-user-center` |
| `test-*` | 测试环境部署 | `test-uvp-user-center` |
| `JAVA-*` | Java 服务（可对应多个环境） | `JAVA-uvp-user-center` |
| `VUE-*` | Vue 前端项目部署 | `VUE-医联宝-admin` |
| `PHP-*` | PHP 项目部署 | `PHP-医健宝` |
| `gwsk-*` | 网关/nginx 部署 | `gwsk-admin`, `gwsh-nginx` |
| `ANDROID-*` | Android 构建 | `ANDROID-doctor-cloud-android` |

## 版本号规范

版本号格式为 **`yyyymmdd-0.0`**（如 `20260702-0.1`），规则如下：

| 条件 | 版本号 |
|------|--------|
| 今天未发布过 | `当天日期-0.1`（如 `20260702-0.1`） |
| 今天已发布过 | 取当天最后一次版本号 +0.1（如 `20260702-0.1` → `20260702-0.2`） |

> 每个服务独立计算版本号，互不影响。禁止重复使用相同版本号。

## 发布描述

发布描述（`message`）从 git 历史获取，取**最近一次提交的 commit message**：

```bash
git log -1 --pretty=%s
```

例如 commit 为 `钱包逻辑调整 结算接口调整`，则 message 填该内容。

## 发布流程（标准六步）

### 第一步：查找 Job

```bash
jcli jobs | grep <关键词>
```

例如发布 user 服务：
```bash
# 测试环境
jcli jobs | grep test-uvp-user

# 正式环境
jcli jobs | grep JAVA-uvp-user
```

### 第二步：确认参数

```bash
jcli params <JOB>
```

查看该 Job 需要哪些构建参数（如 `version`、`branch`、`Mode` 等）。

### 第三步：确认版本号

```bash
jcli history <JOB> -n 1
```

查看本次发布的版本号：
1. 取当天日期，格式 `yyyymmdd`（如 `20260702`）
2. 检查当天是否已有发布记录：
   - **无** → 版本号为 `当天日期-0.1`（如 `20260702-0.1`）
   - **有** → 取当天最后一个版本号，小数部分 +0.1（如 `20260702-0.1` → `20260702-0.2`）

示例（今天为 2026-07-02）：
```
# 今天未发布过 → 20260702-0.1
# 今天已发布 20260702-0.3 → 20260702-0.4
```

### 第四步：获取发布描述

```bash
cd <项目目录>
git log -1 --pretty=%s
```

取最近一条 commit message 作为 `message` 参数。

### 第五步：触发构建

**发布测试环境：**
```bash
jcli build test-uvp-user-center -p version=<版本号> -p message=<说明> -w -s
```

**发布正式环境（通常带 Mode=deploy）：**
```bash
jcli build JAVA-uvp-user-center -p Mode=deploy -p branch=test -p version=<版本号> -p message=<说明> -w -s
```

> **必须加 `-w -s`**：等待构建完成并返回退出码（0=成功, 1=失败, 2=中止）。

### 第六步：构建失败排查

```bash
jcli log <JOB>          # 最新构建日志
jcli log <JOB> 5        # 指定构建号
```

## build 命令参数说明

| 参数 | 说明 |
|------|------|
| `-p KEY=VALUE` | 构建参数，可多次使用 |
| `-w` | 等待构建完成后输出日志 |
| `-s` | 等待构建完成，返回退出码（0=成功, 1=失败, 2=中止） |

## 注意事项

- 版本号格式必须为 `yyyymmdd-0.0`，日期为当天，小数从 0.1 开始递增
- 发布描述从 git `git log -1 --pretty=%s` 获取，不要手动编造
- 部署前先用 `jcli params` 确认 Job 需要哪些参数
- 构建失败时用 `jcli log` 查看日志定位问题
- 用户只说"发布"默认指测试环境，除非明确说"发布正式/生产"
- 不要使用 Jenkins MCP——统一用 `jcli` 命令行工具
