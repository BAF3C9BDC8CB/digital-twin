# build 性能、集合向量结构与 dt clean 完整性（2026-08-11 实测）

本文件记录 digital-twin-v2 三个关联问题的根因与修复，全部在真实数据上验证过。

## 1. backfill 补偿串行 → 并发（max_concurrent 配置不生效的陷阱）

**症状**：用户配置 `max_concurrent=32`，Phase 2 主循环确实 32 并发，但构建仍极慢。

**根因**：`src/application/build/pipeline.rs` 的 `backfill_llm_gaps`（缺口补偿）
用的是**串行 `for point in gap_points { ... await }`**——一次只发 1 个请求。
opencode.go 单请求平均 12.8s（中位 10.4s），417 个 failed 缺口点 ≈ 1.5 小时/轮。
日志特征：大量 `补偿成功` 但**没有 `Phase 2 方法分析` 日志**（说明卡在补偿阶段）。

**修复**：串行循环 → `stream::iter(...).buffer_unordered(backfill_concurrency)`，
`backfill_concurrency = self.phase2_concurrency`（与 Phase 2 主循环同源）。
每个缺口点封装成 `async move` 闭包，返回 `(u64, Option<bool>)`：
- `Some(true)` = 成功补写（upsert + mark_llm_analyzed）
- `Some(false)` = 失败（set_payload 记 llm_status=failed + retries+1）
- `None` = 本轮跳过（重试上限 / Nacos 虚拟文件 / 源文件缺失 / 幂等哈希命中）

统计：`results.iter().filter(|(_, r)| *r == Some(true)).count()`。
实测 16 并发 ≈ 16 倍加速（302 缺口从 ~64 分钟 → ~4 分钟）。

**排查方法**：`grep "缺口补偿" /var/log/digital-twin/dt.log` 看每轮处理量；
`grep "OpenAI-Compatible 响应" ... | python3` 统计 elapsed_ms 均值。
tail -f 日志若只见"补偿成功"不见"Phase 2"→ 正在串行补偿。

## 2. ensure_collection 双向量只限 code_methods

**症状**：构建报 `Qdrant search_with_filter: Not existing vector name error`，
发生在 pipeline::engine 的 store 处理器处理文档类文件（如 .gitlab-ci.yml）时。

**根因**：`src/infrastructure/qdrant/repo.rs` 的 `ensure_collection` 曾对**所有集合**
无条件创建双向量（base+llm named vectors）。但写入方不一致：
- `code_methods`：Phase 2 写入 `vectors: {base, llm}` → 需要双向量 ✓
- `kg_nodes` / `doc_chunks`：kg_bridge / consolidate 用**单向量** `"vector"` 字段
  upsert + 不带 name 的 search → 在双向量集合上报 "Not existing vector name"

**修复**：ensure_collection 分支——仅 `collection == CODE_METHODS` 用
`VectorParamsMap`（base+llm），其余集合用单向量 `VectorParamsBuilder`。
已存在的 kg_nodes/doc_chunks（误建成双向量）需**删除重建**：
```bash
curl -s -X DELETE http://127.0.0.1:6333/collections/kg_nodes
curl -s -X DELETE http://127.0.0.1:6333/collections/doc_chunks
```
重建后验证：`curl http://127.0.0.1:6333/collections/kg_nodes` 的 vectors 配置
应为 `{"size": 1024, ...}`（单向量 dict 无 base/llm 键）；code_methods 应为
`{"base": {...}, "llm": {...}}`。

## 3. dt clean 不完整（漏清 pipeline_progress + 遗留库）

**症状**：`dt clean --confirm` 后 Qdrant 集合 0、Memgraph 0 节点，但 SQLite 有残留：
- `/var/lib/digital-twin/snapshots.db` 的 `pipeline_progress` 表（9910 行）没清
- `/var/lib/digital-twin/lazy.db`（历史遗留，file_snapshots 16627 行）没清
- `/var/lib/digital-twin/engine.db`（历史遗留，file_hashes）没清
- `/var/lib/digital-twin/snapshots/` 目录旧文件（baseline/code/documents/software）没清

**根因**：`src/infrastructure/sqlite/repo.rs` 的 `clear_all()` 只执行
`DELETE FROM file_snapshots` + `DELETE FROM build_progress` 两张表，
漏了 `pipeline_progress`（正式表！repo.rs 创建/使用），且不覆盖
lazy.db / engine.db 遗留库。残留 pipeline_progress 会让下次增量构建
误判部分文件"已完成"而跳过索引（数据不全）。

**手动补清**：
```bash
sqlite3 /var/lib/digital-twin/snapshots.db "DELETE FROM pipeline_progress"
rm -f /var/lib/digital-twin/lazy.db* /var/lib/digital-twin/engine.db*
rm -rf /var/lib/digital-twin/snapshots/code /var/lib/digital-twin/snapshots/documents \
       /var/lib/digital-twin/snapshots/software /var/lib/digital-twin/snapshots/*.json
```
**正式库确认**：main.rs/grpc wiring 用 `/var/lib/digital-twin/snapshots.db`；
lazy.db/engine.db 代码零引用（repo.rs:17 只是注释），可安全删。
SQLite 文件本身不自动收缩（清空后文件仍 5.7MB），属正常。

**✅ 已修复（2026-08-11 实施）**：`clear_all()` 现在补清 `pipeline_progress` +
`pipeline_tasks` 两张表。pipeline_tasks 可能尚未创建（TaskStore 独立连接），
对 `no such table` 宽容（`Err(e) if e.to_string().contains("no such table") => 0`），
其余错误照常上报。端到端验证：向 4 表各注入 1 行 → `dt clean --confirm` →
4 表全部归零（修复前只删 2 表）。dt clean 输出 `已删除快照/进度行: N` 可核对。
lazy.db / engine.db / snapshots 目录旧文件仍不在 dt clean 范围，需手动删（同上）。

## 4. ignore_files 忽略机制（3 处必须一起改）

**需求**：用户要按**文件名精确**忽略无价值文件（.gitlab-ci.yml、banner.txt、
.env、Dockerfile、README 等），且下次构建自动删除已索引数据。

**为什么 .gitlab-ci.yml/banner.txt 会进索引**：`document_extensions` 默认含
`yml/txt` → 被 `collect_document_files` 当文档收进 doc_chunks（chunk 无 LLM
摘要 → 渲染层回退"暂无摘要"）；yml 同时走 collect_files → KG 出现 Config 节点。
`NO_LLM_ANALYSIS = "暂无摘要"`（extract/mod.rs），llm_analysis 缺失/空时渲染。

**改动 1 — scanner.rs**：`collect_document_files` 原本**不应用 ignore_files**
（只过滤 ignore_dirs + document_extensions）→ 补上 ignore_files 精确匹配，
与 collect_files 一致。

**改动 2 — pipeline.rs delete_files_from_graph**：原本只删 `:Method` 节点
（`MATCH (m:Method {project}) WHERE m.file_path IN $files`）。非 Method 实体
（Config 等）**没有 file_path 字段**，entity_id 形如 `dt://entity/{project}/{Type}/{文件名}`。
补充两条 Cypher：
- 按文件名删除文件级节点：`WHERE n.entity_id CONTAINS '/Config/' AND ANY(fn IN $names WHERE n.entity_id ENDS WITH fn)`
- 孤儿清理：`WHERE n.entity_id CONTAINS '/Config/' AND NOT EXISTS { (n)--() }`
  （purge_document 已删 RELATES 边后，内容级子实体如 when_parameter/job 名成为孤儿）

**改动 3 — 配置**：`~/.config/digital-twin/config.yaml` scanner.ignore_files
（注意：用户级与仓库 config/config.yaml 是 hardlink，改一端两端同步）。
本次新增 35 个：CI/CD（.gitlab-ci.yml/.travis.yml/Jenkinsfile/Dockerfile/
azure-pipelines.yml/.github/workflows）、环境（.env/.env.example/.npmrc/
.babelrc/.DS_Store/Thumbs.db/web.config/.cursorrules）、banner/版本
（banner.txt/app-version.txt/version.txt/VERSION/NOTICE/COPYING）、构建
（Makefile/CMakeLists.txt/gradle.properties/settings.gradle/docker-compose*.yml/
robots.txt/favicon.ico）。**md 文档全部保留**（用户选择，README 等有价值）。

**忽略后自动删除的机制**：增量构建 select_files 用当前收集文件与快照差异比对
→ 被忽略文件不在 current_map → 判 deleted → purge_document 清理
（RELATES/MENTIONED_IN 边 + Document 节点 + doc_chunks 向量）+ delete_files_from_graph
（Method + Config 实体）。**无需手动删数据，下次构建自动清**。

**验证方式**：`MATCH (n) WHERE n.name = '.gitlab-ci.yml' AND n.project = 'doctor-center' RETURN count(n)`
应为 0；doc_chunks 应只剩 README.md/bootstrap.yml 等有效文档。
注意 `dt search --project` 过滤不完全（会带出其他项目节点），计数验证
应走 Memgraph Cypher 而非 dt search 输出。

## 5. 性能验证方法（真实构建冒烟）

修复后验证并发补偿：`ss -tn | grep ":443" | grep -c ESTAB` 看活跃连接数
（串行=1，并发=16+）；`grep -c "补偿成功" /var/log/digital-twin/dt.log`
看吞吐。坏 model 注入验证自愈闭环：pipeline.yaml 把 model_llm 改成
`nonexistent-model-fail-test` → 构建产生 failed 状态位 → 恢复 model →
增量构建并发补偿翻转为 success（63 个 failed → 63 个 success，零假成功）。
