#!/usr/bin/env bash
# Regenerates crates/dhis-schema/data/dynamic/*.json: the "dynamic schemas" Java builds at runtime
# for classes without a registered schema (DefaultSchemaService#createDynamicSchema), computed by
# Java's own property introspectors (Jackson, Translatable, Property, Gist; Hibernate adds nothing
# for non-entity classes) from the pinned build.
#
# Usage: tools/gen-dynamic-schemas.sh            (needs target/java/app, see dev/java-local.sh)
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
app=${JAVA_APP:-$repo/target/java/app}
jdk=${DHIS2_JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}
classes=(
  org.hisp.dhis.system.SystemInfo
  org.hisp.dhis.system.database.DatabaseInfo
  org.hisp.dhis.system.capability.SystemCapability
  org.hisp.dhis.schema.Schema
  org.hisp.dhis.schema.Property
  org.hisp.dhis.schema.GistPreferences
  org.hisp.dhis.security.Authority
  org.hisp.dhis.user.sharing.Sharing
  org.hisp.dhis.user.sharing.UserAccess
  org.hisp.dhis.user.sharing.UserGroupAccess
  org.hisp.dhis.security.acl.Access
  org.hisp.dhis.security.acl.AccessData
  org.hisp.dhis.translation.Translation
  'org.hisp.dhis.schema.transformer.UserPropertyTransformer$UserDto'
)
cp=$(ls "$app"/libs/*.jar | tr '\n' ':')$app/classes
out=$(mktemp)
"$jdk/bin/jshell" -q --class-path "$cp" -R-Dclasses="$(IFS=,; echo "${classes[*]}")" \
  "$repo/tools/jshell/dynamic-schemas.jsh" > "$out"
mkdir -p "$repo/crates/dhis-schema/data/dynamic"
while IFS=$'\t' read -r cls json; do
  printf '%s\n' "$json" | python3 -m json.tool --indent 1 > "$repo/crates/dhis-schema/data/dynamic/$cls.json"
done < "$out"
rm -f "$out"
ls "$repo/crates/dhis-schema/data/dynamic"
