---
name: astvcs-structural-diff-merge
description: Implements or debugs astvcs structural diff, merge, edit intents, and graph mutations. Use when editing src/diff, src/merge, src/intent, src/graph, or unparser.rs, or when the user mentions alignment, LCS, EditPayload, merge conflicts, edit intents, or three-way merge.
paths:
  - "src/diff/**"
  - "src/merge/**"
  - "src/intent/**"
  - "src/graph/**"
  - "src/unparser.rs"
metadata:
  project: astvcs
---

# Structural diff and merge

## Gather

1. Read [docs/architecture.md](../../../docs/architecture.md) for `NodeId`, alignment, merge rollback, and diff pipeline.
2. Find the closest fixture and integration test (see [references/fixtures.md](references/fixtures.md)).

## Act

### Diff changes

- Keep mutations anchored to the **old** graph.
- Prefer sibling anchors over index-based inserts so prepends do not cascade `MoveNode`.
- Trivia-only edits use `SetTrivia` / `SetRootTrailingTrivia`. Same-id nodes still recurse into children; the reorder path diffs child trivia after `ReorderChildren`.
- Trailing comment text often lives in leading trivia before the next sibling token, not in the comment node payload.
- After alignment changes, run `workflow_demo_prepend_and_disjoint_merge`, `identity_demo_payload_edit_disjoint_merge_and_conflict`, and `trailing_comment_and_literal_edit_merge`.

### Intent changes

- Intents classify raw mutations for human-readable `diff` output (`PrependComment`, `RenameIdentifier`, etc.).
- Overlap logic in `intent/` drives merge conflict detection. Changing intents can change which merges succeed.

### Merge changes

- Preserve atomic rollback on conflict.
- Merge planning reads committed states only (`plan_merge` / `load_state_files`); the working tree is not consulted. Forced merges clobber dirty paths during materialization after the plan is fixed.
- Disjoint sibling payload edits under the same parent should merge when they touch different nodes.
- Conflicting `SetTrivia` on the same slot should report a structural conflict.
- Do not write conflict markers into the working tree.

### Debugging

```powershell
cargo test workflow_demo identity_demo same_file_demo merge_demo merge_conflict
```

Use `diff -v` on fixtures to see raw mutations alongside intents:

```powershell
cargo build --release
.\target\release\astvcs.exe --repo examples\identity-demo diff -v conflict.rs
```

Three-way inspection:

```powershell
.\target\release\astvcs.exe --repo <repo> diff --base <base> --left <left> --right <right> <path>
```

## Verify

- Targeted `cargo test` for affected scenarios.
- Full suite via `powershell -File .cursor/skills/astvcs-develop/scripts/verify.ps1`.
- Update `examples/` and [examples/README.md](../../../examples/README.md) if user-visible merge or diff behavior changes.
