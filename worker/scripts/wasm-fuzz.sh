#!/usr/bin/env bash
# Run image.rs as wasm32 (32-bit usize) under node: fixed regressions + mutation fuzz.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q --release --target wasm32-unknown-unknown --manifest-path fuzz-wasm/Cargo.toml --target-dir target/fuzz-wasm
node fuzz-wasm/run.mjs target/fuzz-wasm/wasm32-unknown-unknown/release/ad_pool_image_fuzz_wasm.wasm tests/fixtures
