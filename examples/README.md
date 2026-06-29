# Examples

Four fixtures exercise the CLI. Run from the astvcs repo root.

```powershell
cargo build --release
```

Then use `.\target\release\astvcs.exe` (no `cargo run` per command).

**Reset before each walkthrough** (removes `.astvcs` and restores baseline source files):

```powershell
.\examples\reset.ps1
```

Integration tests in `tests/integration.rs` cover the same scenarios in CI. Reset, revert, and materialize-safety behavior are tested there only (no separate fixture directory). The full catalog is in [`.cursor/skills/astvcs-integration-tests/references/test-catalog.md`](../.cursor/skills/astvcs-integration-tests/references/test-catalog.md).

| Fixture | Integration test | What it shows |
|---------|------------------|---------------|
| `workflow-demo/` | `workflow_demo_prepend_and_disjoint_merge` | Prepend comment without move cascade; branch; disjoint file merge |
| `same-file-demo/` | `same_file_demo_disjoint_merge` | Same-file disjoint edits (rename + insert) merge without `--resolve`; formatting preserved |
| `merge-demo/` | `merge_demo_add_add_and_deletion`, `merge_demo_deletion_when_other_branch_unchanged` | Add/add on a new file; modify vs delete |
| `identity-demo/` | `identity_demo_payload_edit_disjoint_merge_and_conflict` | Literal edits as `EditPayload`; sibling literal merge; rename conflict |
| (CLI-only) | `trailing_comment_and_literal_edit_merge` | Trailing comment text merges with sibling literal edit |
| (CLI-only) | `cli_trivia_only_commit` | Whitespace-only formatting commit |
| (CLI-only) | `cli_branch_remove_guardrails` | `branch remove` guardrails and recreate |
| (CLI-only) | `remove_default_branch_updates_config` | Removing default branch updates `config.json` |
| (CLI-only) | `cli_reset_hard_soft_and_force` | Hard/soft reset and `--force` clobber warnings |
| (CLI-only) | `cli_revert_and_dry_run`, `cli_revert_of_revert_restores_content` | Revert success, dry-run conflict, revert-of-revert |
| (CLI-only) | `cli_merge_resolve_conflict` | `merge --resolve path:ours\|theirs` |
| (CLI-only) | `cli_materialize_refuses_dirty_tree_and_force_overrides` | Merge, checkout, revert dirty-tree refusal and `--force` |
| (CLI-only) | `rebase_linear_success` | Linear feature branch replayed onto updated main |
| (CLI-only) | `rebase_conflict_abort_restores`, `rebase_conflict_continue` | Replay conflict, abort restore, and `--resolve` on continue |
| (CLI-only) | `cherry_pick_clean_commit`, `cherry_pick_conflict_leaves_head_unchanged`, `cherry_pick_from_remote_tracking_ref` | Cherry-pick success, conflict rollback, remote-tracking ref |

## Workflow demo

```powershell
.\examples\reset.ps1
$D = "examples\workflow-demo"
.\target\release\astvcs.exe init $D
.\target\release\astvcs.exe --repo $D commit --message "baseline"

Set-Content $D\lib.rs "//! workflow demo crate`npub mod core;`npub mod util;`n"
.\target\release\astvcs.exe --repo $D diff lib.rs
.\target\release\astvcs.exe --repo $D commit --message "prepend doc comment"

.\target\release\astvcs.exe --repo $D branch create feature
.\target\release\astvcs.exe --repo $D checkout --branch feature
Set-Content $D\util.rs "pub fn label() -> &'static str {`n    `"feature-branch`"`n}`n"
.\target\release\astvcs.exe --repo $D commit --message "feature util label"

.\target\release\astvcs.exe --repo $D checkout --branch main
Set-Content $D\core.rs "pub fn answer() -> i32 {`n    43`n}`n"
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
.\target\release\astvcs.exe --repo $D commit --message "base"

.\target\release\astvcs.exe --repo $D branch create feature
.\target\release\astvcs.exe --repo $D checkout --branch feature
Set-Content $D\util.rs "pub fn util() {}`n"
Set-Content $D\lib.rs "pub fn label() -> &'static str { `"feature`" }`n"
.\target\release\astvcs.exe --repo $D commit --message "feature util and lib"

.\target\release\astvcs.exe --repo $D checkout --branch main
Set-Content $D\util.rs "pub fn util() {}`n"
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
.\target\release\astvcs.exe --repo $I commit --message "baseline"

Set-Content $I\core.rs "pub fn answer() -> i32 {`n    43`n}`n"
.\target\release\astvcs.exe --repo $I diff core.rs
.\target\release\astvcs.exe --repo $I commit --message "literal on main"

.\target\release\astvcs.exe --repo $I branch create feature
.\target\release\astvcs.exe --repo $I checkout --branch feature
Set-Content $I\labels.rs "pub fn pair() -> (&'static str, &'static str) {`n    (`"alpha`", `"BETA`")`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "edit second literal"
.\target\release\astvcs.exe --repo $I checkout --branch main
Set-Content $I\labels.rs "pub fn pair() -> (&'static str, &'static str) {`n    (`"ALPHA`", `"beta`")`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "edit first literal"
.\target\release\astvcs.exe --repo $I merge feature --message "merge sibling literals"
Get-Content $I\labels.rs
```

Rename conflict (continues same `$I`):

```powershell
.\target\release\astvcs.exe --repo $I branch create conflict
.\target\release\astvcs.exe --repo $I checkout --branch conflict
Set-Content $I\conflict.rs "fn sample() {`n    let renamed = 1;`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "rename to renamed"

.\target\release\astvcs.exe --repo $I checkout --branch main
Set-Content $I\conflict.rs "fn sample() {`n    let alternate = 1;`n}`n"
.\target\release\astvcs.exe --repo $I commit --message "rename to alternate"
.\target\release\astvcs.exe --repo $I merge conflict --dry-run
```

`merge --dry-run` exits 1 on conflict; the repo is unchanged.

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
.\target\release\astvcs.exe --repo $D commit --message "baseline"

.\target\release\astvcs.exe --repo $D branch create feature
.\target\release\astvcs.exe --repo $D checkout --branch feature
Set-Content $D\sample.rs "fn foo() {`n    let x = 1;`n    let z = 2;`n}`n"
.\target\release\astvcs.exe --repo $D commit --message "insert on feature"

.\target\release\astvcs.exe --repo $D checkout --branch main
Set-Content $D\sample.rs "fn foo() {`n    let y = 1;`n}`n"
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
