#!/usr/bin/env bash
# Regenerates crates/dhis-schema/data/registered/schemas.json: the registered schemas
# (SchemaService#getSortedSchemas, Jackson rendering of Schema/Property) of the pinned build.
#
# Java computes them at startup from the schema descriptors, Jackson annotations and the Hibernate
# mappings, which cannot be run outside the server; so they are captured from a running server at
# the pinned ref (ADR 0007), through its own /api/schemas. A placeholder Host makes the
# request-dependent links recognisable (Rust substitutes the request's API path).
#
# Usage: tools/gen-registered-schemas.sh [java base url]   (default http://localhost:8081)
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
java=${1:-http://localhost:8081}
out="$repo/crates/dhis-schema/data/registered/schemas.json"
mkdir -p "$(dirname "$out")"
curl -sf -u "${DHIS2_USER:-admin}:${DHIS2_PASSWORD:-district}" \
  -H 'Host: rust-capture.invalid' -H 'Accept: application/json' \
  "$java/api/schemas" |
  python3 -c '
import json, sys
schemas = json.load(sys.stdin)["schemas"]
json.dump(schemas, sys.stdout, ensure_ascii=False, indent=1)
print()
' > "$out"
python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))), "schemas")' "$out"
