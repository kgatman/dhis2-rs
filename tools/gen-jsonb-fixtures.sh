#!/usr/bin/env bash
# Regenerates crates/dhis-query/tests/fixtures/jsonb-values.tsv: JSONB column values (sharing,
# translations, attribute values) read through DHIS2's own Hibernate types and written as the
# Gist API writes them, with the server image's Java and DHIS2 classes. Inputs:
# tools/jsonb-inputs.tsv ("kind \t json", e.g. from `select sharing::text from ...`).
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
image=${DHIS2_IMAGE:-dhis2/core-dev:local}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
app=${DHIS2_APP_DIR:-$repo/target/java/app}
build=$(mktemp -d)
"$jdk/bin/javac" --release 17 -cp "$app/classes:$app/libs/*" -d "$build" "$repo/tools/java/JsonbValues.java"
docker run --rm -i --entrypoint java -e JAVA_TOOL_OPTIONS= -v "$build:/jb:ro" "$image" \
  -cp '/jb:/app/classes:/app/libs/*' JsonbValues < "$repo/tools/jsonb-inputs.tsv" \
  > "$repo/crates/dhis-query/tests/fixtures/jsonb-values.tsv"
rm -rf "$build"
