#!/usr/bin/env bash
# Regenerates crates/dhis-core-model/data/tz-display-names.json: Java's English long display name
# of every time zone (SystemInfo.serverTimeZoneDisplayName). Names change between JDK releases
# (CLDR), so they are produced with the Java runtime of the server image (DHIS2_IMAGE).
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
image=${DHIS2_IMAGE:-dhis2/core-dev:local}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
build=$(mktemp -d)
"$jdk/bin/javac" --release 17 -d "$build" "$repo/tools/java/TzNames.java"
docker run --rm --entrypoint java -v "$build:/tz:ro" "$image" -cp /tz TzNames \
  > "$repo/crates/dhis-core-model/data/tz-display-names.json"
rm -rf "$build"
