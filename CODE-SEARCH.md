# 代码搜索：Qdrant 向量语义搜索

> **铁律：需要找代码时，第一步永远是 `dt search`，不是 grep/glob/find。**
> `dt search` 失败或不适用时才能回退到 grep。

---

## 快速开始

```bash
# 指定项目搜索
dt search "<关键词>" --project "<项目名>" --limit 10

# 跨所有项目搜索（不确定项目时）
dt search "<关键词>" --all --limit 10

# JSON 输出（便于解析）
dt search "<关键词>" --project "<项目名>" --json --limit 5
```

返回结果包含：`method_id`、`name`、`file_path`、`start_line`、`end_line`、`signature`、`source_code`、`calls`、`language`。

---

## 三步流程

**Step 1：确定项目名**
按优先级：
- 用户直接提到（如"uvp-oauth-center 的登录逻辑"）
- 工作目录名 / git remote
- 查知识图谱：`MATCH (p:Project) WHERE p.name CONTAINS $keyword RETURN p.name`

**Step 2：执行语义搜索**
```bash
dt search "<关键词>" --project "<项目名>" --limit 10
```

**Step 3：按需读取上下文**
`dt search` 已返回代码片段。需要完整上下文时用 Read 工具读对应文件的行范围。

---

## 回退策略

仅当以下情况才回退到 grep：
- `dt health` 显示 Embed Server 或 Qdrant 不可用
- 项目尚未被 `dt build` 索引过
- `dt search` 返回空结果且用户明确说"你是不是没搜到，用 grep 看看"

> 不要自作主张跳过语义搜索——即使你觉得 grep 更快，也要先用 `dt search`。
