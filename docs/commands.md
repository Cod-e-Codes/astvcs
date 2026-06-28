# Commands

Global flags:

| Flag | Description |
|------|-------------|
| `--repo <path>` | Repository root (default: current directory) |
| `-v`, `--verbose` | Print operational `notice:` detail to stderr |
| `--json` | On failure, print a structured JSON error object on stderr instead of `error: …` |

## Subcommands

| Command | Description |
|---------|-------------|
| `init [path]` | Create a new repository (default path: `.`) |
| `identity get [--global]` | Show configured author name and email (repository or global config) |
| `identity set --name <name> --email <email> [--global]` | Set author identity for future commits, merges, and reverts |
| `status` | Show changed files vs the checked-out state (clean tree: one summary line). Renames show as `R old -> new`. |
| `diff [path]` | Diff working tree, or a single file. Path renames print `(rename)` or `(rename with edits)` with a `RenamePath` intent. |
| `diff --state <ref>` | Diff current HEAD against a branch, remote-tracking ref, or state id |
| `diff --base <ref> --left <ref> --right <ref> [path]` | Three-way diff from merge base |
| `commit -m <msg>` | Commit working tree as a new state (prints when unchanged). Requires configured author identity. |
| `branch list` | List branches |
| `branch create <name> [--from <branch>]` | Create a branch |
| `branch remove <name>` | Remove a branch ref (see guardrails below) |
| `merge-base <left> <right>` | Print lowest common ancestor (branch, remote-tracking ref, or state id) |
| `merge <branch> -m <msg>` | Merge a branch; updates working tree on success |
| `merge <branch> -m <msg> --force` | Merge when the working tree has uncommitted changes (warns per clobbered path) |
| `merge <branch> -m <msg> --resolve <path>:ours` | Merge with per-path conflict resolution (repeatable) |
| `merge <branch> --dry-run` | Simulate merge; print conflicts without changing the repository |
| `checkout --branch <name>` | Switch branch and materialize its HEAD to disk |
| `checkout --branch <name> --force` | Switch branch when the working tree is dirty (warns per clobbered path) |
| `checkout --state <ref>` | Detached checkout: materialize a state and move HEAD to it |
| `checkout --state <ref> --force` | Detached checkout when the working tree is dirty (warns per clobbered path) |
| `reset <ref> [--soft] [--force]` | Move HEAD or the current branch tip to `<ref>` (default: hard, syncs disk) |
| `revert <ref> -m <msg> [--dry-run]` | Create a new state that undoes `<ref>` on top of HEAD |
| `revert <ref> -m <msg> --force` | Revert when the working tree is dirty (warns per clobbered path) |
| `log [-n N]` | Walk timeline history (default 20 entries); shows author when present |
| `remote add <name> <url>` | Register a remote (local path, `file://`, or `http://`) |
| `remote list` | List configured remotes |
| `remote remove <name>` | Remove a remote and its tracking refs |
| `fetch <remote> [--branch <name>]` | Download missing objects; update remote-tracking refs |
| `push <remote> [--branch <name>] [--force]` | Upload missing objects; fast-forward remote branch |
| `clone <url> [path]` | Clone a remote repository (default path: `.`) |
| `serve [--bind <addr>] [--port <n>]` | Serve the repository over HTTP (default `127.0.0.1:9421`) |
| `gc [--prune]` | Report unreachable blobs (default dry-run); `--prune` deletes them |
| `fsck` | Check repository integrity; report-only, exits non-zero when issues are found |

Refs accepted by `diff`, `merge-base`, `checkout --state`, `reset`, and `revert` include local branch names, remote-tracking refs (`<remote>/<branch>`), and 64-character state ids. Resolution order: state id, then `refs/heads/<name>`, then `refs/remotes/<remote>/<branch>` when that file exists (a local branch literally named `origin/main` wins via the heads check).

### `branch remove`

Deletes `.astvcs/refs/heads/<name>` only. Timeline entries and blobs remain until `gc --prune` removes blobs unreachable from any ref tip; states remain reachable by id via the timeline.

| Guardrail | Behavior |
|-----------|----------|
| Checked-out branch | Error: `cannot remove the checked-out branch` |
| Last remaining branch | Error: `cannot remove the last branch` |
| Unmerged commits on the branch | Allowed. Removing a ref does not delete content-addressed states; history stays in the store and can still be checked out by state id. |
| `config.json` `default_branch` | Unchanged when a branch is removed (the field is informational only today). |

### `reset`

Default mode is **hard**: move the branch tip or detached HEAD to the target and materialize the state to disk (sync working tree and `index.json`). This differs from git's default (`--mixed`): astvcs has no staging index, so the meaningful modes are **hard** (move pointer and sync disk) and **soft** (move pointer only).

| Flag | Behavior |
|------|----------|
| (none) | Hard reset: refuse when the working tree has uncommitted changes |
| `--soft` | Move the ref only; disk and `index.json` stay as-is (`status` shows diffs against the new HEAD) |
| `--force` | With hard reset, proceed when the working tree is dirty; emit `warning: reset --force: discarded uncommitted changes in <path>` per clobbered path |

Hard reset to the current tip still materializes (repairs drift between disk and HEAD). Resetting to the root empty state (`0` repeated 64 times) is allowed.

### Working tree safety (`merge`, `checkout`, `revert`, `reset`)

`merge`, `checkout --branch`, `checkout --state`, and hard `reset` all materialize a state manifest to disk and sync `index.json`. They share one dirty-tree policy enforced by a shared **materialize guard** (checked before refs, timeline writes, and disk sync):

| Default | `--force` |
|---------|-----------|
| Refuse when `status` reports any path other than unchanged | Proceed; emit `warning: <command> --force: discarded uncommitted changes in <path>` for each clobbered path |

`reset --soft` skips materialization entirely, so it never clobbers uncommitted work (same as before). Hard reset to the current tip, and checkout of the branch or state already at HEAD, may materialize without `--force` to repair drift between disk and HEAD without moving to a different snapshot.

`checkout --branch` and `checkout --state` use the same contract. Unlike git, astvcs always materializes on checkout; switching branches or detached states is not a pointer-only operation. A dirty tree therefore blocks both forms unless `--force` is passed.

`revert` applies the guard only when it would materialize (no-op reverts that leave HEAD unchanged skip the check). `merge --dry-run` and `revert --dry-run` never touch the working tree.

**Merge planning and the working tree.** `plan_merge` and `prepare_merge` load file content only from committed states (merge base, HEAD, and the branch tip being merged). They do not read the working tree, so uncommitted edits are invisible to conflict detection and to the merged manifest. With `--force`, dirty paths are discarded during materialization *after* the plan is computed; uncommitted content on a path that the merge itself changes cannot leak into the planner or alter the three-way result—the final on-disk file is the committed merge outcome for that path.

### `revert`

Creates a **new** forward state that undoes the target state's changes on top of current HEAD using the same per-path three-way machinery as merge (`base` = target, `left` = target's parent, `right` = HEAD).

Preconditions (error before any write):

- Target exists and has exactly one parent (merge states are rejected)
- Target is an ancestor of HEAD (reverting HEAD tip is allowed)
- Target is not the root empty state

If the reverted manifest is identical to HEAD, revert is a true no-op (same stdout as `commit` with no changes: no new timeline entry, refs unchanged). When the reverted tree matches the target's parent manifest, the branch tip moves to that parent state id instead of writing a duplicate content-addressed state.

Paths added in the target state and modified again on HEAD before revert produce a conflict (`path modified after the reverted state`) rather than silently keeping HEAD's newer content.

`--dry-run` plans in memory only; conflicts print the report and exit non-zero without writes (same contract as `merge --dry-run`).

### Network sync

`fetch` updates `.astvcs/refs/remotes/<remote>/<branch>` only. To work on fetched commits, use `reset`, `checkout --state`, or `merge` with the remote-tracking ref (for example `origin/main`).

`push` requires a fast-forward unless `--force` is passed. Detached HEAD requires `--branch` to name the branch being pushed.

Remote URLs may be a local repository path, a `file://` URL, or an `http://` base URL from `astvcs serve`.

### `gc`

Walks reachability from every local branch tip, remote-tracking ref tip, and detached HEAD, then reports blob store objects not referenced by any reachable state manifest. Timeline entries and state manifests are never deleted.

| Flag | Behavior |
|------|----------|
| (none) | Dry-run: print how many blobs are unreachable and how much space `--prune` would reclaim |
| `--prune` | Delete unreachable blob files and print how many were removed |

Example output (dry-run):

```text
gc dry-run: 2 unreachable blob(s) (examined 5); would reclaim 1.2 KiB
```

After `--prune` with nothing to do:

```text
gc: examined 3 blob(s); nothing to prune
```

### `fsck`

Read-only integrity check. Never modifies refs, HEAD, timeline, blobs, `index.json`, or the working tree. Exits with code 1 when any issue is found.

| Check | Report label |
|-------|----------------|
| State manifest references a missing blob | `missing blob` |
| Branch or remote-tracking ref points to a state with no timeline entry | `dangling ref` |
| `HEAD` names a branch with no `refs/heads/` file | `HEAD branch missing` |
| `index.json` `state_id` or paths disagree with HEAD, or index present while HEAD is invalid | `index inconsistent` |
| `.astvcs-tmp` file with no canonical target (not cleaned by normal commands) | `orphan temp file` |

Clean repository:

```text
fsck: repository ok
```

With findings:

```text
fsck: 2 issue(s) found
  dangling ref: refs/heads/feature points to abc… with no timeline entry
  missing blob: state def… path main.rs: blob 789… missing
```

There is no `fsck --repair`. Fix ref or HEAD problems manually; use `gc --prune` for unreachable blobs after refs reflect the history you want to keep.

### Merge conflict resolution

When a merge would conflict, pass `--resolve <path>:ours` or `--resolve <path>:theirs` for each conflicted path (repository-relative, matching manifest keys). `ours` is the current branch (HEAD); `theirs` is the branch being merged in. Non-conflicted paths keep the structurally merged result from the planner.

- Unresolved conflicts abort the merge with no ref, working tree, or `index.json` changes.
- `--resolve` for a path not in the conflict list errors before any write.
- Duplicate `--resolve` for the same path errors.
- Invalid sides (not `ours` or `theirs`) error at parse time.

With `--dry-run`, resolutions are applied in memory only: a fully resolved plan prints the usual success summary; any remaining conflict prints the conflict report and exits non-zero without writes.

## Stderr output

By default, stderr shows only `warning:` lines (unexpected parse fallback, skipped paths, merge conflicts, materialize `--force` clobbers, index inconsistencies). Known text-only paths such as `.gitignore`, `.md`, `.txt`, `go.sum`, and `.ps1` do not warn; they store as text blobs without stderr output. With `-v`, `notice:` lines are included: scan results, parse mode per file, text-blob storage, blob writes, materialize actions, merge planning, reset/revert planning, and no-op commits. Primary command output stays on stdout.

When another process holds `.astvcs/repo.lock`, any command that needs the repository fails immediately on stderr with:

```text
error: repository is locked by another process; cannot acquire <absolute-or-relative-path>/.astvcs/repo.lock
```

There is no wait/retry; run the command again after the other process finishes.

With `--json`, failures print a single JSON object on stderr, for example:

```json
{"kind":"missing_identity","message":"author identity not configured; run `astvcs identity set --name <name> --email <email>` or set ASTVCS_AUTHOR_NAME and ASTVCS_AUTHOR_EMAIL"}
```

The `message` field matches the text shown after `error:` when `--json` is omitted.

## Ignore rules

Put patterns in `.gitignore` (standard git syntax) or `.astvcsignore` for astvcs-only rules. Build output, dependencies, and binaries are the project's responsibility to list there.
