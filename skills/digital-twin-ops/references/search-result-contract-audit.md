# Search result contract audit (CLI / MCP / world dispatch)

## Verified implementation facts

- CLI `dt search` enters `interfaces/cli/build.rs::handle_search`, constructs `SearchRequest`, calls `CrossWorldSearch::search`, then uses `interfaces/cli/search_render.rs` for human or JSON output.
- MCP `dt_search` is registered in `mcp/mcp-server.py` and shells out to `dt search ... --json`; it does not own a separate DTO or renderer.
- `CrossWorldResult` fields: `query`, `world`, `hits`, `total`, `per_world_counts`, `degraded`.
- `SearchHit` is a wide nullable model. Code hits commonly populate `file_path`, line range, `signature`, `calls`, `element_id`, and indexed `llm_analysis`. Knowledge hits may populate `source_ref`, `element_id`, `score_breakdown`, `hop`, `relations`, and `evidence`. Config/doc/memory hits generally leave code fields empty.
- Current dispatch: `all` fuses `code`, `knowledge`, and `doc`; `config` and `memory` are explicit branches. Do not infer behavior from the world enum or stale docs.
- Post-processing occurs after per-world counts are collected, so `per_world_counts` can differ from final filtered `hits` when file/content filters are used.
- Score semantics differ: native retrieval/rerank score for single-world paths versus RRF score for `all`; consumers should not compare them as probabilities.
- `llm_analysis` is read from indexed payloads. Search-time embed/rerank is not the same as an online chat-LLM explanation call. LLM work is primarily in build/index pipelines.

## Compatibility review checklist

1. Trace CLI and MCP entry points to the same service before comparing outputs.
2. Inspect `run_cmd()` stream handling: concatenating child stderr with stdout can corrupt MCP JSON even when CLI JSON stdout is clean.
3. Enumerate world dispatch from the actual `match world` implementation; compare it with docs, MCP schema, and CLI help.
4. Inventory fields by world and distinguish null, empty list, and absent JSON fields.
5. Check whether counts are measured before or after filtering/post-processing.
6. Record `score_type`/ranking semantics before proposing cross-world ranking changes.
7. Separate indexed LLM fields from online LLM calls; search latency alone does not prove chat inference.
8. Propose additive compatibility first: schema version, explicit score type, stable source/location fields, and a metadata object. Avoid deleting existing `SearchHit` fields.

## Preferred compatibility direction

- Freeze a versioned `CrossWorldResult`/`SearchHit` contract.
- Keep legacy top-level fields while adding normalized aliases such as `canonical_id`, `display_title`, `content`, `source`, `location`, `score_type`, and `metadata`.
- Make MCP return validated stdout JSON only; route subprocess stderr to logging/error metadata.
- Make `searched_worlds` explicit, or document whether `all` includes config/memory.
- Add an explicit LLM mode/source (`off`, `indexed`, `online`) if online analysis is introduced; default to indexed behavior.
