# 忽略机制 + 增量清理 + backfill 并发 + 集合类型 (2026-08-11)

## 背景：为什么 .gitlab-ci.yml / banner.txt 没有摘要

`ScanConfig::document_extensions` 默认 `["md","txt","pdf","yaml","yml","properties"]`
(src/domain/types.rs L419) → 扩展名命中的文件会被 `collect_document_files` 当文档收进
doc_chunks(chunk 级, 无 LLM 摘要 → 渲染层回退"暂无摘要", 常量 `NO_LLM_ANALYSIS`)。
同时 yml 文件也走 `collect_files` 代码收集 → KG 出现 Config 实体节点。
搜索结果同文件两条(一条有摘要一条"暂无摘要")= 早期索引残留的空字段节点。

## 修复 1: collect_document_files 不应用 ignore_files(缺陷)

src/infrastructure/scanner.rs `collect_document_files` 原来只过滤 ignore_dirs +
document_extensions, **没有 ignore_files 过滤** → 配置 ignore_files 只对代码收集
(collect_files)生效, 文档路径仍会把 .gitlab-ci.yml/banner.txt 收进 doc_chunks。
修复: 在 document_extensions 过滤前加同名 ignore_files 精确匹配。

## 修复 2: delete_files_from_graph 只删 Method 不删 Config 实体(缺陷)

src/application/build/pipeline.rs `delete_files_from_graph` 原实现只删
`MATCH (m:Method {project}) WHERE m.file_path IN $files DETACH DELETE m`。
但 Config/Service 等实体节点**没有 file_path 字段**(keys 只有 name/project/summary/
entity_id/type/keywords/aliases), entity_id 形如 `dt://entity/{project}/Config/{name}`。
修复(两段 Cypher):
1. 按文件名后缀匹配: `MATCH (n:Entity {project}) WHERE n.entity_id CONTAINS '/Config/'
   AND ANY(fn IN $names WHERE n.entity_id ENDS WITH fn) DETACH DELETE n`
   ($names = files 每项 rsplit('/') 取最后一段)
2. 文件内容级子实体(如 .gitlab-ci.yml 里的 when_parameter/job 名)无文件溯源字段,
   文件删除后成孤儿 → 连同清理: `MATCH (n:Entity {project}) WHERE n.entity_id
   CONTAINS '/Config/' AND NOT EXISTS { (n)--() } DETACH DELETE n`
   (仅删完全无关系的节点, 避免误删被其他文档引用的共享实体)

## 忽略后下次构建自动清理(回答"配置忽略后是否删除旧数据")

**会, 无需手动删**。增量策略(select_files)用当前收集文件集(含 ignore_files 过滤后)
与快照差异比对 → 被忽略文件不在 current_map → 判为 deleted →
1. 文档类(deleted 且 is_document_path 命中 document_extensions): `purge_document`
   清理 RELATES 边 + MENTIONED_IN 边 + Document 节点 + doc_chunks 向量
2. 代码类: `delete_files_from_graph` 清理 Method + (修复后) Config/实体/孤儿

注意: purge_document 只清文档产物; 实体孤儿清理原 §6.5.4 只有注释没实现,
本次在 delete_files_from_graph 里补了兜底。

## backfill 补偿并发化(用户报 max_concurrent=32 仍慢)

症状: 配置 32 并发, 但构建卡在补偿阶段(日志大量"补偿成功"串行出现, 无 Phase 2
方法分析日志)。根因: `backfill_llm_gaps`(pipeline.rs)用**串行 for 循环**
`for point in gap_points.take(gaps) { ... chat().await ... }` —— 一次 1 个请求,
单请求平均 12.8s(中位 10.4s, opencode.go 响应慢), 417 缺口 ≈ 1.5h/轮。
Phase 2 主循环已是 buffer_unordered(phase2_concurrency) 所以快, 补偿是瓶颈。

修复: 串行 for → `stream::iter(gap_points.take(gaps).map(|point| async move {...}))`
.buffer_unordered(backfill_concurrency).collect().await。并发值 = self.phase2_concurrency
(即 provider max_concurrent, 用户配置 32)。闭包内需 clone: client/snapshot_repo/
embed_svc/vector_repo(llm_model/system_prompt/collection/project 转 String, root 转
PathBuf), 取 `let llm_max_tokens = self.llm_max_tokens` 避免借 self。
每个 async move 返回 `(u64, Option<bool>)`: Some(true)=成功补写 / Some(false)=失败
(已记 retries) / None=本轮跳过(retries≥3、Nacos 虚拟文件、源文件缺失、幂等命中)。
统计: `results.iter().filter(|(_, r)| *r == Some(true)).count()`。
实测: 16 路并发 ≈4min 消化 302 缺口(对比串行约 64min), 约 16 倍加速。

## ensure_collection 双向量副作用(报 "Not existing vector name")

Worker A 把 QdrantRepo::ensure_collection 改成**所有集合**都用 named vectors
(base+llm) 创建 → kg_nodes/doc_chunks 被建成双向量, 但它们的写入方
(kg_bridge/consolidate)是**单向量** upsert(`"vector": vec`) + 不带 name 的
search_with_filter → Qdrant 报 "Not existing vector name"。
修复: ensure_collection 只对 code_methods 用双向量, 其他集合单向量
(VectorParamsBuilder 直传)。已建的 kg_nodes/doc_chunks 需删集合重建
(DELETE /collections/{name}, 数据由下次构建重灌; code_methods 不受影响)。
单向量集合的 config.params.vectors 是单个 dict {size,distance,on_disk} 无 base/llm 键,
双向量是有 base/llm 两个键的 dict——用 `'base' in vectors` 判断结构。

## ⚠️ read_file 脱敏陷阱(重大事故教训)

Hermes read_file 对含 api_key 的 yaml 输出会**脱敏显示** `«redacted:sk-…»`,
但文件里是真实 key。若把脱敏显示的内容当真实内容 patch 回写 → **覆盖真实 key**!
本次事故: patch pipeline.yaml 时 new_string 里写了脱敏值, 覆盖了 openai_compatible
的 api_key(原 sk-NUbJe...vuGg 无备份可恢复, 只能改用 Hermes env 的
OPENCODE_GO_API_KEY sk-kkolo...8E3E, 已验证有效)。
恢复路径: ① config/pipeline.yaml.bak.20260809184926(siliconflow key) ② git HEAD
(openai_compatible 段历史为空) ③ ~/.hermes/.env 的 OPENCODE_GO_API_KEY(兜底, 同端点)。
防御: ① patch 含 key 文件前先 `cp` 备份; ② 不要从 read_file 输出复制内容回写,
用 Python yaml 读写或从备份/其他源取; ③ ~/.config/digital-twin/ 与仓库 config/
的 yaml 是**同一 inode hardlink**(ls -i 验证), patch 一处两边都变, 注意同步。
