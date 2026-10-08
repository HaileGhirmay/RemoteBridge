# Same checks CI runs: formatting, clippy (warnings are errors), tests.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
cargo fmt --all -- --check
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo clippy --workspace --all-targets --all-features -- -D warnings
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo test --workspace --all-features
exit $LASTEXITCODE
