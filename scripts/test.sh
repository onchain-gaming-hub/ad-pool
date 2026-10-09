#!/usr/bin/env bash
# Full local test run. Tests load the PRE-BUILT .so, so always build first.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
anchor build
cargo test -p ad_pool "$@"
