# Examples

Runnable walkthroughs for the astvcs CLI. Project overview and quick start: [README.md](../README.md). Full command reference: [docs/commands.md](../docs/commands.md).

Nine fixtures cover structural merge scenarios, network sync, lifecycle commands, shallow clone, git import, and HTTP serve. Run from the astvcs repo root.

```powershell
cargo build --release
.\target\release\astvcs.exe --version
```

Then use `.\target\release\astvcs.exe` (no `cargo run` per command).

On Windows, use `Set-Content -NoNewline` when editing fixture files so astvcs sees LF-only line endings (`reset.ps1` and `run-demos.ps1` write LF baselines via `[IO.File]::WriteAllText`).

Set author identity once per repository before the first `commit`, `merge`, or `revert` (or use `ASTVCS_AUTHOR_NAME` / `ASTVCS_AUTHOR_EMAIL`):

```powershell
.\target\release\astvcs.exe --repo <path> identity set --name "Example" --email example@astvcs.local
```

**Reset before each walkthrough** (removes `.astvcs`, temp sibling dirs, and restores baseline source files):

```powershell
.\examples\reset.ps1
```

Run all walkthroughs non-interactively (build, reset, log to stdout/stderr):

```powershell
.\examples\run-demos.ps1
.\examples\run-demos.ps1 -LogPath C:\path\to\astvcs-demo-output.txt
```

Integration tests in `tests/integration.rs` cover the same scenarios in CI. The full catalog is in [`.cursor/skills/astvcs-integration-tests/references/test-catalog.md`](../.cursor/skills/astvcs-integration-tests/references/test-catalog.md).

`astvcs diff --view` opens the shipped change-first HTML viewer (same binary assets as production). It starts with compact intents, next and previous change controls, and lazy unchanged branches. Use `--details` on text diffs for node IDs and raw mutations. Integration coverage: `cli_diff_view_writes_html_with_alignment` and `cli_diff_view_large_file_keeps_change_first_controls`.

| Fixture | Integration test | What it shows |
|---------|------------------|---------------|
| `workflow-demo/` | `workflow_demo_prepend_and_disjoint_merge` | Staging (`add .`); prepend comment without move cascade; branch; disjoint file merge |
| `same-file-demo/` | `same_file_demo_disjoint_merge` | Same-file disjoint edits (rename + insert) merge without `--resolve`; formatting preserved |
| `merge-demo/` | `merge_demo_add_add_and_deletion`, `merge_demo_deletion_when_other_branch_unchanged` | Add/add on a new file; modify vs delete |
| `identity-demo/` | `identity_demo_payload_edit_disjoint_merge_and_conflict` | Literal edits as `EditPayload`; sibling literal merge; rename conflict |
| `network-demo/` | `network_file_remote_fetch_push_and_clone` | File remote `clone`, identity, commit, `push` |
| `lifecycle-demo/` | `rebase_linear_success`, `cherry_pick_clean_commit`, `stash_before_checkout`, `stash_pop_restores_files`, `tag_create_and_list`, `blame_linear_two_commits` | Rebase, cherry-pick, stash, tags, blame on linear history |
| `shallow-demo/` | `shallow_clone_has_fewer_timeline_entries_than_full_clone` | `clone --depth 2` vs full clone; `shallow.json` |
| `import-git-demo/` | `import_git_snapshot_from_subprocess` | `import-git` one-way HEAD snapshot (requires `git` on PATH) |
| `serve-demo/` | `serve_requires_token_for_mutations`, `http_transport_sends_bearer_token` | `serve` with bearer token; `clone http://... --token` |
| (CLI-only) | `partial_commit_only_stages_paths`, `status_shows_staged_and_unstaged_columns` | `add` staging index and two-column `status` |
| (CLI-only) | `reset_mixed_unstages_and_keeps_disk`, `reset_modes_soft_mixed_hard_comparison` | `reset --mixed` clears staging, keeps disk |
| (CLI-only) | `rebase_conflict_abort_restores`, `rebase_conflict_continue` | Replay conflict, abort restore, and `--resolve` on continue |
| (CLI-only) | `cherry_pick_conflict_leaves_head_unchanged` | Cherry-pick conflict rollback |
| (CLI-only) | `stash_pop_conflict_keeps_entry` | Conflicting `stash pop` keeps the entry |
| (CLI-only) | `merge_base_fails_on_shallow_clone_with_incomplete_history` | Shallow history limits `merge-base` |
| (CLI-only) | `cli_version_prints_crate_version` | `astvcs --version` prints crate version |
| (CLI-only) | `trailing_comment_and_literal_edit_merge` | Trailing comment text merges with sibling literal edit |
| (CLI-only) | `cli_trivia_only_commit` | Whitespace-only formatting commit |
| (CLI-only) | `cli_branch_remove_guardrails` | `branch remove` guardrails and recreate |
| (CLI-only) | `remove_default_branch_updates_config` | Removing default branch updates `config.json` |
| (CLI-only) | `cli_reset_hard_soft_and_force` | Hard/soft reset and `--force` clobber warnings |
| (CLI-only) | `cli_revert_and_dry_run`, `cli_revert_of_revert_restores_content` | Revert success, dry-run conflict, revert-of-revert |
| (CLI-only) | `cli_merge_resolve_conflict` | `merge --resolve path:ours\|theirs` |
| (CLI-only) | `cli_materialize_refuses_dirty_tree_and_force_overrides` | Merge, checkout, revert dirty-tree refusal and `--force` |
| (CLI-only) | `bisect_linear_four_commits`, `bisect_run_releases_lock_for_nested_astvcs` | Linear bisect finds first bad commit; nested astvcs during script |

## Workflow demo

```powershell
.\examples\reset.ps1
$D = "examples\workflow-demo"
.\target\release\astvcs.exe init $D
.\target\release\astvcs.exe --repo $D identity set --name "Example" --email example@astvcs.local
.\target\release\astvcs.exe --repo $D add .
.\target\release\astvcs.exe --repo $D commit --message "baseline"

Set-Content -NoNewline $D\lib.rs "//! workflow demo crate`npub mod core;`npub mod util;`n"
.\target\release\astvcs.exe --repo $D diff lib.rs
.\target\release\astvcs.exe --repo $D commit --message "prepend doc comment"

.\target\release\astvcs.exe --repo $D branch create feature
.\target\release\astvcs.exe --repo $D checkout --branch feature
Set-Content -NoNewline $D\util.rs "pub fn label() -> &'static str {`n    `"feature-branch`"`n}`n"
.\target\release\astvcs.exe --repo $D commit --message "feature util label"

.\target\release\astvcs.exe --repo $D checkout --branch main
Set-Content -NoNewline $D\core.rs "pub fn answer() -> i32 {`n    43`n}`n"
.\target\release\astvcs.exe --repo $D commit --message "main core answer"

$base = (.\target\release\astvcs.exe --repo $D merge-base main feature | Select-Object -Last 1)
.\target\release\astvcs.exe --repo $D diff --base $base --left main --right feature core.rs
.\target\release\astvcs.exe --repo $D diff --base $base --left main --right feature util.rs

.\target\release\astvcs.exe --repo $D merge feature --message "merge feature into main"
.\target\release\astvcs.exe --repo $D status
Get-Content $D\util.rs
```

Detached checkout (run after `main core answer`, before `merge`):

```powershell
$stateId = (.\target\release\astvcs.exe --repo $D merge-base main feature | Select-Object -Last 1)
.\target\release\astvcs.exe --repo $D checkout --state $stateId
.\target\release\astvcs.exe --repo $D commit --message "noop while detached"
.\target\release\astvcs.exe --repo $D checkout --branch main
```

## Merge demo

```powershell
.\examples\reset.ps1
$D = "examples\merge-demo"
.\target\release\astvcs.exe init $D
.\target\release\astvcs.exe --repo $D identity set --name "Example" --email example@astvcs.local
.\target\release\astvcs.exe --repo $D commit --message "base"

.\target\release\astvcs.exe --repo $D branch create feature
.\target\release\astvcs.exe --repo $D checkout --branch feature
Set-Content -NoNewline $D\util.rs "pub fn util() {}`n"
Set-Content -NoNewline $D\lib.rs "pub fn label() -> &'static str { `"feature`" }`n"
.\target\release\astvcs.exe --repo $D commit --message "feature util and lib"

.\target\release\astvcs.exe --repo $D checkout --branch main
Set-Content -NoNewline $D\util.rs "pub fn util() {}`n"
.\target\release\astvcs.exe --repo $D commit --message "main util"
.\target\release\astvcs.exe --repo $D merge feature --message "merge add/add"
Get-Content $D\util.rs
Get-Content $D\lib.rs
```

Deletion (continues same `$D`; do not run `reset.ps1` between the two parts):

```powershell
.\target\release\astvcs.exe --repo $D checkout --branch main
.\target\release\astvcs.exe --repo $D branch create feature2
.\target\release\astvcs.exe --repo $D checkout --branch feature2
.\target\release\astvcs.exe --repo $D commit --message "feature noop"

.\target\release\astvcs.exe --repo $D checkout --branch main
Remove-Item $D\config.toml
.\target\release\astvcs.exe --repo $D commit --message "delete config on main"
.\target\release\astvcs.exe --repo $D merge feature2 --message "merge deletion"
.\target\release\astvcs.exe --repo $D status
```

## Identity demo

```powershell
.\examples\reset.ps1
$I = "examples\identity-demo"
.\target\release\astvcs.exe init $I
.\target\release\astvcs.exe --repo $I identity set --name "Example" --email example@astvcs.local
.\target\release\astvcs.exe --repo $I commit --message "baseline"

Set-Content -NoNewline $I\core.rs "pub fn answer() -> i32 {`n    43`n}`n"
.\target\release\astvcs.exe --repo $I diff core.rs
.\target\release\astvcs.exe --repo $I commit --message "literal on main"

.\target\release\astvcs.exe --repo $I branch create feature
.\target\release\astvcs.exe --repo $I checkout --branch feature
Set-Content -NoNewline $I\labels.rs "pub fn pair() -> (&'static str, &'static str) {`n    (`"alpha`", `"BETA`")`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "edit second literal"
.\target\release\astvcs.exe --repo $I checkout --branch main
Set-Content -NoNewline $I\labels.rs "pub fn pair() -> (&'static str, &'static str) {`n    (`"ALPHA`", `"beta`")`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "edit first literal"
.\target\release\astvcs.exe --repo $I merge feature --message "merge sibling literals"
Get-Content $I\labels.rs
```

Rename conflict (continues same `$I`):

```powershell
.\target\release\astvcs.exe --repo $I branch create conflict
.\target\release\astvcs.exe --repo $I checkout --branch conflict
Set-Content -NoNewline $I\conflict.rs "fn sample() {`n    let renamed = 1;`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "rename to renamed"

.\target\release\astvcs.exe --repo $I checkout --branch main
Set-Content -NoNewline $I\conflict.rs "fn sample() {`n    let alternate = 1;`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "rename to alternate"
.\target\release\astvcs.exe --repo $I merge conflict --dry-run
```

`merge --dry-run` exits 1 on conflict; the repo is unchanged. The focused report shows the path, ours and theirs intents, overlap reason, and exact resolution syntax. Add `--details` for state IDs, raw mutations, and every overlap.

Resolve by picking one side (`ours` = HEAD, `theirs` = merged branch):

```powershell
.\target\release\astvcs.exe --repo $I merge conflict -m "take feature side" --resolve conflict.rs:theirs
```

## Same-file demo

Uses `sample.rs` (not `main.rs`) so Cargo does not treat this folder as an example crate.

```powershell
.\examples\reset.ps1
$D = "examples\same-file-demo"
.\target\release\astvcs.exe init $D
.\target\release\astvcs.exe --repo $D identity set --name "Example" --email example@astvcs.local
.\target\release\astvcs.exe --repo $D commit --message "baseline"

.\target\release\astvcs.exe --repo $D branch create feature
.\target\release\astvcs.exe --repo $D checkout --branch feature
Set-Content -NoNewline $D\sample.rs "fn foo() {`n    let x = 1;`n    let z = 2;`n}`n"
.\target\release\astvcs.exe --repo $D commit --message "insert on feature"

.\target\release\astvcs.exe --repo $D checkout --branch main
Set-Content -NoNewline $D\sample.rs "fn foo() {`n    let y = 1;`n}`n"
.\target\release\astvcs.exe --repo $D commit --message "rename on main"

$base = (.\target\release\astvcs.exe --repo $D merge-base main feature | Select-Object -Last 1)
.\target\release\astvcs.exe --repo $D diff --base $base --left main --right feature sample.rs

.\target\release\astvcs.exe --repo $D merge feature --message "merge feature"
Get-Content $D\sample.rs
```

After merge, `sample.rs` should be formatted (not collapsed to one line):

```rust
fn foo() {
    let y = 1;
    let z = 2;
}
```

The integration test asserts this exact text on disk, including indentation and newlines.

## Network demo (file remote)

Uses sibling directories `_upstream` and `_clone` under the fixture (gitignored). Mirrors `network_file_remote_fetch_push_and_clone`.

```powershell
.\examples\reset.ps1
$U = "examples\network-demo\_upstream"
$C = "examples\network-demo\_clone"
.\target\release\astvcs.exe init $U
.\target\release\astvcs.exe --repo $U identity set --name "Example" --email example@astvcs.local
Set-Content -NoNewline $U\note.txt "v1`n"
.\target\release\astvcs.exe --repo $U add .
.\target\release\astvcs.exe --repo $U commit --message "v1"

.\target\release\astvcs.exe clone $U $C
.\target\release\astvcs.exe --repo $C identity set --name "Example" --email example@astvcs.local
Set-Content -NoNewline $C\note.txt "v2`n"
.\target\release\astvcs.exe --repo $C commit -m "v2"
.\target\release\astvcs.exe --repo $C push origin --branch main
Get-Content $C\note.txt
```

## Lifecycle demo

Single linear walkthrough for `blame`, `tag`, `stash`, `rebase`, and `cherry-pick`. `run-demos.ps1` runs `stash push` before `checkout` (see `stash_before_checkout`); optional `stash pop` on `feature` after `feature 2` is documented below. Conflict abort/continue paths stay in integration tests only.

```powershell
.\examples\reset.ps1
$L = "examples\lifecycle-demo"
.\target\release\astvcs.exe init $L
.\target\release\astvcs.exe --repo $L identity set --name "Example" --email example@astvcs.local

Set-Content -NoNewline $L\app.txt "line one`n"
.\target\release\astvcs.exe --repo $L commit -m "first line"
Set-Content -NoNewline $L\app.txt "line one`nline two`n"
.\target\release\astvcs.exe --repo $L commit -m "add second line"
.\target\release\astvcs.exe --repo $L blame app.txt

.\target\release\astvcs.exe --repo $L tag create v1.0 main
.\target\release\astvcs.exe --repo $L tag list

.\target\release\astvcs.exe --repo $L branch create feature
.\target\release\astvcs.exe --repo $L checkout --branch feature
Set-Content -NoNewline $L\feat.txt "one`n"
.\target\release\astvcs.exe --repo $L add feat.txt
.\target\release\astvcs.exe --repo $L commit -m "feature 1"
Set-Content -NoNewline $L\feat.txt "two`n"
.\target\release\astvcs.exe --repo $L add feat.txt
.\target\release\astvcs.exe --repo $L commit -m "feature 2"

Set-Content -NoNewline $L\app.txt "wip`n"
.\target\release\astvcs.exe --repo $L stash push
.\target\release\astvcs.exe --repo $L checkout --branch main
Set-Content -NoNewline $L\app.txt "v2-main`n"
.\target\release\astvcs.exe --repo $L add app.txt
.\target\release\astvcs.exe --repo $L commit -m "main advance"

.\target\release\astvcs.exe --repo $L checkout --branch feature
.\target\release\astvcs.exe --repo $L rebase main
Set-Content -NoNewline $L\feat.txt "three`n"
.\target\release\astvcs.exe --repo $L add feat.txt
.\target\release\astvcs.exe --repo $L commit -m "feature 3"
$pick = (.\target\release\astvcs.exe --repo $L log -n 1 | Select-Object -First 1).Split(" ", 2)[1]

.\target\release\astvcs.exe --repo $L checkout --branch main
.\target\release\astvcs.exe --repo $L cherry-pick $pick -m "pick feature 3"
.\target\release\astvcs.exe --repo $L status
```

Optional `stash pop` on `feature` (after `feature 2`, before editing `app.txt` for stash):

```powershell
.\target\release\astvcs.exe --repo $L checkout --branch feature
.\target\release\astvcs.exe --repo $L stash pop
Get-Content $L\app.txt
```

## Shallow demo

Upstream gets five commits on `note.txt`. Compare `clone --depth 2` against a full clone.

```powershell
.\examples\reset.ps1
$U = "examples\shallow-demo\_upstream"
$S = "examples\shallow-demo\_shallow"
$F = "examples\shallow-demo\_full"
.\target\release\astvcs.exe init $U
.\target\release\astvcs.exe --repo $U identity set --name "Example" --email example@astvcs.local
Set-Content -NoNewline $U\note.txt "v1`n"
foreach ($i in 1..5) {
    if ($i -gt 1) { Set-Content -NoNewline $U\note.txt "v$i`n" }
    .\target\release\astvcs.exe --repo $U commit -m "v$i"
}

.\target\release\astvcs.exe clone --depth 2 $U $S
.\target\release\astvcs.exe clone $U $F
(Get-ChildItem $S\.astvcs\timeline -File).Count
(Get-ChildItem $F\.astvcs\timeline -File).Count
Test-Path $S\.astvcs\shallow.json
```

The shallow clone has fewer timeline entries and writes `.astvcs/shallow.json`. A full clone fetches complete history for comparison.

## Import-git demo

`hello.txt` in the fixture shows the expected imported content. The walkthrough builds a small git repo in a temp directory (requires `git` on PATH).

```powershell
$parent = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "astvcs-import-demo")
$gitDir = Join-Path $parent "git-repo"
$astvcsDir = Join-Path $parent "astvcs-repo"
git -C $gitDir init
Set-Content -NoNewline (Join-Path $gitDir "hello.txt") "hello from git`n"
git -C $gitDir add hello.txt
git -C $gitDir commit -m "git baseline"

.\target\release\astvcs.exe init $astvcsDir
.\target\release\astvcs.exe --repo $astvcsDir identity set --name "Example" --email example@astvcs.local
.\target\release\astvcs.exe --repo $astvcsDir import-git $gitDir -m "Imported git snapshot"
Get-Content (Join-Path $astvcsDir "hello.txt")
```

`run-demos.ps1` skips this section with a log line when `git` is not on PATH.

## Serve demo (HTTP)

Start `astvcs serve` with a bearer token, clone over HTTP, then stop the server.

```powershell
.\examples\reset.ps1
$S = "examples\serve-demo"
$C = "examples\serve-demo\_clone"
.\target\release\astvcs.exe init $S
.\target\release\astvcs.exe --repo $S identity set --name "Example" --email example@astvcs.local
.\target\release\astvcs.exe --repo $S add .
.\target\release\astvcs.exe --repo $S commit -m "v1"

$token = "demo-serve-token"
$proc = Start-Process -FilePath .\target\release\astvcs.exe -ArgumentList @(
    "--repo", (Resolve-Path $S), "serve", "--token", $token, "--port", "9421"
) -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2
try {
    .\target\release\astvcs.exe clone http://127.0.0.1:9421/ $C --token $token
    Get-Content $C\note.txt
} finally {
    if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
}
```

See [docs/commands.md](../docs/commands.md) for `serve`, `fetch`, `pull`, and `push` flags (TLS, `--public-read`, SSH remotes).

## Staging and `reset --mixed`

After the first `add`, commits use the staging index. `reset --mixed <ref>` moves the branch tip and syncs `index.json` to the target while clearing staging; the working tree is unchanged (git's default reset mode). See `reset_mixed_unstages_and_keeps_disk` in `tests/integration.rs`.

Mini walkthrough (continues `workflow-demo` after the merge above, or run `reset.ps1` and repeat init + identity + baseline):

```powershell
Set-Content -NoNewline $D\core.rs "pub fn answer() -> i32 {`n    99`n}`n"
.\target\release\astvcs.exe --repo $D add core.rs
.\target\release\astvcs.exe --repo $D status
$tip = (.\target\release\astvcs.exe --repo $D log -n 1 | Select-Object -First 1).Split(" ", 2)[1]
.\target\release\astvcs.exe --repo $D reset --mixed $tip
.\target\release\astvcs.exe --repo $D status
```

`status` should show unstaged edits in `core.rs` and an empty staging area.
