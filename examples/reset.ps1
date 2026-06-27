# Reset example fixtures to baseline (repo root: .\examples\reset.ps1)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot

function Reset-Fixture {
    param([string]$Dir)
    Remove-Item -Recurse -Force (Join-Path $Dir ".astvcs") -ErrorAction SilentlyContinue
}

Reset-Fixture (Join-Path $root "workflow-demo")
Set-Content -NoNewline -Path (Join-Path $root "workflow-demo\lib.rs") -Value "pub mod core;`npub mod util;`n"
Set-Content -Path (Join-Path $root "workflow-demo\core.rs") -Value "pub fn answer() -> i32 {`n    42`n}`n"
Set-Content -Path (Join-Path $root "workflow-demo\util.rs") -Value "pub fn label() -> &'static str {`n    `"base`"`n}`n"

Reset-Fixture (Join-Path $root "merge-demo")
Set-Content -Path (Join-Path $root "merge-demo\lib.rs") -Value "pub fn label() -> &'static str { `"base`" }`n"
Set-Content -Path (Join-Path $root "merge-demo\config.toml") -Value "[settings]`nenabled = true`n"
Remove-Item -Force (Join-Path $root "merge-demo\util.rs") -ErrorAction SilentlyContinue

Reset-Fixture (Join-Path $root "identity-demo")
Set-Content -Path (Join-Path $root "identity-demo\core.rs") -Value "pub fn answer() -> i32 {`n    42`n}`n"
Set-Content -Path (Join-Path $root "identity-demo\labels.rs") -Value "pub fn pair() -> (&'static str, &'static str) {`n    (`"alpha`", `"beta`")`n}`n"
Set-Content -Path (Join-Path $root "identity-demo\conflict.rs") -Value "fn sample() {`n    let value = 1;`n}`n"

Reset-Fixture (Join-Path $root "same-file-demo")
Set-Content -NoNewline -Path (Join-Path $root "same-file-demo\sample.rs") -Value "fn foo() {`n    let x = 1;`n}`n"
Remove-Item -Force (Join-Path $root "same-file-demo\main.rs") -ErrorAction SilentlyContinue

Write-Host "Reset examples: removed .astvcs and restored baseline source files."
