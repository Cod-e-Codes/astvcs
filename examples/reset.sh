#!/usr/bin/env bash
# Reset example fixtures to baseline (repo root: ./examples/reset.sh)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

write_fixture_file() {
  printf '%s' "$2" >"$1"
}

reset_fixture() {
  rm -rf "$1/.astvcs"
}

remove_if_exists() {
  rm -rf "$1"
}

reset_fixture "$root/workflow-demo"
write_fixture_file "$root/workflow-demo/lib.rs" $'pub mod core;\npub mod util;\n'
write_fixture_file "$root/workflow-demo/core.rs" $'pub fn answer() -> i32 {\n    42\n}\n'
write_fixture_file "$root/workflow-demo/util.rs" $'pub fn label() -> &\'static str {\n    "base"\n}\n'

reset_fixture "$root/merge-demo"
write_fixture_file "$root/merge-demo/lib.rs" $'pub fn label() -> &\'static str { "base" }\n'
write_fixture_file "$root/merge-demo/config.toml" $'[settings]\nenabled = true\n'
rm -f "$root/merge-demo/util.rs"

reset_fixture "$root/identity-demo"
write_fixture_file "$root/identity-demo/core.rs" $'pub fn answer() -> i32 {\n    42\n}\n'
write_fixture_file "$root/identity-demo/labels.rs" $'pub fn pair() -> (&\'static str, &\'static str) {\n    ("alpha", "beta")\n}\n'
write_fixture_file "$root/identity-demo/conflict.rs" $'fn sample() {\n    let value = 1;\n}\n'

reset_fixture "$root/same-file-demo"
write_fixture_file "$root/same-file-demo/sample.rs" $'fn foo() {\n    let x = 1;\n}\n'
rm -f "$root/same-file-demo/main.rs"

reset_fixture "$root/go-eof-insert-demo"
cp "$root/go-eof-insert-demo/version.go.base" "$root/go-eof-insert-demo/version.go"
write_fixture_file "$root/go-eof-insert-demo/.astvcsignore" $'version.go.base\nversion.go.ours\nversion.go.theirs\n'

reset_fixture "$root/network-demo"
write_fixture_file "$root/network-demo/note.txt" $'v1\n'
remove_if_exists "$root/network-demo/_upstream"
remove_if_exists "$root/network-demo/_clone"

reset_fixture "$root/lifecycle-demo"
write_fixture_file "$root/lifecycle-demo/app.txt" $'v1\n'
rm -f "$root/lifecycle-demo/feat.txt"

reset_fixture "$root/shallow-demo"
write_fixture_file "$root/shallow-demo/note.txt" $'v1\n'
remove_if_exists "$root/shallow-demo/_upstream"
remove_if_exists "$root/shallow-demo/_shallow"
remove_if_exists "$root/shallow-demo/_full"

reset_fixture "$root/import-git-demo"
write_fixture_file "$root/import-git-demo/hello.txt" $'hello from git\n'

reset_fixture "$root/serve-demo"
write_fixture_file "$root/serve-demo/note.txt" $'v1\n'
remove_if_exists "$root/serve-demo/_clone"

echo "Reset examples: removed .astvcs and restored baseline source files."
