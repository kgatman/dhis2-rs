#!/usr/bin/env bash
# Regenerates crates/dhis-core-model/tests/fixtures/hashset.tsv: java.util.HashSet iteration
# orders, produced with the Java runtime of the server image (DHIS2_IMAGE), because HashSet sizing
# differs between JDK releases (17 vs 21).
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
image=${DHIS2_IMAGE:-dhis2/core-dev:local}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
build=$(mktemp -d)
"$jdk/bin/javac" --release 17 -d "$build" "$repo/tools/java/HashSetOrder.java"
mkdir -p "$repo/crates/dhis-core-model/tests/fixtures"
docker run --rm --entrypoint java -e JAVA_TOOL_OPTIONS= -v "$build:/hs:ro" "$image" \
  -Dfile.encoding=UTF-8 -Dstdout.encoding=UTF-8 -cp /hs HashSetOrder \
  > "$repo/crates/dhis-core-model/tests/fixtures/hashset.tsv"
rm -rf "$build"
