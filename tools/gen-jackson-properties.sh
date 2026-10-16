#!/usr/bin/env bash
# Regenerates crates/dhis-schema/data/jackson/properties.json: for every registered schema class,
# the properties the field filter's copy of DHIS2's JSON mapper writes, in
# serialisation order, from the server image's classes and Java.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
image=${DHIS2_IMAGE:-dhis2/core-dev:local}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
app=${DHIS2_APP_DIR:-$repo/target/java/app}
build=$(mktemp -d)
"$jdk/bin/javac" --release 17 -cp "$app/classes:$app/libs/*" -d "$build" "$repo/tools/java/JacksonProperties.java"
mkdir -p "$repo/crates/dhis-schema/data/jackson"
python3 -c 'import json,sys; [print(s["klass"]) for s in json.load(open(sys.argv[1]))]' \
  "$repo/crates/dhis-schema/data/registered/schemas.json" \
  | docker run --rm -i --entrypoint java -e JAVA_TOOL_OPTIONS= -v "$build:/jp:ro" "$image" \
      -cp '/jp:/app/classes:/app/libs/*' JacksonProperties \
  > "$repo/crates/dhis-schema/data/jackson/properties.json"
rm -rf "$build"
