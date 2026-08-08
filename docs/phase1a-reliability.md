# Phase 1A reliability foundation

`TaskStore` is persisted in the existing SQLite snapshot database and provides
run/file/chunk task rows, leases, recovery, retry/dead-letter state, and a
restart-safe progress summary. A task's `task_id + file_id + chunk_id` is the
SQLite idempotency boundary; `file_hash` and `dataset_version` are retained on
each row so callers can reject stale work before writing downstream.

Qdrant, Memgraph, and snapshot writes remain individually upsert/idempotent,
but are not part of a distributed transaction. The state row must therefore be
marked successful only after the downstream operation returns success.

The SiliconFlow client retries only 429/502/503/504 and connect/timeout
failures, releases its semaphore during backoff, honors numeric `Retry-After`,
and preserves a total request deadline. The semaphore and retry policy are
process-local: no global RPM/TPM coordination is claimed across processes.

## Explicit follow-up gaps

* The Nacos source currently returns the present `VirtualFile` set but does not
  yet enumerate deletions and remove corresponding Qdrant/Memgraph records.
  Deletion cleanup needs repository-specific delete APIs and an integration test
  with fake repositories; it is intentionally not silently treated as success.
* Phase 1A exposes the durable store and client policy but does not yet wire
  `TaskStore` into the production Nacos CLI orchestration. That wiring should
  enqueue files/chunks, checkpoint each file immediately, and call
  `recover_stale()` at process start.
* HTTP-date `Retry-After` parsing and process-wide rate-limit sharing remain
  future work.
