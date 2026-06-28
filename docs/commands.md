# Commands

Global flags:

| Flag | Description |
|------|-------------|
| `--repo <path>` | Repository root (default: current directory) |
| `-v`, `--verbose` | Print operational `notice:` detail to stderr |

## Subcommands

| Command | Description |
|---------|-------------|
| `init [path]` | Create a new repository (default path: `.`) |
| `status` | Show changed files vs the checked-out state (clean tree: one summary line) |
| `diff [path]` | Diff working tree, or a single file |
| `diff --state <ref>` | Diff current HEAD against a branch, remote-tracking ref, or state id |
| `diff --base <ref> --left <ref> --right <ref> [path]` | Three-way diff from merge base |
| `record -m <msg>` | Record working tree as a new state (prints when unchanged) |
| `branch list` | List branches |
| `branch create <name> [--from <branch>]` | Create a branch |
| `merge-base <left> <right>` | Print lowest common ancestor (branch, remote-tracking ref, or state id) |
| `merge <branch> -m <msg>` | Merge a branch; updates working tree on success |
| `merge <branch> -m <msg> --resolve <path>:ours` | Merge with per-path conflict resolution (repeatable) |
| `merge <branch> --dry-run` | Simulate merge; print conflicts without changing the repository |
| `checkout --branch <name>` | Switch branch and materialize its HEAD to disk |
| `checkout --state <ref>` | Detached checkout: materialize a state and move HEAD to it |
| `reset <ref> [--soft] [--force]` | Move HEAD or the current branch tip to `<ref>` (default: hard, syncs disk) |
| `revert <ref> -m <msg> [--dry-run]` | Create a new state that undoes `<ref>` on top of HEAD |
| `log [-n N]` | Walk timeline history (default 20 entries) |
| `remote add <name> <url>` | Register a remote (local path, `file://`, or `http://`) |
| `remote list` | List configured remotes |
| `remote remove <name>` | Remove a remote and its tracking refs |
| `fetch <remote> [--branch <name>]` | Download missing objects; update remote-tracking refs |
| `push <remote> [--branch <name>] [--force]` | Upload missing objects; fast-forward remote branch |
| `clone <url> [path]` | Clone a remote repository (default path: `.`) |
| `serve [--bind <addr>] [--port <n>]` | Serve the repository over HTTP (default `127.0.0.1:9421`) |

Refs accepted by `diff`, `merge-base`, `checkout --state`, `reset`, and `revert` include local branch names, remote-tracking refs (`<remote>/<branch>`), and 64-character state ids. Resolution order: state id, then `refs/heads/<name>`, then `refs/remotes/<remote>/<branch>` when that file exists (a local branch literally named `origin/main` wins via the heads check).

### `reset`

Default mode is **hard**: move the branch tip or detached HEAD to the target and call `materialize_state` (sync working tree and `index.json`). This differs from git's default (`--mixed`): astvcs has no staging index, so the meaningful modes are **hard** (move pointer and sync disk) and **soft** (move pointer only).

| Flag | Behavior |
|------|----------|
| (none) | Hard reset: refuse when the working tree has unrecorded changes |
| `--soft` | Move the ref only; disk and `index.json` stay as-is (`status` shows diffs against the new HEAD) |
| `--force` | With hard reset, proceed when the working tree is dirty; emit `warning: reset --force: discarded unrecorded changes in <path>` per clobbered path |

Hard reset to the current tip still materializes (repairs drift between disk and HEAD). Resetting to the root empty state (`0` repeated 64 times) is allowed.

### `revert`

Creates a **new** forward state that undoes the target state's changes on top of current HEAD using the same per-path three-way machinery as merge (`base` = target, `left` = target's parent, `right` = HEAD).

Preconditions (error before any write):

- Target exists and has exactly one parent (merge states are rejected)
- Target is an ancestor of HEAD (reverting HEAD tip is allowed)
- Target is not the root empty state

If the reverted manifest is identical to HEAD, revert is a true no-op (same stdout as `record` with no changes: no new timeline entry, refs unchanged). When the reverted tree matches the target's parent manifest, the branch tip moves to that parent state id instead of writing a duplicate content-addressed state.

Paths added in the target state and modified again on HEAD before revert produce a conflict (`path modified after the reverted state`) rather than silently keeping HEAD's newer content.

`--dry-run` plans in memory only; conflicts print the report and exit non-zero without writes (same contract as `merge --dry-run`).

### Network sync

`fetch` updates `.astvcs/refs/remotes/<remote>/<branch>` only. To work on fetched commits, use `reset`, `checkout --state`, or `merge` with the remote-tracking ref (for example `origin/main`).

`push` requires a fast-forward unless `--force` is passed. Detached HEAD requires `--branch` to name the branch being pushed.

Remote URLs may be a local repository path, a `file://` URL, or an `http://` base URL from `astvcs serve`.

### Merge conflict resolution

When a merge would conflict, pass `--resolve <path>:ours` or `--resolve <path>:theirs` for each conflicted path (repository-relative, matching manifest keys). `ours` is the current branch (HEAD); `theirs` is the branch being merged in. Non-conflicted paths keep the structurally merged result from the planner.

- Unresolved conflicts abort the merge with no ref, working tree, or `index.json` changes.
- `--resolve` for a path not in the conflict list errors before any write.
- Duplicate `--resolve` for the same path errors.
- Invalid sides (not `ours` or `theirs`) error at parse time.

With `--dry-run`, resolutions are applied in memory only: a fully resolved plan prints the usual success summary; any remaining conflict prints the conflict report and exits non-zero without writes.

## Stderr output

By default, stderr shows only `warning:` lines (unexpected parse fallback, skipped paths, merge conflicts, reset force clobbers, index inconsistencies). Known text-only paths such as `.gitignore`, `.md`, `.txt`, `go.sum`, and `.ps1` do not warn; they store as text blobs without stderr output. With `-v`, `notice:` lines are included: scan results, parse mode per file, text-blob storage, blob writes, materialize actions, merge planning, reset/revert planning, and no-op records. Primary command output stays on stdout.

## Ignore rules

Put patterns in `.gitignore` (standard git syntax) or `.astvcsignore` for astvcs-only rules. Build output, dependencies, and binaries are the project's responsibility to list there.
