#!/usr/bin/env bash
# Run all fixture walkthroughs non-interactively (repo root: ./examples/run-demos.sh)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

LOG_PATH=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --log-path)
      LOG_PATH="$2"
      shift 2
      ;;
    *)
      echo "usage: $0 [--log-path <file>]" >&2
      exit 1
      ;;
  esac
done

if [[ -x "$REPO_ROOT/target/release/astvcs" ]]; then
  ASTVCS="$REPO_ROOT/target/release/astvcs"
elif [[ -f "$REPO_ROOT/target/release/astvcs.exe" ]]; then
  case "$(uname -s 2>/dev/null)" in
    MINGW* | MSYS* | CYGWIN*)
      ASTVCS="$REPO_ROOT/target/release/astvcs.exe"
      ;;
    *)
      if command -v astvcs.exe >/dev/null 2>&1; then
        ASTVCS="$(command -v astvcs.exe)"
      else
        echo "release binary not found; build with: cargo build --release" >&2
        echo "On WSL, build a Linux binary or run from Git Bash." >&2
        exit 1
      fi
      ;;
  esac
else
  echo "release binary not found at $REPO_ROOT/target/release/astvcs" >&2
  exit 1
fi

IDENTITY=(identity set --name Example --email example@astvcs.local)
CLEANUP_DIRS=()
SERVE_PID=""

log_first_state_id() {
  local repo="$1"
  local log_out
  log_out="$("$ASTVCS" --repo "$repo" log -n 1 2>&1)"
  printf '%s\n' "$log_out" | head -n 1 | cut -d' ' -f2-
}

merge_base_id() {
  local repo="$1"
  shift
  local log_out
  log_out="$("$ASTVCS" --repo "$repo" merge-base "$@" 2>&1)"
  printf '%s\n' "$log_out" | tail -n 1
}

write_fixture_file() {
  local path="$1"
  local content="$2"
  local full="$path"
  case "$path" in
    /* | [A-Za-z]:/*) full="$path" ;;
    *) full="$REPO_ROOT/$path" ;;
  esac
  mkdir -p "$(dirname "$full")"
  printf '%s' "$content" >"$full"
}

write_log() {
  if [[ -n "$LOG_PATH" ]]; then
    printf '%s\n' "$1" >>"$LOG_PATH"
  fi
  printf '%s\n' "$1"
}

invoke_astvcs() {
  local repo="${1:-}"
  shift
  local label="$1"
  shift
  local -a args=()
  if [[ -n "$repo" ]]; then
    args+=(--repo "$repo")
  fi
  args+=("$@")
  write_log ""
  write_log ">>> $label"
  write_log "astvcs ${args[*]}"
  local out code
  set +e
  out="$("$ASTVCS" "${args[@]}" 2>&1)"
  code=$?
  set -e
  while IFS= read -r line; do write_log "$line"; done <<<"$out"
  if [[ $code -ne 0 ]]; then
    echo "astvcs failed ($code): $label" >&2
    return "$code"
  fi
}

git_available() {
  command -v git >/dev/null 2>&1
}

register_cleanup_dir() {
  CLEANUP_DIRS+=("$1")
}

stop_serve_process() {
  if [[ -n "$SERVE_PID" ]] && kill -0 "$SERVE_PID" 2>/dev/null; then
    write_log "Stopping serve process (pid $SERVE_PID)..."
    kill "$SERVE_PID" 2>/dev/null || true
    wait "$SERVE_PID" 2>/dev/null || true
  fi
  SERVE_PID=""
}

cleanup() {
  stop_serve_process
  local dir
  for dir in "${CLEANUP_DIRS[@]}"; do
    if [[ -d "$dir" ]]; then
      write_log "Cleaning up $dir"
      rm -rf "$dir"
    fi
  done
}

trap cleanup EXIT

if [[ -n "$LOG_PATH" ]]; then
  mkdir -p "$(dirname "$LOG_PATH")"
  printf 'astvcs demo run %s\n' "$(date -Iseconds 2>/dev/null || date)" >"$LOG_PATH"
fi

write_log "Building release binary..."
build_out=""
if ! build_out="$(cargo build --release 2>&1)"; then
  while IFS= read -r line; do write_log "$line"; done <<<"$build_out"
  echo "cargo build failed" >&2
  exit 1
fi
while IFS= read -r line; do write_log "$line"; done <<<"$build_out"

invoke_astvcs "" "version" --version
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done

# --- workflow-demo ---
D="examples/workflow-demo"
invoke_astvcs "" "workflow: init" init "$D"
invoke_astvcs "$D" "workflow: identity" "${IDENTITY[@]}"
invoke_astvcs "$D" "workflow: add baseline" add .
invoke_astvcs "$D" "workflow: baseline" commit --message baseline
write_fixture_file "$D/lib.rs" $'//! workflow demo crate\npub mod core;\npub mod util;\n'
invoke_astvcs "$D" "workflow: diff prepend" diff lib.rs
invoke_astvcs "$D" "workflow: add prepend" add lib.rs
invoke_astvcs "$D" "workflow: prepend commit" commit --message "prepend doc comment"
invoke_astvcs "$D" "workflow: branch feature" branch create feature
invoke_astvcs "$D" "workflow: checkout feature" checkout --branch feature
write_fixture_file "$D/util.rs" $'pub fn label() -> &\'static str {\n    "feature-branch"\n}\n'
invoke_astvcs "$D" "workflow: add feature util" add util.rs
invoke_astvcs "$D" "workflow: feature commit" commit --message "feature util label"
invoke_astvcs "$D" "workflow: checkout main" checkout --branch main
write_fixture_file "$D/core.rs" $'pub fn answer() -> i32 {\n    43\n}\n'
invoke_astvcs "$D" "workflow: add main core" add core.rs
invoke_astvcs "$D" "workflow: main commit" commit --message "main core answer"
base="$(merge_base_id "$D" main feature)"
invoke_astvcs "$D" "workflow: three-way core" diff --base "$base" --left main --right feature core.rs
invoke_astvcs "$D" "workflow: three-way util" diff --base "$base" --left main --right feature util.rs
invoke_astvcs "$D" "workflow: merge" merge feature --message "merge feature into main"
invoke_astvcs "$D" "workflow: status" status
write_log "$(cat "$REPO_ROOT/$D/util.rs")"

write_fixture_file "$D/core.rs" $'pub fn answer() -> i32 {\n    99\n}\n'
invoke_astvcs "$D" "workflow: stage for mixed reset" add core.rs
invoke_astvcs "$D" "workflow: staged status" status
tip="$(log_first_state_id "$D")"
invoke_astvcs "$D" "workflow: reset --mixed" reset --mixed "$tip"
invoke_astvcs "$D" "workflow: status after mixed reset" status

# --- merge-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
D="examples/merge-demo"
invoke_astvcs "" "merge: init" init "$D"
invoke_astvcs "$D" "merge: identity" "${IDENTITY[@]}"
invoke_astvcs "$D" "merge: base" commit --message base
invoke_astvcs "$D" "merge: branch feature" branch create feature
invoke_astvcs "$D" "merge: checkout feature" checkout --branch feature
write_fixture_file "$D/util.rs" $'pub fn util() {}\n'
write_fixture_file "$D/lib.rs" $'pub fn label() -> &\'static str { "feature" }\n'
invoke_astvcs "$D" "merge: feature commit" commit --message "feature util and lib"
invoke_astvcs "$D" "merge: checkout main" checkout --branch main
write_fixture_file "$D/util.rs" $'pub fn util() {}\n'
invoke_astvcs "$D" "merge: main util" commit --message "main util"
invoke_astvcs "$D" "merge: add/add" merge feature --message "merge add/add"
write_log "$(cat "$REPO_ROOT/$D/util.rs")"
write_log "$(cat "$REPO_ROOT/$D/lib.rs")"
invoke_astvcs "$D" "merge: checkout main for deletion" checkout --branch main
invoke_astvcs "$D" "merge: branch feature2" branch create feature2
invoke_astvcs "$D" "merge: checkout feature2" checkout --branch feature2
invoke_astvcs "$D" "merge: feature noop" commit --message "feature noop"
invoke_astvcs "$D" "merge: checkout main" checkout --branch main
rm -f "$REPO_ROOT/$D/config.toml"
invoke_astvcs "$D" "merge: delete config" commit --message "delete config on main"
invoke_astvcs "$D" "merge: deletion" merge feature2 --message "merge deletion"
invoke_astvcs "$D" "merge: status" status

# --- identity-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
I="examples/identity-demo"
invoke_astvcs "" "identity: init" init "$I"
invoke_astvcs "$I" "identity: identity" "${IDENTITY[@]}"
invoke_astvcs "$I" "identity: baseline" commit --message baseline
write_fixture_file "$I/core.rs" $'pub fn answer() -> i32 {\n    43\n}\n'
invoke_astvcs "$I" "identity: diff core" diff core.rs
invoke_astvcs "$I" "identity: literal main" commit --message "literal on main"
invoke_astvcs "$I" "identity: branch feature" branch create feature
invoke_astvcs "$I" "identity: checkout feature" checkout --branch feature
write_fixture_file "$I/labels.rs" $'pub fn pair() -> (&\'static str, &\'static str) {\n    ("alpha", "BETA")\n}\n'
invoke_astvcs "$I" "identity: feature labels" commit --message "edit second literal"
invoke_astvcs "$I" "identity: checkout main" checkout --branch main
write_fixture_file "$I/labels.rs" $'pub fn pair() -> (&\'static str, &\'static str) {\n    ("ALPHA", "beta")\n}\n'
invoke_astvcs "$I" "identity: main labels" commit --message "edit first literal"
invoke_astvcs "$I" "identity: merge literals" merge feature --message "merge sibling literals"
write_log "$(cat "$REPO_ROOT/$I/labels.rs")"
invoke_astvcs "$I" "identity: branch conflict" branch create conflict
invoke_astvcs "$I" "identity: checkout conflict" checkout --branch conflict
write_fixture_file "$I/conflict.rs" $'fn sample() {\n    let renamed = 1;\n}\n'
invoke_astvcs "$I" "identity: renamed" commit --message "rename to renamed"
invoke_astvcs "$I" "identity: checkout main" checkout --branch main
write_fixture_file "$I/conflict.rs" $'fn sample() {\n    let alternate = 1;\n}\n'
invoke_astvcs "$I" "identity: alternate" commit --message "rename to alternate"
  if ! invoke_astvcs "$I" "identity: dry-run conflict" merge conflict --dry-run 2>/dev/null; then
    write_log "identity: merge --dry-run exited non-zero (expected on conflict)"
  fi
invoke_astvcs "$I" "identity: resolve" merge conflict -m "take feature side" --resolve conflict.rs:theirs

# --- same-file-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
D="examples/same-file-demo"
invoke_astvcs "" "same-file: init" init "$D"
invoke_astvcs "$D" "same-file: identity" "${IDENTITY[@]}"
invoke_astvcs "$D" "same-file: baseline" commit --message baseline
invoke_astvcs "$D" "same-file: branch feature" branch create feature
invoke_astvcs "$D" "same-file: checkout feature" checkout --branch feature
write_fixture_file "$D/sample.rs" $'fn foo() {\n    let x = 1;\n    let z = 2;\n}\n'
invoke_astvcs "$D" "same-file: feature insert" commit --message "insert on feature"
invoke_astvcs "$D" "same-file: checkout main" checkout --branch main
write_fixture_file "$D/sample.rs" $'fn foo() {\n    let y = 1;\n}\n'
invoke_astvcs "$D" "same-file: main rename" commit --message "rename on main"
base="$(merge_base_id "$D" main feature)"
invoke_astvcs "$D" "same-file: three-way" diff --base "$base" --left main --right feature sample.rs
invoke_astvcs "$D" "same-file: merge" merge feature --message "merge feature"
write_log "$(cat "$REPO_ROOT/$D/sample.rs")"

# --- network-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
net_root="examples/network-demo"
upstream="$net_root/_upstream"
clone="$net_root/_clone"
register_cleanup_dir "$REPO_ROOT/$upstream"
register_cleanup_dir "$REPO_ROOT/$clone"
invoke_astvcs "" "network: init upstream" init "$upstream"
invoke_astvcs "$upstream" "network: upstream identity" "${IDENTITY[@]}"
write_fixture_file "$upstream/note.txt" $'v1\n'
invoke_astvcs "$upstream" "network: upstream add" add .
invoke_astvcs "$upstream" "network: upstream v1" commit --message v1
invoke_astvcs "" "network: clone" clone "$upstream" "$clone"
invoke_astvcs "$clone" "network: clone identity" "${IDENTITY[@]}"
write_fixture_file "$clone/note.txt" $'v2\n'
invoke_astvcs "$clone" "network: clone commit v2" commit -m v2
invoke_astvcs "$clone" "network: push" push origin --branch main
write_log "$(cat "$REPO_ROOT/$clone/note.txt")"

# --- lifecycle-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
L="examples/lifecycle-demo"
invoke_astvcs "" "lifecycle: init" init "$L"
invoke_astvcs "$L" "lifecycle: identity" "${IDENTITY[@]}"
write_fixture_file "$L/app.txt" $'line one\n'
invoke_astvcs "$L" "lifecycle: first line" commit -m "first line"
write_fixture_file "$L/app.txt" $'line one\nline two\n'
invoke_astvcs "$L" "lifecycle: second line" commit -m "add second line"
invoke_astvcs "$L" "lifecycle: blame" blame app.txt
invoke_astvcs "$L" "lifecycle: tag create" tag create v1.0 main
invoke_astvcs "$L" "lifecycle: tag list" tag list
invoke_astvcs "$L" "lifecycle: branch feature" branch create feature
invoke_astvcs "$L" "lifecycle: checkout feature" checkout --branch feature
write_fixture_file "$L/feat.txt" $'one\n'
invoke_astvcs "$L" "lifecycle: add feat 1" add feat.txt
invoke_astvcs "$L" "lifecycle: feature 1" commit -m "feature 1"
write_fixture_file "$L/feat.txt" $'two\n'
invoke_astvcs "$L" "lifecycle: add feat 2" add feat.txt
invoke_astvcs "$L" "lifecycle: feature 2" commit -m "feature 2"
write_fixture_file "$L/app.txt" $'wip\n'
invoke_astvcs "$L" "lifecycle: stash push" stash push
invoke_astvcs "$L" "lifecycle: checkout main after stash" checkout --branch main
write_fixture_file "$L/app.txt" $'v2-main\n'
invoke_astvcs "$L" "lifecycle: add main advance" add app.txt
invoke_astvcs "$L" "lifecycle: main advance" commit -m "main advance"
invoke_astvcs "$L" "lifecycle: checkout feature" checkout --branch feature
invoke_astvcs "$L" "lifecycle: rebase main" rebase main
write_fixture_file "$L/feat.txt" $'three\n'
invoke_astvcs "$L" "lifecycle: add feat 3" add feat.txt
invoke_astvcs "$L" "lifecycle: feature 3" commit -m "feature 3"
pick_id="$(log_first_state_id "$L")"
invoke_astvcs "$L" "lifecycle: checkout main for cherry-pick" checkout --branch main
invoke_astvcs "$L" "lifecycle: cherry-pick" cherry-pick "$pick_id" -m "pick feature 3"
invoke_astvcs "$L" "lifecycle: status" status

# --- shallow-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
shallow_root="examples/shallow-demo"
shallow_upstream="$shallow_root/_upstream"
shallow_clone="$shallow_root/_shallow"
full_clone="$shallow_root/_full"
register_cleanup_dir "$REPO_ROOT/$shallow_upstream"
register_cleanup_dir "$REPO_ROOT/$shallow_clone"
register_cleanup_dir "$REPO_ROOT/$full_clone"
invoke_astvcs "" "shallow: init upstream" init "$shallow_upstream"
invoke_astvcs "$shallow_upstream" "shallow: upstream identity" "${IDENTITY[@]}"
write_fixture_file "$shallow_upstream/note.txt" $'v1\n'
for i in 1 2 3 4 5; do
  if [[ "$i" -gt 1 ]]; then
    write_fixture_file "$shallow_upstream/note.txt" "v$i"$'\n'
  fi
  invoke_astvcs "$shallow_upstream" "shallow: commit v$i" commit -m "v$i"
done
invoke_astvcs "" "shallow: clone depth 2" clone --depth 2 "$shallow_upstream" "$shallow_clone"
invoke_astvcs "" "shallow: full clone" clone "$shallow_upstream" "$full_clone"
shallow_count="$(find "$REPO_ROOT/$shallow_clone/.astvcs/timeline" -maxdepth 1 -type f | wc -l | tr -d ' ')"
full_count="$(find "$REPO_ROOT/$full_clone/.astvcs/timeline" -maxdepth 1 -type f | wc -l | tr -d ' ')"
write_log "shallow timeline entries: $shallow_count (full: $full_count)"
if [[ "$shallow_count" -ge "$full_count" ]]; then
  echo "shallow clone should have fewer timeline entries than full clone" >&2
  exit 1
fi
if [[ ! -f "$REPO_ROOT/$shallow_clone/.astvcs/shallow.json" ]]; then
  echo "shallow.json missing in shallow clone" >&2
  exit 1
fi

# --- import-git-demo ---
if git_available; then
  import_parent="$(mktemp -d "${TMPDIR:-/tmp}/astvcs-import-demo-XXXXXX")"
  git_dir="$import_parent/git-repo"
  astvcs_dir="$import_parent/astvcs-repo"
  register_cleanup_dir "$import_parent"
  mkdir -p "$git_dir"
  if ! git -C "$git_dir" init > >(while IFS= read -r line; do write_log "$line"; done) 2> >(while IFS= read -r line; do write_log "$line"; done); then
    echo "git init failed" >&2
    exit 1
  fi
  write_fixture_file "$git_dir/hello.txt" $'hello from git\n'
  export GIT_AUTHOR_NAME=Example
  export GIT_AUTHOR_EMAIL=example@astvcs.local
  export GIT_COMMITTER_NAME=Example
  export GIT_COMMITTER_EMAIL=example@astvcs.local
  git -C "$git_dir" add hello.txt 2>&1 | while IFS= read -r line; do write_log "$line"; done
  git -C "$git_dir" commit -m "git baseline" 2>&1 | while IFS= read -r line; do write_log "$line"; done
  mkdir -p "$astvcs_dir"
  invoke_astvcs "" "import-git: init" init "$astvcs_dir"
  invoke_astvcs "$astvcs_dir" "import-git: identity" "${IDENTITY[@]}"
  invoke_astvcs "$astvcs_dir" "import-git: import" import-git "$git_dir" -m "Imported git snapshot"
  write_log "$(cat "$astvcs_dir/hello.txt")"
else
  write_log ""
  write_log ">>> import-git: skipped (git not on PATH)"
fi

# --- serve-demo ---
"$SCRIPT_DIR/reset.sh" 2>&1 | while IFS= read -r line; do write_log "$line"; done
serve_root="examples/serve-demo"
serve_clone="$serve_root/_clone"
register_cleanup_dir "$REPO_ROOT/$serve_clone"
invoke_astvcs "" "serve: init" init "$serve_root"
invoke_astvcs "$serve_root" "serve: identity" "${IDENTITY[@]}"
invoke_astvcs "$serve_root" "serve: add" add .
invoke_astvcs "$serve_root" "serve: commit v1" commit -m v1
serve_token="demo-serve-token"
serve_port=9421
"$ASTVCS" --repo "$REPO_ROOT/$serve_root" serve --token "$serve_token" --port "$serve_port" &
SERVE_PID=$!
sleep 2
if ! kill -0 "$SERVE_PID" 2>/dev/null; then
  echo "serve process exited early" >&2
  exit 1
fi
invoke_astvcs "" "serve: http clone" clone "http://127.0.0.1:$serve_port/" "$serve_clone" --token "$serve_token"
write_log "$(cat "$REPO_ROOT/$serve_clone/note.txt")"
stop_serve_process

write_log ""
write_log "All fixture walkthroughs completed successfully."
