# KG 检索质量修复与团队测试闭环（2026-08-12）

背景：im-center 团队测试（团队 A 功能 + 团队 B KG 审计）发现 dt_search_kg 对代码项目 0% 命中。
根因（2026-08-12 已修复）：`mcp/mcp-server.py:586` 曾硬编码 `--world knowledge`，代码实体在 code 世界——现已支持 world/project 参数透传。
修复：dt_search_kg 增加 `world`（默认 knowledge）+ `project` 参数。

## 团队 B 建议落地（5 项）

1. **低分降级提示** — `src/interfaces/cli/search_render.rs`
   `const LOW_SCORE_THRESHOLD: f64 = 0.5`；全部命中最高分 <0.5 时输出
   `⚠️ 结果可能不相关`（附 world 错配/跨项目噪音排查指引：--world code + --project）。
   实测：污染结果普遍 <0.5，有效命中 >0.66。

2. **索引对账巡检** — `src/interfaces/cli/cleanup.rs` 的 `run_health`
   `MATCH (m:Method) RETURN count(m)`（read_query）vs
   `collection_info(CODE_METHODS).points_count`，不一致输出"索引漂移，建议 --full 重建"。
   ⚠️ **陷阱**：漂移 ≠ 必为 bug。先 `ps aux | grep "dt build"` —— 并发构建中
   （另一进程写 Memgraph 未同步 Qdrant）是瞬态中间态，等构建完自然一致。

3. **跨项目分组展示** — `render_human` 按 project 分组 + `📦 命中项目分布: a / b` 统计行。
   多项目命中一眼可见，跨项目噪音不淹没目标项目结果。

4. **残留项目清理**（误用目录名构建产生的死数据，如 uvp-im-center）— Python 脚本：
   - Memgraph：`neo4j` 驱动 `MATCH (n) WHERE n.project=$p DETACH DELETE n`（bolt://127.0.0.1:7688）
   - Qdrant：`qdrant-client` `qm.FilterSelector(filter=qm.Filter(must=[qm.FieldCondition(key="project", match=qm.MatchValue(value=$p))]))`，
     遍历 code_methods/doc_chunks/kg_nodes 三集合删除。
   清理后对账立即一致（实测 16366≠17920 → 16355=16355）。

5. **knowledge 世界补知识层**（code 项目无 knowledge 节点时）：
   `dt learn "<主题>" --project <名> --entities "A,B,C" --pattern "..." --pitfalls "..." --decisions "..." --success true`
   生成 Knowledge/Experience/Playbook 节点，再 `dt build --source knowledge` 同步向量
   （kg-sync 已弃用，走 --source knowledge）。同步后检索分数 0.9+。

## 查询词策略（复测验证：中文 80% vs 英文 100%）

- 中文自然语言查询对中文短语语义召回偏弱（"群成员管理" 0 命中，"添加群成员" 命中）
- 中文查询尽量带具体动词；关键功能（accountImport/addGroupMember/getUserSig）用
  英文标识符或中英混搭兜底可满分召回
- 已写入项目 AGENTS.md 防再犯

## 自测脚本

`scripts/verify-kg.sh`（项目根，bash 运行）— 8 组检查：dt sense 索引状态、
中文检索 5 查询、英文标识符 4 查询、注释错位回归（groupMsgGetSimple/groupMsgRecall）、
dt health 索引对账、错 world 低分提示、分组展示、knowledge 世界检索。
`bash scripts/verify-kg.sh --full` 追加 cargo test 全量（676 passed）。
注意第 5 项对账在有并发构建时显示警告属正常。

## 验证模式

每次代码改动后跑新鲜 ad-hoc 验证：写 `/tmp/hermes-verify-<名>.sh` 覆盖改动行为，
bash 运行 + PASS/FAIL 统计，结束后删除临时脚本。dt 无 canonical test 套件，
cargo test 与 ad-hoc 脚本双轨验证。
