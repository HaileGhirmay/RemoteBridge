#!/usr/bin/env sh
# Same checks CI runs: formatting, clippy (warnings are errors), tests.
set -eu
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
