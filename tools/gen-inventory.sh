#!/usr/bin/env bash
# Regenerates docs/inventory/*.csv and crates/dhis-config/src/keys_generated.rs from the Java
# source. Pass --check to fail instead of writing when anything is out of date (CI does this).
#
# Usage: tools/gen-inventory.sh [--check]    (DHIS2_CORE_DIR defaults to ../dhis2-core)
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
core=${DHIS2_CORE_DIR:-$repo/../dhis2-core}
cd "$repo"
exec cargo run --quiet --locked -p dhis-inventory -- --java-root "$core" --repo-root "$repo" "$@"
