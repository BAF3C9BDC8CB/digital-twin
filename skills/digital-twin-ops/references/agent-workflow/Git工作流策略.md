# Git Workflow Rules

## 1. Git 仓库检测

```bash
git rev-parse --is-inside-work-tree
```

返回 `true` 时，启用以下 Git 规则。

---

## 2. 修改文件后

每次完成修改后，必须本地提交，禁止自动推送：

```bash
git status        # 检查状态
git diff          # 查看修改
git add <files>
git commit -m "<清晰描述修改内容>"
```

---

## 3. 提交粒度

一个完整任务对应一次本地 commit，不要为每个小修改单独提交。

以下情况合并为一次提交：

- 同一功能开发过程
- 同一 Bug 修复过程
- 同一次重构
- 同一需求中的多次调整

---

## 4. Commit Message 格式

```text
<type>: <summary>

详细说明:
- 修改内容
- 修改原因
- 影响范围
```

type 类型：

```text
feat     新功能
fix      Bug修复
refactor 重构
perf     性能优化
docs     文档修改
style    格式调整
test     测试相关
chore    工程配置修改
```

禁止使用无意义描述：`update`、`fix`、`change`、`modify`、`test`、`tmp`。

---

## 5. 禁止推送

默认禁止 `git push`，除非用户明确要求（如"推送"、"提交到远程"、"发布代码"）。

---

## 6. 用户要求提交/推送时

### 6.1 检查提交

```bash
git status
git branch --show-current
git log --oneline --decorate -20
git log @{u}..HEAD --oneline    # 查看未推送提交
```

### 6.2 合并本地提交

存在多个未推送提交时，合并为一个并总结全部内容：

```bash
git reset --soft $(git merge-base HEAD origin/$(git branch --show-current))
git commit -m "<summary-message>"
```

### 6.3 推送

```bash
git status
git log --oneline -5
git push origin <branch>        # 远程无此分支时用 git push -u origin <branch>
```

---

## 7. 安全规则

禁止执行以下操作，除非用户明确授权：

```bash
git reset --hard
git clean -fd
git branch -D
```

任何可能导致代码丢失的操作前，先执行 `git status`、`git diff` 确认。

---

## 8. 操作报告

每次 Git 操作完成后，向用户输出：

- **本地提交**：Commit ID、Message、修改文件、内容摘要、是否推送
- **推送完成**：最终 Commit ID、Message、推送分支、推送结果
