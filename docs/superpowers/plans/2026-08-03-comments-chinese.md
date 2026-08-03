# 项目英文内容中文化（注释/日志/文档）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 digital-twin-v2 项目中所有源码注释、日志/错误消息字符串、文档中的英文内容转为中文，同时保证编译、clippy、测试全绿。

**Architecture:** 按模块分片（domain/shared/interfaces/application 子模块/infrastructure/入口/脚本/文档），每片独立委托翻译，文件集互不重叠的片可并行（每批最多 3 个），每片完成后运行 `cargo check` + `cargo clippy` + `cargo test --lib` 验证并提交。

**Tech Stack:** Rust (dt-daemon, Cargo workspace)，Python，Protocol Buffers，Shell

**设计文档:** `docs/superpowers/specs/2026-08-03-comments-chinese-design.md`

## Global Constraints

1. **翻译范围**：`//`、`///`、`//!`、`/* */` 注释正文、文档注释、`tracing::info!/warn!/error!/debug!`、`anyhow!`、`bail!`、`println!`、`eprintln!`、`.expect()`/`assert!` 消息（仅测试内部提示语）。
2. **保留原样（禁止翻译）**：
   - 代码标识符、变量名、函数名、类型名、结构体/枚举字段名、宏名
   - URL、路径、端口号、CLI 命令名与子命令名、配置键名
   - 英文专有名词：Jenkins、K8s/Kubernetes、Nacos、Memgraph、Qdrant、Xinference、MCP、Rust、Cargo、gRPC、Cypher、WASM 等
   - License/版权法律文本
   - 注释内嵌的代码示例、配置片段、Cypher 查询语句、功能性的匹配字符串（如 pipeline/test/runner.rs 中的 `"Query methods from graph"`）
   - `#[cfg(test)]`、属性宏、`#![...]` 等语法
3. **翻译风格**：语义保真、简洁自然的中文技术文档语气；中文与英文之间不加多余空格；保持原有注释标记结构（`///` 行仍为 `///`）。
4. **范围排除（禁止改动）**：`target/`、`logs/`、`.git/`、`.weave/runtime/`、`test/fixtures/` 的 JSON 数据、`Cargo.lock`、`.claude/`、`.idea/`、`config/config.yaml.bak`。
5. **基线（开工前已实测）**：`cargo check` 通过（1 个既有 warning：unused_variables，可不动）；`cargo test --lib` = **675 passed / 2 failed**。既有失败与本任务无关，不得视为新增：
   - `infrastructure::parser::ts_java::tests::parses_hello_service`
   - `interfaces::cli::backup_sqlite::tests::copy_database_writes_file`
6. **验证标准（每个任务）**：
   - `cargo check` 编译通过
   - `cargo clippy --all-targets` 无**新增**告警（既有的 1 个 unused_variables 除外）
   - `cargo test --lib` 仍为 675 passed / 2 failed（允许既有 2 个失败，不允许新增失败）
   - 集成测试 `tests/*.rs` 需要外部服务（Memgraph bolt://localhost:7688、Qdrant :6334、Xinference :9997），环境可用时运行、不可用则跳过并在 commit 消息中注明
7. **并行性**：各任务文件集互不重叠，可并行（每批最多 3 个）；`cargo` 的 target/ 目录锁会自动串行化同时构建，属正常现象。

---

### Task 1: 基线复核与测试断言盘点

**Files:**
- 只读：`Cargo.toml`、`tests/*.rs`、`src/application/pipeline/test/runner.rs`

**Interfaces:**
- Consumes: 无
- Produces: 本任务的基线结论供后续所有任务使用

- [ ] **Step 1: 确认编译基线**

运行：
```bash
cargo check
cargo test --lib 2>&1 | tail -5
```
Expected: check 通过；`test result: FAILED. 675 passed; 2 failed`（失败项为 ts_java::parses_hello_service 与 backup_sqlite::copy_database_writes_file）。若基线不同，在 commit 消息中记录实际数字。

- [ ] **Step 2: 盘点测试中需同步的英文提示语**

用 `rg -n '"(.*[A-Za-z]{2,}.*)+"|expect\(|assert!\(|assert_eq!\('` 扫描 `tests/*.rs` 与 `src/application/pipeline/test/`，列出所有英文 `expect()`/`assert!` 消息，判断哪些是「内部提示语」（可译为中文）、哪些是「功能性字符串」（如 Cypher/命令，禁止翻译）。将结论写入 `docs/superpowers/specs/2026-08-03-comments-chinese-design.md` 的附录「测试断言清单」。

- [ ] **Step 3: 提交**

```bash
git add docs/superpowers/specs/2026-08-03-comments-chinese-design.md
git commit -m "docs(comments-chinese): 记录测试断言中文化清单与基线"
```

---

### Task 2: 翻译 src/domain/

**Files:**
- Modify: `src/domain/types.rs`、`src/domain/traits.rs`、`src/domain/config.rs`、`src/domain/error.rs`、`src/domain/id.rs`、`src/domain/mod.rs`
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: domain 层全部注释/日志字符串中文化；`///` 文档注释转中文（类型/方法名不变）

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

按 Global Constraints 规则翻译上述 6 个文件：约 240 行英文注释（types.rs 94、traits.rs 84、config.rs 18、error.rs 11、id.rs 10、mod.rs 少量）。文件内 `#[cfg(test)]` 模块中的英文 `expect()`/`assert!` 提示语同步译为中文（内部提示语；功能性字符串除外）。

- [ ] **Step 2: 编译验证**

运行：`cargo check`
Expected: 编译通过，无错误。

- [ ] **Step 3: Clippy 验证**

运行：`cargo clippy --all-targets 2>&1 | tail -3`
Expected: 无新增告警。

- [ ] **Step 4: 单元测试验证**

运行：`cargo test --lib`
Expected: 仍为 675 passed / 2 failed（既有失败不变，无新增失败）。

- [ ] **Step 5: 提交**

```bash
git add src/domain/
git commit -m "i18n(domain): 注释与日志消息中文化"
```

---

### Task 3: 翻译 src/shared/

**Files:**
- Modify: `src/shared/chunker.rs`、`src/shared/coordinator.rs`、`src/shared/collections.rs`、`src/shared/logging/context.rs`、`src/shared/logging/formatter.rs`、`src/shared/logging/init.rs`、`src/shared/logging/metrics.rs`、`src/shared/logging/mod.rs`、`src/shared/mod.rs`

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: shared 层注释与日志字符串中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 380 行英文注释（chunker.rs 191、coordinator.rs 101 为主要）。注意 `logging/formatter.rs` 与 `metrics.rs` 中的格式化字段名、指标名保持原样。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

`cargo check` → 通过；`cargo clippy --all-targets` → 无新增告警；`cargo test --lib` → 675/2 不变。

- [ ] **Step 5: 提交**

```bash
git add src/shared/
git commit -m "i18n(shared): 注释与日志消息中文化"
```

---

### Task 4: 翻译 src/interfaces/

**Files:**
- Modify: `src/interfaces/cli/*.rs`（20 个文件：build.rs、sync.rs、cleanup.rs、backup*.rs、event.rs、jcli.rs、jenkins_sync.rs、kub.rs、learn.rs、memorize.rs、mod.rs、search_render.rs）、`src/interfaces/grpc/*.rs`（17 个文件：auth.rs、mod.rs、server.rs、wiring.rs、services/*.rs）、`src/interfaces/mod.rs`
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块（含 backup_sqlite.rs 既有失败测试，只译提示语、不修逻辑）

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: CLI 与 gRPC 层注释与日志字符串中文化（本层日志字符串最多）

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 400 行英文注释 + 大量日志字符串（backup_memgraph.rs 38、build.rs 61、grpc/wiring.rs 46 等）。CLI 命令帮助文本、用户可见提示、错误消息均译为中文。注意：`search_render.rs` 中用于渲染搜索结果的**功能性字段/标签**若被测试或下游消费则保留，纯展示文本可译。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

`cargo check` → 通过；`cargo clippy --all-targets` → 无新增告警；`cargo test --lib` → 675/2 不变（backup_sqlite 既有失败仍是同一失败）。

- [ ] **Step 5: 提交**

```bash
git add src/interfaces/
git commit -m "i18n(interfaces): CLI 与 gRPC 注释及日志中文化"
```

---

### Task 5: 翻译 src/application/knowledge/

**Files:**
- Modify: `src/application/knowledge/**`（20 个文件：extract/{consolidate,model,mod,retrieve}.rs、knowledge/{annotation,entities,mod,service}.rs、learn.rs、memory/{entities,mod,service}.rs、mod.rs、reasoning/{lifecycle,mod,service}.rs、thread/{mod,service}.rs）
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: knowledge 子模块注释与日志中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 700 行英文注释（kg_bridge 属于 sync 不在本任务；本任务含 consolidate.rs 95、knowledge/entities.rs 92、knowledge/service.rs 86、reasoning/service.rs 80、learn.rs 76 等）。`extract/model.rs` 中与 JSON 字段对应的方法名/字段保持原样。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

- [ ] **Step 5: 提交**

```bash
git add src/application/knowledge/
git commit -m "i18n(knowledge): 知识子模块注释与日志中文化"
```

---

### Task 6: 翻译 src/application/{build,context,hooks}/

**Files:**
- Modify: `src/application/build/**`（builder.rs、mod.rs、pipeline.rs、service.rs、strategy/*.rs、updater.rs、watcher.rs）、`src/application/context/**`（fusion.rs、graph_parse.rs、mod.rs、search_config.rs、search_mcp.rs、search_memory.rs）、`src/application/hooks/**`（engine.rs、id_generator.rs、mod.rs、node_writer.rs、property_mapper.rs、registry.rs、relationship_writer.rs、side_effect_runner.rs、types.rs）、`src/application/mod.rs`
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: build/context/hooks 子模块注释与日志中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 500 行英文注释（watcher.rs 73、pipeline.rs 73、updater.rs 58、engine.rs 20 等）。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

- [ ] **Step 5: 提交**

```bash
git add src/application/build/ src/application/context/ src/application/hooks/ src/application/mod.rs
git commit -m "i18n(build-context-hooks): 注释与日志中文化"
```

---

### Task 7: 翻译 src/application/pipeline/

**Files:**
- Modify: `src/application/pipeline/**`（config.rs、context.rs、engine.rs、infer_client.rs、mod.rs、output.rs、processor.rs、processors/{chunk,hanlp_client,llm_client,mod,store,tree_sitter}.rs、prompt.rs、registry.rs、test/{cleanup,mod,report,runner}.rs）
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: pipeline 子模块注释与日志中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 600 行英文注释（engine.rs 148、config.rs 62、test/runner.rs 57 等）。**重点注意**：`pipeline/test/runner.rs` 中的 `"Query methods from graph"`、`"Query classes from graph"`、`"Query CALLS relationships"` 等是**功能性 Cypher 查询标记，禁止翻译**；`prompt.rs` 中的提示词模板若含英文说明文字可译，但模板内的占位符 `{...}` 与指令关键字保持原样。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

- [ ] **Step 5: 提交**

```bash
git add src/application/pipeline/
git commit -m "i18n(pipeline): 流水线注释与日志中文化"
```

---

### Task 8: 翻译 src/application/{plugins,sync}/

**Files:**
- Modify: `src/application/plugins/**`（jenkins/{build,client,mod,service}.rs、k8s/{logs,mod,service,status}.rs、mod.rs、registry.rs、svc/{logs,manager,mod,service}.rs）、`src/application/sync/**`（batch.rs、jenkins/{job_sync,mod}.rs、k8s/{client,mod,resource_sync,timeline_sync,types}.rs、kg_bridge.rs、mod.rs、nacos/{client,config_sync,mod,service_sync}.rs、queue.rs、service.rs、traits.rs）
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: plugins/sync 子模块注释与日志中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 600 行英文注释（kg_bridge.rs 177、nacos/config_sync.rs 78、sync/queue.rs 72、k8s/types.rs 42 等）。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

- [ ] **Step 5: 提交**

```bash
git add src/application/plugins/ src/application/sync/
git commit -m "i18n(plugins-sync): 插件与同步模块注释及日志中文化"
```

---

### Task 9: 翻译 src/infrastructure/

**Files:**
- Modify: `src/infrastructure/**`（embedder.rs、hanlp.rs、inference.rs、memgraph/{client,mod,schema}.rs、mod.rs、parser/{document,go,java,javascript,mod,php,python,rust_parser,tree_sitter_utils,ts_go,ts_java,ts_javascript,ts_php,ts_python,ts_rust,ts_typescript,typescript}.rs、provider_router.rs、qdrant/{client,collection,mod,repo}.rs、scanner.rs、siliconflow.rs、sqlite/{mod,repo}.rs、xinference.rs）
- 同步测试断言：以上文件内 `#[cfg(test)]` 模块（含 ts_java.rs 既有失败测试，只译提示语、不修逻辑）

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: infrastructure 层注释与日志中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 600 行英文注释（siliconflow.rs 57、memgraph/schema.rs 51、parser/document.rs 42、hanlp.rs 33、parser/java.rs 36 等）。**重点注意**：parser 各 ts_*.rs 与 xxx.rs 中与 tree-sitter 查询/语法相关的字符串（如 query 片段、node 类型名）保持原样。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

`cargo test --lib` 中 ts_java::parses_hello_service 仍为既有失败（仅提示语已译）。

- [ ] **Step 5: 提交**

```bash
git add src/infrastructure/
git commit -m "i18n(infrastructure): 基础设施注释与日志中文化"
```

---

### Task 10: 翻译入口文件与 build.rs

**Files:**
- Modify: `src/main.rs`、`src/lib.rs`、`src/proto.rs`、`build.rs`

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: 入口文件注释与日志字符串中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 180 行英文注释 + main.rs 的 64 处日志宏（main.rs 162 行注释为主）。注意 `build.rs` 与 `src/proto.rs` 中的生成代码相关注释可译，但 `include!`/`tonic::include_proto!` 等生成代码文件保持原样。

- [ ] **Step 2-4: 验证（同 Task 2 的 Step 2-4）**

- [ ] **Step 5: 提交**

```bash
git add src/main.rs src/lib.rs src/proto.rs build.rs
git commit -m "i18n(entry): 入口文件注释与日志中文化"
```

---

### Task 11: 翻译 python/proto/mcp/scripts

**Files:**
- Modify: `python/dt_log.py`、`mcp/mcp-server.py`、`proto/*.proto`（8 个文件）、`scripts/*.sh`（build-all.sh、check_claude.sh、setup.sh）
- 排除：`scripts/fixes/` 下的 .diff/.json 数据

**Interfaces:**
- Consumes: Task 1 的翻译规范与基线
- Produces: 脚本与协议文件注释中文化

- [ ] **Step 1: 翻译全部英文注释与日志字符串**

约 82 行英文注释（`#`、`//`）。`*.proto` 文件中 `package`、`option`、`rpc` 方法名、字段名、`google.protobuf.*` 类型保持原样，仅注释与描述转中文。

- [ ] **Step 2: 语法验证**

运行：
```bash
python3 -m py_compile python/dt_log.py mcp/mcp-server.py
bash -n scripts/build-all.sh scripts/check_claude.sh scripts/setup.sh
protoc --version 2>/dev/null && echo "protoc 可用(如需可再验证 proto)"
```
Expected: py_compile 无错误；bash -n 无错误。

- [ ] **Step 3: 提交**

```bash
git add python/ proto/ mcp/ scripts/
git commit -m "i18n(scripts): Python/proto/shell 注释中文化"
```

---

### Task 12: 翻译集成测试断言提示语

**Files:**
- Modify: `tests/extract_real_docs.rs`、`tests/s5_knowledge_search.rs`、`tests/unified_search.rs`

**Interfaces:**
- Consumes: Task 1 生成的测试断言清单
- Produces: 集成测试内部提示语中文化

- [ ] **Step 1: 翻译测试内部提示语**

将 3 个测试文件中英文 `expect()`/`assert!` 消息译为中文（如 `"search must succeed"` → `"搜索必须成功"`）。**功能性字符串禁止翻译**：常量 URL（`bolt://localhost:7688` 等）、Cypher 查询、`XINFERENCE_URL` 环境变量名。

- [ ] **Step 2: 编译验证**

运行：`cargo check --all-targets`
Expected: 编译通过。

- [ ] **Step 3: 集成测试（环境可用时）**

运行：`cargo test --test extract_real_docs --test s5_knowledge_search --test unified_search`
Expected: 若 Memgraph/Qdrant/Xinference 可用则通过；不可用则记录跳过。

- [ ] **Step 4: 提交**

```bash
git add tests/
git commit -m "i18n(tests): 集成测试断言提示语中文化"
```

---

### Task 13: 文档中文化

**Files:**
- Modify: `CLAUDE.md`（全英文 → 全中文，保留结构/命令/路径）、`README.md`（残留英文清理）、`docs/superpowers/plans/2026-08-02-knowledge-hybrid-search.md`、`docs/superpowers/plans/2026-08-03-dt-sense.md`、`docs/superpowers/plans/2026-08-03-unified-search.md`、`docs/superpowers/specs/2026-07-31-universal-knowledge-pipeline-design.md`、`docs/superpowers/specs/2026-08-01-knowledge-search-design.md`、`docs/superpowers/specs/2026-08-03-dt-sense-design.md`、`docs/superpowers/specs/2026-08-03-unified-search-design.md`、`docs/superpowers/specs/2026-08-03-comments-chinese-design.md`（若需要清理残留）

**Interfaces:**
- Consumes: Task 1 的翻译规范
- Produces: 全部文档中文版

- [ ] **Step 1: CLAUDE.md 全文中译**

翻译全部英文正文为中文，保留代码块、命令、路径、文件名、格式符号（`**bold**`、列表、代码 fence）不变。

- [ ] **Step 2: docs/ 与 README 残留清理**

对 9 个 docs 文件与 README 执行 `rg -n "^[A-Za-z][A-Za-z0-9 ,.'\"()\-]{10,}$"` 扫描，将残留英文行（每个文件 0-2 行）译为中文；确认无英文正文残留。

- [ ] **Step 3: 提交**

```bash
git add CLAUDE.md README.md docs/
git commit -m "docs(i18n): CLAUDE.md 中译及文档残留英文清理"
```

---

### Task 14: 全量回归验证

**Files:**
- 只读：整个仓库

**Interfaces:**
- Consumes: 所有前序任务的产物
- Produces: 最终回归结论

- [ ] **Step 1: 全量编译与静态检查**

运行：
```bash
cargo build
cargo clippy --all-targets 2>&1 | tail -5
cargo test --lib 2>&1 | tail -5
```
Expected: build 通过；clippy 无新增告警；`675 passed / 2 failed`（既有 2 失败不变）。

- [ ] **Step 2: 英文残留扫描**

运行：
```bash
rg -c "^\s*(//|///|//!|#|/\*)+\s*[A-Za-z]{4,}" src/ python/ proto/ mcp/ scripts/ 2>/dev/null | awk -F: '$2>0' | head -20
rg -n "^[A-Za-z][A-Za-z0-9 ,.'\"()\-]{10,}$" README.md CLAUDE.md 2>/dev/null
```
Expected: 源码中无可翻译的英文注释残留（允许 1-2 个技术性例外并注明）；README/CLAUDE 无英文正文。

- [ ] **Step 3: 提交（若有修正）**

```bash
git add -u
git commit -m "i18n: 全量回归修正" 2>/dev/null || echo "无修正，跳过"
```
