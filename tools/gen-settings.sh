#!/usr/bin/env bash
# Regenerates crates/dhis-core-model/data/settings.json from the pinned Java build: every system
# and user setting key with its typed default, confidential/translatable flags and property name,
# exactly as org.hisp.dhis.setting.LazySettings extracts them at runtime.
#
# Usage: tools/gen-settings.sh            (needs target/java/app, see dev/java-local.sh)
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
app=${JAVA_APP:-$repo/target/java/app}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
cp=$(ls "$app"/libs/*.jar | tr '\n' ':')$app/classes
mkdir -p "$repo/crates/dhis-core-model/data"
"$jdk/bin/jshell" -q --class-path "$cp" "$repo/tools/jshell/settings.jsh" \
  > "$repo/crates/dhis-core-model/data/settings.json"
python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print({k: len(v) for k, v in d.items()})" \
  "$repo/crates/dhis-core-model/data/settings.json"
