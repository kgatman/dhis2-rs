#!/usr/bin/env bash
# Creates the fixtures the phase-2 corpus expects, through the Java API (idempotent):
#   role  "corpus basic role" (uid Crp0Role001) with F_DATAVALUE_ADD only
#   user  "corpusbasic" / "Corpus123!" (uid Crp0User001), not a superuser, with some stored
#         user settings
#   role  "corpus super role" (uid Crp0Role002) with ALL, user "corpussuper" / "Corpus123!"
#         (uid Crp0User002), a superuser whose database locale is fr (translated display names)
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
post '{"userRoles":[{"id":"Crp0Role002","name":"corpus super role","authorities":["ALL"]}],
       "users":[{"id":"Crp0User002","username":"corpussuper","password":"Corpus123!",
                 "firstName":"Corpus","surname":"Super","userRoles":[{"id":"Crp0Role002"}]}]}'
curl -sf "${auth[@]}" -X POST "$java/api/userSettings/keyDbLocale?user=corpussuper&value=fr" \
  -o /dev/null -w "%{http_code}\n"

# Metadata list fixtures (generic reads / Gist): group "corpus group" (Crp0Grp0001) with
# corpusbasic as member, and 60 constants Crp0Cnst001..060 (more than one default page) with a mix
# of codes, descriptions, translations and sharing:
#   i % 4 == 0  public r------- (readable by everyone)
#   i % 4 == 1  public --------, no other access (superuser only)
#   i % 4 == 2  public --------, corpusbasic r-------
#   i % 4 == 3  public --------, "corpus group" rw------
# Odd i have a code and a description; i % 3 == 0 has fr/es_419 translations of name and
# shortName; i % 5 == 0 has a description translation (fr).
constants=$(python3 - <<'PY'
import json
cs = []
for i in range(1, 61):
    uid = "Crp0Cnst%03d" % i
    c = {"id": uid, "name": "Corpus constant %02d" % i, "shortName": "CC %02d" % i,
         "value": round(i * 1.25 - 20, 2)}
    if i % 2:
        c["code"] = "CORPUS_C%02d" % i
        c["description"] = "Description of constant %d, with \"quotes\" & <tags>" % i
    tr = []
    if i % 3 == 0:
        tr += [{"property": "NAME", "locale": "fr", "value": "Constante corpus %02d" % i},
               {"property": "SHORT_NAME", "locale": "fr", "value": "CF %02d" % i},
               {"property": "NAME", "locale": "es_419", "value": "Constante del corpus %02d" % i}]
    if i % 5 == 0:
        tr.append({"property": "DESCRIPTION", "locale": "fr", "value": "Déscription %d" % i})
    c["translations"] = tr
    sharing = {"public": "r-------" if i % 4 == 0 else "--------", "users": {}, "userGroups": {}}
    if i % 4 == 2:
        sharing["users"] = {"Crp0User001": {"id": "Crp0User001", "access": "r-------"}}
    if i % 4 == 3:
        sharing["userGroups"] = {"Crp0Grp0001": {"id": "Crp0Grp0001", "access": "rw------"}}
    c["sharing"] = sharing
    cs.append(c)
print(json.dumps({"constants": cs}))
PY
)
post '{"userGroups":[{"id":"Crp0Grp0001","name":"corpus group","users":[{"id":"Crp0User001"}]}]}'
post "$constants"
