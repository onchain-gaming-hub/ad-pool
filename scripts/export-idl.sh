#!/usr/bin/env bash
# Export the IDL that consumers (FE / Worker / SDK) use, e.g. `declare_program!(ad_pool)`
# (reads `idls/<program_name>.json` from the nearest ancestor `idls/` dir).
# Run after `anchor build`; tests/test_idl_export.rs fails if this copy drifts.
set -euo pipefail
cd "$(dirname "$0")/.."
cp target/idl/ad_pool.json idls/ad_pool.json
echo "exported idls/ad_pool.json ($(wc -c < idls/ad_pool.json) bytes)"
