#!/bin/sh
# Rebuilds loyalty.wasm, the test plugin, from loyalty/:
# rustup target add wasm32-unknown-unknown first.
set -e
cd "$(dirname "$0")"
T="${CARGO_TARGET_DIR:-/tmp/soat-plugin-fixtures}"
cargo build --release --target wasm32-unknown-unknown --manifest-path loyalty/Cargo.toml --target-dir "$T"
cargo run --release --manifest-path componentize/Cargo.toml --target-dir "$T" -- \
    "$T/wasm32-unknown-unknown/release/loyalty.wasm" loyalty.wasm
