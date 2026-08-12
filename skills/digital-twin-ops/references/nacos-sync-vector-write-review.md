# Nacos Sync → Vector Write Review Notes

## Scope
Read-only review of the Nacos synchronization and vector-write path. Treat sync/upsert/delete operations as writes requiring explicit approval.

## Contract checks

1. **Trigger and invocation:** trace Nacos change → event/hook → wrapper/MCP → CLI/API. Compare the public argument schema with the actual parser usage.
2. **Source identity:** preserve and verify environment, namespace, group, dataId, service/resource identity, raw content, and raw-content hash.
3. **Storage boundary:** distinguish Memgraph truth from Qdrant semantic index. Confirm the intended collection is `config_chunks`; `doc_chunks` is a separate document-world index.
4. **Embedding:** record provider, model, dimension, batch limits, timeout, fallback, and whether a fallback can silently change the index.
5. **Upsert:** verify deterministic point IDs, payload provenance, retry behavior, and whether repeated sync is idempotent.
6. **Observability:** require batch ID and counts for fetch, parse/chunk, graph write, embedding, upsert, skips, retries, and failures.
7. **Read-only evidence:** use GET/list/count/scroll/search and source inspection. Do not call sync, kg-sync, build-source, upsert, delete, or graph mutation tools.

## Observed integration blocker

The MCP tool contract exposed `nacos_sync(env=...)`, while the underlying CLI usage was `dt nacos-sync [ENV]`. Passing the schema field as `--env` produced `unexpected argument '--env'` before execution. This proves an adapter/argument translation mismatch, not a Nacos, embedding, or Qdrant health failure. Align schema, wrapper translation, and CLI usage before an approved write test.

## Reporting format

Report: chain diagram; confirmed evidence; blockers by severity; read-only limitations; exact write tests still required. Do not claim successful vector writes from backend health checks alone.
