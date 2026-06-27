---
name: astvcs-integration-tests
description: Writes or extends astvcs integration tests and CLI fixtures. Use when editing tests/integration.rs, examples/, or when the user asks for end-to-end tests, fixture walkthroughs, merge scenarios, or CLI regression coverage.
paths:
  - "tests/**"
  - "examples/**"
metadata:
  project: astvcs
---

# Integration tests and fixtures

## Gather

1. Read existing helpers at the top of `tests/integration.rs`: `astvcs_bin()`, `run_astvcs()`, `copy_fixture()`.
2. Pick or create a fixture directory under `examples/`.
3. Map the scenario to an existing test in [references/test-catalog.md](references/test-catalog.md) before adding a duplicate.

## Act

### Test patterns

**Library API** (diff/merge internals):

```rust
let old_graph = parse_source("lib.rs", old_src).unwrap();
let new_graph = parse_source("lib.rs", new_src).unwrap();
let diff = diff_graphs(&old_graph, &new_graph);
```

**CLI subprocess** (end-to-end):

```rust
let out = run_astvcs(Some(repo.path()), &["merge", "feature", "-m", "msg"]);
assert!(out.status.success());
```

**Fixture copy**: use `copy_fixture(&dir, &fixture_root())` and skip `.astvcs` so each test starts clean.

### Fixture conventions

- One directory per scenario (`workflow-demo`, `merge-demo`, `identity-demo`, `same-file-demo`).
- Add `.gitignore` in the fixture if generated artifacts appear during manual runs.
- Document CLI steps in [examples/README.md](../../../examples/README.md) with a table row linking fixture, test name, and what it shows.
- Use `.\examples\reset.ps1` in docs to restore baseline files.

### Assertions worth including

- `repo.working_tree_is_clean()` after successful merge.
- No `MoveNode` mutations on prepend-only edits (see `workflow_demo_prepend_and_disjoint_merge`).
- Merge conflicts leave repo unchanged (see `merge_conflict_diagnostics_without_side_effects`).
- `parse_all_supported_languages` covers every `supported_extensions()` entry.
- `edit_roundtrip_preserves_structure_across_languages` checks parse → apply trivial edit → unparse → re-parse for several languages.
- `same_file_demo_disjoint_merge` is the main stress test for same-file alignment heuristics; watch overlapping cases when changing diff/merge.

### What to avoid

- Tests that depend on a pre-existing `.astvcs` directory in fixtures.
- Asserting exact state ids unless the test controls the full history.
- Trivial tests that only restate type signatures or obvious parser success without behavioral claims.

## Verify

```powershell
cargo test <new_test_name>
cargo test
```

For CLI-heavy tests, also run the documented fixture commands from [examples/README.md](../../../examples/README.md).
