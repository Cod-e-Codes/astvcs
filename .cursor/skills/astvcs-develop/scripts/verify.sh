#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../../.."

echo "cargo test..."
cargo test

echo "cargo clippy..."
cargo clippy --all-targets --all-features -- -D warnings

echo "OK"
