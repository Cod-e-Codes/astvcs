# Architecture

## Repository model

**States.** Each `record` writes a content-addressed state (64-character hex id) and a timeline entry with parent link(s). Merge states have two parents. Identical file content is stored once in the blob store; states hold only a manifest (`path -> blob hash`). Recording with no file changes is a no-op.

**HEAD.** The `HEAD` file holds either a branch name or a state id (detached HEAD). `status`, `diff`, and `record` compare against the checked-out state, not the branch tip when detached.

**On-disk layout.**

```
.astvcs/blobs/       content-addressed file payloads (sharded by hash prefix)
.astvcs/states/      state manifests
.astvcs/timeline/    parent links and metadata
.astvcs/refs/heads/  branch tips
HEAD                 branch name or state id
index.json           last recorded manifest (working-tree baseline)
```

**Working tree scan.** Honors `.gitignore`, `.astvcsignore`, and git exclude files (ripgrep semantics). Always skips `.astvcs/` and `.git/`. Non-UTF-8 paths are not tracked as text.

## Parsing and storage

Supported extensions are parsed with tree-sitter into an `AstGraph` DAG. Each node has a `NodeId`, a `NodeKind`, an optional payload (literal text, identifier name, etc.), and ordered children.

**`NodeId` (one snapshot).** `NodeId` hashes `kind`, `payload`, and child ids. It names a node inside one parsed graph. A payload edit (for example `1` to `2` on a literal) produces a new id for that node. Applying a mutation can reseal ancestors to new ids when child ids change.

**Cross-version continuity.** astvcs does not assign persistent node ids across `record` calls. Continuity is reconstructed: `diff_graphs` aligns an old graph to a new graph, then emits mutations (`EditPayload`, `RenameIdentifier`, `InsertSubtree`, and others) that reference nodes in the **old** graph. Three-way merge diffs each branch from the merge base and applies those mutations to a copy of the base.

| Extensions | Language |
|------------|----------|
| `.rs` | Rust |
| `.py`, `.pyw` | Python |
| `.js`, `.mjs`, `.cjs` | JavaScript |
| `.go` | Go |
| `go.mod` | Go module manifest |
| `.c`, `.h` | C |
| `.json` | JSON |
| `.toml` | TOML |
| `.yaml`, `.yml` | YAML |
| `.ts` | TypeScript |
| `.tsx` | TSX |
| `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh` | C++ |
| `.java` | Java |
| `.cs` | C# |
| `.swift` | Swift |
| `.kt`, `.kts` | Kotlin |
| `.zig` | Zig |
| `.sql` | SQL (`tree-sitter-sequel` on crates.io) |
| `.sh`, `.bash` | Bash |

All other paths use line-oriented text blob storage. Parse failures on supported extensions fall back to text and emit `warning:` on stderr. Known text-only paths (for example `.gitignore`, `.md`, `.txt`) store as text blobs silently; use `-v` to see `stored as text blob` notices. Unknown extensions warn once per path per process.

Extension detection uses the substring after the last `.` in the path (case-sensitive). A file named `types.d.ts` is treated as `.ts`, not a separate extension.

Checkout and merge call `materialize_state` to write the state manifest to disk and sync `index.json`. AST materialization uses trivia-aware unparsing: leading gaps before each child are stored at parse time and replayed on output. When a named tree-sitter node spans past its last leaf (common in Go blocks), the gap before the next sibling is taken from the previous sibling's rightmost leaf end byte, not the named node's extended end byte.

## Structural diff

1. Parse old and new sources into graphs.
2. Align children between old and new: LCS on matching `NodeId` (unchanged subtrees), then LCS on `(NodeKind, child_count)` (role pass; payload ignored), then further pairing for structural nodes and payload-editable leaves.
3. Emit mutations anchored to the old graph: `EditPayload`, `InsertSubtree`, `DeleteSubtree`, `RenameIdentifier`, `ReorderChildren`, and others. Insertions use sibling anchors (`before: Option<NodeId>`) rather than absolute indices, so prepending one node does not emit move cascades for trailing siblings.

Alignment is heuristic. Wrong sibling pairing can produce delete+insert instead of `EditPayload`, or mis-anchored mutations. The `identity-demo` fixture exercises literal `EditPayload` and cases where alignment fails (rename conflict).

**Edit intents.** Raw mutations are classified for human-readable output (`EditLiteral`, `RenameIdentifier`, `PrependComment`, `InsertStatement`, etc.). `diff` prints intents by default; pass `-v` to also print raw mutations.

**Text diff.** Fallback files use Myers line diff via the `similar` crate.

Mutations locate children by `node_id`, not stored indices.

## Merge

1. Find the lowest common ancestor on the timeline (`merge-base`).
2. Per-path three-way logic: add/add, delete on one side, modify/delete (keeps the modification), unchanged sides short-circuit inside `merge_files`.
3. Overlap detection uses edit intents, ancestor checks (a deletion covering an edit inside its subtree), and precise insert-site checks. Sibling payload edits under the same parent merge when they touch different nodes. Disjoint structural edits apply in one batch with redirect rebasing. Text merges use disjoint line edits.

Failed merges roll back atomically: HEAD, branch tips, working tree, and `index.json` are unchanged. The error report lists edit intents and raw mutations from each side, plus the overlapping pair (same node, deletion covering a nested edit, same insert site, or same intent). Use `merge --dry-run` to preview, and `diff --base --left --right` to inspect both sides.

When conflicts cannot be merged structurally, `merge --resolve path:ours|theirs` picks the full file from HEAD or the other branch for that path only. astvcs does not write conflict markers into the working tree.

## Network sync

Remotes are stored in `.astvcs/remotes.json`. Remote-tracking branch tips live under `.astvcs/refs/remotes/<name>/`.

Supported remote URLs:

| Scheme | Example |
|--------|---------|
| Local path | `C:/repos/project` or `file:///C:/repos/project` |
| HTTP | `http://127.0.0.1:9421` (from `astvcs serve`) |

Sync transfers content-addressed objects only: blobs, state manifests, timeline entries, and branch refs. `fetch` downloads missing history and updates remote-tracking refs; it does not change local branches or the working tree. `push` uploads missing objects and fast-forwards the remote branch (use `--force` to override). `clone` initializes a repository, fetches from the remote, and checks out the default branch.

The HTTP API uses `/v1/` paths for blobs, states, timeline entries, branch refs, and repository config.

## Source layout

```
src/
  lib.rs
  trace.rs       stderr notice/warning output (notices gated by -v)
  graph/
    dag.rs       AstGraph, snapshots, apply_batch
    node.rs      Node, NodeId, NodeKind
    mutation.rs  structural edit operations
    edge.rs      TriviaSlot
  frontend/
    languages.rs extension to tree-sitter language map
    treesitter.rs parser and translator
    textblob.rs  text fallback
  unparser.rs
  diff/
    lcs.rs       longest common subsequence matching
    ast_diff.rs  structural diff; sibling alignment heuristics
    text_diff.rs Myers line diff
  intent/
    mod.rs       edit intent classification and overlap reasoning
  merge/
  store/
    blobs.rs     content-addressed blob store
    history.rs   timeline walk and merge-base (LCA)
    walk.rs      gitignore-style working tree scan
    repo.rs      repository and CLI backend
  network/
    transport.rs file and HTTP remotes
    sync.rs      fetch, push, clone
    remote.rs    remote configuration
    serve.rs     HTTP repository server
  main.rs
examples/
  workflow-demo/  disjoint AST merge walkthrough
  merge-demo/     add/add, deletion, and config fixtures
  identity-demo/  literal EditPayload, sibling literal merge, rename conflict
  same-file-demo/ same-file disjoint AST merge (rename + insert)
tests/
  integration.rs
```

## Testing

Unit tests live beside modules under `src/`. `tests/integration.rs` exercises the CLI and library together.

| Test | What it guards |
|------|----------------|
| `parse_all_supported_languages` | Every `supported_extensions()` entry and `supported_special_paths()` basename parses and validates |
| `edit_roundtrip_preserves_structure_across_languages` | Parse, trivial `EditPayload` diff, apply, unparse, re-parse: no structural drift; text matches edited source (Rust, Python, JS, JSON, TS, Go with multiline block returns) |
| `rust_unparse_roundtrip_via_repo` | Record and reload preserves Rust source bytes |
| `go_unparse_roundtrip_via_repo` | Record, reload, and checkout preserve Go source bytes including block closing newlines |
| `same_file_demo_disjoint_merge` | Same-file rename + insert merge keeps formatting (stress test for alignment heuristics) |
| `identity_demo_payload_edit_disjoint_merge_and_conflict` | Sibling literal merge and rename conflicts |

Run `cargo test`, then `cargo clippy --all-targets --all-features -- -D warnings`. Fixture walkthroughs in `examples/README.md` mirror several integration tests.
