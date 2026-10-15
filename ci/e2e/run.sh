#!/usr/bin/env bash
# Runs dhis2-core's dhis-test-e2e suite against Java *through the gateway*.
#
# Usage: ci/e2e/run.sh
#
# Environment:
#   DHIS2_CORE_DIR       dhis2-core checkout (default: ../dhis2-core next to this repo).
#                        Only read; compose builds the test image from it.
#   DHIS2_IMAGE          Java server image (default: dhis2/core-dev:local, as upstream).
#   GATEWAY_IMAGE        gateway image (default: dhis2-rust/gateway:local, built unless
#                        SKIP_GATEWAY_BUILD=1).
#   GATEWAY_ROUTES_FILE  route table to mount (default: config/routes.toml).
#   SELENIUM_IMAGE       e.g. seleniarm/standalone-chromium on Apple Silicon.
#   DHIS2_E2E_TEST_ARGS  extra Maven args, e.g. "-Dtest=MeActionsTest".
#   KEEP_STACK=1         leave containers running afterwards.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
core=${DHIS2_CORE_DIR:-$repo/../dhis2-core}
e2e="$core/dhis-2/dhis-test-e2e"
[[ -f "$e2e/docker-compose.e2e.yml" ]] || { echo "no dhis-test-e2e in $core" >&2; exit 2; }

expected=$(cat "$repo/ci/dhis2-core.ref")
actual=$(git -C "$core" rev-parse HEAD 2>/dev/null || echo unknown)
if [[ "$actual" != "$expected" ]]; then
  echo "warning: dhis2-core is at $actual, ci/dhis2-core.ref pins $expected" >&2
fi

export GATEWAY_IMAGE=${GATEWAY_IMAGE:-dhis2-rust/gateway:local}
export GATEWAY_ROUTES_FILE=${GATEWAY_ROUTES_FILE:-$repo/config/routes.toml}
export SELENIUM_IMAGE=${SELENIUM_IMAGE:-selenium/standalone-chrome:latest}

if [[ -z "${SKIP_GATEWAY_BUILD:-}" ]]; then
  docker build -f "$repo/Dockerfile.gateway" -t "$GATEWAY_IMAGE" "$repo"
fi

compose=(docker compose --project-directory "$e2e"
  -f "$e2e/docker-compose.yml" -f "$e2e/docker-compose.e2e.yml"
  -f "$repo/ci/e2e/docker-compose.gateway.yml")

logs="$repo/target/e2e"
mkdir -p "$logs"
cleanup() {
  "${compose[@]}" logs --no-color gateway > "$logs/gateway.log" 2>&1 || true
  "${compose[@]}" logs --no-color web > "$logs/web.log" 2>&1 || true
  "${compose[@]}" cp test:/target/surefire-reports "$logs/" > /dev/null 2>&1 || true
  if [[ -z "${KEEP_STACK:-}" ]]; then
    "${compose[@]}" down --volumes --remove-orphans > /dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

"${compose[@]}" up --build --remove-orphans --exit-code-from test
