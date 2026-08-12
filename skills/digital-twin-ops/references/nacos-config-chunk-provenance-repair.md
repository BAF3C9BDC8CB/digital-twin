# Session-specific provenance repair checklist

## Contract

Canonical helper inputs: `namespace`, `group`, `data_id`, `key` (section or dotted key path).

```text
doc_id     = dt://nacos/{namespace}/{group}/{data_id}
source_ref = {doc_id}#{key}
```

Required new `config_chunks` payload fields:

```json
{
  "source": "nacos",
  "doc_id": "dt://nacos/ns/GROUP/app.yaml",
  "namespace": "ns",
  "group": "GROUP",
  "data_id": "app.yaml",
  "source_ref": "dt://nacos/ns/GROUP/app.yaml#spring.cloud"
}
```

## Code trace

1. Inspect `NacosVirtualFileSource` and `VirtualFile` metadata.
2. Inspect chunk output and `StoreProcessor`/`Consolidator` to confirm whether the unified path writes `doc_chunks` or `config_chunks`.
3. Inspect `ConfigChunkVectorizer` and `KgBridge::sync_config_chunks`; these are separate `config_chunks` writers and must share the helper.
4. Inspect `search_config` source projection. Prefer `payload.source_ref`; retain the old namespace/group/data_id/section fallback.
5. Check purge filters and point IDs. Purge by namespace+data_id (or doc_id where supported); use source_ref only for point identity.

## Safe incremental repair plan

- Read-only: scroll `config_chunks` with payloads, measure missing-field coverage, group by namespace/group/data_id, and produce a dry-run repair manifest.
- Source refresh: fetch only affected Nacos records and recompute content/chunks/hashes.
- Staged write: review the manifest, then separately authorize deletes/upserts. Never silently repair production data during a code task.
- Do not run `dt build --source nacos`, `dt nacos-sync --config-chunks`, or a full build unless explicitly requested.

## Focused validation

Run each Cargo filter separately:

```bash
cargo test nacos_chunk_source --lib
cargo test chunk_vectorizer_upserts_to_config_chunks --lib
cargo test config_world_maps_config_chunks_payload --lib
git diff --check
```

Cargo accepts one test filter per invocation. If compilation fails in unrelated pre-existing code, report that separately; do not “fix” unrelated files or include them in the commit.
