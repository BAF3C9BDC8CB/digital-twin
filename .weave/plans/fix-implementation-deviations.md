# 修复实现偏差计划

> 状态：已完成 | 日期：2026-07-10

## 目标
修复 docs 架构文档与实际代码之间的 5 类偏差，并同步更新文档。

## 偏差清单

1. **MAJOR**: 5 个 gRPC service 未实现 (build/context/sync/knowledge/memory)
2. **IMPORTANT**: wiring.rs 使用 Noop 后端，gRPC daemon 模式无法连接真实 Neo4j/Qdrant
3. **MODERATE**: Thread 服务在 context/ 而非 knowledge/thread/
4. **MODERATE**: CLI 命令内联在 main.rs (2624行)，未拆分到 interfaces/cli/
5. **DOCS**: V2 docs 未标注 deprecated，V3 doc 未反映实际命名偏差

## 任务列表

- [x] 1. 移动 Thread 服务到 knowledge/thread/
  **Files**: src/application/knowledge/thread/mod.rs, src/application/knowledge/thread/service.rs, src/application/context/mod.rs, src/application/knowledge/mod.rs, src/main.rs (import 更新)
  **Acceptance**: thread_service.rs 从 context/ 移到 knowledge/thread/service.rs，所有引用更新，cargo check 通过

- [x] 2. 修复 wiring.rs 使用真实后端
  **Files**: src/interfaces/grpc/wiring.rs, src/interfaces/grpc/server.rs
  **Acceptance**: wire() 连接真实 Neo4j/Qdrant（从 config.yaml 读取），server.rs 使用 wire() 的后端而非 Noop，cargo check 通过

- [x] 3. 实现 5 个 gRPC service
  **Files**: src/interfaces/grpc/services/build_service.rs, context_service.rs, sync_service.rs, knowledge_service.rs, memory_service.rs, mod.rs, src/interfaces/grpc/server.rs
  **Acceptance**: 5 个 service 实现 DtCore gRPC trait，委托到 application 层，在 server.rs 注册，cargo check 通过

- [x] 4. 拆分 CLI 命令到 interfaces/cli/
  **Files**: src/interfaces/cli/build.rs, sync.rs, event.rs, memorize.rs, learn.rs, health.rs, mod.rs, src/main.rs
  **Acceptance**: main.rs 从 2624 行减少到 <800 行，CLI 逻辑移到 interfaces/cli/ 模块，cargo check 通过

- [x] 5. 更新文档
  **Files**: docs/architecture-v3-single-crate-layered.md, docs/architecture-v2-*.md (标注 deprecated), .weave/plans/v2-implementation-roadmap.md
  **Acceptance**: V3 doc 反映实际文件命名和结构，V2 docs 标注 deprecated，roadmap 更新偏差修复状态

## 实测中发现的额外偏差（已修复）

### 6. `dt search-kg` CLI 子命令缺失
- **原因**: MCP Server 调用 `dt search-kg` 但 CLI 无此子命令
- **修复**: 新增 `SearchKg` 命令到 main.rs + `handle_search_kg` 函数到 interfaces/cli/build.rs

### 7. `dt kg-sync` 使用 Noop 后端
- **原因**: `handle_kg_sync` 硬编码 `NoopEmbedService` + `NoopVectorRepo`
- **修复**: 改为连接真实 dt-embed gRPC (:50052) + Qdrant gRPC (:6334)

## 实测验证结果

| 测试项 | 结果 |
|--------|------|
| cargo check | ✅ 0 errors |
| cargo test | ✅ 528/529 (1 pre-existing) |
| cargo build --release | ✅ 24s |
| dt health | ✅ Neo4j + Qdrant |
| dt build (全链路) | ✅ 94 methods, 16s |
| dt search (Cypher) | ✅ |
| dt search-kg (向量) | ✅ |
| dt context (六世界) | ✅ 20 items |
| dt plan / domain / history / dependency / verify | ✅ |
| dt event / memorize / learn / thread | ✅ |
| dt kg-sync (1778 nodes) | ✅ 25s |
| dt cleanup / backup / metrics | ✅ |
| MCP 34 tools 全测试 | ✅ |
