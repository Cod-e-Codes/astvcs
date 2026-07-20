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
- `EditPayload` / `RenameIdentifier` carry optional `occurrence` (like `SetTrivia`) so duplicate content-addressed siblings under the same parent stay distinct across merge; stamp it from the matched sibling slot (and inherit an outer duplicate-sibling scope when descending). List-slot edits merge; nested leaf edits inside duplicated trees still conflict (see merge notes).
- `diff_child_trivia` must key occurrences by sibling *index*, never `child_occurrence_at` by id alone (that always hits occurrence 0 for duplicates).
- Trivia-only edits use `SetTrivia` / `SetRootTrailingTrivia`. Same-id nodes still recurse into children; the reorder path diffs child trivia after `ReorderChildren`. After occurrence COW replace, renumber remaining duplicate-slot trivia under that parent.
- Trailing comment text often lives in leading trivia before the next sibling token, not in the comment node payload.
- `diff_graphs` stays mutation-only for merge. `diff_graphs_detailed` shares the same recursion and records `AlignEdge` / `AlignMethod` for the HTML viewer (`src/diff/view.rs`, `diff --view`). Do not invent confidence scores; label the pass that produced each match.
- `pair_equal_node_ids` documents list-order zip for duplicate ids; wide sibling lists use `lcs_pairs` on the full child sequence so prepend/append shifts do not mis-pair commas.
- `InsertSubtree` `before` anchors use `resolve_insert_before_in_old` with `before_occurrence` for duplicate content-addressed siblings: prefer the next matched old-graph sibling (scanning past pending inserts), else fall back to the new-graph next sibling id. `MoveNode` / `MoveSubtree` keep `insert_anchor_new`.
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
- Identical mutations on both branches (byte-for-byte equal) must not report overlap via `SameIntent`. Shared merge-equivalent mutations are applied once from the left/ours batch; shared phantom punctuation inserts stay omitted entirely. `InsertSubtree` mutations are merge-equivalent only when parent, `before`, `before_occurrence`, and inserted subtree id match. Substantive sibling inserts (functions, fields, declarations, decorators, attributes/annotations, and similar) at the same anchor with different subtree ids are disjoint and all apply in ours-then-theirs order; competing literal or punctuation inserts at the same anchor still overlap. When several same-site inserts share one anchor, synthesize separator trivia on the 2nd+ insert (from the shared anchor `SetTrivia`, else `\n`) so unparse does not abut siblings (Python `@a@b@x` would parse as matmul). First-time wraps that introduce a wrapper node (Python `decorated_definition`, Java `modifiers`) still conflict when both sides wrap differently. Unknown kind strings in `substantive_sibling_insert` must match real `NodeKind::from_ts_kind` passthrough names from the current grammars; lock them with unit tests that assert insert kinds. Same-site insert merge tests must assert separator text and reparsed node counts/names, not only `contains()` plus tree-sitter reparse. Omit shared phantom commas only at the same insert site.
- `EditPayload` / `RenameIdentifier` with the same `node_id` but different `occurrence` are disjoint when that node is duplicated under the scoped parent (list slots). Inherited scope tags (occurrence set when the leaf is *not* duplicated under its immediate parent) on different duplicate-sibling occurrences conflict, including when the leaves themselves have different `node_id`s; never allow that path to “merge clean” into a dangling graph.
- `redirect` / `redirect_map` in `graph/dag.rs` follow one cascade hop per step and stop on cycles; multi-mutation `apply_batch` depends on this when rebasing `MoveNode` parent ids.
- Shared fixtures for every AST frontend live in `src/merge/language_merge_cases.rs`; extend them when adding a language or changing merge overlap rules.
- Conflicting `SetTrivia` on the same slot should report a structural conflict.
- Standalone CLI: do not write conflict markers into the working tree. The Git merge driver writes markers into `%A` on structural conflict (see [docs/git-integration.md](../../../docs/git-integration.md)).

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
