#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../../.."

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

echo "cargo test..."
cargo test

echo "cargo clippy..."
cargo clippy --all-targets --all-features -- -D warnings

echo "OK"
