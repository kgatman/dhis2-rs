#!/usr/bin/env bash
# Creates the fixtures the phase-2 corpus expects, through the Java API (idempotent):
#   role  "corpus basic role" (uid Crp0Role001) with F_DATAVALUE_ADD only
#   user  "corpusbasic" / "Corpus123!" (uid Crp0User001), not a superuser, with some stored
#         user settings
#
# Usage: corpus/phase2/setup.sh [java base url]   (default http://localhost:8081, admin/district)
set -euo pipefail
java=${1:-http://localhost:8081}
auth=(-u "${DIFFCHECK_USER:-admin}:${DIFFCHECK_PASSWORD:-district}")
post() {
  curl -sf "${auth[@]}" -H 'Content-Type: application/json' -X POST "$java/api/metadata" -d "$1" \
    -o /dev/null -w "%{http_code}\n"
}
post '{"userRoles":[{"id":"Crp0Role001","name":"corpus basic role","authorities":["F_DATAVALUE_ADD"]}],
       "users":[{"id":"Crp0User001","username":"corpusbasic","password":"Corpus123!",
                 "firstName":"Corpus","surname":"Basic","userRoles":[{"id":"Crp0Role001"}]}]}'
# Stored user settings for corpusbasic (admin has none), so /api/me/settings covers stored values
# next to defaults.
setting() {
  curl -sf "${auth[@]}" -X POST "$java/api/userSettings/$1?user=corpusbasic&value=$2" \
    -o /dev/null -w "%{http_code}\n"
}
setting keyUiLocale fr
setting keyDbLocale es_419
setting keyStyle green/green.css
setting keyMessageEmailNotification true
setting keyAnalysisDisplayProperty shortName
