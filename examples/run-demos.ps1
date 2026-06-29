# Run all fixture walkthroughs non-interactively (repo root: .\examples\run-demos.ps1)
param(
    [string]$LogPath = ""
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot

$astvcs = Join-Path $repoRoot "target\release\astvcs.exe"
$identity = @("identity", "set", "--name", "Example", "--email", "example@astvcs.local")

$cleanupDirs = [System.Collections.Generic.List[string]]::new()
$serveProcess = $null

function Write-FixtureFile {
    param([string]$Path, [string]$Content)
    $full = Join-Path $repoRoot $Path
    [System.IO.File]::WriteAllText($full, $Content)
}

function Write-Log {
    param([string]$Text)
    if ($LogPath) {
        Add-Content -Path $LogPath -Value $Text
    }
    Write-Host $Text
}

function Invoke-Astvcs {
    param(
        [string]$Repo,
        [string[]]$AstvcsArgs,
        [string]$Label
    )
    $allArgs = @()
    if ($Repo) { $allArgs += @("--repo", $Repo) }
    $allArgs += $AstvcsArgs
    Write-Log ""
    Write-Log ">>> $Label"
    Write-Log "astvcs $($allArgs -join ' ')"
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $out = & $astvcs @allArgs 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prevEap
    foreach ($line in $out) { Write-Log $line }
    if ($code -ne 0) {
        throw "astvcs failed ($code): $Label"
    }
}

function Test-GitAvailable {
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & git --version 2>&1 | Out-Null
    $ok = ($LASTEXITCODE -eq 0)
    $ErrorActionPreference = $prevEap
    return $ok
}

function Register-CleanupDir {
    param([string]$Path)
    $cleanupDirs.Add($Path) | Out-Null
}

function Stop-ServeProcess {
    if ($serveProcess -and -not $serveProcess.HasExited) {
        Write-Log "Stopping serve process (pid $($serveProcess.Id))..."
        Stop-Process -Id $serveProcess.Id -Force -ErrorAction SilentlyContinue
        $serveProcess.WaitForExit(5000) | Out-Null
    }
    $serveProcess = $null
}

if ($LogPath) {
    $logDir = Split-Path $LogPath -Parent
    if ($logDir -and -not (Test-Path $logDir)) {
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    }
    Set-Content -Path $LogPath -Value "astvcs demo run $(Get-Date -Format o)"
}

try {
    Write-Log "Building release binary..."
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & cargo build --release 2>&1 | ForEach-Object { Write-Log $_ }
    $buildCode = $LASTEXITCODE
    $ErrorActionPreference = $prevEap
    if ($buildCode -ne 0) { throw "cargo build failed" }

    Invoke-Astvcs "" @("--version") "version"
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }

    # --- workflow-demo ---
    $D = "examples\workflow-demo"
    Invoke-Astvcs "" @("init", $D) "workflow: init"
    Invoke-Astvcs $D $identity "workflow: identity"
    Invoke-Astvcs $D @("add", ".") "workflow: add baseline"
    Invoke-Astvcs $D @("commit", "--message", "baseline") "workflow: baseline"
    Write-FixtureFile $D\lib.rs "//! workflow demo crate`npub mod core;`npub mod util;`n"
    Invoke-Astvcs $D @("diff", "lib.rs") "workflow: diff prepend"
    Invoke-Astvcs $D @("commit", "--message", "prepend doc comment") "workflow: prepend commit"
    Invoke-Astvcs $D @("branch", "create", "feature") "workflow: branch feature"
    Invoke-Astvcs $D @("checkout", "--branch", "feature") "workflow: checkout feature"
    Write-FixtureFile $D\util.rs "pub fn label() -> &'static str {`n    `"feature-branch`"`n}`n"
    Invoke-Astvcs $D @("commit", "--message", "feature util label") "workflow: feature commit"
    Invoke-Astvcs $D @("checkout", "--branch", "main") "workflow: checkout main"
    Write-FixtureFile $D\core.rs "pub fn answer() -> i32 {`n    43`n}`n"
    Invoke-Astvcs $D @("commit", "--message", "main core answer") "workflow: main commit"
    $base = (& $astvcs --repo $D merge-base main feature | Select-Object -Last 1)
    Invoke-Astvcs $D @("diff", "--base", $base, "--left", "main", "--right", "feature", "core.rs") "workflow: three-way core"
    Invoke-Astvcs $D @("diff", "--base", $base, "--left", "main", "--right", "feature", "util.rs") "workflow: three-way util"
    Invoke-Astvcs $D @("merge", "feature", "--message", "merge feature into main") "workflow: merge"
    Invoke-Astvcs $D @("status") "workflow: status"
    Write-Log (Get-Content $D\util.rs -Raw)

    Write-FixtureFile $D\core.rs "pub fn answer() -> i32 {`n    99`n}`n"
    Invoke-Astvcs $D @("add", "core.rs") "workflow: stage for mixed reset"
    Invoke-Astvcs $D @("status") "workflow: staged status"
    $tip = (& $astvcs --repo $D log -n 1 | Select-Object -First 1).Split(" ", 2)[1]
    Invoke-Astvcs $D @("reset", "--mixed", $tip) "workflow: reset --mixed"
    Invoke-Astvcs $D @("status") "workflow: status after mixed reset"

    # --- merge-demo ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $D = "examples\merge-demo"
    Invoke-Astvcs "" @("init", $D) "merge: init"
    Invoke-Astvcs $D $identity "merge: identity"
    Invoke-Astvcs $D @("commit", "--message", "base") "merge: base"
    Invoke-Astvcs $D @("branch", "create", "feature") "merge: branch feature"
    Invoke-Astvcs $D @("checkout", "--branch", "feature") "merge: checkout feature"
    Write-FixtureFile $D\util.rs "pub fn util() {}`n"
    Write-FixtureFile $D\lib.rs "pub fn label() -> &'static str { `"feature`" }`n"
    Invoke-Astvcs $D @("commit", "--message", "feature util and lib") "merge: feature commit"
    Invoke-Astvcs $D @("checkout", "--branch", "main") "merge: checkout main"
    Write-FixtureFile $D\util.rs "pub fn util() {}`n"
    Invoke-Astvcs $D @("commit", "--message", "main util") "merge: main util"
    Invoke-Astvcs $D @("merge", "feature", "--message", "merge add/add") "merge: add/add"
    Write-Log (Get-Content $D\util.rs -Raw)
    Write-Log (Get-Content $D\lib.rs -Raw)
    Invoke-Astvcs $D @("checkout", "--branch", "main") "merge: checkout main for deletion"
    Invoke-Astvcs $D @("branch", "create", "feature2") "merge: branch feature2"
    Invoke-Astvcs $D @("checkout", "--branch", "feature2") "merge: checkout feature2"
    Invoke-Astvcs $D @("commit", "--message", "feature noop") "merge: feature noop"
    Invoke-Astvcs $D @("checkout", "--branch", "main") "merge: checkout main"
    Remove-Item $D\config.toml
    Invoke-Astvcs $D @("commit", "--message", "delete config on main") "merge: delete config"
    Invoke-Astvcs $D @("merge", "feature2", "--message", "merge deletion") "merge: deletion"
    Invoke-Astvcs $D @("status") "merge: status"

    # --- identity-demo ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $I = "examples\identity-demo"
    Invoke-Astvcs "" @("init", $I) "identity: init"
    Invoke-Astvcs $I $identity "identity: identity"
    Invoke-Astvcs $I @("commit", "--message", "baseline") "identity: baseline"
    Write-FixtureFile $I\core.rs "pub fn answer() -> i32 {`n    43`n}`n"
    Invoke-Astvcs $I @("diff", "core.rs") "identity: diff core"
    Invoke-Astvcs $I @("commit", "--message", "literal on main") "identity: literal main"
    Invoke-Astvcs $I @("branch", "create", "feature") "identity: branch feature"
    Invoke-Astvcs $I @("checkout", "--branch", "feature") "identity: checkout feature"
    Write-FixtureFile $I\labels.rs "pub fn pair() -> (&'static str, &'static str) {`n    (`"alpha`", `"BETA`")`n}`n"
    Invoke-Astvcs $I @("commit", "--message", "edit second literal") "identity: feature labels"
    Invoke-Astvcs $I @("checkout", "--branch", "main") "identity: checkout main"
    Write-FixtureFile $I\labels.rs "pub fn pair() -> (&'static str, &'static str) {`n    (`"ALPHA`", `"beta`")`n}`n"
    Invoke-Astvcs $I @("commit", "--message", "edit first literal") "identity: main labels"
    Invoke-Astvcs $I @("merge", "feature", "--message", "merge sibling literals") "identity: merge literals"
    Write-Log (Get-Content $I\labels.rs -Raw)
    Invoke-Astvcs $I @("branch", "create", "conflict") "identity: branch conflict"
    Invoke-Astvcs $I @("checkout", "--branch", "conflict") "identity: checkout conflict"
    Write-FixtureFile $I\conflict.rs "fn sample() {`n    let renamed = 1;`n}`n"
    Invoke-Astvcs $I @("commit", "--message", "rename to renamed") "identity: renamed"
    Invoke-Astvcs $I @("checkout", "--branch", "main") "identity: checkout main"
    Write-FixtureFile $I\conflict.rs "fn sample() {`n    let alternate = 1;`n}`n"
    Invoke-Astvcs $I @("commit", "--message", "rename to alternate") "identity: alternate"
    try {
        Invoke-Astvcs $I @("merge", "conflict", "--dry-run") "identity: dry-run conflict"
    } catch {
        Write-Log "identity: merge --dry-run exited non-zero (expected on conflict)"
    }
    Invoke-Astvcs $I @("merge", "conflict", "-m", "take feature side", "--resolve", "conflict.rs:theirs") "identity: resolve"

    # --- same-file-demo ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $D = "examples\same-file-demo"
    Invoke-Astvcs "" @("init", $D) "same-file: init"
    Invoke-Astvcs $D $identity "same-file: identity"
    Invoke-Astvcs $D @("commit", "--message", "baseline") "same-file: baseline"
    Invoke-Astvcs $D @("branch", "create", "feature") "same-file: branch feature"
    Invoke-Astvcs $D @("checkout", "--branch", "feature") "same-file: checkout feature"
    Write-FixtureFile $D\sample.rs "fn foo() {`n    let x = 1;`n    let z = 2;`n}`n"
    Invoke-Astvcs $D @("commit", "--message", "insert on feature") "same-file: feature insert"
    Invoke-Astvcs $D @("checkout", "--branch", "main") "same-file: checkout main"
    Write-FixtureFile $D\sample.rs "fn foo() {`n    let y = 1;`n}`n"
    Invoke-Astvcs $D @("commit", "--message", "rename on main") "same-file: main rename"
    $base = (& $astvcs --repo $D merge-base main feature | Select-Object -Last 1)
    Invoke-Astvcs $D @("diff", "--base", $base, "--left", "main", "--right", "feature", "sample.rs") "same-file: three-way"
    Invoke-Astvcs $D @("merge", "feature", "--message", "merge feature") "same-file: merge"
    Write-Log (Get-Content $D\sample.rs -Raw)

    # --- network-demo (file remote) ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $netRoot = "examples\network-demo"
    $upstream = Join-Path $netRoot "_upstream"
    $clone = Join-Path $netRoot "_clone"
    Register-CleanupDir (Join-Path $repoRoot $upstream)
    Register-CleanupDir (Join-Path $repoRoot $clone)
    Invoke-Astvcs "" @("init", $upstream) "network: init upstream"
    Invoke-Astvcs $upstream $identity "network: upstream identity"
    Write-FixtureFile "$upstream\note.txt" "v1`n"
    Invoke-Astvcs $upstream @("add", ".") "network: upstream add"
    Invoke-Astvcs $upstream @("commit", "--message", "v1") "network: upstream v1"
    Invoke-Astvcs "" @("clone", $upstream, $clone) "network: clone"
    Invoke-Astvcs $clone $identity "network: clone identity"
    Write-FixtureFile "$clone\note.txt" "v2`n"
    Invoke-Astvcs $clone @("commit", "-m", "v2") "network: clone commit v2"
    Invoke-Astvcs $clone @("push", "origin", "--branch", "main") "network: push"
    Write-Log (Get-Content "$clone\note.txt" -Raw)

    # --- lifecycle-demo ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $L = "examples\lifecycle-demo"
    Invoke-Astvcs "" @("init", $L) "lifecycle: init"
    Invoke-Astvcs $L $identity "lifecycle: identity"
    Write-FixtureFile "$L\app.txt" "line one`n"
    Invoke-Astvcs $L @("commit", "-m", "first line") "lifecycle: first line"
    Write-FixtureFile "$L\app.txt" "line one`nline two`n"
    Invoke-Astvcs $L @("commit", "-m", "add second line") "lifecycle: second line"
    Invoke-Astvcs $L @("blame", "app.txt") "lifecycle: blame"
    Invoke-Astvcs $L @("tag", "create", "v1.0", "main") "lifecycle: tag create"
    Invoke-Astvcs $L @("tag", "list") "lifecycle: tag list"
    Invoke-Astvcs $L @("branch", "create", "feature") "lifecycle: branch feature"
    Invoke-Astvcs $L @("checkout", "--branch", "feature") "lifecycle: checkout feature"
    Write-FixtureFile "$L\feat.txt" "one`n"
    Invoke-Astvcs $L @("add", "feat.txt") "lifecycle: add feat 1"
    Invoke-Astvcs $L @("commit", "-m", "feature 1") "lifecycle: feature 1"
    Write-FixtureFile "$L\feat.txt" "two`n"
    Invoke-Astvcs $L @("add", "feat.txt") "lifecycle: add feat 2"
    Invoke-Astvcs $L @("commit", "-m", "feature 2") "lifecycle: feature 2"
    Write-FixtureFile "$L\app.txt" "wip`n"
    Invoke-Astvcs $L @("stash", "push") "lifecycle: stash push"
    Invoke-Astvcs $L @("checkout", "--branch", "main") "lifecycle: checkout main after stash"
    Write-FixtureFile "$L\app.txt" "v2-main`n"
    Invoke-Astvcs $L @("add", "app.txt") "lifecycle: add main advance"
    Invoke-Astvcs $L @("commit", "-m", "main advance") "lifecycle: main advance"
    Invoke-Astvcs $L @("checkout", "--branch", "feature") "lifecycle: checkout feature"
    Invoke-Astvcs $L @("rebase", "main") "lifecycle: rebase main"
    Write-FixtureFile "$L\feat.txt" "three`n"
    Invoke-Astvcs $L @("add", "feat.txt") "lifecycle: add feat 3"
    Invoke-Astvcs $L @("commit", "-m", "feature 3") "lifecycle: feature 3"
    $pickId = (& $astvcs --repo $L log -n 1 | Select-Object -First 1).Split(" ", 2)[1]
    Invoke-Astvcs $L @("checkout", "--branch", "main") "lifecycle: checkout main for cherry-pick"
    Invoke-Astvcs $L @("cherry-pick", $pickId, "-m", "pick feature 3") "lifecycle: cherry-pick"
    Invoke-Astvcs $L @("status") "lifecycle: status"

    # --- shallow-demo ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $shallowRoot = "examples\shallow-demo"
    $shallowUpstream = Join-Path $shallowRoot "_upstream"
    $shallowClone = Join-Path $shallowRoot "_shallow"
    $fullClone = Join-Path $shallowRoot "_full"
    Register-CleanupDir (Join-Path $repoRoot $shallowUpstream)
    Register-CleanupDir (Join-Path $repoRoot $shallowClone)
    Register-CleanupDir (Join-Path $repoRoot $fullClone)
    Invoke-Astvcs "" @("init", $shallowUpstream) "shallow: init upstream"
    Invoke-Astvcs $shallowUpstream $identity "shallow: upstream identity"
    Write-FixtureFile "$shallowUpstream\note.txt" "v1`n"
    foreach ($i in 1..5) {
        if ($i -gt 1) { Write-FixtureFile "$shallowUpstream\note.txt" "v$i`n" }
        Invoke-Astvcs $shallowUpstream @("commit", "-m", "v$i") "shallow: commit v$i"
    }
    Invoke-Astvcs "" @("clone", "--depth", "2", $shallowUpstream, $shallowClone) "shallow: clone depth 2"
    Invoke-Astvcs "" @("clone", $shallowUpstream, $fullClone) "shallow: full clone"
    $shallowCount = (Get-ChildItem "$shallowClone\.astvcs\timeline" -File).Count
    $fullCount = (Get-ChildItem "$fullClone\.astvcs\timeline" -File).Count
    Write-Log "shallow timeline entries: $shallowCount (full: $fullCount)"
    if ($shallowCount -ge $fullCount) {
        throw "shallow clone should have fewer timeline entries than full clone"
    }
    if (-not (Test-Path "$shallowClone\.astvcs\shallow.json")) {
        throw "shallow.json missing in shallow clone"
    }

    # --- import-git-demo ---
    if (Test-GitAvailable) {
        $importParent = Join-Path $env:TEMP "astvcs-import-demo-$([guid]::NewGuid().ToString('N'))"
        $gitDir = Join-Path $importParent "git-repo"
        $astvcsDir = Join-Path $importParent "astvcs-repo"
        Register-CleanupDir $importParent
        New-Item -ItemType Directory -Force -Path $gitDir | Out-Null
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        & git -C $gitDir init 2>&1 | ForEach-Object { Write-Log $_ }
        if ($LASTEXITCODE -ne 0) { throw "git init failed" }
        $ErrorActionPreference = $prevEap
        [System.IO.File]::WriteAllText((Join-Path $gitDir "hello.txt"), "hello from git`n")
        $env:GIT_AUTHOR_NAME = "Example"
        $env:GIT_AUTHOR_EMAIL = "example@astvcs.local"
        $env:GIT_COMMITTER_NAME = "Example"
        $env:GIT_COMMITTER_EMAIL = "example@astvcs.local"
        & git -C $gitDir add hello.txt 2>&1 | ForEach-Object { Write-Log $_ }
        & git -C $gitDir commit -m "git baseline" 2>&1 | ForEach-Object { Write-Log $_ }
        New-Item -ItemType Directory -Force -Path $astvcsDir | Out-Null
        Invoke-Astvcs "" @("init", $astvcsDir) "import-git: init"
        Invoke-Astvcs $astvcsDir $identity "import-git: identity"
        Invoke-Astvcs $astvcsDir @("import-git", $gitDir, "-m", "Imported git snapshot") "import-git: import"
        Write-Log (Get-Content (Join-Path $astvcsDir "hello.txt") -Raw)
    } else {
        Write-Log ""
        Write-Log ">>> import-git: skipped (git not on PATH)"
    }

    # --- serve-demo (HTTP) ---
    & (Join-Path $PSScriptRoot "reset.ps1") 2>&1 | ForEach-Object { Write-Log $_ }
    $serveRoot = "examples\serve-demo"
    $serveClone = Join-Path $serveRoot "_clone"
    Register-CleanupDir (Join-Path $repoRoot $serveClone)
    Invoke-Astvcs "" @("init", $serveRoot) "serve: init"
    Invoke-Astvcs $serveRoot $identity "serve: identity"
    Invoke-Astvcs $serveRoot @("add", ".") "serve: add"
    Invoke-Astvcs $serveRoot @("commit", "-m", "v1") "serve: commit v1"
    $serveToken = "demo-serve-token"
    $servePort = 9421
    $serveProcess = Start-Process -FilePath $astvcs -ArgumentList @(
        "--repo", (Join-Path $repoRoot $serveRoot),
        "serve", "--token", $serveToken, "--port", "$servePort"
    ) -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 2
    if ($serveProcess.HasExited) {
        throw "serve process exited early (code $($serveProcess.ExitCode))"
    }
    Invoke-Astvcs "" @(
        "clone", "http://127.0.0.1:$servePort/", $serveClone,
        "--token", $serveToken
    ) "serve: http clone"
    Write-Log (Get-Content "$serveClone\note.txt" -Raw)
    Stop-ServeProcess

    Write-Log ""
    Write-Log "All fixture walkthroughs completed successfully."
}
finally {
    Stop-ServeProcess
    foreach ($dir in $cleanupDirs) {
        if (Test-Path $dir) {
            Write-Log "Cleaning up $dir"
            Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
        }
    }
}
