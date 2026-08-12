# Nacos Batch Build Progress Review

## Verified review pattern

For `dt build --source nacos`, trace the remote source through `fetch_virtual_files` → `IncrementalStrategy::select_virtual_files` → `ProcessorEngine::analyze_virtual_batch` → snapshot persistence.

Audit these invariants:

- **Bounded work:** the selected virtual-file list should be processed in bounded batches; a concurrency semaphore alone does not bound memory or recovery scope.
- **Checkpoint granularity:** successful items should be checkpointed after each batch, not only after the entire `analyze_virtual_batch` call. Otherwise a process crash reprocesses all successful-but-unsnapshotted items.
- **Failure state:** retain per-item failure reason, retry status, and last update; do not represent a long-running batch only with a final `success/total` log.
- **Deletion symmetry:** `select_virtual_files` returns deleted paths; the build handler must purge corresponding graph/vector records and remove stale snapshots. Ignoring `deleted` creates data drift even when progress reporting looks healthy.
- **Run-level observability:** report run ID, source/project, discovered, selected, skipped, completed, failed, persisted, deleted, elapsed time, and last update.
- **Snapshot failure semantics:** if indexing succeeds but snapshot persistence fails, explicitly surface the replay risk; idempotent writes reduce damage but do not remove wasted LLM/embed work.

## Evidence from the reviewed implementation

The reviewed handler selected all changed Nacos files, invoked one `analyze_virtual_batch(selected, project)` call, and persisted successful snapshots only after the call returned. It logged only aggregate selection and completion counts. The `deleted` result was selected but not purged. This is sufficient for basic incremental behavior, but not crash-resumable batch progress.

## Acceptance tests

1. Inject a failure after batch N and assert restart processes only uncheckpointed items.
2. Assert each successful batch writes snapshots before the next batch begins.
3. Delete a remote config and assert graph, vector, and snapshot records are removed.
4. Assert progress output distinguishes selected, succeeded, failed, checkpointed, and deleted counts.
5. Force snapshot persistence failure and assert the run reports replay risk clearly.
