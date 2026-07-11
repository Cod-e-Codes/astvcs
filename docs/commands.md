# Commands

Complete CLI reference for `astvcs`. For a project overview, see [README.md](../README.md). For repository internals (states, merge planning, locking, network transport), see [architecture.md](architecture.md). Runnable walkthroughs: [examples/README.md](../examples/README.md).

**Command groups:** `init`, `identity` · `status`, `add`, `diff`, `commit` · `branch`, `tag` · `merge`, `merge-base`, `checkout` · `reset`, `revert`, `rebase`, `cherry-pick`, `stash` · `log`, `blame`, `bisect` · `remote`, `fetch`, `pull`, `push`, `clone`, `serve`, `remote-serve` · `gc`, `fsck`, `repack` · `import-git`

Detailed behavior for reset modes, hooks, network sync, stash, rebase, and related topics is in the sections below the subcommand table.

## Global flags

| Flag | Description |
|------|-------------|
| `--version` | Print crate version and exit |
| `--repo <path>` | Repository root (default: current directory) |
| `-v`, `--verbose` | Print operational `notice:` detail to stderr (also forces a full working-tree scan) |
| `--details` | Show state IDs, node IDs, raw mutations, and complete conflict diagnostics without enabling operational notices |
| `--json` | On failure, print a structured JSON error object on stderr instead of `error: …` |

## Subcommands

| Command | Description |
|---------|-------------|
| `init [path]` | Create a new repository (default path: `.`) |
| `identity get [--global]` | Show configured author name and email (repository or global config) |
| `identity set --name <name> --email <email> [--global]` | Set author identity for future commits, merges, and reverts |
| `status [--full-scan]` | Git-style two-column status: staged vs HEAD (`M `, `A `, `D `, `R `) and unstaged vs effective index (` M`, ` A`, ` D`, `??`). Combined `MM` when both. Clean tree: `nothing to commit, working tree clean`. Renames show as `R old -> new`. AST-capable paths stored as text blobs show ` (text fallback)` on the path line. Incremental scan is used by default; pass `--full-scan` to walk every directory. |
| `add [-u\|--update] [-A\|--all] <paths...>` | Stage paths for commit. `-u`: tracked modifications and deletions only. `-A`: all changes including untracked. Directory paths recurse. |
| `diff [path]` | Unstaged diff (working tree vs staging overlay or HEAD). Every semantic edit is shown with compact intent labels; repeated formatting-only intents are aggregated. Path renames print `(rename)` or `(rename with edits)`. Binary paths show `(binary file - content diff omitted)`. AST-capable text fallback paths show `(text fallback - structural diff unavailable)` and a `parse mode:` intent when storage kind differs. |
| `diff --staged` / `--cached [path]` | Staged diff vs HEAD |
| `diff --state <ref>` | Diff current HEAD against a branch, remote-tracking ref, or state id |
| `diff --base <ref> --left <ref> --right <ref> [path]` | Three-way diff from merge base |
| `diff --view [...]` | Same comparison modes as text `diff` (`[path]`, `--staged`, `--state`, or `--base`/`--left`/`--right`). Writes a self-contained HTML file under the system temp directory, opens it in the default browser (skipped when `CI` or `ASTVCS_NO_BROWSER` is set), and prints the file path. The viewer opens on a change summary, expands changed context, and lazily reveals count-labelled unchanged branches. Use `n`/`p` for changes and `j`/`k` for files. Alignment, node IDs, mutations, and pipeline data stay in collapsed details. |
| `commit -m <msg> [--full-scan] [--no-verify]` | Commit staged changes when staging is active (after first `add`); otherwise legacy whole-tree commit. Errors when staging is active but empty while the working tree has changes. Requires configured author identity. Pass `--full-scan` to bypass the scan cache. Runs `pre-commit` and `commit-msg` hooks unless `--no-verify`. |
| `branch list` | List branches |
| `branch create <name> [--from <branch>]` | Create a branch |
| `branch remove <name>` | Remove a branch ref (see guardrails below) |
| `tag create <name> <ref>` | Create a lightweight tag pointing at a resolved ref |
| `tag list` | List tags with state ids (sorted by name) |
| `tag remove <name>` | Delete a tag ref |
| `merge-base <left> <right>` | Print lowest common ancestor (branch, tag, remote-tracking ref, or state id) |
| `merge <branch> -m <msg> [--no-verify]` | Merge a branch; updates working tree on success |
| `merge <branch> -m <msg> --force` | Merge when the working tree has uncommitted changes (warns per clobbered path) |
| `merge <branch> -m <msg> --no-verify` | Merge without running `pre-merge` |
| `merge <branch> -m <msg> --resolve <path>:ours` | Merge with per-path conflict resolution (repeatable) |
| `merge <branch> --dry-run` | Simulate merge; print conflicts without changing the repository |
| `checkout --branch <name>` | Switch branch and materialize its HEAD to disk |
| `checkout --branch <name> --force` | Switch branch when the working tree is dirty (warns per clobbered path) |
| `checkout --state <ref>` | Detached checkout: materialize a state and move HEAD to it |
| `checkout --state <ref> --force` | Detached checkout when the working tree is dirty (warns per clobbered path) |
| `reset <ref> [--soft] [--mixed] [--force]` | Move HEAD or the current branch tip to `<ref>` (default: hard, syncs disk) |
| `revert <ref> -m <msg> [--dry-run]` | Create a new state that undoes `<ref>` on top of HEAD |
| `revert <ref> -m <msg> --force` | Revert when the working tree is dirty (warns per clobbered path) |
| `rebase <upstream> [--force]` | Replay linear commits from the current branch onto `<upstream>` |
| `rebase --abort` | Restore the branch tip and working tree from before the rebase; delete rebase state |
| `rebase --continue [--force] [--resolve <path>:ours\|theirs]` | After resolving a replay conflict, finish the current commit and continue the queue |
| `cherry-pick <ref> -m <msg>` | Apply a single state's changes onto HEAD as a new commit |
| `cherry-pick <ref> -m <msg> --force` | Cherry-pick when the working tree is dirty (warns per clobbered path) |
| `log [-n N]` | Walk timeline history (default 20 entries); shows author when present |
| `blame <path>` | Annotate each line with the commit that last changed it (linear first-parent history) |
| `bisect start [bad] good` | Start binary search between a known good and bad revision (default bad: HEAD) |
| `bisect bad [ref]` | Mark a revision as bad during bisect (default: HEAD) |
| `bisect good [ref]` | Mark a revision as good during bisect (default: HEAD) |
| `bisect run <script> [args...]` | Checkout midpoint revisions and run a test script (exit 0=good, 1=bad, 125=skip) |
| `bisect reset` | End bisect and restore the original checkout |
| `remote add <name> <url> [--token <token>]` | Register a remote (local path, `file://`, `http://`, `https://`, or SSH); optional bearer token for HTTP/SSH remotes |
| `remote list` | List configured remotes |
| `remote remove <name>` | Remove a remote and its tracking refs |
| `fetch <remote> [--branch <name>] [--depth <N>] [--insecure]` | Download missing objects; update remote-tracking refs and all remote tags |
| `pull <remote> [--branch <name>] [--depth <N>] [-m <msg>] [--force] [--no-verify] [--insecure] [--resolve <path>:ours\|theirs]` | Fetch then merge remote-tracking branch into current branch |
| `stash push [-m <msg>] [-u]` | Save working-tree changes to `.astvcs/stash/` and reset disk to HEAD |
| `stash list` | List stashes (`stash@{n}`; 0 is newest) |
| `stash pop [index]` | Apply stash (default `0`) and remove entry on success |
| `stash apply [index]` | Apply stash without removing the entry |
| `push <remote> [--branch <name>] [--force] [--no-verify] [--insecure]` | Upload missing objects; fast-forward remote branch; upload local tags missing on remote |
| `clone <url> [path] [--token <token>] [--depth <N>] [--insecure]` | Clone a remote repository (default path: `.`); HTTP token stored in `origin` remote config |
| `serve [--bind <addr>] [--port <n>] [--token <token>] [--public-read] [--tls-cert <path>] [--tls-key <path>]` | Serve the repository over HTTP or HTTPS (default `127.0.0.1:9421`); token from `--token` or `ASTVCS_SERVE_TOKEN` |
| `remote-serve --repo <path> [--token <token>] [--public-read]` | Internal operator command: newline-delimited JSON protocol on stdin/stdout (used by SSH remotes; same `/v1/` API as HTTP serve) |
| `gc [--prune] [--prune-history]` | Report unreachable blobs and history (default dry-run); `--prune` deletes blobs; `--prune-history` deletes unreachable states |
| `repack` | Pack loose blobs into compressed pack files; remove loose copies |
| `fsck` | Check repository integrity; report-only by default, exits non-zero when issues are found; optional `--repair` and `--prune-refs` |
| `import-git <git-path> [-m <msg>]` | Import git HEAD tree snapshot into the astvcs repo (one commit); auto-`init` if `.astvcs` is missing; requires author identity; commits only git HEAD paths (ignores unrelated files on disk); skips binary blobs and submodules with `warning:` |

Refs accepted by `diff`, `merge-base`, `checkout --state`, `reset`, `revert`, `rebase`, `cherry-pick`, `bisect`, and `merge` include local branch names, lightweight tags, remote-tracking refs (`<remote>/<branch>`), and 64-character commit ids from `log`. Branch and tag refs store commit ids; manifest content is deduplicated separately in `states/`. Resolution order: commit id, then `refs/heads/<name>`, then `refs/tags/<name>`, then `refs/remotes/<remote>/<branch>` when that file exists (a local branch literally named `origin/main` wins via the heads check).

### Lightweight tags

Tags are named refs under `.astvcs/refs/tags/<name>` containing a single state id (same format as branch refs). v1 supports lightweight tags only: no annotated tag message, author, or separate tag object. Tag names cannot be empty, contain `/`, or contain `..`. `tag create` resolves `<ref>` like other commands. Tags participate in reachability for `gc` and are synced on every `fetch`, `push`, and `clone` (all remote tags are fetched even when `fetch --branch` limits branch refs). Remote tag updates are not fast-forward checked; `set_tag` overwrites like git tags.

### `branch remove`

Deletes `.astvcs/refs/heads/<name>` only. Timeline entries and blobs remain until `gc --prune` removes blobs unreachable from any ref tip. Unreachable states remain until `gc --prune-history`; until then they stay checkoutable by state id.

| Guardrail | Behavior |
|-----------|----------|
| Checked-out branch | Error: `cannot remove the checked-out branch` |
| Last remaining branch | Error: `cannot remove the last branch` |
| Unmerged commits on the branch | Allowed. Removing a ref does not delete content-addressed states; history stays in the store and can still be checked out by state id. |
| `config.json` `default_branch` | Updated when the default branch ref is removed (promote `main` if present, else lexicographically first remaining branch) or when `branch create` runs while the configured default ref is missing (set to the new branch). Used by `clone` and push default branch resolution. |

### `reset`

Default mode is **hard**: move the branch tip or detached HEAD to the target and materialize the state to disk (sync working tree and `index.json`, clear staging). **Mixed** (`--mixed`) moves the ref and syncs `index.json` to the target manifest while clearing staging and leaving the working tree unchanged (git-like default). **Soft** (`--soft`) moves the ref only; disk, `index.json`, and staging stay as-is.

| astvcs | git equivalent | Notes |
|--------|----------------|-------|
| `reset <ref>` (no mode flag) | `git reset --hard <ref>` | astvcs defaults to hard; git defaults to mixed |
| `reset --mixed <ref>` | `git reset --mixed <ref>` | git's default mode when no `--soft`/`--hard` |
| `reset --soft <ref>` | `git reset --soft <ref>` | ref only; index and working tree unchanged |

| Flag | Behavior |
|------|----------|
| (none) | Hard reset: refuse when the working tree is dirty (unstaged changes or non-empty staging) |
| `--mixed` | Move ref, sync `index.json` to target, clear staging; disk unchanged |
| `--soft` | Move the ref only; disk, index, and staging unchanged |
| `--force` | With hard reset, proceed when the working tree is dirty; emit `warning: reset --force: discarded uncommitted changes in <path>` per clobbered path |

Hard reset to the current tip still materializes (repairs drift between disk and HEAD). Resetting to the root empty state (`0` repeated 64 times) is allowed.

### Working tree safety (`merge`, `checkout`, `revert`, `reset`, `cherry-pick`)

`merge`, `checkout --branch`, `checkout --state`, and hard `reset` all materialize a state manifest to disk and sync `index.json`. They share one dirty-tree policy enforced by a shared **materialize guard** (checked before refs, timeline writes, and disk sync):

| Default | `--force` |
|---------|-----------|
| Refuse when `status` reports unstaged changes or staging is non-empty | Proceed; emit `warning: <command> --force: discarded uncommitted changes in <path>` for each clobbered path |

`reset --soft` skips materialization entirely, so it never clobbers uncommitted work (same as before). Hard reset to the current tip, and checkout of the branch or state already at HEAD, may materialize without `--force` to repair drift between disk and HEAD without moving to a different snapshot.

`checkout --branch` and `checkout --state` use the same contract. Unlike git, astvcs always materializes on checkout; switching branches or detached states is not a pointer-only operation. A dirty tree therefore blocks both forms unless `--force` is passed.

`revert` applies the guard only when it would materialize (no-op reverts that leave HEAD unchanged skip the check). `merge --dry-run` and `revert --dry-run` never touch the working tree.

**Merge planning and the working tree.** `plan_merge` and `prepare_merge` load file content only from committed states (merge base, HEAD, and the branch tip being merged). They do not read the working tree, so uncommitted edits are invisible to conflict detection and to the merged manifest. With `--force`, dirty paths are discarded during materialization *after* the plan is computed; uncommitted content on a path that the merge itself changes cannot leak into the planner or alter the three-way result - the final on-disk file is the committed merge outcome for that path.

### `stash`

`stash push` captures the working-tree diff against HEAD (disk is source of truth; staged content already on disk is included). By default only tracked paths (HEAD manifest entries and tracked deletions) are stashed; pass `-u` / `--include-untracked` to include untracked files from the working-tree scan. Errors with `no local changes to stash` when nothing differs. After saving, astvcs materializes HEAD to disk (clears staging) so the tree is clean for checkout.

`stash apply` and `stash pop` three-way merge only paths listed in the stash manifest (`base` = stash `base_state_id`, `left` = current HEAD, `right` = stashed manifest) and write results to the working tree only (`index.json` stays at HEAD). Other tracked files are left unchanged. Refuses when the working tree is dirty (same message as merge). On any path conflict, aborts with `merge would conflict`, labels current HEAD and the stashed change, omits unsupported `--resolve` guidance, and leaves the working tree and stash unchanged. `pop` removes the entry only on full success.

Default push message: `WIP on <branch>: <head-short>` (first 8 hex chars of HEAD state id).

### `rebase`

`rebase <upstream>` replays the linear commits on the checked-out branch (from the merge base with `<upstream>` up to the branch tip) onto the upstream tip. Requires a checked-out branch (not detached HEAD). Refuses when staging is non-empty or the working tree is dirty unless `--force`. Refuses when a rebase is already in progress. Errors with `already up to date` when there is nothing to replay.

Progress is stored in `.astvcs/rebase-state.json` (`branch`, `upstream`, `onto`, `original_tip`, `current_head`, `remaining`, `conflicted`). Each commit is replayed with a three-way merge (`base` = original parent, `left` = current replay head, `right` = commit being replayed). Replayed states keep the original commit message and author metadata.

On replay conflict, the branch tip stays at `original_tip` until the first successful replay; after partial progress the tip is the last good `current_head`. The conflicted merge result is materialized to the working tree (no conflict markers). Focused stderr says `rebase would conflict` and shows `rebase --continue --resolve path:ours|theirs` (ours = current replay head, theirs = replayed commit). You can also edit conflicted paths on disk and continue. `rebase --abort` restores `original_tip` and materializes it.

v1 does not include an interactive rebase editor or commit reordering.

### `cherry-pick`

`cherry-pick <ref> -m <msg>` applies the changes introduced by a single state onto HEAD as a new commit. Resolves `<ref>` like other commands (branch, tag, remote-tracking ref, or state id). Refuses merge commits and the root empty state. Refuses when staging is non-empty or the working tree is dirty unless `--force`. On conflict, labels current HEAD and the picked state, omits unsupported `--resolve` guidance, and aborts with no side effects (HEAD, refs, disk, and timeline unchanged). On success, writes a new state with the user message and current author identity, materializes it, and updates the branch tip or detached HEAD.

Three-way roles match rebase replay geometry (`base` = parent of the cherry-picked commit, `left` = HEAD, `right` = cherry-picked commit). Unlike `revert` (which inverts roles: `base` = target, `left` = parent, `right` = HEAD), cherry-pick applies **right vs base** onto **left**.

### `blame`

`blame <path>` prints one annotation block per source line (git-blame style):

```
<short_state_id> (<author_name> <author_email> <timestamp>) <message>
<line content>
```

The short state id is the first 8 characters of the 64-character state id. Blame walks linear first-parent history from HEAD, comparing parent vs child file content at each step with a line-oriented diff. Lines introduced or modified in a commit are attributed to that commit; unchanged lines are traced further back. AST files are unparsed to text for line-based blame. Binary files and symlinks error with `blame does not support binary files` or `blame does not support symlinks`. Merge commits in the ancestry block further walk with an error.

### `bisect`

`bisect` finds the first bad state between a known good and bad revision using binary search on the linear first-parent chain. Progress is stored in `.astvcs/bisect-state.json` (`original_head`, `original_branch`, `good`, `bad`, `skipped`, `candidates`, `low`, `high`).

`bisect start [bad] good` saves the current checkout, resolves refs, and builds the candidate list from the first commit after `good` through `bad` (oldest first). Default `bad` is HEAD; `good` is required (pass a single revision as `good` when `bad` is HEAD). Refuses when bisect is already in progress. Errors when `good` is not a linear first-parent ancestor of `bad` or when a merge commit lies on the path.

`bisect bad [ref]` and `bisect good [ref]` update boundaries and recompute candidates (default `ref` is HEAD).

`bisect run <script> [args...]` checks out midpoint candidates (materializes with `--force` semantics for dirty trees), runs the script with cwd = repository root, and narrows the search from the exit code: **0** = good, **1** = bad, **125** = skip (git convention). Prints `Bisecting: N revisions left...` during the run and `first bad state: <id> (<message>)` when done. Sets `ASTVCS_BISECT_STATE` to the checked-out state id. The repository lock is suspended while the script runs so nested `astvcs` invocations do not deadlock (same mechanism as client hooks).

`bisect reset` deletes bisect state and restores `original_branch` or detached `original_head`.

v1 supports linear first-parent history only; DAG or merge-heavy histories are out of scope.

### Client hooks

Optional scripts in `.astvcs/hooks/` run as child processes. `init` creates an empty `hooks/` directory. Missing hooks are skipped; non-zero exit aborts the operation with `hook <name> failed with exit code N`.

| Hook | When | Env vars |
|------|------|----------|
| `pre-commit` | Before commit persist (when changes exist) | `ASTVCS_ROOT`, `ASTVCS_BRANCH`, `ASTVCS_HEAD_STATE_ID` |
| `commit-msg` | After `pre-commit`, before persist | above + `ASTVCS_COMMIT_MSG_FILE` (`.astvcs/hooks/commit-msg-input`) |
| `pre-merge` | After clean merge plan, before writes | above + `ASTVCS_MERGE_BRANCH` |
| `pre-push` | Before upload (when push would send objects) | above + `ASTVCS_REMOTE` |

Pass `--no-verify` on `commit`, `merge`, `pull`, or `push` to skip hooks. Hooks and `bisect run` release the repository lock while running subprocesses so nested `astvcs` calls succeed. On Windows use `.cmd`/`.bat` (via `cmd /C`) or `.ps1` (via `powershell -NoProfile -File`); on Unix use an executable script or rely on `sh hookpath`.

### `revert`

Creates a **new** forward state that undoes the target state's changes on top of current HEAD using the same per-path three-way machinery as merge (`base` = target, `left` = target's parent, `right` = HEAD).

Preconditions (error before any write):

- Target exists and has exactly one parent (merge states are rejected)
- Target is an ancestor of HEAD (reverting HEAD tip is allowed)
- Target is not the root empty state

If the reverted manifest is identical to HEAD, revert is a true no-op (same stdout as `commit` with no changes: no new timeline entry, refs unchanged). When the reverted tree matches the target's parent manifest, the branch tip moves to that parent commit id instead of writing a new commit with the same manifest.

Paths added in the target state and modified again on HEAD before revert produce a conflict (`path modified after the reverted state`) rather than silently keeping HEAD's newer content.

`--dry-run` plans in memory only; conflicts label the reverted parent and current HEAD, omit unsupported `--resolve` guidance, and exit non-zero without writes.

### Network sync

`fetch` updates `.astvcs/refs/remotes/<remote>/<branch>` only. To work on fetched commits without merging, use `reset` or `checkout --state` with the remote-tracking ref (for example `origin/main`).

**Shallow fetch.** `--depth N` limits downloaded timeline entries to `N` from each tip (`N=1` is tip only). Boundaries are stored in `.astvcs/shallow.json`. A full fetch (no `--depth`) clears shallow boundaries and downloads all missing history. Shallow tag fetch uses the same depth per tag tip. `merge-base` and `merge` may fail with a `shallow history` error when history is incomplete; deepen with a higher `--depth` or a full fetch.

`pull <remote>` runs `fetch` then merges the remote-tracking ref (`<remote>/<branch>`) into the current branch. Default branch name is the checked-out branch (same as `push`); detached HEAD requires `--branch`. `--depth` is passed through to the fetch step. Default merge message is `Merge <remote>/<branch>`; override with `-m`. On fetch failure, no merge is attempted. On fetch success with merge conflicts, remote-tracking refs are updated but the local branch tip and working tree are unchanged (same abort guarantee as `merge`). When already up to date after fetch, `pull` succeeds with an `Already up to date` message. Merge failure after a successful fetch prints `warning: pull: merge failed after successful fetch` on stderr.

`push` requires a fast-forward unless `--force` is passed. Detached HEAD requires `--branch` to name the branch being pushed.

Remote URLs may be a local repository path, a `file://` URL, an `http://` or `https://` base URL from `astvcs serve`, or an SSH URL. SSH examples: `ssh://alice@example.com/var/repos/project`, `bob@host.example:/srv/astvcs.git`. The remote machine must have `astvcs` on `PATH`; authentication and host keys are handled by OpenSSH. Register an HTTP or SSH bearer token with `remote add --token` (stored in `.astvcs/remotes.json`). For `serve`, pass `--token` or set `ASTVCS_SERVE_TOKEN`; use `--public-read` to allow anonymous reads while still requiring a token for writes. `clone` removes a partial `.astvcs` tree when the operation fails after init so retry into the same empty destination path is not blocked.

**HTTPS serve.** Pass `--tls-cert` and `--tls-key` together with PEM files to listen on HTTPS instead of HTTP. Both flags are required when either is set. Startup logs `https://` or `http://` accordingly.

**Serve concurrency.** Multiple clients can read simultaneously (`GET`/`HEAD` on blobs, states, timeline, refs, config, and shallow ancestry). `PUT` uploads serialize and take advisory `repo.lock` per operation; if local CLI holds the lock, the server returns HTTP 503 with body `repository locked`. Reads do not block local CLI commands on the same repository. When a `PUT` advances `refs/heads/<branch>` and that branch is the server's checked-out branch, `index.json` is rewritten from the new tip (metadata only; the working tree is not materialized). `fsck` checks `index.json` against HEAD, not working-tree drift after push.

**HTTPS remotes.** The client validates TLS certificates via rustls (default). Self-signed or otherwise untrusted certificates fail closed unless you pass `--insecure` on `fetch`, `push`, `pull`, or `clone`. `--insecure` skips certificate verification and is intended for local development only. It does not apply to SSH remotes.

**SSH remotes.** Use `ssh://user@host/absolute/path` or scp-style `user@host:/absolute/path`. `--insecure` is ignored for SSH. Tokens from `clone --token` or `remotes.json` are sent over the remote-serve protocol.

**remote-serve (operator/internal).** `astvcs remote-serve --repo <path> [--token <token>] [--public-read]` serves the repository on stdin/stdout using newline-delimited JSON (same `/v1/` paths as HTTP serve). Normally invoked by SSH on the remote host, not run directly. Honors `ASTVCS_SERVE_TOKEN` when `--token` is omitted.

**Self-signed cert workflow (local testing).** Generate a cert and key, serve with TLS, then clone with `--insecure`:

```powershell
# OpenSSL (if installed)
openssl req -x509 -newkey rsa:2048 -nodes -keyout serve-key.pem -out serve-cert.pem -days 365 -subj "/CN=localhost"

astvcs serve --tls-cert serve-cert.pem --tls-key serve-key.pem --token dev-secret
astvcs clone https://127.0.0.1:9421 ./clone-dir --token dev-secret --insecure
```

Bearer token auth works over both HTTP and HTTPS. File remotes ignore tokens and TLS flags.

### `gc`

Walks reachability from every local branch tip, every tag tip, every remote-tracking branch tip, and the current HEAD state when HEAD is detached. **Tier 1:** reports and optionally deletes blob store objects not referenced by any reachable state manifest (`--prune`). **Tier 2:** reports and optionally deletes unreachable timeline entries and state manifests (`--prune-history`). The root empty state is never deleted.

| Flag | Behavior |
|------|----------|
| (none) | Dry-run: report unreachable blobs and unreachable states with reclaimable bytes for each tier |
| `--prune` | Delete unreachable blob files (loose and packed) |
| `--prune-history` | Delete unreachable `.astvcs/timeline/{id}.json` and `.astvcs/states/{id}.json` files |
| `--prune --prune-history` | Delete both unreachable blobs and unreachable state history in one run |

After `gc --prune-history`, `checkout --state <id>` fails for pruned state ids because the timeline entry is gone. States retained when history is not pruned remain checkoutable by id even when no ref names them.

Example output (dry-run):

```text
gc dry-run: 2 unreachable blob(s) (examined 5); would reclaim 1.2 KiB
gc dry-run: 3 unreachable state(s) (examined 10); would reclaim 4.5 KiB history (use --prune-history to delete)
```

After `--prune` with nothing to do:

```text
gc: examined 3 blob(s); nothing to prune
gc: 10 state(s) examined; no unreachable history
```

After `--prune-history` with removals:

```text
gc: examined 10 state(s); removed 3 unreachable; reclaimed 4.5 KiB history
```

### `repack`

Packs all loose blobs under `.astvcs/blobs/` into zstd-compressed pack files under `.astvcs/packs/`, updates `packs/index.json`, and removes the loose copies. Safe to run online under the repository lock. New commits continue writing loose blobs until the next `repack`. Content-addressed blob ids are unchanged.

```text
repack: packed 12 blob(s); removed 12 loose file(s); 48.2 KiB -> 9.1 KiB on disk
```

### `fsck`

Integrity check. By default report-only: never modifies refs, HEAD, timeline, blobs, `index.json`, or the working tree. Exits with code 1 when any issue is found.

| Check | Report label |
|-------|----------------|
| State manifest references a missing blob | `missing blob` |
| Branch, tag, or remote-tracking ref points to a state with no timeline entry | `dangling ref` |
| Timeline entry exists but `.astvcs/states/{id}.json` is missing | `missing state manifest` |
| `.astvcs/states/{id}.json` exists but timeline entry is missing | `orphaned state manifest` |
| `HEAD` names a branch with no `refs/heads/` file | `HEAD branch missing` |
| `index.json` `state_id` or paths disagree with HEAD, or index present while HEAD is invalid | `index inconsistent` |
| Pack index entry fails to decompress or hash does not match blob id | `pack corrupt` |
| `.astvcs-tmp` file with no canonical target (not cleaned by normal commands) | `orphan temp file` |
| `config.json` `format_version` newer than this binary supports | `unknown format version` |

**`--repair`** (conservative, under repo lock): when HEAD resolves to a state with timeline and manifest, rewrite `index.json` from the HEAD manifest if the index is inconsistent (wrong `state_id`, stale paths, or paths absent from HEAD). Remove stray `.astvcs-tmp` files when the canonical target already exists. Refuses when HEAD names a missing branch while other local branches exist (update HEAD manually). Never repairs missing blobs, pack corruption, or missing state manifests.

**`--prune-refs`**: delete `refs/heads/*`, `refs/tags/*`, and `refs/remotes/*/*` files that point at state ids with no timeline entry. Never modifies the `HEAD` file. Can be combined with `--repair`.

Unreachable blob cleanup belongs to `gc --prune`; unreachable history cleanup to `gc --prune-history`.

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

After repairs:

```text
fsck: 2 repair(s) applied
  index rewritten: rewrote index.json from HEAD state abc…
  dangling ref pruned: removed refs/heads/dangling (pointed to fff… with no timeline entry)
fsck: repository ok
```

### Merge conflict resolution

When a merge would conflict, default output lists each path, the overlapping ours and theirs intents, the reason, and resolution syntax. Repeated overlap examples are limited per path with an omitted count. Pass `--details` for state IDs, raw mutations, and every overlap. Resolve with `--resolve <path>:ours` or `--resolve <path>:theirs` for each conflicted path (repository-relative, matching manifest keys). `ours` is the current branch (HEAD); `theirs` is the branch being merged in. Non-conflicted paths keep the structurally merged result from the planner.

- Unresolved conflicts abort the merge with no ref, working tree, or `index.json` changes.
- `--resolve` for a path not in the conflict list errors before any write.
- Duplicate `--resolve` for the same path errors.
- Invalid sides (not `ours` or `theirs`) error at parse time.

With `--dry-run`, resolutions are applied in memory only: a fully resolved plan prints the usual success summary; any remaining conflict prints the conflict report and exits non-zero without writes.

## Stderr output

By default, stderr shows only `warning:` lines (unexpected parse fallback, merge conflicts, materialize `--force` clobbers, index inconsistencies). Known text-only paths such as `.gitignore`, `.md`, `.txt`, `go.sum`, and `.ps1` do not warn; they store as text blobs without stderr output. NUL-containing or non-UTF-8 file content is stored as binary blobs (no warning). `--details` expands structural output without operational notices. With `-v`, structural details and `notice:` lines are included: scan results, parse mode per file, text fallback reasons, text/binary blob storage, blob writes, materialize actions, merge planning, reset/revert planning, and no-op commits. Primary command output stays on stdout.

`status` and `diff` also surface text fallback on stdout for AST-capable paths (see `status` and `diff` rows above) so CI and operators see structural diff loss without relying on stderr alone.

When another process holds `.astvcs/repo.lock`, any command that needs the repository fails immediately on stderr with:

```text
error: repository is locked by another process; cannot acquire <absolute-or-relative-path>/.astvcs/repo.lock
```

There is no wait/retry; run the command again after the other process finishes.

With `--json`, failures print a single JSON object on stderr, for example:

```json
{"kind":"missing_identity","message":"author identity not configured; run `astvcs identity set --name <name> --email <email>` or set ASTVCS_AUTHOR_NAME and ASTVCS_AUTHOR_EMAIL"}
```

The JSON `message` field and library `Display` keep the complete diagnostic. Focused plain CLI errors may be shorter; pass `--details` to print the complete message.

## Ignore rules

Put patterns in `.gitignore` (standard git syntax) or `.astvcsignore` for astvcs-only rules. Build output, dependencies, and binaries are the project's responsibility to list there.
