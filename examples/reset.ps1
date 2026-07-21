# Reset example fixtures to baseline (repo root: .\examples\reset.ps1 or ./examples/reset.sh)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot

function Write-FixtureFile {
    param([string]$Path, [string]$Content)
    [System.IO.File]::WriteAllText($Path, $Content)
}

function Reset-Fixture {
    param([string]$Dir)
    Remove-Item -Recurse -Force (Join-Path $Dir ".astvcs") -ErrorAction SilentlyContinue
}

function Remove-IfExists {
    param([string]$Path)
    Remove-Item -Recurse -Force $Path -ErrorAction SilentlyContinue
}

Reset-Fixture (Join-Path $root "workflow-demo")
Write-FixtureFile (Join-Path $root "workflow-demo\lib.rs") "pub mod core;`npub mod util;`n"
Write-FixtureFile (Join-Path $root "workflow-demo\core.rs") "pub fn answer() -> i32 {`n    42`n}`n"
Write-FixtureFile (Join-Path $root "workflow-demo\util.rs") "pub fn label() -> &'static str {`n    `"base`"`n}`n"

Reset-Fixture (Join-Path $root "merge-demo")
Write-FixtureFile (Join-Path $root "merge-demo\lib.rs") "pub fn label() -> &'static str { `"base`" }`n"
Write-FixtureFile (Join-Path $root "merge-demo\config.toml") "[settings]`nenabled = true`n"
Remove-Item -Force (Join-Path $root "merge-demo\util.rs") -ErrorAction SilentlyContinue

Reset-Fixture (Join-Path $root "identity-demo")
Write-FixtureFile (Join-Path $root "identity-demo\core.rs") "pub fn answer() -> i32 {`n    42`n}`n"
Write-FixtureFile (Join-Path $root "identity-demo\labels.rs") "pub fn pair() -> (&'static str, &'static str) {`n    (`"alpha`", `"beta`")`n}`n"
Write-FixtureFile (Join-Path $root "identity-demo\conflict.rs") "fn sample() {`n    let value = 1;`n}`n"

Reset-Fixture (Join-Path $root "same-file-demo")
Write-FixtureFile (Join-Path $root "same-file-demo\sample.rs") "fn foo() {`n    let x = 1;`n}`n"
Remove-Item -Force (Join-Path $root "same-file-demo\main.rs") -ErrorAction SilentlyContinue

Reset-Fixture (Join-Path $root "go-eof-insert-demo")
Copy-Item -Force (Join-Path $root "go-eof-insert-demo\version.go.base") (Join-Path $root "go-eof-insert-demo\version.go")
Write-FixtureFile (Join-Path $root "go-eof-insert-demo\.astvcsignore") "version.go.base`nversion.go.ours`nversion.go.theirs`n"

Reset-Fixture (Join-Path $root "network-demo")
Write-FixtureFile (Join-Path $root "network-demo\note.txt") "v1`n"
Remove-IfExists (Join-Path $root "network-demo\_upstream")
Remove-IfExists (Join-Path $root "network-demo\_clone")

Reset-Fixture (Join-Path $root "lifecycle-demo")
Write-FixtureFile (Join-Path $root "lifecycle-demo\app.txt") "v1`n"
Remove-Item -Force (Join-Path $root "lifecycle-demo\feat.txt") -ErrorAction SilentlyContinue

Reset-Fixture (Join-Path $root "shallow-demo")
Write-FixtureFile (Join-Path $root "shallow-demo\note.txt") "v1`n"
Remove-IfExists (Join-Path $root "shallow-demo\_upstream")
Remove-IfExists (Join-Path $root "shallow-demo\_shallow")
Remove-IfExists (Join-Path $root "shallow-demo\_full")

Reset-Fixture (Join-Path $root "import-git-demo")
Write-FixtureFile (Join-Path $root "import-git-demo\hello.txt") "hello from git`n"

Reset-Fixture (Join-Path $root "serve-demo")
Write-FixtureFile (Join-Path $root "serve-demo\note.txt") "v1`n"
Remove-IfExists (Join-Path $root "serve-demo\_clone")

Write-Host "Reset examples: removed .astvcs and restored baseline source files."
