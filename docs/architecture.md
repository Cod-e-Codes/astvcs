# Architecture

## Repository model

**States.** Each `commit` writes a content-addressed state (64-character hex id) and a timeline entry with parent link(s). Merge states have two parents. Identical file content is stored once in the blob store; states hold only a manifest (`path -> blob hash`). Committing with no file changes is a no-op.

**Author identity.** Timeline entries record `author_name` and `author_email` metadata for every state created by `commit`, `merge`, and `revert`. Identity is resolved at state-creation time from, in precedence order: `ASTVCS_AUTHOR_NAME` / `ASTVCS_AUTHOR_EMAIL` environment variables, repository-local `config.json` (`author` object), then global `~/.astvcs/config.json`. If none are set, those commands fail with an actionable error rather than guessing from the OS user account. The initial empty root state and other pre-existing timeline entries without author fields deserialize with empty author strings. Identity is **not** part of the content-addressed state id: state ids remain `hash_manifest(manifest)` only, so adding author metadata does not change ids for unchanged file content.

**Structured errors.** Repository operations return `RepoResult<T>` (`Result<T, RepoError>`). `RepoError` carries a `kind` enum (for example `lock_contention`, `dirty_working_tree`, `merge_conflict`, `missing_identity`), a human-readable `message` (matching legacy string output for `.contains(...)` compatibility via `Deref` to `str`), and optional `path` / `reference` fields. The library exposes the full struct; the CLI accepts `--json` on any command to print one JSON object on stderr on failure instead of `error: …`. Plain stderr output is unchanged when `--json` is omitted.

**HEAD.** The `HEAD` file holds either a branch name or a state id (detached HEAD). `status`, `diff`, and `commit` compare against the checked-out state, not the branch tip when detached. `reset` moves the branch tip or detached HEAD; `revert` creates a new state that undoes a prior state on top of HEAD.

**Working tree materialization.** `merge`, `checkout --branch`, `checkout --state`, and hard `reset` materialize a state manifest to disk and sync `index.json`. Dirty-tree refusal and `--force` clobber warnings are centralized in a shared materialize guard (checked before refs, timeline writes, and disk sync) so every command that overwrites the tree behaves the same: refuse by default, warn per path with `--force`. Hard reset to the current tip and checkout of the branch or state already at HEAD may materialize without `--force` to repair index/disk drift. `reset --soft` and no-op reverts skip materialization.

**Repository locking and atomic writes.** Every command that reads or writes refs, `HEAD`, the timeline, the blob store, `index.json`, or the working tree acquires a single exclusive advisory lock on `.astvcs/repo.lock` for the duration of that logical operation. astvcs uses one coarse exclusive lock rather than separate read/write locks: there is no staging area, most commands either read the full repository snapshot or write it end-to-end, and concurrent readers against a writer materializing the tree would still be unsafe. The lock is OS advisory (`flock` on Unix, `LockFileEx` on Windows) via `std::fs::File::try_lock`, not a marker file; when a process exits or is killed the kernel releases the lock, so a stale lock cannot permanently deadlock the repository. If another process holds the lock, astvcs fails fast with `repository is locked by another process; cannot acquire <path>` rather than waiting, because local CLI contention is rare and a clear immediate error is better UX than a silent hang. Reentrant acquisition on the same thread allows nested repo calls without double-locking. Network sync entry points (`fetch`, `push`, remote config) acquire the same lock once per operation. The lock file descriptor is cached per thread and explicitly unlocked (not closed) between commands so sequential in-process repo calls on Linux do not fail reopening `repo.lock` with `WouldBlock`.

Single-file metadata writes (`HEAD`, branch and remote refs, `index.json`, state/timeline JSON, blob payloads, `config.json`, `remotes.json`, and working-tree files during materialization) use same-directory temp files with the `.astvcs-tmp` suffix followed by `rename` into place, so a crash mid-write leaves either the previous complete file or the new complete file, never a partial target. Multi-file materialization is not atomic as a unit: each path and `index.json` are independently atomic, but the operation as a whole can stop between files; the exclusive lock prevents another process from observing that window, and `index.json` is written last so a crash mid-materialize leaves the old index until the command completes or is retried. At the start of each outermost locked command, stray `.astvcs-tmp` files are removed when the canonical file already exists; orphan temps without a canonical target are left alone. Mutating commands update refs and `HEAD` after disk materialization and `index.json` so a failed materialize does not advance branch tips.

**Merge planning is commit-only.** Three-way merge plans are built from blob manifests at the merge base, HEAD, and the other branch tip (`load_state_files` only). The working tree is not consulted, so a forced merge cannot incorporate uncommitted edits into conflict detection or the merged file set; `--force` only clobbers dirty paths when writing the already-computed plan to disk.

**Branches.** Local branch tips live under `.astvcs/refs/heads/`. `branch remove` deletes a ref file only; it refuses the checked-out branch and the last remaining branch. Unmerged commits do not block removal because states are content-addressed and remain in the timeline and blob store until `gc --prune` removes unreachable blobs and optionally `gc --prune-history` removes unreachable state metadata. `config.json` `default_branch` is not updated when a branch is removed.

**Reachability and garbage collection.** A state is reachable if it is the root empty state (`0` repeated 64 times), or if it is reachable by walking parent links starting from every local branch tip, every remote-tracking branch tip, and the current HEAD state when HEAD is detached. A blob is reachable if it appears in the manifest of any reachable state. The shared reachability walk in `store/reachability.rs` is read-only and runs under the repository lock; `gc` and `fsck` both call it.

`gc` uses a two-tier retention model. **Tier 1 (default safe):** `--prune` deletes unreachable blobs from loose storage and the pack index. **Tier 2 (destructive, opt-in):** `--prune-history` deletes unreachable state metadata: both `.astvcs/timeline/{id}.json` and `.astvcs/states/{id}.json`. The root empty state (`ROOT_STATE_ID`) is never deleted. Unreachable states are those not in the `reachable_from_tips` result; the same ref tips apply as for blob GC (branch tips, remote-tracking tips, detached HEAD).

By default `gc` is a dry-run for both tiers and reports unreachable blob count, unreachable state count, and reclaimable bytes for each. Unreachable timeline entries and state manifests are kept until `--prune-history` so you can still `checkout --state <id>` after all refs to a commit are gone. That audit-log retention was the original deliberate choice; operators who prefer disk over recoverability can opt in to history pruning. After `--prune-history`, detached checkout by id of a pruned state fails because the timeline entry is gone. States retained when history is not pruned remain checkoutable by id.

**Blob pack storage.** New commits still write loose sharded JSON files under `.astvcs/blobs/`. Run `repack` to pack loose blobs into zstd-compressed pack files under `.astvcs/packs/` with an `index.json` mapping blob ids to pack offsets. Reads check loose files first, then the pack index. Content addressing is unchanged: blob ids remain SHA-256 over the canonical serialized `FileContent` JSON. Delta encoding (prefix/suffix against a same-shard base blob) is used only when it beats plain zstd compression. Packed blobs participate in reachability, `gc`, `fsck`, and network sync the same as loose blobs.

**Repository integrity (`fsck`).** Default `fsck` is report-only. It checks: state manifests referencing missing blobs; refs pointing to state ids with no timeline entry; timeline entries with no state manifest (`missing state manifest`); state manifests with no timeline entry (`orphaned state manifest`); HEAD naming a branch with no ref file; `index.json` entries inconsistent with HEAD (wrong `state_id`, paths absent from HEAD manifest, or index present while HEAD is invalid); pack index entries that fail decompression or hash verification; and orphan `.astvcs-tmp` files whose canonical target does not exist (the cases `cleanup_stray_temp_files` leaves alone). Unreachable states that were intentionally retained are not reported as errors; after `gc --prune-history` removed them, they are simply absent.

**`fsck --repair`** applies conservative fixes under the repo lock: rewrite `index.json` from HEAD when HEAD is valid and the index is inconsistent; remove stray `.astvcs-tmp` files when the canonical file exists. It refuses when HEAD names a missing branch while other local branches exist. It never repairs missing blobs, pack corruption, or missing state manifests. **`fsck --prune-refs`** deletes dangling local and remote-tracking ref files (never the `HEAD` file). Repairs run before a full re-check; applied fixes are listed in the output. Missing blobs and unreachable history still require `gc --prune` and optionally `gc --prune-history` after refs reflect the history you want to keep.

**On-disk layout.**

```
.astvcs/blobs/       content-addressed file payloads (sharded by hash prefix; loose writes)
.astvcs/packs/       optional zstd-compressed pack files and index.json (via repack)
.astvcs/states/      state manifests
.astvcs/timeline/    parent links and metadata
.astvcs/refs/heads/  branch tips
.astvcs/repo.lock    exclusive advisory lock (empty; OS lock on open)
HEAD                 branch name or state id
index.json           last committed manifest (working-tree baseline)
scan-cache.json      mtime/size snapshot for incremental working-tree scans
```

**Working tree scan.** Honors `.gitignore`, `.astvcsignore`, and git exclude files (ripgrep semantics). Always skips `.astvcs/` and `.git/`. Non-UTF-8 path names are not tracked; file content may be binary.

**Incremental scan cache.** `status` and `commit` reuse `.astvcs/scan-cache.json` when HEAD matches the cache `head_state_id` and the cache version is current. The sidecar stores per-path `{ mtime, size, is_symlink, unix_mode }` and per-directory `{ mtime, child_count }` from the last successful scan. An incremental pass re-stats cached paths and index paths (detecting removals and edits), prunes unchanged directories only when both mtime and child count match, and skips the tracked-file load (parse and mode detection) for paths whose metadata and raw-byte digest still match the last verified snapshot against HEAD. Pass `--full-scan` on `status` or `commit`, or `-v` / `--verbose`, to force a complete walk and full tracked loads. The cache is invalidated on checkout, merge, and hard reset materialization (via `materialize_state_inner`), and rebuilt on the next scan. It is updated after every scan and its `head_state_id` is advanced after a successful commit. Updates run under the repository lock and use atomic writes like other metadata.

**Remaining limitations.** Incremental scans still read raw bytes to confirm verified paths; the win is skipping AST parsing and symlink or mode classification on unchanged files. Directory pruning depends on accurate child counts; if a filesystem reports a stale count, use `--full-scan` or `-v`.

**Binary files.** Files whose bytes contain a NUL or are not valid UTF-8 are stored as `FileContent::Binary` blobs. UTF-8 text (including known text-only paths and parse-fallback sources) continues to use AST or text blobs. Binary payloads share the same content-addressed `blobs/` tree as AST and text: each blob is a JSON envelope `{"kind":"binary","bytes":"<base64>"}` hashed by the serialized bytes (same sharded layout as other kinds). A separate `blobs-bin/` tree was not added: one store keeps deduplication, `gc`, `fsck`, and network sync unified; the `kind` field distinguishes encodings on read. There is no maximum file size policy beyond available disk and memory. Materialization writes raw bytes via `atomic::write_atomic`. `status` reports binary paths as added, modified, or removed like text. `diff` and `diff --state` print path headers and `(binary file - content diff omitted)` instead of a byte-level diff. Three-way merge treats binary paths as opaque whole-file replace only (no structural or line merge); add/add with different bytes conflicts like text add/add.

**File modes and symlinks.** Manifest entries are `path -> { blob, mode }` where `mode` is `regular` (default, serialized as a plain blob id string for backward compatibility), `executable`, or `symlink`. Mode metadata is separate from blob content hashing: the same text blob id with different modes produces different state ids (`hash_manifest` appends the mode tag only for non-regular entries). Symlink targets are stored as `FileContent::Symlink` blobs (`{"kind":"symlink","target":"..."}`) referenced from the manifest. On Unix, checkout creates symlinks via `symlink(2)` and restores the executable bit (`chmod +x`) for `executable` entries. On Windows, astvcs attempts `symlink_file`; if creation fails (common without Developer Mode or elevation), it emits `warning:` and skips the link rather than copying the target. CI enables Developer Mode on `windows-latest` so symlink integration tests run on both platforms. Executable detection on Unix uses the file mode bit; on Windows, `.sh`/`.bash`/`.zsh` files with a `#!` shebang are stored as `executable` (manifest round-trip; `+x` is not applied on disk). The working-tree scan includes symlinks (not followed). Merge treats symlinks as opaque whole-path values like binaries; replacing a symlink with a regular file (or vice versa) on one branch conflicts; mode-only edits merge when one side changed the mode from base.

## Parsing and storage

Supported extensions are parsed with tree-sitter into an `AstGraph` DAG. Each node has a `NodeId`, a `NodeKind`, an optional payload (literal text, identifier name, etc.), and ordered children.

**`NodeId` (one snapshot).** `NodeId` hashes `kind`, `payload`, and child ids. It names a node inside one parsed graph. A payload edit (for example `1` to `2` on a literal) produces a new id for that node. Applying a mutation can reseal ancestors to new ids when child ids change.

**Cross-version continuity.** astvcs does not assign persistent node ids across `commit` calls. Continuity is reconstructed: `diff_graphs` aligns an old graph to a new graph, then emits mutations (`EditPayload`, `RenameIdentifier`, `InsertSubtree`, `SetTrivia`, and others) that reference nodes in the **old** graph. Three-way merge diffs each branch from the merge base and applies those mutations to a copy of the base.

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
| `.html`, `.htm` | HTML |
| `.css` | CSS |

All other paths use line-oriented text blob storage when the file is valid UTF-8 without NUL bytes. NUL-containing or invalid UTF-8 content is stored as a binary blob regardless of extension. Parse failures on supported extensions fall back to text and emit `warning:` on stderr. Known text-only paths (for example `.gitignore`, `.md`, `.txt`, `go.sum`, `.ps1`) store as text blobs silently; use `-v` to see `stored as text blob` notices. Unknown extensions warn once per path per process. Commits are not blocked on text fallback (partially broken sources still need versioning).

When an AST-capable path is stored as a text blob on either HEAD or the working tree, `status` appends ` (text fallback)` to the path line and `diff` prints `(text fallback - structural diff unavailable)` in the path header plus a `parse mode:` intent when left and right differ in storage kind. Use `-v` to see `notice: … text fallback (reason)` detail on stderr in addition to warnings.

Extension detection uses the substring after the last `.` in the path (case-sensitive). A file named `types.d.ts` is treated as `.ts`, not a separate extension.

Materialization uses trivia-aware unparsing (see **Working tree materialization** above): leading gaps before each child are stored at parse time and replayed on output. When a named tree-sitter node spans past its last leaf (common in Go blocks), the gap before the next sibling is taken from the previous sibling's rightmost leaf end byte, not the named node's extended end byte.

## Structural diff

1. Parse old and new sources into graphs.
2. Align children between old and new: LCS on matching `NodeId` (unchanged subtrees), then LCS on `(NodeKind, child_count)` (role pass; payload ignored), then further pairing for structural nodes and payload-editable leaves.
3. Emit mutations anchored to the old graph: `EditPayload`, `InsertSubtree`, `DeleteSubtree`, `RenameIdentifier`, `MoveNode`, `MoveSubtree`, `ReorderChildren`, `SetTrivia`, `SetRootTrailingTrivia`, and others. Insertions use sibling anchors (`before: Option<NodeId>`) rather than absolute indices, so prepending one node does not emit move cascades for trailing siblings. When matched siblings keep the same `NodeId` but leading trivia changes (for example trailing comment text stored before the next sibling token), `SetTrivia` captures the gap. Same-id internal nodes still recurse into children so trivia-only edits and reorder-with-trivia changes are not skipped.

Alignment is heuristic. Wrong sibling pairing can produce delete+insert instead of `EditPayload`, or mis-anchored mutations. The `identity-demo` fixture exercises literal `EditPayload` and cases where alignment fails (rename conflict).

**Rename and move detection.** Two problems are handled separately:

| Kind | Detection | Representation |
|------|-----------|----------------|
| Path-level rename | Pair removed and added manifest paths in `detect_path_renames` | `EditIntent::RenamePath` in `status`/`diff` output (not delete+add) |
| Intra-file subtree move | Post-LCS pass in `diff_children`: unique bijective structure fingerprint match | `Mutation::MoveSubtree` → `EditIntent::MoveSubtree` |

**Path rename pairing.** Text files pair only on exact content (`semantic_eq`). AST files also pair when `diff_graphs` reports edit-only changes (no `DeleteSubtree` or `InsertSubtree`), surfaced as `rename with edits`. Near-identical text paths stay unpaired (delete+add). Merge planning correlates base paths through per-side rename maps; conflicting renames of the same base path to different destinations conflict; keeping the source path while the other branch renamed it to a destination that HEAD also modified independently conflicts.

**Intra-file move scope.** `MoveSubtree` runs after id/role/key LCS passes when exactly one unmatched old child and one unmatched new child share a structure fingerprint (kind tree, ignoring payloads and `NodeId`). Ambiguous siblings (multiple same-shape items) are left to existing `MoveNode`/`ReorderChildren` heuristics or delete+insert. Merge treats `MoveSubtree`/`MoveNode` as disjoint from payload edits on the same `node_id`, so a move on one branch and a body edit on the other apply together. Cross-file function moves are out of scope (path rename only).

**Edit intents.** Raw mutations are classified for human-readable output (`EditLiteral`, `RenameIdentifier`, `RenamePath`, `MoveSubtree`, `PrependComment`, `InsertStatement`, etc.). `diff` prints intents by default; pass `-v` to also print raw mutations.

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

Sync transfers content-addressed objects only: blobs, state manifests, timeline entries, and branch refs. `fetch` downloads missing history and updates remote-tracking refs; it does not change local branches or the working tree. Use `reset`, `checkout --state`, or `merge` with a remote-tracking ref (for example `origin/main`) to work on fetched commits. `push` uploads missing objects and fast-forwards the remote branch (use `--force` to override). `clone` initializes a repository, fetches from the remote, and checks out the default branch.

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
    binaryblob.rs binary detection and working-tree load
    symlinkblob.rs symlink target blobs
  unparser.rs
  diff/
    lcs.rs       longest common subsequence matching
    ast_diff.rs  structural diff; sibling alignment heuristics
    text_diff.rs Myers line diff
  intent/
    mod.rs       edit intent classification and overlap reasoning
  merge/
  store/
    atomic.rs    same-directory rename writes, stray temp cleanup
    blobs.rs     content-addressed blob store
    manifest.rs  manifest entries, file modes, hash_manifest
    tracked.rs   TrackedFile (content + mode)
    working.rs   load working-tree paths with mode detection
    error.rs     RepoError kinds and structured failures
    history.rs   timeline walk and merge-base (LCA)
    identity.rs  author config resolution and persistence
    integrity.rs gc and fsck (calls reachability)
    lock.rs      exclusive advisory repo lock (repo.lock)
    reachability.rs ref-tip reachability walk (shared by gc/fsck)
    walk.rs      gitignore-style working tree scan (full and incremental)
    scan_cache.rs scan-cache.json load/save and path stat helpers
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
| `edit_roundtrip_preserves_structure_across_languages` | Parse, trivial `EditPayload` diff, apply, unparse, re-parse: no structural drift; text matches edited source (Rust, Python, JS, JSON, TS, Go, HTML, CSS) |
| `rust_unparse_roundtrip_via_repo` | Commit and reload preserves Rust source bytes |
| `go_unparse_roundtrip_via_repo` | Commit, reload, and checkout preserve Go source bytes including block closing newlines |
| `same_file_demo_disjoint_merge` | Same-file rename + insert merge keeps formatting (stress test for alignment heuristics) |
| `identity_demo_payload_edit_disjoint_merge_and_conflict` | Sibling literal merge and rename conflicts |
| `trailing_comment_and_literal_edit_merge` | Trailing comment text survives merge when a sibling literal is edited on the other branch |
| `cli_trivia_only_commit` | Whitespace-only formatting commit round-trips through the CLI |
| `cli_branch_remove_guardrails` | Branch remove: checked-out, last branch, not found, recreate name |
| `cli_reset_hard_soft_and_force` | Hard/soft reset, drift repair, force clobber warnings |
| `cli_revert_and_dry_run` | Revert conflicts, dry-run, and successful undo |
| `resolve_remote_ref_for_diff_merge_base_and_checkout` | `origin/main`-style ref resolution |
| `cli_reports_repository_lock_contention` | Lock held externally: CLI fails fast with lock path |
| `concurrent_repo_lock_fails_fast_with_actionable_error` | Second writer gets lock error; succeeds after release (unit, `src/store/repo.rs`) |
| `sequential_acquire_after_release_on_same_thread` | Back-to-back lock acquire on same thread (unit, `src/store/lock.rs`) |
| `stray_temp_file_cleaned_on_next_locked_command` | Leftover `.astvcs-tmp` removed when canonical file exists (unit) |
| `merge_conflict_still_leaves_refs_and_disk_unchanged_under_lock` | Merge rollback with locking enabled (unit) |
| `gc_no_unreachable_is_noop`, `gc_preserves_remote_tracking_blobs`, `gc_twice_is_idempotent`, `gc_prune_history_idempotent`, `gc_preserves_unreachable_states_until_prune_history`, `gc_prune_history_does_not_remove_reachable_states` | Reachability GC and prune-history unit tests (`store/integrity.rs`) |
| `fsck_clean_repository`, `fsck_repair_fixes_index_inconsistency`, `fsck_repair_refuses_ambiguous_head`, `fsck_prune_refs_removes_dangling_ref` | Healthy repo, index repair, ambiguous HEAD refusal, dangling ref prune (unit) |
| `cli_fsck_clean_repository`, `cli_fsck_detects_corruption`, `cli_fsck_repair_fixes_index_inconsistency`, `cli_fsck_repair_refuses_ambiguous_head`, `cli_fsck_repair_leaves_missing_blob`, `cli_fsck_prune_refs_removes_dangling_ref` | fsck clean, corruption detection, repair, and prune-refs (integration) |
| `cli_gc_dry_run_and_prune` | gc dry-run reports blobs and history; `--prune` deletes unreachable blobs (integration) |
| `path_rename_status_and_diff_integration` | Path rename in `status` (`R old -> new`) and `diff` (`RenamePath` intent) |
| `commit_without_identity_fails_with_actionable_error` | `commit` without configured identity fails (no OS-user fallback) |
| `identity_set_and_read_roundtrip_via_repo_open` | Repository `identity set` survives `Repo::open` |
| `identity_recorded_on_commit_merge_and_revert` | Author on timeline entries from commit, merge, and revert |
| `identity_does_not_change_content_addressed_state_id` | State id remains manifest hash; identity is separate metadata |
| `structured_errors_match_plain_messages_and_kinds` | `RepoError.kind` and `--json` stderr; plain message matches string path |
| `path_rename_exact_reports_rename_intent_in_diff` | Exact path rename intent, not delete+add (unit) |
| `path_rename_with_edits_reports_rename_with_edits` | AST rename+edit pairing (unit) |
| `path_rename_merges_with_independent_content_edit` | Rename on one branch + content edit on other merges at renamed path (unit) |
| `path_rename_conflicts_with_independent_add_at_destination` | Rename vs independent add at destination conflicts (unit) |
| `conflicting_path_renames_report_conflict` | Both branches rename same path to different destinations (unit) |
| `move_subtree_and_sibling_payload_edit_merge` | Move + concurrent payload edit merge cleanly (unit) |
| `moved_function_reports_move_not_delete_insert` | Intra-file reposition avoids delete+insert (unit) |
| `binary_commit_status_and_diff` | Binary PNG fixture: status modified, diff omits content |
| `binary_roundtrip_checkout_on_branch` | Byte-for-byte checkout across branches |
| `binary_merge_add_add_conflict` | Add/add conflict on binary paths |
| `binary_fsck_clean_after_commit` | NUL-containing binary: `fsck` clean |
| `binary_push_clone_roundtrip` | Network clone preserves binary bytes |
| `binary_reset_hard_roundtrip` | Hard reset restores binary bytes after working-tree edit |
| `binary_diff_state` | `diff --state` omits binary content between commits |
| `symlink_commit_and_checkout` | Symlink target round-trip (all CI platforms) |
| `executable_mode_commit_and_checkout` | Executable manifest round-trip; Unix restores `+x` |
| `symlink_vs_file_merge_conflict` | Merge conflict when one branch has symlink, other regular file |
| `manifest_hash_regular_backward_compatible` | Regular-mode manifest hash matches legacy string map (unit, `store/manifest.rs`) |
| `incremental_status_reuses_unchanged_file_reads` | Second `status` is incremental and skips tracked loads; touched file alone is re-read (unit, `store/repo.rs`) |
| `incremental_scan_reuses_unchanged_paths` | Incremental walk reuses cached path stats (unit, `store/walk.rs`) |
| `incremental_scan_finds_new_file_in_changed_dir` | Incremental walk discovers new files when directory metadata changes (unit, `store/walk.rs`) |
| `verified_detects_content_change_with_unchanged_stat` | Byte digest catches content edits when metadata is stale (unit, `store/scan_cache.rs`) |

Run `cargo test`, then `cargo clippy --all-targets --all-features -- -D warnings`. Fixture walkthroughs in `examples/README.md` mirror several integration tests.
