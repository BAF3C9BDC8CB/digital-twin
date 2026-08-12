# Nacos/config_chunks metadata review notes

## Field provenance

| Field | Current source/meaning | Review rule |
|---|---|---|
| `namespace_id` | Nacos namespace API `namespace` | Preserve as an independent identifier |
| `namespace_name` | Nacos `namespaceShowName` | Do not overload the ID field |
| `environment` | Sync task/CLI `env_name` | Never leave as a hardcoded empty string |
| `group` / `data_id` | Config list/detail API | Preserve verbatim |
| original body | Config detail `content`; Memgraph `NacosConfig.content` | Keep separate from reconstructed chunk `text` |
| `resource_type` | deterministic source classification | e.g. `nacos_config` |
| `resource_role` | deterministic role in index | e.g. `config_chunk` |
| `service` | authoritative metadata if available; otherwise nullable/inferred | Do not let an LLM silently invent identity |

## Minimal additive design

Keep existing `namespace`, `text`, collection name, filters, and search mapping for compatibility. Add `namespace_id`, `namespace_name`, `environment`, `raw_content`, `resource_type`, `resource_role`, and nullable `service` to Memgraph/Qdrant writes. Update both create and match paths; otherwise metadata stays stale when content does not change. Keep section text for embedding/search and raw content for exact provenance/recovery.

## Migration and rollback checklist

1. Back up Memgraph and Qdrant; record counts, point IDs, content hashes, and old payload fields.
2. Prefer `config_chunks_v2` when point IDs or identity keys change; dual-write and compare counts, hashes, and provenance before switching readers.
3. For additive in-place backfill, populate only values recoverable from API/task context; leave ambiguous values null.
4. Roll back code independently from Memgraph and Qdrant. Retain the old collection until post-cutover verification passes.
5. Treat sync commands, upserts, deletes, and seed/cleanup tests as writes requiring explicit approval.

## Regression boundary

Do not change ordinary `WalkDir` directory scanning, hidden/binary filters, file limits, or Fs mtime incremental behavior. Nacos metadata belongs at the Nacos source/persistence boundary, not in ordinary file scanning.