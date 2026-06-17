# Knowledge Graph Behavior

This project uses a Neo4j knowledge graph for persistent memory.

## ⚠️ 必须先加载 digital-twin 技能

执行任何任务前，先调用 `skill` 工具加载 **digital-twin** 技能，获取完整指令后再按流程执行。仅靠本文件不够——skill 文件中包含最新的详细工作流。

## 唯一不查 KG 的情况

当前环境无任何项目上下文（刚启动、无目录、无打开的文件）且用户消息中也无任何关键词。除此以外都必须先查 KG。

## 代码搜索：语义优先

需要定位代码、找函数、找文件时，**禁止直接 grep / glob / find**，必须先用 `dt search` 语义搜索。详见 digital-twin skill 的 [CODE-SEARCH.md](./CODE-SEARCH.md)。`dt search` 失败时才回退 grep。

## 代码提交流程规范

### 一、核心原则

用户说"提交"，实际执行链路为：**更新代码 → 展示完整 diff → 用户确认 → commit → push**。严禁跳过任何环节。

### 二、提交范围控制

1. **只提交会话内修改** — 每次 commit 前，确认暂存区只包含本次会话修改的文件。
2. **禁止混入无关变更** — 不得连带提交之前会话的遗留修改、未跟踪文件、临时文件或调试代码。

### 三、高危变更确认制度

以下类型的变更，在展示 diff 时必须**额外标注风险等级**，逐项等待用户确认：

| 风险等级 | 范围 | 确认要求 |
|---------|------|---------|
| **生产环境** | 数据库连接、API 地址、域名、HTTPS 配置、日志级别、缓存策略、第三方服务密钥 | 必须逐条确认，不得批量确认 |
| **配置文件** | `.env*`、`config/`、`vue.config.js`、`babel.config.js`、`jest.config.js`、`package.json`、`composer.json`、路由/权限配置 | 逐行审查后确认 |
| **构建/部署** | CI 配置（`.travis.yml`、Jenkinsfile）、Dockerfile、部署脚本 | 必须确认无误 |

> 原则：**不确认，不提交；有疑虑，停下来问。**

### 四、提交前必须展示完整 diff

在 `git commit` 执行前，必须执行 `git diff` 向用户展示本次提交的**全部变更内容**，等待用户明确确认。

### 五、提交后自动推送

用户确认 commit 后，必须执行 `git push`（除非用户明确要求暂不推送）。

---

## Jenkins 操作：使用 jcli 替代 Jenkins MCP

当需要与 Jenkins 交互时，使用本地安装的 `jcli` 命令行工具，**不要使用 Jenkins MCP**。

### 配置

配置文件位于 `~/.jcli.toml`，已包含服务器地址和认证信息，无需额外传参。

### 可用命令

| 用途 | 命令 |
|------|------|
| 列出所有 Job | `jcli jobs` |
| 查看 Job 参数定义 | `jcli params <JOB>` |
| 查看构建历史（含参数） | `jcli history <JOB> [-n 数量]` |
| 触发构建 | `jcli build <JOB> [-p KEY=VALUE...] [-w] [-s]` |
| 查看构建日志 | `jcli log <JOB> [BUILD_NUMBER]` |

### 典型使用场景

**查找 Job：**
```bash
jcli jobs | grep <关键词>
```

**查看 Job 参数和上次发布版本：**
```bash
jcli params <JOB>
jcli history <JOB> -n 3
```

**触发构建并等待结果：**
```bash
jcli build <JOB> -p Mode=deploy -p branch=master -p version=<版本号> -p message=<说明> --status
```

**查看构建日志（排错）：**
```bash
jcli log <JOB>          # 最新构建
jcli log <JOB> 5        # 指定构建号
```

### build 命令参数说明

| 参数 | 说明 |
|------|------|
| `-p KEY=VALUE` | 构建参数，可多次使用 |
| `-w` | 等待构建完成后输出日志 |
| `-s` | 等待构建完成，返回退出码（0=成功, 1=失败, 2=中止） |

### 注意事项

- 优先使用 `jcli history` 查看上次发布的版本号，再递增版本号触发新构建
- 部署前先用 `jcli params` 确认 Job 需要哪些参数
- 构建失败时用 `jcli log` 查看日志定位问题
