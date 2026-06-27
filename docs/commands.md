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
| `status` | Show changed files vs the checked-out state |
| `diff [path]` | Diff working tree, or a single file |
| `diff --state <ref>` | Diff current HEAD against a branch or state id |
| `diff --base <ref> --left <ref> --right <ref> [path]` | Three-way diff from merge base |
| `record -m <msg>` | Record working tree as a new state (prints when unchanged) |
| `branch list` | List branches |
| `branch create <name> [--from <branch>]` | Create a branch |
| `merge-base <left> <right>` | Print lowest common ancestor (branch name or state id) |
| `merge <branch> -m <msg>` | Merge a branch; updates working tree on success |
| `merge <branch> -m <msg> --resolve <path>:ours` | Merge with per-path conflict resolution (repeatable) |
| `merge <branch> --dry-run` | Simulate merge; print conflicts without changing the repository |
| `checkout --branch <name>` | Switch branch and materialize its HEAD to disk |
| `checkout --state <id>` | Detached checkout: materialize a state and move HEAD to it |
| `log [-n N]` | Walk timeline history (default 20 entries) |
| `remote add <name> <url>` | Register a remote (local path, `file://`, or `http://`) |
| `remote list` | List configured remotes |
| `remote remove <name>` | Remove a remote and its tracking refs |
| `fetch <remote> [--branch <name>]` | Download missing objects; update remote-tracking refs |
| `push <remote> [--branch <name>] [--force]` | Upload missing objects; fast-forward remote branch |
| `clone <url> [path]` | Clone a remote repository (default path: `.`) |
| `serve [--bind <addr>] [--port <n>]` | Serve the repository over HTTP (default `127.0.0.1:9421`) |

Refs accepted by `diff`, `merge-base`, and `checkout --state` include branch names and 64-character state ids.

### Network sync

`fetch` updates `.astvcs/refs/remotes/<remote>/<branch>` only. To work on fetched commits, checkout or merge the branch locally (for example after `fetch`, set the local branch tip to the remote-tracking ref and run `checkout --branch`).

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

By default, stderr shows only `warning:` lines (unexpected parse fallback, skipped paths, merge conflicts, index inconsistencies). Known text-only paths such as `.gitignore`, `.md`, and `.txt` do not warn; they store as text blobs without stderr output. With `-v`, `notice:` lines are included: scan results, parse mode per file, text-blob storage, blob writes, materialize actions, merge planning, and no-op records. Primary command output stays on stdout.

## Ignore rules

Put patterns in `.gitignore` (standard git syntax) or `.astvcsignore` for astvcs-only rules. Build output, dependencies, and binaries are the project's responsibility to list there.
