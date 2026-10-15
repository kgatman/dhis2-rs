#!/usr/bin/env bash
# Regenerates crates/dhis-web/data/rfd-safe-extensions.txt: the path extensions for which Spring
# MVC does not add `Content-Disposition: inline;filename=f.txt` (reflected file download
# protection), computed with the classes of the server image (DHIS2_IMAGE).
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
image=${DHIS2_IMAGE:-dhis2/core-dev:local}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cid=$(docker create "$image")
docker cp -q "$cid:/app/libs" "$work/libs"
docker cp -q "$cid:/app/classes" "$work/app-classes"
docker rm -f "$cid" >/dev/null
"$jdk/bin/javac" --release 17 -proc:none -cp "$work/app-classes:$work/libs/*" -d "$work/classes" \
  "$repo/tools/java/RfdSafeExtensions.java"
docker run --rm --entrypoint java -v "$work/classes:/rfd:ro" "$image" \
  -cp "/rfd:/app/resources:/app/classes:/app/libs/*" RfdSafeExtensions > "$repo/crates/dhis-web/data/rfd-safe-extensions.txt"
