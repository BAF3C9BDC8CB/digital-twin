# Search display-type mechanics & IMPLEMENTED two-dimension design (2026-08-06)

User report on `dt search "支付"` (world=all): two display-type complaints. **Both were resolved by the two-dimension design (file_type + content_type) which is IMPLEMENTED, BUILT, and VERIFIED (2026-08-06).** This doc records root causes, the final design, and the verified implementation.

## Issue 1: yaml files display as `[Doc]` instead of `[Config]`

Symptom: `[0.0164] [Doc] config.yaml` with `原文: pay:； service:； url: ...` — a fixture yaml (`test-pipeline/fixtures/yaml/config.yaml`) shown as Doc.

Root cause chain (historical, now fixed):
1. `ScanConfig::document_extensions` (src/domain/types.rs:404) = `["md","txt","pdf","yaml","yml","properties"]` — yaml/yml/properties classified as *documents*, indexed into `doc_chunks`.
2. `doc_chunks` hits hardcoded `entity_type: "Doc"` at src/application/context/search_mcp.rs:641 / :702 (vector path + long-query keyword channel). No extension inference.

Note: `config_chunks` is NOT the home for arbitrary yaml files — it holds only the project's *own* config keys (snapshot_dir, scanner, ... parsed from config.yaml; payloads carry `key`/`section_name`/`data_id`).

## Issue 2: KG entity extracted from .md displays as `[Service]`

Symptom: `[0.0161] [Service] payment | 摘要: 一个示例服务名称... | 来源: dt://doc/.../K8S-LOGS-GUIDE.md` — user: "md 应该属于文档类的".

Root cause: NOT a display bug. It's a KG node — the extraction LLM classified entity `payment` as Service (doc literally says "示例服务名称"). `dt://doc/` prefix only records the *source*; node type is LLM's choice.

## IMPLEMENTED two-dimension design

| dimension | meaning | source | examples |
|---|---|---|---|
| `file_type` | what the FILE is | file extension (static map) | 文档/代码/配置/其他 |
| `content_type` (`entity_type`) | what the CONTENT means | LLM semantic (knowledge), AST (code), extension heuristic (doc world) | Config, Service, Standard, Method, Doc |

### Implementation (files changed 2026-08-06)

1. **NEW `src/domain/file_type.rs`**: `FileCategory::{Document,Code,Config,Other}`; `categorize_path()` / `categorize_ext()` / `resolve_file_types()`. Category slugs: `document` (NOT `doc` — avoids clashing with `world=doc`), `code`, `config`. Labels: 文档/代码/配置/其他. Suffix map: md/markdown/doc/docs/docx/txt/rtf/pdf/rst/adoc → Document; java/go/rs/php/py/js/ts/jsx/tsx/c/h/cpp/cs/rb/kt/swift/sh/bash/lua/pl/sql/dart/zig/proto/gradle/tf → Code; yaml/yml/properties/json/toml/ini/conf/cfg/config/env/xml/lock → Config. `resolve_file_types` accepts category names (`document`/`doc`/`docs`/`code`/`config`/`other`/`all`) or a concrete suffix.
2. **`search_mcp.rs`**: `SearchHit` + `file_type: Option<String>` (slug), `file_type_label: Option<String>` (中文), both `#[serde(default)]`; `SearchRequest` + `file_type: Option<String>`, `entity_type_filter: Option<String>`.
3. **`postprocess_hits(hits, req)`** — unified post-processing called once at end of `search()`: (1) fills file_type/label from `file_path` → `source_ref` → `id` via `infer_file_type_pub()` (private `infer_file_type` delegates to it); (2) filters by file_type: `resolve_file_types(spec)` → match slug; (3) filters by entity_type: case-insensitive equality. **This is the key design choice: one central filter, per-world constructors untouched** (except doc_chunks sites which now fill file_type at construction because their `id` is `doc:block` without a path).
4. **doc_chunks hits** (search_mcp.rs:646 & :707): file_type/label from `doc` (doc_id) via `infer_file_type(Some(doc))`; entity_type = `Config` if file_type==config else `Doc` — the doc-world content-type heuristic (yaml/properties→Config, md/txt→Doc).
5. **`retrieve.rs`** (knowledge hits, ~L1140): fill file_type/label from `c.source_ref` via `crate::application::context::search_mcp::infer_file_type_pub()`.
6. **CLI**: main.rs clap Search gains `--file-type` (`document|code|config|<ext>`) + `--content-type` (alias `--type`); `handle_search` (build.rs) gains both params, passes into SearchRequest.
7. **Rendering** (`search_render.rs` `render_hit`): type_tag = `"{label}/{entity_type}"` when label present and entity_type non-empty/`?`, else label alone, else entity_type, else `?`. Output like `[0.0164] [配置/Config] config.yaml`.
8. **grpc**: `proto/dt_core.proto` SearchRequest + `file_type = 11`, `entity_type_filter = 12`; mapped in `build_service.rs` (`String::new()` defaults for proto string fields, `Option` for internal).
9. **Test constructors**: every `SearchHit { ... }` / `SearchRequest { ... }` in `#[cfg(test)]` needs the new fields — internal types take `None`, proto types take `String::new()`.

### Verified results (real runs)

```
dt search '支付' --limit 8
[0.0164] [配置/Config] config.yaml
[0.0164] [文档/Channel] 支付宝
[0.0164] [代码/Method] doPay
[0.0161] [文档/Service] 支付服务
[0.0161] [文档/Doc] decision-ifcode-waycode.md

--file-type document  → 支付宝/支付服务/decision-ifcode-waycode.md (文档类 only)
--content-type Config → config.yaml only
--file-type code --content-type Method → doPay only
--file-type yaml      → config.yaml only
--file-type java      → doPay only
```
JSON output includes `"file_type":"document","file_type_label":"文档"`. Regression 12/12.

## ⚠️ Pitfall: blanket regex/bulk insertion scripts corrupt Rust source

While adding the new struct fields, a Python script that inserted `file_type: None, file_type_label: None,` after every line matching `entity_type:` **matched wrong locations** and broke compilation:

- Inserted into struct DEFINITIONS: `RankedItem` (fusion.rs — field on the struct def itself), `SearchRequest` struct def in search_mcp.rs → `E0573: expected type, found variant None`.
- Inserted into FUNCTION SIGNATURES: `blank_hit(...)` in search_config.rs gained `file_type: None,` as parameters → `E0061: takes 7 arguments but 5 were supplied`.
- Inserted INSIDE an `if` expression: grpc `doc_id: if req.doc_id.is_empty() { file_type: None, ...` → `struct literal body without path`.
- Inserted twice at some sites → `E0124 field already declared`, `E0062 specified more than once`.
- proto-string fields (`core::SearchRequest` in grpc tests) got `None` instead of `String::new()` → `E0308 mismatched types`.

Recovery: per-site manual patches (remove stray lines, fix indent, `None`→`String::new()` where the field type is String). Because the repo had 60+ pre-existing uncommitted changes (earlier optimization work), `git checkout` was NOT an option — every fix was surgical.

**Lesson**: for Rust struct-literal field additions across many sites, fix ONE compile error at a time via `cargo check` (the compiler lists every site), or patch each constructor precisely. Never blanket-insert by regex/pattern. Always `cargo check` + inspect `git diff` after any bulk or worker-assisted edit.

## Related perf note

`dt build --path ... --file <one md>` with LLM extraction (qwen3.5, CPU ~34s/query) can exceed a 300s tool timeout — when smoke-testing the extraction pipeline, budget 10-15 min or use a tiny fixture. GPU mode (verified working when models load in order embed→rerank→LLM with `--n-gpu 1`) is much faster.
