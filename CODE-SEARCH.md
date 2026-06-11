# 代码搜索：Qdrant 向量语义搜索

涉及代码搜索时，**不要直接 grep 或 glob 读文件**，按以下流程执行。

---

**Step 1：确定项目名**
从以下来源获取项目名（按优先级）：
- 用户直接提到（如"uvp-oauth-center 里的登录逻辑"）
- 从环境提取（git remote、工作目录名）
- 查知识图谱确认：`MATCH (p:Project) WHERE p.name CONTAINS $keyword RETURN p.name`

**Step 2：执行语义搜索**
```bash
dt search "<关键词>" --project "<项目名>" --limit 10
```
返回结果包含：`method_id`、`name`、`file_path`、`start_line`、`end_line`、`signature`、`source_code`、`calls` 等。

**Step 3：按需查看完整上下文**
`dt search` 已返回方法签名和代码片段，需要完整上下文时再通过 Read 工具读取对应文件的指定行范围。
