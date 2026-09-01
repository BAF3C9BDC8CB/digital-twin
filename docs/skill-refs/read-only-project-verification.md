# Read-only project verification reference

Use this reference for a real validation of an already-indexed digital-twin project when builds, writes, and commits are forbidden.

## Command sequence

```bash
# From the requested repository
rg -n -i -C 3 '<repo-name-or-path>' ~/.config/digital-twin/config.yaml
readlink -f ~/.config/digital-twin/pipeline.yaml
dt sense --json
dt health

dt search '<code-positive-term>' --world code --project '<confirmed-project>' --limit 5 --json
dt search '<knowledge-positive-term>' --world knowledge --project '<confirmed-project>' --limit 5 --json
dt search '<doc-positive-term>' --world doc --project '<confirmed-project>' --limit 5 --json
```

Use terms expected to exist in the indexed project, but label them as verification probes rather than universal golden queries.

## Acceptance record

- Discovery: project name/path/registered/indexed, last build, vector count, and config/pipeline effective paths.
- Health: exit code and each backend status.
- Code: `hits`/`total`; verify `file_path`, line range, `signature`; report `llm_analysis: null` separately from search failure.
- Knowledge: entity ID, `score_breakdown`, `hop`, relations, and project/source boundary.
- Doc: `source_ref` or document path and returned content.
- Hook: configured command, script command, path guard, lock, log, and whether the real build command was intentionally not run.

## Known documentation traps

- A referenced guide can exist but be empty; record that as a defect.
- `dt sense` top-level method totals and per-directory method totals may have different semantics. Report the discrepancy, not an unverified conclusion that indexing is corrupt.
- `dt health` may print a provider/backend health check that is not the same as `providers.llm_provider` in pipeline configuration. Report both.
- Hook documentation may describe `dt build`, while the wrapper actually invokes `cargo run --manifest-path ... -- build`. The wrapper is the effective behavior.
- Do not use a hook verification example that invokes the hook if the user forbids builds; inspect the script and config instead.
