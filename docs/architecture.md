# Architecture

Design reference for astvcs: on-disk layout, structural diff and merge, locking, network sync, and maintenance commands. For a project overview and quick start, see [README.md](../README.md). For every CLI flag and subcommand, see [commands.md](commands.md).

## Feature scope

User-facing boundaries (in scope vs out of scope) are summarized in the [README scope section](../README.md#scope-at-a-glance). This document describes **how** those features are implemented.

## Repository model

**States.** Each `commit` writes a content-addressed state (64-character hex id) and a timeline entry with parent link(s). Merge states have two parents. Identical file content is stored once in the blob store; states hold only a manifest (`path -> blob hash`). Committing with no file changes is a no-op.

**Author identity.** Timeline entries record `author_name` and `author_email` metadata for every state created by `commit`, `merge`, `revert`, and `cherry-pick`. Identity is resolved at state-creation time from, in precedence order: `ASTVCS_AUTHOR_NAME` / `ASTVCS_AUTHOR_EMAIL` environment variables, repository-local `config.json` (`author` object), then global `~/.astvcs/config.json`. If none are set, those commands fail with an actionable error rather than guessing from the OS user account. The initial empty root state and other pre-existing timeline entries without author fields deserialize with empty author strings. Identity is **not** part of the content-addressed state id: state ids remain `hash_manifest(manifest)` only, so adding author metadata does not change ids for unchanged file content.

**Structured errors.** Repository operations return `RepoResult<T>` (`Result<T, RepoError>`). `RepoError` carries a `kind` enum (for example `lock_contention`, `dirty_working_tree`, `merge_conflict`, `missing_identity`), a human-readable `message` (matching legacy string output for `.contains(...)` compatibility via `Deref` to `str`), and optional `path` / `reference` fields. The library exposes the full struct; the CLI accepts `--json` on any command to print one JSON object on stderr on failure instead of `error: …`. Plain stderr output is unchanged when `--json` is omitted.

**HEAD.** The `HEAD` file holds either a branch name or a state id (detached HEAD). `status`, `diff`, and `commit` compare against the checked-out state, not the branch tip when detached. `reset` moves the branch tip or detached HEAD; `revert` creates a new state that undoes a prior state on top of HEAD.

**Working tree materialization.** `merge`, `checkout --branch`, `checkout --state`, and hard `reset` materialize a state manifest to disk and sync `index.json`. Dirty-tree refusal and `--force` clobber warnings are centralized in a shared materialize guard (checked before refs, timeline writes, and disk sync) so every command that overwrites the tree behaves the same: refuse by default, warn per path with `--force`. The working tree is **dirty** when it differs from HEAD **or** when `staging.json` has staged entries (staged changes would be lost on materialize). Unstaged-only edits block materialize the same as before; staged changes also block unless `--force`. Hard reset to the current tip and checkout of the branch or state already at HEAD may materialize without `--force` to repair index/disk drift. `reset --soft` and no-op reverts skip materialization. `reset --mixed` moves the ref and syncs `index.json` without touching disk. Materialization clears `staging.json` entries.

**Staging index.** `.astvcs/staging.json` holds staged path entries (`blob_id`, `content_kind`, `mode`, or `deleted: true`). `index.json` remains the HEAD baseline (last committed manifest). After the first `add`, `commit` snapshots staged paths only; empty staging with pending working-tree changes errors with `nothing staged; use astvcs add`. Repositories that never run `add` keep legacy whole-tree `commit` behavior. `status` uses git-style two-column labels (staged vs HEAD, unstaged vs effective index). `diff` without flags shows unstaged changes; `diff --staged` shows staged vs HEAD. `merge` and `cherry-pick` refuse when staging is non-empty.

**Stash.** `.astvcs/stash/` stores numbered JSON entries (`{ id, message, base_state_id, created_at, manifest }`) and an optional `stack.json` listing stash ids (newest at index 0). `stash push` writes changed paths as content-addressed blobs, saves the entry, then materializes HEAD (clearing staging). `stash apply` / `stash pop` three-way merge only paths in the stash manifest onto current HEAD and write results to the working tree only; other tracked files are left unchanged; conflicts abort without side effects; `pop` drops the entry on success.

**Rebase.** `.astvcs/rebase-state.json` records an in-progress linear rebase (`branch`, `upstream`, `onto`, `original_tip`, `current_head`, `remaining`, `conflicted`). `rebase <upstream>` collects single-parent commits from the branch tip down to the merge base with upstream (exclusive), oldest first, and replays each onto `current_head` via the same three-way merge planner as `merge` (`plan_three_way_unlocked`). Replay conflicts materialize the partial merge to disk without conflict markers; `rebase --abort` restores `original_tip`. v1 has no interactive rebase editor.

**Cherry-pick.** `cherry-pick <ref>` applies one state's changes onto HEAD as a new commit using the same replay geometry as rebase (not revert). Three-way roles:

| Role | State |
|------|-------|
| **base** | Parent of the cherry-picked commit |
| **left** | Current HEAD |
| **right** | Cherry-picked commit (target) |

Revert inverts this (`base` = target, `left` = parent, `right` = HEAD). Cherry-pick applies **right vs base** onto **left**. Merge commits and the root state are rejected. Conflicts abort with no side effects (same contract as `merge`); unlike rebase, no in-progress state file is written.

**Blame (line-based v1).** `blame <path>` walks HEAD ancestors along the linear first-parent chain (`linear_timeline_parent`). For each commit (newest to oldest), it compares parent vs child file content at the path using line-oriented diff (`diff_text`). Lines introduced or modified in the child are attributed to that timeline entry (state id, author metadata, message). Line indices are mapped backward through the chain to HEAD line numbers. Remaining unattributed lines at the root are credited to the oldest commit where the file exists. AST files are **unparsed** to text for line-based blame; structural intent blame (which AST node last changed a region) is out of scope for v1. Binary files and symlinks return actionable errors. Merge commits block further history walk with an error.

**Bisect (linear v1).** `.astvcs/bisect-state.json` records an in-progress bisect (`original_head`, `original_branch`, `good`, `bad`, `skipped`, `candidates`, `low`, `high`). `bisect start` builds `candidates` by walking the first-parent chain from `bad` back to `good` (commits strictly after `good` through `bad`, oldest first). `good` must be a linear ancestor of `bad`; merge commits on the path error. `bisect run` binary-searches `candidates`, checking out midpoint states and classifying them via a user script (exit 0 = good, 1 = bad, 125 = skip). Like client hooks, `bisect run` calls `suspend_repo_lock` before spawning the script and `resume_repo_lock` after so nested `astvcs` commands succeed. `bisect reset` restores the saved checkout. DAG or merge-heavy histories are out of scope for v1; bisect requires linear first-parent ancestry between good and bad.

**Repository locking and atomic writes.** Every CLI command that reads or writes refs, `HEAD`, the timeline, the blob store, `index.json`, `staging.json`, or the working tree acquires a single exclusive advisory lock on `.astvcs/repo.lock` for the duration of that logical operation. astvcs uses one coarse exclusive lock for local commands: concurrent readers against a writer materializing the tree would be unsafe, and staging writes must stay consistent with commits. The lock is OS advisory (`flock` on Unix, `LockFileEx` on Windows) via `std::fs::File::try_lock`, not a marker file; when a process exits or is killed the kernel releases the lock, so a stale lock cannot permanently deadlock the repository. If another process holds the lock, astvcs fails fast with `repository is locked by another process; cannot acquire <path>` rather than waiting, because local CLI contention is rare and a clear immediate error is better UX than a silent hang. Reentrant acquisition on the same thread allows nested repo calls without double-locking. Network sync entry points (`fetch`, `push`, remote config) acquire the same lock once per operation. The lock file descriptor is cached per thread and explicitly unlocked (not closed) between commands so sequential in-process repo calls on Linux do not fail reopening `repo.lock` with `WouldBlock`. Before running client hooks or bisect test scripts, mutating commands call `suspend_repo_lock` to release the OS lock while keeping the cached descriptor open, then `resume_repo_lock` to re-acquire after the subprocess exits. That lets hooks and bisect scripts invoke `astvcs` again without self-deadlock.

**HTTP serve concurrency.** `astvcs serve` does not hold advisory `repo.lock` for the server lifetime. An in-process `RwLock<Repo>` allows concurrent `GET`/`HEAD` handlers while `PUT` handlers take the write lock. Immutable content-addressed reads (blobs, states, timeline entries, shallow ancestry) skip advisory locking and use unlocked repo read paths so multiple clients and local CLI commands can proceed in parallel. Ref and config reads also skip advisory locking: each ref or config file is read atomically, so serve reads do not block local CLI and vice versa (reads may be momentarily stale during a concurrent CLI ref update). Each `PUT` acquires the write lock, then tries advisory `repo.lock` for the duration of that upload; if local CLI holds the lock, serve returns HTTP 503 with plain body `repository locked`. Writes serialize with each other and with no cross-process lock ordering that could deadlock CLI and serve on the same machine. `remote-serve` remains single-threaded over stdin and uses the standard locked dispatch path per request.

**Client hooks.** Optional executable scripts under `.astvcs/hooks/` run as child processes during mutating operations. v1 hooks: `pre-commit` and `commit-msg` (before commit persist, only when the commit would create a new state), `pre-merge` (after a clean merge plan, before `finish_merge` writes), and `pre-push` (before upload, when the push would send new objects). Missing hooks are skipped. Non-zero exit aborts the operation with `hook_failed`. Pass `--no-verify` on `commit`, `merge`, `pull` (merge step), or `push` to skip hooks. Hooks run with cwd = repository root and environment variables `ASTVCS_ROOT`, `ASTVCS_BRANCH` (empty when detached), `ASTVCS_HEAD_STATE_ID`, plus hook-specific vars (`ASTVCS_COMMIT_MSG_FILE`, `ASTVCS_MERGE_BRANCH`, `ASTVCS_REMOTE`). The commit message is written to `.astvcs/hooks/commit-msg-input` before `commit-msg`; the hook may edit that file. On Windows, `.cmd`/`.bat` hooks run via `cmd /C`, `.ps1` via `powershell -NoProfile -File`; on Unix, executable hooks run directly, otherwise `sh hookpath`.

**On-disk format versioning.** Layout migrations are tracked in `config.json` as `format_version` (separate from the legacy `version` field, which records the config schema revision and remains `2` on init). `format_version` absent or `0` means a pre-format-versioning repository. New repositories write `format_version: 1`. Migrations run on the first outermost `repo_lock` acquisition (alongside stray temp cleanup), in order from the stored version to the current version, each step using `write_atomic_json` on `config.json` or other metadata as needed. Rules: each migration must be idempotent; each step is atomic; refs and `HEAD` are never advanced before on-disk data is consistent. Optional new JSON fields with `serde(default)` do not require a format bump. `fsck` reports `unknown format version` when `format_version` is greater than the binary supports (warning in the report; does not auto-migrate down).

Single-file metadata writes (`HEAD`, branch and remote refs, `index.json`, state/timeline JSON, blob payloads, `config.json`, `remotes.json`, and working-tree files during materialization) use same-directory temp files with the `.astvcs-tmp` suffix followed by `rename` into place, so a crash mid-write leaves either the previous complete file or the new complete file, never a partial target. Multi-file materialization is not atomic as a unit: each path and `index.json` are independently atomic, but the operation as a whole can stop between files; the exclusive lock prevents another process from observing that window, and `index.json` is written last so a crash mid-materialize leaves the old index until the command completes or is retried. At the start of each outermost locked command, stray `.astvcs-tmp` files are removed when the canonical file already exists; orphan temps without a canonical target are left alone. Mutating commands update refs and `HEAD` after disk materialization and `index.json` so a failed materialize does not advance branch tips.

**Merge planning is commit-only.** Three-way merge plans are built from blob manifests at the merge base, HEAD, and the other branch tip (`load_state_files` only). The working tree is not consulted, so a forced merge cannot incorporate uncommitted edits into conflict detection or the merged file set; `--force` only clobbers dirty paths when writing the already-computed plan to disk.

**Branches.** Local branch tips live under `.astvcs/refs/heads/`. `branch remove` deletes a ref file only; it refuses the checked-out branch and the last remaining branch. Unmerged commits do not block removal because states are content-addressed and remain in the timeline and blob store until `gc --prune` removes unreachable blobs and optionally `gc --prune-history` removes unreachable state metadata. When the removed branch is `config.json` `default_branch`, config is updated atomically: prefer `main` if it still exists among remaining branches, otherwise the lexicographically first remaining branch name. `branch create` sets `default_branch` to the new branch when the configured default ref is missing (dangling config). `clone` checks out the remote `default_branch` from upstream `config.json`.

**Tags.** Lightweight tags live under `.astvcs/refs/tags/<name>` as a single state id per file (same atomic write pattern as branch refs). v1 has no annotated tag objects. Tag names cannot contain `/` or `..`. Tags are resolved after local branches and before remote-tracking refs. Tag tips are included in reachability walks so tagged states stay reachable until the tag is removed. `tag remove` deletes the ref only; timeline and blobs remain until `gc`.

**Reachability and garbage collection.** A state is reachable if it is the root empty state (`0` repeated 64 times), or if it is reachable by walking parent links starting from every local branch tip, every tag tip, every remote-tracking branch tip, and the current HEAD state when HEAD is detached. A blob is reachable if it appears in the manifest of any reachable state. The shared reachability walk in `store/reachability.rs` is read-only and runs under the repository lock; `gc` and `fsck` both call it.

`gc` uses a two-tier retention model. **Tier 1 (default safe):** `--prune` deletes unreachable blobs from loose storage and the pack index. **Tier 2 (destructive, opt-in):** `--prune-history` deletes unreachable state metadata: both `.astvcs/timeline/{id}.json` and `.astvcs/states/{id}.json`. The root empty state (`ROOT_STATE_ID`) is never deleted. Unreachable states are those not in the `reachable_from_tips` result; the same ref tips apply as for blob GC (branch tips, remote-tracking tips, detached HEAD).

By default `gc` is a dry-run for both tiers and reports unreachable blob count, unreachable state count, and reclaimable bytes for each. Unreachable timeline entries and state manifests are kept until `--prune-history` so you can still `checkout --state <id>` after all refs to a commit are gone. That audit-log retention was the original deliberate choice; operators who prefer disk over recoverability can opt in to history pruning. After `--prune-history`, detached checkout by id of a pruned state fails because the timeline entry is gone. States retained when history is not pruned remain checkoutable by id.

**Blob pack storage.** New commits still write loose sharded JSON files under `.astvcs/blobs/`. Run `repack` to pack loose blobs into zstd-compressed pack files under `.astvcs/packs/` with an `index.json` mapping blob ids to pack offsets. Reads check loose files first, then the pack index. Content addressing is unchanged: blob ids remain SHA-256 over the canonical serialized `FileContent` JSON. Delta encoding (prefix/suffix against a same-shard base blob) is used only when it beats plain zstd compression. Packed blobs participate in reachability, `gc`, `fsck`, and network sync the same as loose blobs.

**Repository integrity (`fsck`).** Default `fsck` is report-only. It checks: state manifests referencing missing blobs; refs pointing to state ids with no timeline entry; timeline entries with no state manifest (`missing state manifest`); state manifests with no timeline entry (`orphaned state manifest`); HEAD naming a branch with no ref file; `index.json` entries inconsistent with HEAD (wrong `state_id`, paths absent from HEAD manifest, or index present while HEAD is invalid); pack index entries that fail decompression or hash verification; orphan `.astvcs-tmp` files whose canonical target does not exist (the cases `cleanup_stray_temp_files` leaves alone); and `config.json` `format_version` greater than the binary supports (`unknown format version`). Unreachable states that were intentionally retained are not reported as errors; after `gc --prune-history` removed them, they are simply absent.

**`fsck --repair`** applies conservative fixes under the repo lock: rewrite `index.json` from HEAD when HEAD is valid and the index is inconsistent; remove stray `.astvcs-tmp` files when the canonical file exists. It refuses when HEAD names a missing branch while other local branches exist. It never repairs missing blobs, pack corruption, or missing state manifests. **`fsck --prune-refs`** deletes dangling local branch refs, tag refs, and remote-tracking ref files (never the `HEAD` file). Repairs run before a full re-check; applied fixes are listed in the output. Missing blobs and unreachable history still require `gc --prune` and optionally `gc --prune-history` after refs reflect the history you want to keep.

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
staging.json         staged paths overlay (`active` flag set on first `add`)
stash/               numbered stash entries (`0.json`, …) plus `stack.json` index (0 = newest)
rebase-state.json    in-progress linear rebase queue (absent when idle)
bisect-state.json    in-progress bisect search (absent when idle)
scan-cache.json      mtime/size snapshot for incremental working-tree scans
config.json          repository settings (`version`, `format_version`, `default_branch`, optional `author`)
```

`format_version` in `config.json` tracks on-disk layout migrations (see **On-disk format versioning** above). The legacy `version` field is the config schema revision.

**Working tree scan.** Honors `.gitignore`, `.astvcsignore`, and git exclude files (ripgrep semantics). Always skips `.astvcs/` and `.git/`. Non-UTF-8 path names are not tracked; file content may be binary.

**Incremental scan cache.** `status` and `commit` reuse `.astvcs/scan-cache.json` when HEAD matches the cache `head_state_id` and the cache version is current. The sidecar stores per-path `{ mtime, size, is_symlink, unix_mode }` and per-directory `{ mtime, child_count }` from the last successful scan. An incremental pass re-stats cached paths and index paths (detecting removals and edits), prunes unchanged directories only when both mtime and child count match, and skips the tracked-file load (parse and mode detection) for paths whose metadata and raw-byte digest still match the last verified snapshot against HEAD. Pass `--full-scan` on `status` or `commit`, or `-v` / `--verbose`, to force a complete walk and full tracked loads. The cache is invalidated on checkout, merge, and hard reset materialization (via `materialize_state_inner`), and rebuilt on the next scan. It is updated after every scan and its `head_state_id` is advanced after a successful commit. Updates run under the repository lock and use atomic writes like other metadata.

**Remaining limitations.** Incremental scans still read raw bytes to confirm verified paths; the win is skipping AST parsing and symlink or mode classification on unchanged files. Directory pruning depends on accurate child counts; if a filesystem reports a stale count, use `--full-scan` or `-v`.

**Binary files.** Files whose bytes contain a NUL or are not valid UTF-8 are stored as `FileContent::Binary` blobs. UTF-8 text (including known text-only paths and parse-fallback sources) continues to use AST or text blobs. Binary payloads share the same content-addressed `blobs/` tree as AST and text: each blob is a JSON envelope `{"kind":"binary","bytes":"<base64>"}` hashed by the serialized bytes (same sharded layout as other kinds). A separate `blobs-bin/` tree was not added: one store keeps deduplication, `gc`, `fsck`, and network sync unified; the `kind` field distinguishes encodings on read. There is no maximum file size policy beyond available disk and memory. Materialization writes raw bytes via `atomic::write_atomic`. `status` reports binary paths as added, modified, or removed like text. `diff` and `diff --state` print path headers and `(binary file - content diff omitted)` instead of a byte-level diff. Three-way merge treats binary paths as opaque whole-file replace only (no structural or line merge); add/add with different bytes conflicts like text add/add.

**File modes and symlinks.** Manifest entries are `path -> { blob, mode }` where `mode` is `regular` (default, serialized as a plain blob id string for backward compatibility), `executable`, or `symlink`. Mode metadata is separate from blob content hashing: the same text blob id with different modes produces different state ids (`hash_manifest` appends the mode tag only for non-regular entries). Symlink targets are stored as `FileContent::Symlink` blobs (`{"kind":"symlink","target":"..."}`) referenced from the manifest. On Unix, checkout creates symlinks via `symlink(2)` and restores the executable bit (`chmod +x`) for `executable` entries. On Windows, astvcs attempts `symlink_file`; if creation fails (common without Developer Mode or elevation), it emits `warning:` and skips the link rather than copying the target. CI enables Developer Mode on `windows-latest` so symlink integration tests run on both platforms. Executable detection on Unix uses the file mode bit; on Windows, `.sh`/`.bash`/`.zsh` files with a `#!` shebang are stored as `executable` (manifest round-trip; `+x` is not applied on disk). The working-tree scan includes symlinks (not followed). Merge treats symlinks as opaque whole-path values like binaries; replacing a symlink with a regular file (or vice versa) on one branch conflicts; mode-only edits merge when one side changed the mode from base. Absent paths are not treated as `regular` during three-way mode merge.

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
2. Align children between old and new using hash-anchor passes on wide sibling lists (`old.len() * new.len() > 48`), otherwise the original full-list LCS path:
   - **Id pass**: `HashMap` lookup pairing equal `NodeId` values (each id is unique per graph).
   - **Key pass** (wide): in-order zip within each `(NodeKind, payload, child_count)` bucket; **role pass** (wide): same for `(NodeKind, child_count)`.
   - **Bounded LCS**: when the unmatched cross-product is at most 48, run full-list role then key LCS on the remainder (same anchor semantics as before).
   - **Fingerprint pass**: hash buckets of preorder structure signatures; pair when bucket size is 1 on each side.
   - **Fallback**: position-aware structural and payload-editable leaf pairing, then delete/insert.
3. Emit mutations anchored to the old graph: `EditPayload`, `InsertSubtree`, `DeleteSubtree`, `RenameIdentifier`, `MoveNode`, `MoveSubtree`, `ReorderChildren`, `SetTrivia`, `SetRootTrailingTrivia`, and others. Insertions use sibling anchors (`before: Option<NodeId>`) rather than absolute indices, so prepending one node does not emit move cascades for trailing siblings. When matched siblings keep the same `NodeId` but leading trivia changes (for example trailing comment text stored before the next sibling token), `SetTrivia` captures the gap. Same-id internal nodes still recurse into children so trivia-only edits and reorder-with-trivia changes are not skipped.

Alignment is heuristic. Wrong sibling pairing can still produce delete+insert instead of `EditPayload`, or mis-anchored mutations. The `identity-demo` fixture exercises literal `EditPayload` and cases where alignment fails (rename conflict).

**Sibling fallback pairing.** After hash-anchor and bounded LCS passes, unmatched structural siblings are paired by kind, preferring equal `child_count` and smallest index distance (so swapped same-shape siblings are not matched to the first same-kind candidate in scan order). Unmatched payload-editable leaves use the same proximity rule. Pass `-v` to see `notice: diff: … fallback paired siblings …` when this path runs.

**Structure fingerprints.** Fingerprints used for `MoveSubtree` pairing are a preorder `(kind, child_count, payload)` list. Payload is included for editable leaves (`Literal`, `Identifier`, `Token`, `Unknown`) so subtrees that share shape but differ in literal text can be distinguished. Fingerprints still ignore `NodeId` and non-editable node text.

**Known limitations.** Ambiguous siblings with identical structure and identical editable payloads (for example two functions whose bodies are both `1`) are not uniquely pairable; `MoveNode`/`ReorderChildren` or delete+insert may still result. Cross-file subtree moves are out of scope (path rename only). Adding or removing children (for example a new comment sibling) can still force a looser structural match when no equal-`child_count` candidate exists. Delete+insert coalescing into `EditPayload` when fingerprints match is not implemented; full subtree replacement stays delete+insert.

**Rename and move detection.** Two problems are handled separately:

| Kind | Detection | Representation |
|------|-----------|----------------|
| Path-level rename | Pair removed and added manifest paths in `detect_path_renames` | `EditIntent::RenamePath` in `status`/`diff` output (not delete+add) |
| Intra-file subtree move | Post-LCS pass in `diff_children`: unique bijective structure fingerprint match | `Mutation::MoveSubtree` → `EditIntent::MoveSubtree` |

**Path rename pairing.** Text files pair only on exact content (`semantic_eq`). AST files also pair when `diff_graphs` reports edit-only changes (no `DeleteSubtree` or `InsertSubtree`), surfaced as `rename with edits`, and only when source and destination share the same file extension (cross-extension delete+add stays unpaired unless byte-identical). Near-identical text paths stay unpaired (delete+add). Merge planning correlates base paths through per-side rename maps; conflicting renames of the same base path to different destinations conflict; keeping the source path while the other branch renamed it to a destination that HEAD also modified independently conflicts.

**Intra-file move scope.** `MoveSubtree` runs after id/role/key alignment when exactly one unmatched old child and one unmatched new child share a structure fingerprint (preorder kind tree with editable-leaf payloads; `NodeId` ignored). Ambiguous siblings (multiple same-shape items with identical fingerprints) are left to `MoveNode`/`ReorderChildren` heuristics or delete+insert. Merge treats `MoveSubtree`/`MoveNode` as disjoint from payload edits on the same `node_id`, so a move on one branch and a body edit on the other apply together. Cross-file function moves are out of scope (path rename only).

**Planned optimization (AST blob size).** AST blobs are stored as canonical JSON (`kind: "ast"`) and blob ids are content hashes of that JSON. A more compact snapshot encoding (for example `kind: "ast_compact"`) would require a format-version migration and dual deserialize; micro-optimizations that skip empty fields would also change existing blob hashes. Storage compaction is deferred until a backward-compatible migration path exists.

**Edit intents.** Raw mutations are classified for human-readable output (`EditLiteral`, `RenameIdentifier`, `RenamePath`, `MoveSubtree`, `PrependComment`, `InsertStatement`, etc.). `diff` prints intents by default; pass `-v` to also print raw mutations.

**Alignment export and graphical viewer.** `diff_graphs` remains mutation-only for merge and existing callers. `diff_graphs_detailed` shares the same recursion and also records `AlignEdge` values: each sibling pair as `Match` with an `AlignMethod` (`Id`, `Key`, `Role`, `Lcs`, `Fingerprint`, `StructuralFallback`, `LeafFallback`), plus `Insert`/`Delete` for unmatched children. `diff --view` builds a `DiffViewDocument` (graph snapshots, alignment, mutations, classified intents, optional unparsed source) and writes a self-contained HTML page that visualizes paired trees. The viewer consumes real edges only; it does not invent confidence scores or persistent cross-commit node ids.

**Text diff.** Fallback files use Myers line diff via the `similar` crate.

Mutations locate children by `node_id`, not stored indices.

## Merge

1. Find the lowest common ancestor on the timeline (`merge-base`).
2. Per-path three-way logic: add/add, delete on one side, modify/delete (keeps the modification), unchanged sides short-circuit inside `merge_files`.
3. Overlap detection uses edit intents, ancestor checks (a deletion covering an edit inside its subtree), and precise insert-site checks. Sibling payload edits under the same parent merge when they touch different nodes. Disjoint structural edits apply in one batch with redirect rebasing. Text merges use disjoint line edits.

Failed merges roll back atomically: HEAD, branch tips, working tree, and `index.json` are unchanged. The error report lists edit intents and raw mutations from each side, plus the overlapping pair (same node, deletion covering a nested edit, same insert site, or same intent). Use `merge --dry-run` to preview, and `diff --base --left --right` to inspect both sides.

When conflicts cannot be merged structurally, `merge --resolve path:ours|theirs` picks the full file from HEAD or the other branch for that path only. astvcs does not write conflict markers into the working tree.

## Network sync

Remotes are stored in `.astvcs/remotes.json`. Remote-tracking branch tips live under `.astvcs/refs/remotes/<name>/`. HTTP remotes may include an optional bearer token in `remotes.json` (stored in plaintext; file permissions are the operator's responsibility). Local path and `file://` remotes ignore tokens.

Supported remote URLs:

| Scheme | Example |
|--------|---------|
| Local path | `C:/repos/project` or `file:///C:/repos/project` |
| HTTP | `http://127.0.0.1:9421` (from `astvcs serve`) |
| HTTPS | `https://127.0.0.1:9421` (from `astvcs serve --tls-cert ... --tls-key ...`) |
| SSH | `ssh://user@host/path/to/repo` or `user@host:/path/to/repo` (remote must have `astvcs` on `PATH`) |

Sync transfers content-addressed objects only: blobs, state manifests, timeline entries, branch refs, and tags. `fetch` downloads missing history, updates remote-tracking refs, and syncs all remote tags (even when `--branch` limits which branch refs are updated). It does not change local branches or the working tree. `pull` is fetch followed by merge of the remote-tracking branch into the current branch. Use `reset` or `checkout --state` with a remote-tracking ref (for example `origin/main`) or a tag name to inspect fetched commits without merging. `push` uploads missing objects, fast-forwards the remote branch (use `--force` to override), and uploads local tags missing on the remote (tag updates are not fast-forward checked). `clone` initializes a repository, fetches branches and tags from the remote, and checks out the default branch.

**Shallow fetch and clone.** Pass `--depth N` on `fetch`, `clone`, or `pull` to download at most `N` timeline entries counting from each branch or tag tip (`N=1` is tip only), matching git shallow clone semantics. Omit `--depth` for unlimited history (default). The client uses `GET /v1/timeline/{tip}/ancestry?depth=N` over HTTP, SSH remote-serve (query on the path), or a direct repo walk for file remotes. Shallow boundaries are recorded in `.astvcs/shallow.json` as state ids where parent history was intentionally not fetched; boundaries clear on a full fetch (no depth) or when a deeper fetch imports the missing parents. Tag fetch during shallow sync applies the same depth limit per tag tip. Shallow repositories may fail `merge-base`, `merge`, and `pull` when a tip is a shallow boundary or the lowest common ancestor is not present locally; deepen history with `fetch --depth` (higher `N`) or a full fetch.

HTTP API: `GET /v1/refs/tags` returns a JSON map of tag name to state id; `GET`/`PUT`/`HEAD /v1/refs/tags/{name}` read or overwrite a tag tip as plain text.

The HTTP API uses `/v1/` paths for blobs, states, timeline entries, branch refs, repository config, and shallow ancestry listing (`GET /v1/timeline/{tip}/ancestry?depth=N` returns `{"states":["id",...],"shallow_boundary":null|"state_id"}`).

**HTTP authentication.** `astvcs serve` accepts an optional bearer token via `--token` or the `ASTVCS_SERVE_TOKEN` environment variable (CLI wins when both are set). With no token configured, the server is open for local development. When a token is configured, the server fails closed: `PUT` on `/v1/*` always requires `Authorization: Bearer <token>`; `GET` and `HEAD` require the token unless `--public-read` is set. Wrong or missing credentials return HTTP 401 with a plain text body. Token comparison uses constant-time equality. The HTTP client transport sends the stored remote token on every request when configured. Local file remotes remain unrestricted.

**TLS on serve.** Optional `--tls-cert` and `--tls-key` PEM paths enable HTTPS via tiny_http's rustls backend (`ssl-rustls` feature). Both flags must be supplied together. Without them, serve listens on plain HTTP.

**HTTPS client validation.** HTTP remotes use reqwest with rustls. Certificate validation is enabled by default (fail closed on bad or self-signed certs). Pass `--insecure` on `fetch`, `push`, `pull`, or `clone` to call `danger_accept_invalid_certs(true)` for local dev with self-signed serve certs. Bearer tokens work over HTTPS the same as HTTP. `--insecure` does not apply to SSH remotes.

**SSH remotes.** SSH URLs use OpenSSH as the transport. The client runs `ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new user@host astvcs remote-serve --repo <path>` and speaks a newline-delimited JSON protocol over the SSH session stdin/stdout. Host key verification and authentication (keys, ssh-agent) are delegated to the system `ssh` binary. The remote host must have `astvcs` installed on `PATH`. Scp-style URLs (`user@host:/absolute/path`) require `user@` to avoid mistaking Windows drive letters for remotes; the path must be absolute on the remote. Bearer tokens from `remotes.json` or `clone --token` are sent as `Authorization: Bearer ...` in protocol requests. When `ASTVCS_SERVE_TOKEN` is set on the remote (or `--token` on `remote-serve`), the same auth rules as HTTP serve apply.

**remote-serve protocol.** Internal subcommand `astvcs remote-serve --repo <path>` reads one JSON request per line on stdin and writes one JSON response per line on stdout. Request: `{"method":"GET|HEAD|PUT","path":"/v1/...","body":"<base64 optional>","headers":{...}}`. Response: `{"status":200,"body":"<base64 optional>","error":"<text on failure>"}`. Paths and semantics match the HTTP `/v1/` API (config, refs, blobs, states, timeline). Shared dispatch lives in `network/api.rs` for HTTP serve and remote-serve.

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
    align.rs     hash-anchor sibling pairing helpers
    ast_diff.rs  structural diff; sibling alignment; `diff_graphs_detailed` / `AlignEdge`
    text_diff.rs Myers line diff
    path_rename.rs path-level rename detection
    view.rs      `DiffViewDocument` and self-contained HTML for `diff --view`
    view/viewer.html alignment-first viewer assets (inlined via `include_str!`)
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
    lock.rs      exclusive advisory repo lock (repo.lock); suspend/resume for hooks
    hooks.rs     client hook runner (pre-commit, commit-msg, pre-merge, pre-push)
    reachability.rs ref-tip reachability walk (shared by gc/fsck)
    rebase.rs    rebase state and linear replay
    cherry_pick.rs  single-commit replay onto HEAD
    blame.rs        line-based blame along linear history
    bisect.rs       linear bisect state and binary search
    git_import.rs   import-git snapshot migration via git subprocess
    walk.rs      gitignore-style working tree scan (full and incremental)
    scan_cache.rs scan-cache.json load/save and path stat helpers
    repo.rs      repository and CLI backend
  network/
    api.rs       shared /v1/ request dispatch (HTTP and remote-serve)
    transport.rs file, HTTP, and SSH remotes
    ssh.rs       SSH URL parsing and subprocess transport
    remote_serve.rs  newline JSON protocol for SSH
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

## Interoperability

`import-git` is a one-way migration aid: it reads a local git repository via the `git` CLI and imports the **HEAD tree snapshot** into an astvcs repository as a single commit. It uses `git rev-parse`, `git ls-tree -r HEAD`, and `git cat-file` subprocess calls only (no libgit2). UTF-8 text paths are written to the working tree and committed with normal `commit` semantics (author identity required). NUL-containing or invalid UTF-8 blobs are skipped with `warning:` on stderr. Symlinks (git mode `120000`) are imported when the target is valid UTF-8. Submodule entries (mode `160000`) are skipped with a warning. The astvcs tree is synced to the git snapshot: paths tracked at astvcs HEAD but absent from git HEAD are removed from disk before commit. If the target repository has no `.astvcs` directory, `import-git` runs `init` first.

**Non-goals (v1 and beyond for full git parity):**

- No git object hash compatibility (astvcs state ids remain manifest hashes).
- No native `.git` directory mode for astvcs.
- No bidirectional sync with git remotes or working trees.
- No full commit history import (snapshot only in v1).

## Testing

Unit tests live beside modules under `src/`. `tests/integration.rs` exercises the CLI and library together.

| Test | What it guards |
|------|----------------|
| `parse_all_supported_languages` | Every `supported_extensions()` entry and `supported_special_paths()` basename parses and validates |
| `edit_roundtrip_preserves_structure_across_languages` | Parse, trivial `EditPayload` diff, apply, unparse, re-parse: no structural drift; text matches edited source (Rust, Python, JS, JSON, TS, Go, HTML, CSS) |
| `rust_unparse_roundtrip_via_repo` | Commit and reload preserves Rust source bytes |
| `go_unparse_roundtrip_via_repo` | Commit, reload, and checkout preserve Go source bytes including block closing newlines |
| `same_file_demo_disjoint_merge` | Same-file rename + insert merge keeps formatting (stress test for alignment heuristics) |
| `cli_diff_view_writes_html_with_alignment` | `diff --view` writes temp HTML embedding path, intents, and alignment export |
| `identity_demo_payload_edit_disjoint_merge_and_conflict` | Sibling literal merge and rename conflicts |
| `trailing_comment_and_literal_edit_merge` | Trailing comment text survives merge when a sibling literal is edited on the other branch |
| `cli_trivia_only_commit` | Whitespace-only formatting commit round-trips through the CLI |
| `cli_branch_remove_guardrails` | Branch remove: checked-out, last branch, not found, recreate name |
| `cli_reset_hard_soft_and_force` | Hard/soft reset, drift repair, force clobber warnings |
| `reset_modes_soft_mixed_hard_comparison` | Soft, mixed, and hard reset with dirty tree and staging |
| `reset_mixed_unstages_and_keeps_disk` | Mixed reset clears staging and keeps disk |
| `partial_commit_only_stages_paths`, `status_shows_staged_and_unstaged_columns`, `cli_commit_empty_staging_errors` | Staging index: partial commit, two-column status, empty staging error |
| `serve_requires_token_for_mutations`, `serve_read_requires_token_by_default`, `serve_public_read_allows_anonymous_get`, `serve_put_returns_503_when_advisory_lock_held`, `serve_concurrent_reads_during_writes` | HTTP serve bearer auth, TLS config pairing, advisory lock 503, concurrent reads during writes (unit, `network/serve.rs`) |
| `parse_remote_url_accepts_https`, `insecure_client_accepts_self_signed_cert`, `http_transport_sends_bearer_token` | HTTPS remotes, `--insecure`, client bearer token (unit, `network/transport.rs`) |
| `parse_remote_url_accepts_ssh`, `ssh_session_sends_bearer_token`, `remote_serve_io_get_config_put_blob_head_404` | SSH remotes and remote-serve protocol (unit, `network/ssh.rs`, `network/remote_serve.rs`) |
| `repack_roundtrip_and_fsck`, `repack_fetch_push_roundtrip`, `gc_preserves_packed_blobs` | Blob repack and network round-trip after repack |
| `cli_revert_and_dry_run` | Revert conflicts, dry-run, and successful undo |
| `resolve_remote_ref_for_diff_merge_base_and_checkout` | `origin/main`-style ref resolution |
| `pull_merges_upstream_changes` | `pull` fetches and merges upstream commits |
| `pull_detached_head_requires_branch` | `pull` on detached HEAD requires `--branch` |
| `pull_merge_conflict_after_fetch` | Fetch succeeds; merge conflict leaves local branch unchanged |
| `stash_before_checkout` | `stash push` cleans tree so checkout succeeds without `--force` |
| `tag_create_and_list` | `tag create`, `tag list`, `tag remove` |
| `checkout_tag_detached` | `checkout --state <tagname>` detached at tagged state |
| `tag_fetch_push_between_repos` | Tags sync on fetch/push between file remotes |
| `shallow_clone_has_fewer_timeline_entries_than_full_clone` | `--depth` limits timeline entries vs full clone |
| `merge_base_fails_on_shallow_clone_with_incomplete_history` | Shallow tips block merge-base and merge |
| `stash_pop_restores_files` | `stash pop` restores stashed file content to disk |
| `stash_pop_preserves_unstashed_tracked_files` | `stash pop` leaves tracked files outside the stash manifest on disk |
| `stash_pop_conflict_keeps_entry` | Conflicting `stash pop` aborts and keeps the stash entry |
| `rebase_linear_success` | Feature branch commits replayed onto updated main |
| `rebase_conflict_abort_restores` | Replay conflict then `rebase --abort` restores tip and disk |
| `rebase_conflict_continue` | `--resolve` on `rebase --continue` finishes replay |
| `cherry_pick_clean_commit` | Cherry-pick feature commit onto diverged main |
| `cherry_pick_conflict_leaves_head_unchanged` | Conflicting cherry-pick aborts without side effects |
| `cherry_pick_from_remote_tracking_ref` | Cherry-pick `origin/feature` after fetch |
| `blame_linear_two_commits` | Line blame attributes edits to correct commits in linear history |
| `bisect_linear_four_commits` | Bisect finds first bad commit via script in linear history |
| `bisect_run_releases_lock_for_nested_astvcs` | Bisect script runs nested `astvcs status` without lock error |
| `merge_remote_tracking_ref` | `merge origin/main` after remote ref update (unit, `src/store/repo.rs`) |
| `cli_reports_repository_lock_contention` | Lock held externally: CLI fails fast with lock path |
| `concurrent_repo_lock_fails_fast_with_actionable_error` | Second writer gets lock error; succeeds after release (unit, `src/store/repo.rs`) |
| `suspend_and_resume_releases_for_subprocess` | Lock suspend allows nested subprocess acquire (unit, `src/store/lock.rs`) |
| `hook_pre_commit_aborts_commit` | `pre-commit` exit 1 aborts commit; refs unchanged |
| `hook_commit_msg_edits_message` | `commit-msg` edits message file; timeline shows edited message |
| `hook_nested_astvcs_status_in_pre_commit` | `pre-commit` runs nested `astvcs status`; commit succeeds |
| `hook_no_verify_skips_pre_commit` | Failing hook + `--no-verify` commits successfully |
| `hook_pre_merge_aborts` | `pre-merge` exit 1 aborts merge; refs unchanged |
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
| `import_git_snapshot_from_subprocess` | `import-git` reads local git HEAD via subprocess; one commit with import message |
| `parse_ls_tree_line_*` | `git ls-tree` line parsing (unit, `store/git_import.rs`) |

Run `cargo test`, then `cargo clippy --all-targets --all-features -- -D warnings`. Fixture walkthroughs in `examples/README.md` mirror several integration tests.
