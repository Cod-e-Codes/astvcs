# Benchmark astvcs vs Git: parse/commit, diff, merge, repo size
# Usage: powershell -File benchmark-git.ps1 [-AstvcsPath <path>] [-WorkRoot <path>]

param(
    [string]$AstvcsPath = "",
    [string]$WorkRoot = ""
)

$ErrorActionPreference = "Stop"
$Astvcs = if ($AstvcsPath) { $AstvcsPath } else { Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..")).Path "target\release\astvcs.exe" }
if (-not (Test-Path $Astvcs)) { throw "astvcs binary not found at $Astvcs" }

$Root = if ($WorkRoot) { $WorkRoot } else { Join-Path $env:TEMP "astvcs-bench-$(Get-Date -Format 'yyyyMMdd-HHmmss')" }
New-Item -ItemType Directory -Force -Path $Root | Out-Null

function Measure-Cmd {
    param([string]$Label, [scriptblock]$Block, [int]$Runs = 3)
    $times = @()
    $saved = Get-Location
    for ($i = 0; $i -lt $Runs; $i++) {
        Set-Location $saved
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        & $Block $i | Out-Null
        $sw.Stop()
        $times += $sw.Elapsed.TotalMilliseconds
    }
    Set-Location $saved
    $sorted = $times | Sort-Object
    $median = $sorted[[int][math]::Floor($sorted.Count / 2)]
    [PSCustomObject]@{ Label = $Label; MedianMs = [math]::Round($median, 1); MinMs = [math]::Round(($times | Measure-Object -Minimum).Minimum, 1); MaxMs = [math]::Round(($times | Measure-Object -Maximum).Maximum, 1) }
}

function Dir-SizeBytes {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return 0 }
    (Get-ChildItem -Recurse -Force -File $Path -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
}

function Setup-AstvcsIdentity {
    param([string]$Repo)
    & $Astvcs --repo $Repo identity set --name "Bench" --email bench@example.com 2>$null
}

function Copy-Tree {
    param([string]$Src, [string]$Dst, [string[]]$Include = @("*"))
    New-Item -ItemType Directory -Force -Path $Dst | Out-Null
    foreach ($pattern in $Include) {
        Get-ChildItem $Src -Filter $pattern -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
            $rel = $_.FullName.Substring($Src.Length).TrimStart('\', '/')
            $target = Join-Path $Dst $rel
            $dir = Split-Path $target -Parent
            if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
            Copy-Item $_.FullName $target -Force
        }
    }
}

function New-LinearHistory-Git {
    param([string]$Repo, [int]$Commits)
    Set-Location $Repo
    git init -q
    git config user.email bench@example.com
    git config user.name Bench
    "v0" | Set-Content README.md
    git add -A; git commit -q -m "init"
    for ($i = 1; $i -lt $Commits; $i++) {
        Add-Content README.md "`nedit $i"
        # touch a source file cyclically
        $rs = Get-ChildItem -Recurse -Filter *.rs -File | Select-Object -First 1
        if ($rs) { Add-Content $rs.FullName "`n// bench $i" }
        git add -A; git commit -q -m "commit $i"
    }
}

function New-LinearHistory-Astvcs {
    param([string]$Repo, [int]$Commits)
    Set-Location $Repo
    & $Astvcs init | Out-Null
    Setup-AstvcsIdentity $Repo
    "v0" | Set-Content README.md
    & $Astvcs add -A | Out-Null
    & $Astvcs commit -m "init" --full-scan | Out-Null
    for ($i = 1; $i -lt $Commits; $i++) {
        Add-Content README.md "`nedit $i"
        $rs = Get-ChildItem -Recurse -Filter *.rs -File | Select-Object -First 1
        if ($rs) { Add-Content $rs.FullName "`n// bench $i" }
        & $Astvcs add -A | Out-Null
        & $Astvcs commit -m "commit $i" --full-scan | Out-Null
    }
}

function Prepare-MergeBranches {
    param([string]$Repo, [string]$Vcs)
    Set-Location $Repo
    if ($Vcs -eq "git") {
        git checkout -q -b feature
        "pub fn feature_side() -> i32 { 1 }" | Set-Content feature.rs
        git add -A; git commit -q -m "feature edit"
        git checkout -q main 2>$null; if ($LASTEXITCODE -ne 0) { git checkout -q master }
        "pub fn main_side() -> i32 { 2 }" | Set-Content main_edit.rs
        git add -A; git commit -q -m "main edit"
        git checkout -q feature
    } else {
        & $Astvcs branch create feature | Out-Null
        & $Astvcs checkout --branch feature | Out-Null
        "pub fn feature_side() -> i32 { 1 }" | Set-Content feature.rs
        & $Astvcs add -A | Out-Null; & $Astvcs commit -m "feature edit" --full-scan | Out-Null
        & $Astvcs checkout --branch main | Out-Null
        "pub fn main_side() -> i32 { 2 }" | Set-Content main_edit.rs
        & $Astvcs add -A | Out-Null; & $Astvcs commit -m "main edit" --full-scan | Out-Null
        & $Astvcs checkout --branch feature | Out-Null
    }
}

function Copy-Repo {
    param([string]$Src, [string]$Dst)
    Remove-Item $Dst -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $Dst | Out-Null
    robocopy $Src $Dst /E /NFL /NDL /NJH /NJS /nc /ns /np | Out-Null
}

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..")).Path
$results = @()
$sizes = @()

# --- Codebase 1: astvcs src (medium Rust) ---
$srcBench = Join-Path $Root "astvcs-src"
New-Item -ItemType Directory -Force -Path $srcBench | Out-Null
Copy-Item (Join-Path $ProjectRoot "src") (Join-Path $srcBench "src") -Recurse
Copy-Item (Join-Path $ProjectRoot "tests") (Join-Path $srcBench "tests") -Recurse
Copy-Item (Join-Path $ProjectRoot "Cargo.toml") (Join-Path $srcBench "Cargo.toml")
Copy-Item (Join-Path $ProjectRoot "Cargo.lock") (Join-Path $srcBench "Cargo.lock") -ErrorAction SilentlyContinue
$fileCount = (Get-ChildItem $srcBench -Recurse -File).Count
$workBytes = Dir-SizeBytes $srcBench

# Git initial commit
$gitRepo = Join-Path $Root "git-astvcs-src"
Remove-Item $gitRepo -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $srcBench $gitRepo -Recurse
$gitInit = Measure-Cmd "git: initial commit (astvcs-src)" {
    param($i)
    $r = Join-Path $Root "git-init-run-$i"
    Copy-Repo $srcBench $r
    Set-Location $r
    git init -q
    git config user.email bench@example.com
    git config user.name Bench
    git add -A
    git commit -q -m "initial"
}
$results += $gitInit

# astvcs initial commit (full scan = parse all)
$avRepo = Join-Path $Root "av-astvcs-src"
Remove-Item $avRepo -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $srcBench $avRepo -Recurse
$avInit = Measure-Cmd "astvcs: initial commit --full-scan (astvcs-src)" {
    param($i)
    $r = Join-Path $Root "av-init-run-$i"
    Copy-Repo $srcBench $r
    Set-Location $r
    & $Astvcs init | Out-Null
    Setup-AstvcsIdentity $r
    & $Astvcs add -A | Out-Null
    & $Astvcs commit -m "initial" --full-scan
}
# one-shot repos for size measurement
Copy-Repo $srcBench $gitRepo
Set-Location $gitRepo; git init -q; git config user.email bench@example.com; git config user.name Bench; git add -A; git commit -q -m "initial"
Copy-Repo $srcBench $avRepo
Set-Location $avRepo; & $Astvcs init | Out-Null; Setup-AstvcsIdentity $avRepo; & $Astvcs add -A | Out-Null; & $Astvcs commit -m "initial" --full-scan | Out-Null
$results += $avInit

# Diff HEAD vs empty (full tree diff) - use second commit with small edit
$gitHist = Join-Path $Root "git-hist"
Remove-Item $gitHist -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $srcBench $gitHist -Recurse
New-LinearHistory-Git $gitHist 20
$gitDiff = Measure-Cmd "git: diff HEAD~10..HEAD (20-commit history)" {
    Set-Location $gitHist
    git diff HEAD~10 HEAD | Out-Null
}
$results += $gitDiff

$avHist = Join-Path $Root "av-hist"
Remove-Item $avHist -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $srcBench $avHist -Recurse
New-LinearHistory-Astvcs $avHist 20
# get state ids for diff
Set-Location $avHist
$logLines = & $Astvcs log -n 20 2>&1
$stateIds = @()
foreach ($line in $logLines) {
    if ($line -match '\b([0-9a-f]{64})\b') { $stateIds += $Matches[1] }
}
$oldState = $stateIds[10]
$newState = $stateIds[0]
$avDiff = Measure-Cmd "astvcs: diff --base/--left/--right (20-commit span)" {
    Set-Location $avHist
    & $Astvcs diff --base $oldState --left $oldState --right $newState | Out-Null
}
$results += $avDiff

# Merge scenario
$gitMerge = Join-Path $Root "git-merge"
Remove-Item $gitMerge -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $srcBench $gitMerge -Recurse
Set-Location $gitMerge
git init -q; git config user.email bench@example.com; git config user.name Bench
git add -A; git commit -q -m "base"
Prepare-MergeBranches $gitMerge "git"
$gitMergeTime = Measure-Cmd "git: three-way merge (feature + main)" {
    param($i)
    $tmp = Join-Path $Root "git-merge-run-$i"
    Copy-Repo $gitMerge $tmp
    Set-Location $tmp
    git checkout -q feature
    git reset --hard -q HEAD
    git clean -fd -q
    git merge main --no-edit -q
}
$results += $gitMergeTime

$avMerge = Join-Path $Root "av-merge"
Remove-Item $avMerge -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $srcBench $avMerge -Recurse
Set-Location $avMerge
& $Astvcs init | Out-Null; Setup-AstvcsIdentity $avMerge
& $Astvcs add -A | Out-Null; & $Astvcs commit -m "base" --full-scan | Out-Null
Prepare-MergeBranches $avMerge "astvcs"
$avMergeTime = Measure-Cmd "astvcs: three-way merge --dry-run (feature + main)" {
    param($i)
    $tmp = Join-Path $Root "av-merge-run-$i"
    Copy-Repo $avMerge $tmp
    Set-Location $tmp
    & $Astvcs checkout --branch feature | Out-Null
    & $Astvcs merge main --dry-run
}
$results += $avMergeTime

# Repo sizes after 20-commit history
$gitHistSize = Dir-SizeBytes (Join-Path $gitHist ".git")
$avHistSize = Dir-SizeBytes (Join-Path $avHist ".astvcs")
$sizes += [PSCustomObject]@{
    Codebase = "astvcs-src ($fileCount files, $([math]::Round($workBytes/1KB)) KB working tree)"
    GitDotGitKB = [math]::Round($gitHistSize / 1KB, 1)
    AstvcsDotAstvcsKB = [math]::Round($avHistSize / 1KB, 1)
    RatioAstvcsToGit = if ($gitHistSize -gt 0) { [math]::Round($avHistSize / $gitHistSize, 2) } else { 0 }
    Commits = 20
}

# Initial commit sizes
$sizes += [PSCustomObject]@{
    Codebase = "astvcs-src initial commit only"
    GitDotGitKB = [math]::Round((Dir-SizeBytes (Join-Path $gitRepo ".git")) / 1KB, 1)
    AstvcsDotAstvcsKB = [math]::Round((Dir-SizeBytes (Join-Path $avRepo ".astvcs")) / 1KB, 1)
    RatioAstvcsToGit = if ((Dir-SizeBytes (Join-Path $gitRepo ".git")) -gt 0) {
        [math]::Round((Dir-SizeBytes (Join-Path $avRepo ".astvcs")) / (Dir-SizeBytes (Join-Path $gitRepo ".git")), 2)
    } else { 0 }
    Commits = 1
}

# --- Codebase 2: large single Rust file (stress parse/diff) ---
$largeDir = Join-Path $Root "large-file"
New-Item -ItemType Directory -Force -Path $largeDir | Out-Null
$bigRs = Join-Path $largeDir "big.rs"
$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine("pub fn f0() -> i32 { 0 }")
for ($i = 1; $i -le 2000; $i++) {
    [void]$sb.AppendLine("pub fn f$i() -> i32 { $i }")
}
$sb.ToString() | Set-Content $bigRs -NoNewline
$largeKB = [math]::Round((Get-Item $bigRs).Length / 1KB, 1)

$gitLargeInit = Measure-Cmd "git: initial commit (single ${largeKB}KB .rs)" {
    param($i)
    $r = Join-Path $Root "git-large-run-$i"
    Copy-Repo $largeDir $r
    Set-Location $r
    git init -q; git config user.email bench@example.com; git config user.name Bench
    git add -A; git commit -q -m "big"
}
$results += $gitLargeInit

$avLargeInit = Measure-Cmd "astvcs: initial commit --full-scan (single ${largeKB}KB .rs)" {
    param($i)
    $r = Join-Path $Root "av-large-run-$i"
    Copy-Repo $largeDir $r
    Set-Location $r
    & $Astvcs init | Out-Null; Setup-AstvcsIdentity $r
    & $Astvcs add -A | Out-Null; & $Astvcs commit -m "big" --full-scan
}
$results += $avLargeInit

# size repos (one-shot)
$gitLarge = Join-Path $Root "git-large"
$avLarge = Join-Path $Root "av-large"
Copy-Repo $largeDir $gitLarge
Set-Location $gitLarge; git init -q; git config user.email bench@example.com; git config user.name Bench; git add -A; git commit -q -m "big"
Copy-Repo $largeDir $avLarge
Set-Location $avLarge; & $Astvcs init | Out-Null; Setup-AstvcsIdentity $avLarge; & $Astvcs add -A | Out-Null; & $Astvcs commit -m "big" --full-scan | Out-Null

# edit large file for diff benchmark
$editedLarge = Join-Path $Root "large-file-edited"
Copy-Repo $largeDir $editedLarge
Add-Content (Join-Path $editedLarge "big.rs") "`npub fn injected() -> i32 { 999 }"
$gitLargeDiff = Measure-Cmd "git: diff large file (1-line add)" {
    param($i)
    $r = Join-Path $Root "git-large-diff-$i"
    Copy-Repo $largeDir $r
    Set-Location $r
    git init -q; git config user.email bench@example.com; git config user.name Bench
    git add -A; git commit -q -m "big"
    Add-Content (Join-Path $r "big.rs") "`npub fn injected() -> i32 { 999 }"
    git diff | Out-Null
}
$results += $gitLargeDiff
$avLargeDiff = Measure-Cmd "astvcs: diff large file (1-line add, unstaged)" {
    param($i)
    $r = Join-Path $Root "av-large-diff-$i"
    Copy-Repo $largeDir $r
    Set-Location $r
    & $Astvcs init | Out-Null; Setup-AstvcsIdentity $r
    & $Astvcs add -A | Out-Null; & $Astvcs commit -m "big" --full-scan | Out-Null
    Add-Content (Join-Path $r "big.rs") "`npub fn injected() -> i32 { 999 }"
    & $Astvcs diff | Out-Null
}
$results += $avLargeDiff

$sizes += [PSCustomObject]@{
    Codebase = "single large .rs ($largeKB KB)"
    GitDotGitKB = [math]::Round((Dir-SizeBytes (Join-Path $gitLarge ".git")) / 1KB, 1)
    AstvcsDotAstvcsKB = [math]::Round((Dir-SizeBytes (Join-Path $avLarge ".astvcs")) / 1KB, 1)
    RatioAstvcsToGit = [math]::Round((Dir-SizeBytes (Join-Path $avLarge ".astvcs")) / (Dir-SizeBytes (Join-Path $gitLarge ".git")), 2)
    Commits = 1
}

# --- Codebase 3: mixed extensions (clone ripgrep subset if available) ---
$rgRoot = Join-Path $Root "ripgrep"
if (-not (Test-Path (Join-Path $rgRoot "Cargo.toml"))) {
    git clone --depth 1 --quiet https://github.com/BurntSushi/ripgrep.git $rgRoot 2>$null
}
if (Test-Path (Join-Path $rgRoot "Cargo.toml")) {
    $rgFiles = (Get-ChildItem $rgRoot -Recurse -File | Where-Object { $_.FullName -notmatch '\\(\.git|target)\\' }).Count
    $rgKB = [math]::Round((Dir-SizeBytes $rgRoot) / 1KB, 0)

    $gitRgInit = Measure-Cmd "git: initial commit (ripgrep ~${rgKB}KB, $rgFiles files)" {
        param($i)
        $r = Join-Path $Root "git-rg-run-$i"
        robocopy $rgRoot $r /E /XD .git target /NFL /NDL /NJH /NJS /nc /ns /np | Out-Null
        Set-Location $r
        git init -q; git config user.email bench@example.com; git config user.name Bench
        git add -A; git commit -q -m "import"
    }
    $results += $gitRgInit

    $avRgInit = Measure-Cmd "astvcs: initial commit --full-scan (ripgrep)" {
        param($i)
        $r = Join-Path $Root "av-rg-run-$i"
        robocopy $rgRoot $r /E /XD .git target /NFL /NDL /NJH /NJS /nc /ns /np | Out-Null
        Set-Location $r
        & $Astvcs init | Out-Null; Setup-AstvcsIdentity $r
        & $Astvcs add -A | Out-Null; & $Astvcs commit -m "import" --full-scan
    }
    $results += $avRgInit

    $gitRg = Join-Path $Root "git-rg"
    $avRg = Join-Path $Root "av-rg"
    robocopy $rgRoot $gitRg /E /XD .git target /NFL /NDL /NJH /NJS /nc /ns /np | Out-Null
    Set-Location $gitRg; git init -q; git config user.email bench@example.com; git config user.name Bench; git add -A; git commit -q -m "import"
    robocopy $rgRoot $avRg /E /XD .git target /NFL /NDL /NJH /NJS /nc /ns /np | Out-Null
    Set-Location $avRg; & $Astvcs init | Out-Null; Setup-AstvcsIdentity $avRg; & $Astvcs add -A | Out-Null; & $Astvcs commit -m "import" --full-scan | Out-Null

    $sizes += [PSCustomObject]@{
        Codebase = "ripgrep ($rgFiles files, ~${rgKB}KB)"
        GitDotGitKB = [math]::Round((Dir-SizeBytes (Join-Path $gitRg ".git")) / 1KB, 1)
        AstvcsDotAstvcsKB = [math]::Round((Dir-SizeBytes (Join-Path $avRg ".astvcs")) / 1KB, 1)
        RatioAstvcsToGit = [math]::Round((Dir-SizeBytes (Join-Path $avRg ".astvcs")) / (Dir-SizeBytes (Join-Path $gitRg ".git")), 2)
        Commits = 1
    }
}

# repack astvcs for fair size comparison
Set-Location $avHist
& $Astvcs repack 2>$null | Out-Null
$avHistRepack = Dir-SizeBytes (Join-Path $avHist ".astvcs")
$sizes += [PSCustomObject]@{
    Codebase = "astvcs-src 20 commits after repack"
    GitDotGitKB = [math]::Round($gitHistSize / 1KB, 1)
    AstvcsDotAstvcsKB = [math]::Round($avHistRepack / 1KB, 1)
    RatioAstvcsToGit = [math]::Round($avHistRepack / $gitHistSize, 2)
    Commits = 20
}

Write-Host "`n=== BENCHMARK: astvcs vs Git ===" -ForegroundColor Cyan
Write-Host "astvcs: $Astvcs"
Write-Host "work dir: $Root"
Write-Host "`n--- Timing (median of 3 runs, ms) ---"
$results | Format-Table -AutoSize
Write-Host "`n--- Repository size (.git vs .astvcs, KB) ---"
$sizes | Format-Table -AutoSize

# export json for report
$report = @{
    astvcs_binary = $Astvcs
    work_root = $Root
    timings = $results
    sizes = $sizes
    platform = [System.Environment]::OSVersion.VersionString
    date = (Get-Date).ToString("o")
}
$reportPath = Join-Path $Root "benchmark-report.json"
$report | ConvertTo-Json -Depth 5 | Set-Content $reportPath
Write-Host "`nReport: $reportPath"
