# Test-failure classification reference

## Read-only evidence recipe

From the repository root:

```bash
git status --short
git log -8 --oneline
cargo test --lib 2>&1
cargo test 2>&1
```

Interpret the results in this order:

- `cargo test --lib` succeeds: source unit tests compile and execute; this says nothing about integration-test target compilation.
- Full `cargo test` fails with `error[E...]` while compiling `tests/*.rs`: classify as a test-target/API compatibility blocker, not a runtime backend failure.
- Only after all test targets compile should Memgraph/Qdrant/Xinference availability be considered.

## Evidence categories

| Evidence | Classification | Typical action |
|---|---|---|
| `missing field ...` in a test struct literal | Test/API drift | Compare current struct definition and all test initializers |
| `takes N arguments but M supplied` | Constructor drift | Compare signature, production call sites, and test call sites |
| `unresolved import` after a removal commit | Stale test against decommissioned module | Retire, migrate, or quarantine the test according to product intent |
| Panic/failed assertion after successful compilation | Executed test failure | Reproduce narrowly and inspect fixture/backend state |
| Connection/health error during an ignored live test | Environment/runtime issue | Record dependency and setup state separately |

## Reporting rule

Do not label a failure “pre-existing” merely because an old plan says so. Verify the exact current test names and whether the code has since changed. Report unit baseline, integration compile blockers, executed failures, ignored tests, warnings, recent refactor commits, and dirty-worktree state as separate facts.

This reference was distilled from a digital-twin-v2 audit where unit tests were green but full `cargo test` was blocked by stale `ProviderConfig` initializers, an outdated chat-client constructor call, and an integration test importing a deliberately removed Nacos module.
