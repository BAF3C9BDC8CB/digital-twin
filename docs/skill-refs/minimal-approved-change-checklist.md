# Minimal approved config/client change checklist

Use this for small, explicitly approved digital-twin-v2 changes where production execution is excluded.

1. Establish baseline:
   - `git status --short --branch`
   - `git log -1 --oneline`
   - locate tracked and user-level `pipeline.yaml` paths; resolve symlinks with `realpath`.
   - inspect the exact config/code blocks before editing.
2. Scope the edit:
   - If the user names two config files, verify they are actually distinct. A project config and `~/.config/digital-twin/pipeline.yaml` may resolve to the same inode/path.
   - Preserve API keys byte-for-byte; do not print or rewrite secrets.
   - Search all SiliconFlow chat request implementations. In this codebase that includes `src/application/pipeline/infer_client.rs` and `src/infrastructure/siliconflow.rs`.
   - For model-specific thinking control, make the shared DTO field optional and omit it for unsupported/unrelated models; DeepSeek-V3.2 is the supported case for `enable_thinking: false`.
3. Validate without side effects:
   - `cargo fmt --check`
   - `cargo check --release`
   - focused unit/integration tests relevant to the changed client/config.
   - Do not run real `dt build` unless separately approved.
   - Cargo accepts one positional test filter per invocation; run multiple focused filters as separate commands.
4. Review and commit:
   - `git diff --check`
   - inspect `git diff --stat` and the full diff for scope/secrets.
   - stage only requested files and use the exact requested commit message.
   - report SHA, effective values, tests (including pre-existing warnings), and skipped operations.
