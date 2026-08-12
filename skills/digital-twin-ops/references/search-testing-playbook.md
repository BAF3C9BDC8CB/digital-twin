# Search Testing Playbook (multi-agent, via Hermes Kanban)

Methodology verified on digital-twin-v2 (2 rounds, 2026-08). Purpose: measure search quality with **expected-vs-actual comparison**, distinguishing environment failures from real capability gaps.

## Pipeline (4 agents via kanban, artifact passing via shared `dir:` workspace)

```
T1 [fast model]   Search planner   → search_plan.json  (queries + per-query expected)
T2a/T2b [fast]    Executors (parallel, split by world) → code_results.json / doc_results.json
T3 [strong model] Evaluator        → evaluation_report.md
```

- Every task uses the SAME `--workspace dir:/abs/path`; named output files pass artifacts down the chain (see hermes-kanban-orchestration)
- Task bodies must say「产出即结束,不要反复修改」+ `--max-runtime` (slow-model discipline)

## search_plan.json shape

```json
{"meta": {"data_grounding": {"memgraph": {...}, "qdrant": {...}},
          "environment_observed": {"issue": "..."}},
 "plan": [{"id": "Q1", "world": "code", "query": "saveToDb", "limit": 5,
           "tool": "dt_search", "expected": "应命中 ..."}]}
```

Design ~12 queries across all worlds: code (exact identifier + Chinese semantic), doc, knowledge (dt_search AND dt_search_kg), config, plus 2 edge cases (garbage string that must return 0; single-word fuzzy query).

## results JSON shape (executors)

```json
{"results": [{"id": "Q1", "world": "code", "found": 5,
  "expected": "...", "hits": [{"method": "...", "file": "...", "score": 0.73, "summary": "..."}],
  "hit_expected": true/false, "note": "..."}]}
```

Executors must record REAL results, never fabricate; note when a failure is environmental (degraded flags, 401) vs genuine.

## Evaluation report (evaluator)

- Overview table: queries, hit rate, vs previous baseline
- Per-world scoring table (code/doc/knowledge/config/edge)
- Per-query detail table (ID/world/query/found/hit/note)
- Baseline comparison (degraded round vs healthy round) — this separates environment issues from system issues
- Quality analysis: semantic relevance, Top1 precision, score distribution
- Problems ranked by severity + improvement suggestions with priorities

## Key insights from round 1→2 (degraded → healthy)

- Round 1 (embed 401): 8.3% hit rate — ALL failures attributable to environment; still valuable as degradation-path baseline
- Round 2 (local Xinference embed fixed): 50% — reveals real capability gaps (exact-identifier recall, config world empty, rerank ineffectiveness on knowledge)
- Always record `data_grounding` (node counts per world) so later rounds can detect index drift

## Artifacts

- `/data/myProject/digital-twin-tests/` — round 1 (degraded baseline)
- `/data/myProject/digital-twin-tests-xinference/` — round 2 (healthy) + k3 improvement_plan.md
