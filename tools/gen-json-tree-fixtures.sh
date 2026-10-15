#!/usr/bin/env bash
# Regenerates crates/dhis-query/tests/fixtures/json-tree-doubles.tsv: how json-tree (the JSON
# writer of the Gist API) prints doubles, run with the server image's Java and json-tree jar.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
image=${DHIS2_IMAGE:-dhis2/core-dev:local}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
app=${DHIS2_APP_DIR:-$repo/target/java/app}
build=$(mktemp -d)
"$jdk/bin/javac" --release 17 -cp "$app/libs/*" -d "$build" "$repo/tools/java/JsonTreeDoubles.java"
mkdir -p "$repo/crates/dhis-query/tests/fixtures"
docker run --rm --entrypoint java -e JAVA_TOOL_OPTIONS= -v "$build:/jt:ro" "$image" \
  -cp '/jt:/app/libs/*' JsonTreeDoubles > "$repo/crates/dhis-query/tests/fixtures/json-tree-doubles.tsv"
rm -rf "$build"
