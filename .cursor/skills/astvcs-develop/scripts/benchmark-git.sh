#!/usr/bin/env bash
# Benchmark astvcs vs Git: parse/commit, diff, merge, repo size
# Usage: bash benchmark-git.sh [--astvcs-path <path>] [--work-root <path>]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

ASTVCS_PATH=""
WORK_ROOT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --astvcs-path)
      ASTVCS_PATH="$2"
      shift 2
      ;;
    --work-root)
      WORK_ROOT="$2"
      shift 2
      ;;
    *)
      echo "usage: $0 [--astvcs-path <path>] [--work-root <path>]" >&2
      exit 1
      ;;
  esac
done

if [[ -n "$ASTVCS_PATH" ]]; then
  ASTVCS="$ASTVCS_PATH"
elif [[ -x "$PROJECT_ROOT/target/release/astvcs" ]]; then
  ASTVCS="$PROJECT_ROOT/target/release/astvcs"
elif [[ -f "$PROJECT_ROOT/target/release/astvcs.exe" ]]; then
  ASTVCS="$PROJECT_ROOT/target/release/astvcs.exe"
else
  ASTVCS="$PROJECT_ROOT/target/release/astvcs"
fi

if [[ ! -f "$ASTVCS" ]]; then
  echo "astvcs binary not found at $ASTVCS" >&2
  exit 1
fi

ROOT="${WORK_ROOT:-${TMPDIR:-/tmp}/astvcs-bench-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$ROOT"

declare -a TIMING_LABELS=()
declare -a TIMING_MEDIAN=()
declare -a TIMING_MIN=()
declare -a TIMING_MAX=()

declare -a SIZE_CODEBASE=()
declare -a SIZE_GIT_KB=()
declare -a SIZE_AV_KB=()
declare -a SIZE_RATIO=()
declare -a SIZE_COMMITS=()

now_ms() {
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import time; print(int(time.time() * 1000))'
  else
    date +%s%3N 2>/dev/null || echo $(($(date +%s) * 1000))
  fi
}

measure_cmd() {
  local label="$1"
  local runs="${2:-3}"
  local func="$3"
  local -a times=()
  local i start end ms sorted mid min max
  for ((i = 0; i < runs; i++)); do
    start="$(now_ms)"
    "$func" "$i" >/dev/null
    end="$(now_ms)"
    ms=$((end - start))
    times+=("$ms")
  done
  IFS=$'\n' sorted=($(printf '%s\n' "${times[@]}" | sort -n))
  mid="${sorted[$((runs / 2))]}"
  min="${sorted[0]}"
  max="${sorted[$((runs - 1))]}"
  TIMING_LABELS+=("$label")
  TIMING_MEDIAN+=("$mid")
  TIMING_MIN+=("$min")
  TIMING_MAX+=("$max")
}

dir_size_bytes() {
  local path="$1"
  if [[ ! -d "$path" ]]; then
    echo 0
    return
  fi
  du -sk "$path" 2>/dev/null | awk '{print $1 * 1024}'
}

setup_astvcs_identity() {
  local repo="$1"
  "$ASTVCS" --repo "$repo" identity set --name Bench --email bench@example.com >/dev/null 2>&1 || true
}

copy_repo() {
  local src="$1"
  local dst="$2"
  rm -rf "$dst"
  mkdir -p "$dst"
  cp -a "$src/." "$dst/"
}

copy_tree_excluding() {
  local src="$1"
  local dst="$2"
  rm -rf "$dst"
  mkdir -p "$dst"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a --exclude .git --exclude target "$src/" "$dst/"
  else
    (
      cd "$src"
      find . \( -path './.git' -o -path './.git/*' -o -path './target' -o -path './target/*' \) -prune -o -print
    ) | while IFS= read -r rel; do
      [[ "$rel" == . ]] && continue
      if [[ -d "$src/$rel" ]]; then
        mkdir -p "$dst/$rel"
      elif [[ -f "$src/$rel" ]]; then
        mkdir -p "$dst/$(dirname "$rel")"
        cp "$src/$rel" "$dst/$rel"
      fi
    done
  fi
}

new_linear_history_git() {
  local repo="$1"
  local commits="$2"
  local i rs
  cd "$repo"
  git init -q
  git config user.email bench@example.com
  git config user.name Bench
  printf 'v0\n' >README.md
  git add -A
  git commit -q -m init
  for ((i = 1; i < commits; i++)); do
    printf '\nedit %s\n' "$i" >>README.md
    rs="$(find . -name '*.rs' -type f | head -n 1)"
    if [[ -n "$rs" ]]; then
      printf '\n// bench %s\n' "$i" >>"$rs"
    fi
    git add -A
    git commit -q -m "commit $i"
  done
}

new_linear_history_astvcs() {
  local repo="$1"
  local commits="$2"
  local i rs
  cd "$repo"
  "$ASTVCS" init >/dev/null
  setup_astvcs_identity "$repo"
  printf 'v0\n' >README.md
  "$ASTVCS" add -A >/dev/null
  "$ASTVCS" commit -m init --full-scan >/dev/null
  for ((i = 1; i < commits; i++)); do
    printf '\nedit %s\n' "$i" >>README.md
    rs="$(find . -name '*.rs' -type f | head -n 1)"
    if [[ -n "$rs" ]]; then
      printf '\n// bench %s\n' "$i" >>"$rs"
    fi
    "$ASTVCS" add -A >/dev/null
    "$ASTVCS" commit -m "commit $i" --full-scan >/dev/null
  done
}

prepare_merge_branches() {
  local repo="$1"
  local vcs="$2"
  cd "$repo"
  if [[ "$vcs" == git ]]; then
    git checkout -q -b feature
    printf 'pub fn feature_side() -> i32 { 1 }\n' >feature.rs
    git add -A
    git commit -q -m "feature edit"
    git checkout -q main 2>/dev/null || git checkout -q master
    printf 'pub fn main_side() -> i32 { 2 }\n' >main_edit.rs
    git add -A
    git commit -q -m "main edit"
    git checkout -q feature
  else
    "$ASTVCS" branch create feature >/dev/null
    "$ASTVCS" checkout --branch feature >/dev/null
    printf 'pub fn feature_side() -> i32 { 1 }\n' >feature.rs
    "$ASTVCS" add -A >/dev/null
    "$ASTVCS" commit -m "feature edit" --full-scan >/dev/null
    "$ASTVCS" checkout --branch main >/dev/null
    printf 'pub fn main_side() -> i32 { 2 }\n' >main_edit.rs
    "$ASTVCS" add -A >/dev/null
    "$ASTVCS" commit -m "main edit" --full-scan >/dev/null
    "$ASTVCS" checkout --branch feature >/dev/null
  fi
}

record_size() {
  local codebase="$1"
  local git_kb="$2"
  local av_kb="$3"
  local ratio="$4"
  local commits="$5"
  SIZE_CODEBASE+=("$codebase")
  SIZE_GIT_KB+=("$git_kb")
  SIZE_AV_KB+=("$av_kb")
  SIZE_RATIO+=("$ratio")
  SIZE_COMMITS+=("$commits")
}

kb_round() {
  awk -v b="$1" 'BEGIN { printf "%.1f", b / 1024 }'
}

ratio_round() {
  awk -v a="$1" -v g="$2" 'BEGIN { if (g > 0) printf "%.2f", a / g; else print "0" }'
}

# --- Codebase 1: astvcs src ---
src_bench="$ROOT/astvcs-src"
mkdir -p "$src_bench"
cp -a "$PROJECT_ROOT/src" "$src_bench/"
cp -a "$PROJECT_ROOT/tests" "$src_bench/"
cp "$PROJECT_ROOT/Cargo.toml" "$src_bench/"
cp "$PROJECT_ROOT/Cargo.lock" "$src_bench/" 2>/dev/null || true
file_count="$(find "$src_bench" -type f | wc -l | tr -d ' ')"
work_bytes="$(dir_size_bytes "$src_bench")"

git_init_run() {
  local i="$1"
  local r="$ROOT/git-init-run-$i"
  copy_repo "$src_bench" "$r"
  cd "$r"
  git init -q
  git config user.email bench@example.com
  git config user.name Bench
  git add -A
  git commit -q -m initial
}
measure_cmd "git: initial commit (astvcs-src)" 3 git_init_run

av_init_run() {
  local i="$1"
  local r="$ROOT/av-init-run-$i"
  copy_repo "$src_bench" "$r"
  cd "$r"
  "$ASTVCS" init >/dev/null
  setup_astvcs_identity "$r"
  "$ASTVCS" add -A >/dev/null
  "$ASTVCS" commit -m initial --full-scan
}
measure_cmd "astvcs: initial commit --full-scan (astvcs-src)" 3 av_init_run

git_repo="$ROOT/git-astvcs-src"
av_repo="$ROOT/av-astvcs-src"
copy_repo "$src_bench" "$git_repo"
cd "$git_repo"
git init -q
git config user.email bench@example.com
git config user.name Bench
git add -A
git commit -q -m initial
copy_repo "$src_bench" "$av_repo"
cd "$av_repo"
"$ASTVCS" init >/dev/null
setup_astvcs_identity "$av_repo"
"$ASTVCS" add -A >/dev/null
"$ASTVCS" commit -m initial --full-scan >/dev/null

git_hist="$ROOT/git-hist"
copy_repo "$src_bench" "$git_hist"
new_linear_history_git "$git_hist" 20
git_diff_run() {
  cd "$git_hist"
  git diff HEAD~10 HEAD >/dev/null
}
measure_cmd "git: diff HEAD~10..HEAD (20-commit history)" 3 git_diff_run

av_hist="$ROOT/av-hist"
copy_repo "$src_bench" "$av_hist"
new_linear_history_astvcs "$av_hist" 20
cd "$av_hist"
log_out="$("$ASTVCS" log -n 20 2>&1)"
mapfile -t state_ids < <(printf '%s\n' "$log_out" | grep -oE '[0-9a-f]{64}')
old_state="${state_ids[10]}"
new_state="${state_ids[0]}"
av_diff_run() {
  cd "$av_hist"
  "$ASTVCS" diff --base "$old_state" --left "$old_state" --right "$new_state" >/dev/null
}
measure_cmd "astvcs: diff --base/--left/--right (20-commit span)" 3 av_diff_run

git_merge="$ROOT/git-merge"
copy_repo "$src_bench" "$git_merge"
cd "$git_merge"
git init -q
git config user.email bench@example.com
git config user.name Bench
git add -A
git commit -q -m base
prepare_merge_branches "$git_merge" git
git_merge_run() {
  local i="$1"
  local tmp="$ROOT/git-merge-run-$i"
  copy_repo "$git_merge" "$tmp"
  cd "$tmp"
  git checkout -q feature
  git reset --hard -q HEAD
  git clean -fd -q
  git merge main --no-edit -q
}
measure_cmd "git: three-way merge (feature + main)" 3 git_merge_run

av_merge="$ROOT/av-merge"
copy_repo "$src_bench" "$av_merge"
cd "$av_merge"
"$ASTVCS" init >/dev/null
setup_astvcs_identity "$av_merge"
"$ASTVCS" add -A >/dev/null
"$ASTVCS" commit -m base --full-scan >/dev/null
prepare_merge_branches "$av_merge" astvcs
av_merge_run() {
  local i="$1"
  local tmp="$ROOT/av-merge-run-$i"
  copy_repo "$av_merge" "$tmp"
  cd "$tmp"
  "$ASTVCS" checkout --branch feature >/dev/null
  "$ASTVCS" merge main --dry-run
}
measure_cmd "astvcs: three-way merge --dry-run (feature + main)" 3 av_merge_run

git_hist_size="$(dir_size_bytes "$git_hist/.git")"
av_hist_size="$(dir_size_bytes "$av_hist/.astvcs")"
record_size \
  "astvcs-src ($file_count files, $(kb_round "$work_bytes") KB working tree)" \
  "$(kb_round "$git_hist_size")" \
  "$(kb_round "$av_hist_size")" \
  "$(ratio_round "$av_hist_size" "$git_hist_size")" \
  20

git_init_size="$(dir_size_bytes "$git_repo/.git")"
av_init_size="$(dir_size_bytes "$av_repo/.astvcs")"
record_size \
  "astvcs-src initial commit only" \
  "$(kb_round "$git_init_size")" \
  "$(kb_round "$av_init_size")" \
  "$(ratio_round "$av_init_size" "$git_init_size")" \
  1

# --- Codebase 2: large single Rust file ---
large_dir="$ROOT/large-file"
mkdir -p "$large_dir"
big_rs="$large_dir/big.rs"
{
  printf 'pub fn f0() -> i32 { 0 }\n'
  for ((i = 1; i <= 2000; i++)); do
    printf 'pub fn f%s() -> i32 { %s }\n' "$i" "$i"
  done
} >"$big_rs"
large_kb="$(kb_round "$(wc -c <"$big_rs" | tr -d ' ')")"

git_large_init_run() {
  local i="$1"
  local r="$ROOT/git-large-run-$i"
  copy_repo "$large_dir" "$r"
  cd "$r"
  git init -q
  git config user.email bench@example.com
  git config user.name Bench
  git add -A
  git commit -q -m big
}
measure_cmd "git: initial commit (single ${large_kb}KB .rs)" 3 git_large_init_run

av_large_init_run() {
  local i="$1"
  local r="$ROOT/av-large-run-$i"
  copy_repo "$large_dir" "$r"
  cd "$r"
  "$ASTVCS" init >/dev/null
  setup_astvcs_identity "$r"
  "$ASTVCS" add -A >/dev/null
  "$ASTVCS" commit -m big --full-scan
}
measure_cmd "astvcs: initial commit --full-scan (single ${large_kb}KB .rs)" 3 av_large_init_run

git_large="$ROOT/git-large"
av_large="$ROOT/av-large"
copy_repo "$large_dir" "$git_large"
cd "$git_large"
git init -q
git config user.email bench@example.com
git config user.name Bench
git add -A
git commit -q -m big
copy_repo "$large_dir" "$av_large"
cd "$av_large"
"$ASTVCS" init >/dev/null
setup_astvcs_identity "$av_large"
"$ASTVCS" add -A >/dev/null
"$ASTVCS" commit -m big --full-scan >/dev/null

git_large_diff_run() {
  local i="$1"
  local r="$ROOT/git-large-diff-$i"
  copy_repo "$large_dir" "$r"
  cd "$r"
  git init -q
  git config user.email bench@example.com
  git config user.name Bench
  git add -A
  git commit -q -m big
  printf '\npub fn injected() -> i32 { 999 }\n' >>big.rs
  git diff >/dev/null
}
measure_cmd "git: diff large file (1-line add)" 3 git_large_diff_run

av_large_diff_run() {
  local i="$1"
  local r="$ROOT/av-large-diff-$i"
  copy_repo "$large_dir" "$r"
  cd "$r"
  "$ASTVCS" init >/dev/null
  setup_astvcs_identity "$r"
  "$ASTVCS" add -A >/dev/null
  "$ASTVCS" commit -m big --full-scan >/dev/null
  printf '\npub fn injected() -> i32 { 999 }\n' >>big.rs
  "$ASTVCS" diff >/dev/null
}
measure_cmd "astvcs: diff large file (1-line add, unstaged)" 3 av_large_diff_run

git_large_size="$(dir_size_bytes "$git_large/.git")"
av_large_size="$(dir_size_bytes "$av_large/.astvcs")"
record_size \
  "single large .rs (${large_kb} KB)" \
  "$(kb_round "$git_large_size")" \
  "$(kb_round "$av_large_size")" \
  "$(ratio_round "$av_large_size" "$git_large_size")" \
  1

# --- Codebase 3: ripgrep subset ---
rg_root="$ROOT/ripgrep"
if [[ ! -f "$rg_root/Cargo.toml" ]]; then
  git clone --depth 1 --quiet https://github.com/BurntSushi/ripgrep.git "$rg_root" 2>/dev/null || true
fi
if [[ -f "$rg_root/Cargo.toml" ]]; then
  rg_files="$(find "$rg_root" -type f ! -path '*/.git/*' ! -path '*/target/*' | wc -l | tr -d ' ')"
  rg_kb="$(kb_round "$(dir_size_bytes "$rg_root")")"

  git_rg_init_run() {
    local i="$1"
    local r="$ROOT/git-rg-run-$i"
    copy_tree_excluding "$rg_root" "$r"
    cd "$r"
    git init -q
    git config user.email bench@example.com
    git config user.name Bench
    git add -A
    git commit -q -m import
  }
  measure_cmd "git: initial commit (ripgrep ~${rg_kb}KB, $rg_files files)" 3 git_rg_init_run

  av_rg_init_run() {
    local i="$1"
    local r="$ROOT/av-rg-run-$i"
    copy_tree_excluding "$rg_root" "$r"
    cd "$r"
    "$ASTVCS" init >/dev/null
    setup_astvcs_identity "$r"
    "$ASTVCS" add -A >/dev/null
    "$ASTVCS" commit -m import --full-scan
  }
  measure_cmd "astvcs: initial commit --full-scan (ripgrep)" 3 av_rg_init_run

  git_rg="$ROOT/git-rg"
  av_rg="$ROOT/av-rg"
  copy_tree_excluding "$rg_root" "$git_rg"
  cd "$git_rg"
  git init -q
  git config user.email bench@example.com
  git config user.name Bench
  git add -A
  git commit -q -m import
  copy_tree_excluding "$rg_root" "$av_rg"
  cd "$av_rg"
  "$ASTVCS" init >/dev/null
  setup_astvcs_identity "$av_rg"
  "$ASTVCS" add -A >/dev/null
  "$ASTVCS" commit -m import --full-scan >/dev/null

  git_rg_size="$(dir_size_bytes "$git_rg/.git")"
  av_rg_size="$(dir_size_bytes "$av_rg/.astvcs")"
  record_size \
    "ripgrep ($rg_files files, ~${rg_kb}KB)" \
    "$(kb_round "$git_rg_size")" \
    "$(kb_round "$av_rg_size")" \
    "$(ratio_round "$av_rg_size" "$git_rg_size")" \
    1
fi

cd "$av_hist"
"$ASTVCS" repack >/dev/null 2>&1 || true
av_hist_repack="$(dir_size_bytes "$av_hist/.astvcs")"
record_size \
  "astvcs-src 20 commits after repack" \
  "$(kb_round "$git_hist_size")" \
  "$(kb_round "$av_hist_repack")" \
  "$(ratio_round "$av_hist_repack" "$git_hist_size")" \
  20

echo ""
echo "=== BENCHMARK: astvcs vs Git ==="
echo "astvcs: $ASTVCS"
echo "work dir: $ROOT"
echo ""
echo "--- Timing (median of 3 runs, ms) ---"
printf '%-55s %10s %10s %10s\n' Label MedianMs MinMs MaxMs
for ((i = 0; i < ${#TIMING_LABELS[@]}; i++)); do
  printf '%-55s %10s %10s %10s\n' "${TIMING_LABELS[$i]}" "${TIMING_MEDIAN[$i]}" "${TIMING_MIN[$i]}" "${TIMING_MAX[$i]}"
done
echo ""
echo "--- Repository size (.git vs .astvcs, KB) ---"
printf '%-55s %12s %18s %8s %8s\n' Codebase GitDotGitKB AstvcsDotAstvcsKB Ratio Commits
for ((i = 0; i < ${#SIZE_CODEBASE[@]}; i++)); do
  printf '%-55s %12s %18s %8s %8s\n' "${SIZE_CODEBASE[$i]}" "${SIZE_GIT_KB[$i]}" "${SIZE_AV_KB[$i]}" "${SIZE_RATIO[$i]}" "${SIZE_COMMITS[$i]}"
done

report_path="$ROOT/benchmark-report.json"
{
  echo '{'
  echo "  \"astvcs_binary\": $(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$ASTVCS"),"
  echo "  \"work_root\": $(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$ROOT"),"
  echo '  "timings": ['
  for ((i = 0; i < ${#TIMING_LABELS[@]}; i++)); do
    comma=","
    [[ $i -eq $((${#TIMING_LABELS[@]} - 1)) ]] && comma=""
    python3 -c 'import json,sys; print("    "+json.dumps({"Label":sys.argv[1],"MedianMs":float(sys.argv[2]),"MinMs":float(sys.argv[3]),"MaxMs":float(sys.argv[4])})+sys.argv[5])' \
      "${TIMING_LABELS[$i]}" "${TIMING_MEDIAN[$i]}" "${TIMING_MIN[$i]}" "${TIMING_MAX[$i]}" "$comma"
  done
  echo '  ],'
  echo '  "sizes": ['
  for ((i = 0; i < ${#SIZE_CODEBASE[@]}; i++)); do
    comma=","
    [[ $i -eq $((${#SIZE_CODEBASE[@]} - 1)) ]] && comma=""
    python3 -c 'import json,sys; print("    "+json.dumps({"Codebase":sys.argv[1],"GitDotGitKB":float(sys.argv[2]),"AstvcsDotAstvcsKB":float(sys.argv[3]),"RatioAstvcsToGit":float(sys.argv[4]),"Commits":int(sys.argv[5])})+sys.argv[6])' \
      "${SIZE_CODEBASE[$i]}" "${SIZE_GIT_KB[$i]}" "${SIZE_AV_KB[$i]}" "${SIZE_RATIO[$i]}" "${SIZE_COMMITS[$i]}" "$comma"
  done
  echo '  ],'
  echo "  \"platform\": $(python3 -c 'import json,platform; print(json.dumps(platform.platform()))'),"
  echo "  \"date\": $(python3 -c 'import json; from datetime import datetime, timezone; print(json.dumps(datetime.now(timezone.utc).isoformat()))')"
  echo '}'
} >"$report_path" 2>/dev/null || printf '{"astvcs_binary":"%s","work_root":"%s"}\n' "$ASTVCS" "$ROOT" >"$report_path"

echo ""
echo "Report: $report_path"
