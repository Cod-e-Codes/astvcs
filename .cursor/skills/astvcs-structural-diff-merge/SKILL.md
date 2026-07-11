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
- Path-level renames use `detect_path_renames` and surface as `EditIntent::RenamePath` in `status`/`diff` (exact content or same-extension AST edit-only pairing).
- Intra-file moves use `Mutation::MoveSubtree` after LCS when structure fingerprints match uniquely; fingerprints include editable-leaf payloads so same-shape siblings with different literals can disambiguate. `MoveNode` remains for role/key LCS repositioning.
- After LCS, unmatched structural siblings pair by kind with a child-count + index-distance score (not first-match scan order); editable leaves use index-distance only.
- Trivia-only edits use `SetTrivia` / `SetRootTrailingTrivia`. Same-id nodes still recurse into children; the reorder path diffs child trivia after `ReorderChildren`.
- Trailing comment text often lives in leading trivia before the next sibling token, not in the comment node payload.
- `diff_graphs` stays mutation-only for merge. `diff_graphs_detailed` shares the same recursion and records `AlignEdge` / `AlignMethod` for the HTML viewer (`src/diff/view.rs`, `diff --view`). Do not invent confidence scores; label the pass that produced each match.
- `pair_equal_node_ids` must pair duplicate content-addressed sibling ids in list order (e.g. multiple `,` tokens under `parameters`). A single-index map drops duplicates and emits phantom punctuation inserts on wide sibling lists (`7×7 > LCS_THRESHOLD`).
- `InsertSubtree` `before` anchors use `resolve_insert_before_in_old`: prefer the next sibling already in the old parent, else scan forward over pending inserts to the next matched old-graph sibling (parameter lists before `)`), else fall back to the new-graph next sibling id (trailing expression inserts). `MoveNode` / `MoveSubtree` keep `insert_anchor_new` (new-graph next sibling).
- After alignment changes, run `workflow_demo_prepend_and_disjoint_merge`, `identity_demo_payload_edit_disjoint_merge_and_conflict`, `trailing_comment_and_literal_edit_merge`, and `cli_diff_view_writes_html_with_alignment`.

### Intent changes

- Intents classify raw mutations for human-readable `diff` output (`PrependComment`, `RenameIdentifier`, `RenamePath`, `MoveSubtree`, etc.).
- Default output uses compact intent labels and aggregates formatting-only intents. `--details` and `-v` restore node IDs and raw mutations.
- Overlap logic in `intent/` drives merge conflict detection. Changing intents can change which merges succeed.
- Wire new mutation variants through `classify_mutation`, `intents_disjoint`, `remap_mutation`, and `are_disjoint_edits`.

### Merge changes

- Preserve atomic rollback on conflict.
- Focused conflict output names paths, both sides' intents, overlap reasons, and `--resolve` syntax. Keep full reports available to library callers and `--details`.
- Merge planning reads committed states only (`plan_merge` / `load_state_files`); the working tree is not consulted. Forced merges clobber dirty paths during materialization after the plan is fixed.
- `plan_merge` correlates paths through `detect_path_renames` per side before per-file `merge_path`.
- `MoveSubtree`/`MoveNode` are disjoint from payload edits on the same `node_id` during merge.
- Disjoint sibling payload edits under the same parent should merge when they touch different nodes.
- Identical mutations on both branches (byte-for-byte equal) must not report overlap via `SameIntent`. Omit shared merge-equivalent mutations from the combined apply batch (do not apply even once). Also omit punctuation-only `InsertSubtree` token inserts when both branches emitted any under the same parent (covers side-unique phantom commas, e.g. JSON array alignment).
- Shared fixtures for every AST frontend live in `src/merge/language_merge_cases.rs`; extend them when adding a language or changing merge overlap rules.
- Conflicting `SetTrivia` on the same slot should report a structural conflict.
- Do not write conflict markers into the working tree.

### Debugging

```powershell
cargo test workflow_demo identity_demo same_file_demo merge_demo merge_conflict
```

Use `diff --details` for raw mutations without notices, or `-v` for raw mutations plus operational notices:

```powershell
cargo build --release
.\target\release\astvcs.exe --repo examples\identity-demo diff --details conflict.rs
```

Open the change-first HTML viewer (skips the browser when `CI` is set; still prints the temp HTML path):

```powershell
.\target\release\astvcs.exe --repo examples\identity-demo diff --view conflict.rs
```

Three-way inspection:

```powershell
.\target\release\astvcs.exe --repo <repo> diff --base <base> --left <left> --right <right> <path>
```

## Verify

- Targeted `cargo test` for affected scenarios.
- Full suite via `bash .cursor/skills/astvcs-develop/scripts/verify.sh`.
- Update `examples/` and [examples/README.md](../../../examples/README.md) if user-visible merge or diff behavior changes.
