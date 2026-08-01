# 代码搜索：Qdrant 向量语义搜索

> **铁律：需要找代码时，第一步永远是 MCP Tool `dt_search_expand`，不是 grep/glob/find。**
> **也不允许用 `ls` + `read` 浏览目录结构来替代语义搜索。**
> MCP Tool 失败时降级为 `dt search` CLI，CLI 也失败才能回退到 grep。

---

## 快速开始

```bash
# 按目录路径搜索（推荐：自动匹配路径下所有已配置项目）
dt search "<关键词>" --path "<目录路径>" --limit 10

# 指定单个项目搜索
dt search "<关键词>" --project "<项目名>" --limit 10

# 查询扩展（多查询变体合并，提升召回率，低级模型推荐）
dt search "<关键词>" --project "<项目名>" --expand --limit 10

# 跨所有项目搜索（不确定项目时）
dt search "<关键词>" --all --limit 10

# JSON 输出（便于解析）
dt search "<关键词>" --project "<项目名>" --json --limit 5
```

返回结果包含：`method_id`、`name`、`file_path`、`start_line`、`end_line`、`signature`、`source_code`、`calls`、`language`。

> **`--expand` 参数**：将查询扩展为 3 个语义变体（格式：`"{query}"` / `"{query} 实现 函数 代码"` / `"{query} 定义 逻辑"`），分别搜索后去重合并。低级模型生成的搜索词不够精确时，用此参数可提升 30-50% 召回率。

> **`--path` 参数**：接受一个目录路径，自动从 `config.yaml` 中匹配该路径下的所有项目，跨项目搜索后合并结果。这解决了**目录名 ≠ 项目名**的常见问题（如目录 `/warehouse` 下实际项目是 `warehouse-center`、`warehouse-api` 等）。

---

## 三步流程

**Step 0：确定搜索范围（选择 `--project`、`--path` 还是 `--all`）**

按优先级：
- **已知代码所在目录** → 用 `--path`（最安全，自动匹配）：
  ```bash
  dt search "<关键词>" --path "<工作目录路径>" --limit 10
  ```
- **用户提到明确项目名** → 先验证再用 `--project`：
  ```bash
  dt list --all | grep -i "<关键词>"
  dt search "<关键词>" --project "<确认的项目名>" --limit 10
  ```
- **项目名不确定** → 用 `--path` 或 `--all`：
  ```bash
  dt search-kg "<项目关键词>" --limit 5
  ```
  或 Cypher 兜底：
  ```cypher
  MATCH (p:Project) WHERE p.name CONTAINS $keyword RETURN p.name
  ```

**Step 1：执行语义搜索**
```bash
# 推荐：按路径搜索
dt search "<关键词>" --path "<目录>" --limit 10

# 或按项目搜索
dt search "<关键词>" --project "<项目名>" --limit 10
```

**Step 2：按需读取上下文**
`dt search` 已返回代码片段。需要完整上下文时用 Read 工具读对应文件的行范围。

---

## 常见陷阱

### 陷阱：目录名 ≠ 项目名

**症状**：`dt search --project warehouse` 报错或返回空，但 `dt list --all` 显示项目已索引。

**原因**：目录 `/data/aflmProjects/warehouse` 下的实际项目名是 `warehouse-center`、`warehouse-api` 等，没有叫 `warehouse` 的项目。

**解决**：用 `--path` 替代 `--project`：
```bash
# ❌ 错误：目录名不一定是项目名
dt search "..." --project warehouse

# ✅ 正确：按路径自动匹配所有子项目
dt search "..." --path /data/aflmProjects/warehouse
```

### 陷阱：项目名拼写或记忆偏差

先用 `dt list --all | grep <关键词>` 确认项目名，再搜索。不要凭记忆或目录名猜测。

---

## 回退策略

仅当以下情况才回退到 grep：
- `dt health` 显示 Embed Server 或 Qdrant 不可用
- 项目尚未被 `dt build` 索引过
- `dt search` 返回空结果且用户明确说"你是不是没搜到，用 grep 看看"

> 不要自作主张跳过语义搜索——即使你觉得 grep 更快，也要先用 `dt search`。

---

## 找不到项目名怎么办

如果用户描述模糊，无法确定项目名：

```bash
# 1. 先用 KG 向量搜索定位项目
dt search-kg "订单 支付 中心" --limit 5
# → 从返回的 Project 节点中找到项目名

# 2. 再用代码搜索
dt search "订单状态变更" --project <上一步找到的项目名> --expand
```

> `dt search` 的 `--all` 参数可以跨所有项目搜索，但精度较低，不推荐首选使用。`--path` 比 `--all` 更精准（限定在目录范围内）。
