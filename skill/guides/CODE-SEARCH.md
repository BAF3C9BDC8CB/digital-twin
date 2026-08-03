# 代码搜索：语义搜索（MCP 优先）

> **铁律：需要找代码时，第一步永远是 MCP Tool `dt_search`，不是 grep/glob/find。**
> **也不允许用 `ls` + `read` 浏览目录结构来替代语义搜索。**
> MCP 不可用时降级为 `dt search` CLI；两者都失败才能回退到 grep。

---

## 快速开始（MCP Tool）

```
dt_search(query="<关键词>", world="code", limit=10)                    # 搜代码方法
dt_search(query="<关键词>")                                            # 默认 world=all，代码+知识+文档一起搜
dt_search(query="<关键词>", world="code", project="<项目名>", limit=10)  # 限定项目
```

返回 JSON，每条命中包含：

| 字段 | 说明 |
|------|------|
| `title` / `name` | 方法名 |
| `llm_analysis` | **LLM 分析**（用途 + 逻辑，直接可判断是否目标代码） |
| `file_path` / `start_line` / `end_line` | **精确位置**（文件 + 行号范围） |
| `signature` | 方法签名 |
| `score` | 相关性得分 |

> **world 取值**：`all`（默认，code+knowledge+doc 经 RRF 融合）/ `code`（代码方法）/ `knowledge`（知识图谱实体）/ `doc`（文档块）/ `config`（配置）/ `memory`（事件）。
> 找代码用 `code` 或 `all`；找基础设施/凭证/服务信息用 `knowledge`（或专用 `dt_search_kg`）。

---

## 三步流程

**Step 0：确定搜索范围**

按优先级：
- **用户提到明确项目名** → 先验证再用 `project` 参数：
  ```bash
  rg -i "<关键词>" ~/.config/digital-twin/config.yaml    # 项目注册表 = config.yaml projects 段
  ```
  然后 `dt_search(query="...", world="code", project="<确认的项目名>")`
- **项目名不确定** → 不传 `project`（跨全部已索引项目），或用 `dt_search_kg(query="<项目关键词>")` 先从 KG 定位项目节点
- **不确定代码还是知识** → 直接默认 `world=all`

**Step 1：执行语义搜索**

```
dt_search(query="<关键词>", world="code", project="<项目名>", limit=10)
```

**Step 2：按需读取上下文**

`dt_search` 已返回 `llm_analysis`、`file_path`、行号和 `signature`。需要完整实现时用 Read 工具读对应文件的行范围。

---

## 常见陷阱

### 陷阱：目录名 ≠ 项目名

**症状**：`project="warehouse"` 返回空，但 config.yaml 显示该目录下项目已注册。

**原因**：目录 `/data/aflmProjects/warehouse` 下的实际项目名是 `warehouse-center`、`warehouse-api` 等，没有叫 `warehouse` 的项目。

**解决**：先查注册表确认真实项目名，不要凭目录名猜测：
```bash
rg -i warehouse ~/.config/digital-twin/config.yaml
```

### 陷阱：项目名拼写或记忆偏差

先用 `rg -i <关键词> ~/.config/digital-twin/config.yaml` 确认项目名，再搜索。不要凭记忆猜测。

---

## 回退策略

仅当以下情况才回退到 grep：
- `dt_health` 显示 Embed Server 或 Qdrant 不可用
- 项目尚未被索引过（`dt_build` / `dt build` 从未执行）
- `dt_search` 返回空结果且用户明确说"你是不是没搜到，用 grep 看看"

> 不要自作主张跳过语义搜索——即使你觉得 grep 更快，也要先用 `dt_search`。

---

## CLI 降级（MCP 不可用时）

```bash
# 搜代码
dt search "<关键词>" --world code --project "<项目名>" --limit 10

# 跨世界搜索（代码+知识+文档）
dt search "<关键词>" --limit 10

# JSON 输出（便于解析）
dt search "<关键词>" --world code --json --limit 5
```

> 注意：`dt search` CLI 参数为 `query / --world / --limit / --project / --json`。
> 旧的 `--path` / `--expand` / `--all` 参数与 `dt search-kg` 子命令**已移除**；
> KG 搜索请用 MCP `dt_search_kg` 或 CLI `dt search "<关键词>" --world knowledge`。

---

## 找不到项目名怎么办

如果用户描述模糊，无法确定项目名：

```
# 1. 先用 KG 搜索定位项目节点
dt_search_kg(query="订单 支付 中心", limit=5)
# → 从返回的 Project 节点中找到项目名

# 2. 再用代码搜索
dt_search(query="订单状态变更", world="code", project="<上一步找到的项目名>")
```

> 不传 `project` 的 `dt_search` 会跨所有项目搜索，适合探索；确定项目后传 `project` 精度更高。
