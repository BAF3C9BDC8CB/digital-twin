---
name: digital-twin
description: Use for KG search, project indexing, single-file updates, and change-triggered verification.
---

# Digital Twin 项目技能

本技能用于：知识图谱查询、代码语义搜索、项目索引、单文件增量更新、OpenCode 修改后同步，以及构建结果验收。

## 0. 总原则

1. **先判断意图，再选择动作**：查询只读；代码/配置变更触发单文件索引；首次接入或批量变更才运行项目级构建。
2. **MCP 优先，CLI 降级**：MCP 不可用时使用 `dt` CLI；不要直接写 Memgraph/Qdrant。
3. **配置以实际加载路径为准**：
   - 项目注册：`~/.config/digital-twin/config.yaml`
   - Pipeline/provider：`~/.config/digital-twin/pipeline.yaml`
   - 当前仓库配置通常是上述 pipeline 文件的链接或来源文件，修改前先确认 `readlink -f`。
4. **永远验证副作用**：命令返回成功不等于数据正确；构建后检查日志、Qdrant 集合和搜索结果。
5. **不要输出或提交 API Key**。只报告 provider、model、URL（必要时脱敏）、状态和错误码。

## 1. 会话开始/进入新目录

优先执行：

```bash
dt sense --json
```

确认项目名、根路径和索引状态。项目名必须来自 `config.yaml`，不要凭目录名猜测。

需要搜索代码时，先使用：

```bash
dt search "<关键词>" --world code --project "<项目名>" --limit 10 --json
```

MCP 可用时优先调用 `dt_search` / `dt_search_kg`。

## 2. 变更触发规则

| 场景 | 动作 |
|---|---|
| 只查询代码/KG/文档 | 搜索，不构建 |
| 修改单个源码/文档/配置文件 | `dt build --path <项目根> --file <文件>` |
| 批量修改多个文件 | `dt build --path <项目根>` |
| 首次接入项目 | `dt build --path <项目根> --full` |
| 删除文件 | 使用项目级增量构建清理删除项；若无法确认快照，使用 `--full` |
| 修改知识实体/人工经验 | 使用 `dt memorize` / `dt learn`，再按需 `dt kg-sync` |

### OpenCode after-edit Hook

当前 Hook 脚本：

```text
scripts/opencode-after-edit.sh
```

用户级配置：

```text
/home/luis/opencode.json
```

Hook 接收文件路径，使用 `flock` 防止同一文件并发，并调用：

```bash
cargo run --quiet --manifest-path /data/myProject/digital-twin-v2/Cargo.toml -- \
  build --path /data/myProject/digital-twin-v2 --file <edited-file>
```

Hook 当前脚本直接调用仓库源码的 `cargo run`，不是依赖 PATH 中的 `dt` 二进制。

日志：

```text
/var/log/digital-twin/opencode-build.log
```

验证 Hook：

```bash
scripts/opencode-after-edit.sh /data/myProject/digital-twin-v2/src/main.rs
sed -n '1,120p' /var/log/digital-twin/opencode-build.log
```

注意：脚本级触发已验证；真实 OpenCode 会话是否执行 Hook，必须在 OpenCode CLI 可用时单独验证。

`dt health` 的 SiliconFlow 项是嵌入/后端健康检查，不等同于当前 LLM provider。当前 LLM provider 以
`~/.config/digital-twin/pipeline.yaml` 的 `providers.llm_provider` 为准。

## 3. 构建命令

### 单文件增量

```bash
dt build --path <项目根> --file <相对或绝对文件路径>
```

`--file` 会限制代码索引扫描到目标文件，并执行该文件的图谱、向量、LLM 和进度更新。若发现流水线日志仍处理其他文件，应停止并报告，不要假设单文件语义生效。

### 项目级增量

```bash
dt build --path <项目根>
```

### 全量重建

```bash
dt build --path <项目根> --full
```

### 仅基础索引/排查后端

```bash
dt build --path <项目根> --no-pipeline
dt health
```

## 4. LLM/provider 规则

provider 来自 `pipeline.yaml` 的 `providers.llm_provider`：

- `glmcoding` → GLM Coding
- `siliconflow` → SiliconFlow
- `xinference` → XInference

当前推荐低并发：

```yaml
glmcoding:
  max_concurrent: 4
llm:
  chunk_concurrency: 1
```

LLM 结果只有在以下步骤全部成功后才标记完成：

```text
LLM → embedding → Qdrant upsert → SQLite progress
```

任一步失败，下次增量构建应补偿。遇到 `401` 检查配置实际加载路径和 provider；遇到 `429` 降低并发并检查 Retry-After；遇到 JSON 解析错误检查模型响应和 prompt 日志摘要。

## 5. KG/搜索验收

基础健康检查：

```bash
dt health
curl -sS http://localhost:6333/collections
```

应按需存在：`code_methods`、`doc_chunks`、`kg_nodes`。

只读查看集合：

```bash
curl -sS http://localhost:6333/collections
```

搜索：

```bash
dt search "<代码关键词>" --world code --limit 5 --json
dt search "<知识关键词>" --world knowledge --limit 5 --json
dt search "<文档关键词>" --world doc --limit 5 --json
```

验收至少检查：

- `hits` 非空且 `total` 合理；
- code 命中有 `file_path/start_line/end_line/signature`；
- 方法命中应检查 `llm_analysis`，为空则查看 Phase 2 日志并允许后续增量补偿；
- knowledge 命中有实体 ID、`score_breakdown`、关系/`hop`（若图扩展启用）；
- doc 命中有 `source_ref` 或文档路径；
- 不出现跨项目污染；
- `cargo test --test unified_search -- --ignored --nocapture` 的失败必须区分数据缺失、上游服务失败和断言格式问题。

只读查看 Phase 2 日志：

```bash
grep -E 'phase2|LLM 方法分析|upsert_success|progress_success|429|401|JSON' \
  /var/log/digital-twin/dt-daemon.log | tail -100
```

`dt sense` 的 `methods` 是 sense 汇总口径，不等于源码目录的 AST 方法总数；精确搜索结果以 `dt search --world code` 为准。

## 6. 失败报告格式

```text
动作：<命令>
项目：<project>
目标文件：<file 或 none>
provider/model：<脱敏后的运行配置>
结果：成功/失败/部分成功
后端：Memgraph/Qdrant/SQLite/LLM
关键日志：HTTP 状态、重试次数、阶段，不含密钥和完整提示词
索引：集合名与可验证状态
搜索验收：通过数/失败数及失败原因
下一步：明确的补偿命令或修复项
```

## 7. 禁止事项

- 不要把 API Key 写入 Git。
- 不要把项目级全量构建当作单文件更新的替代品。
- 不要在 LLM 失败时伪造 `llm_analysis` 或手工标记完成。
- 不要只依据 SQLite checkpoint 判断数据已存在；必要时用搜索/Qdrant 回读验证。
- 不要把 `401`、`429`、JSON 解析失败和 Qdrant 持久化失败混为同一类问题。

详细命令见：

- `guides/DT-CLI-REFERENCE.md`
- `guides/KG-QUERY.md`
- `guides/TRIGGER-RULES.md`
- `guides/WRITE-EVENTS.md`
- `guides/PROJECT-DISCOVERY.md`
- `guides/LONG-TASK-WORKFLOW.md`
