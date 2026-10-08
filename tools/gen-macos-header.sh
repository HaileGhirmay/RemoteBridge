#!/usr/bin/env sh
# Generate the C header for the macOS bridge. Requires: cargo install cbindgen
set -eu
cd "$(dirname "$0")/../platform/macos/bridge"
mkdir -p include
cbindgen --config cbindgen.toml --crate rb-platform-macos-bridge --output include/rb_macos_bridge.h
