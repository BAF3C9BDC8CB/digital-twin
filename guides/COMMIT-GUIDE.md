# Git 提交规范

## 核心原则

所有提交通道均基于**当前分支**操作。严禁跨分支提交、合并或 cherry-pick。

用户说"提交"的执行链路：**确认分支 → 同步远端 → 检查状态 → 展示 diff → 用户确认 → commit → push**。严禁跳过任何环节。

## 提交流程

### 第一步：确认当前分支

```bash
git branch --show-current
git log --oneline -3
```

- 确认当前在正确的分支上
- 确认最近提交记录，避免重复提交或遗漏

### 第二步：同步远端更新

```bash
git fetch origin <当前分支>
git log --oneline -3 HEAD..origin/<当前分支>
```

检查本地分支是否落后于远端：

- 如果 `git log` 有输出（有远端新提交），必须先合并再提交：
  ```bash
  git pull --rebase origin <当前分支>
  ```
- 如果无输出（无远端更新），继续下一步
- 若有冲突，解决冲突后 `git rebase --continue` 再继续

> **原则**：确保提交基于最新的远端代码，避免推送被拒绝。

### 第三步：检查变更状态

```bash
git status
```

- 确认暂存区只包含**本次会话修改**的文件
- 排除：之前会话的遗留修改、未跟踪文件、临时文件、调试代码
- 如有无关变更，先 `git restore --staged <file>` 或 `git reset` 清理

### 第四步：展示完整 diff 等待确认

```bash
git diff --stat    # 简要统计
git diff           # 完整变更内容
```

diff 中必须**额外标注高危变更**（见下方表格），逐项确认。

| 风险等级 | 范围 | 确认要求 |
|---------|------|---------|
| **生产环境** | 数据库连接、API 地址、域名、HTTPS 配置、日志级别、缓存策略、第三方服务密钥 | 必须逐条确认，不得批量确认 |
| **配置文件** | `.env*`、`config/`、`vue.config.js`、`babel.config.js`、`jest.config.js`、`package.json`、`composer.json`、路由/权限配置 | 逐行审查后确认 |
| **构建/部署** | CI 配置（`.travis.yml`、Jenkinsfile）、Dockerfile、部署脚本 | 必须确认无误 |

> **不确认，不提交；有疑虑，停下来问。**

### 第五步：提交

```bash
git add <文件1> <文件2> ...   # 只添加本次修改的文件
git commit -m "<类型>: <描述>"
```

- commit message 需简洁明确，匹配仓库风格
- **禁止** `git add .`、`git add -A` 等批量添加

### 第六步：推送

```bash
git push
```

用户确认 commit 后自动推送，除非用户明确要求暂不推送。

## 禁止行为

- ❌ 跨分支提交代码
- ❌ 在提交流程中合并其他分支
- ❌ `git add .` / `git add -A` 批量添加
- ❌ `git commit -a` 跳过审查
- ❌ `git commit --amend` 修改已推送的提交
- ❌ `git push --force` 强制推送
- ❌ 连带提交之前会话的遗留修改、临时文件、调试代码
