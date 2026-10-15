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

# Core metadata for the Gist corpus of more types: option sets with options, a legend set,
# category options/categories/a category combo, data elements (value and aggregation types,
# option set and category combo references, legend sets, translations, sharing as above), data
# element groups, an indicator type with indicators, and an organisation unit tree with a group.
core=$(python3 - <<'PY'
import json
def sh(i):
    s = {"public": "r-------" if i % 4 == 0 else "--------", "users": {}, "userGroups": {}}
    if i % 4 == 2: s["users"] = {"Crp0User001": {"id": "Crp0User001", "access": "r-------"}}
    if i % 4 == 3: s["userGroups"] = {"Crp0Grp0001": {"id": "Crp0Grp0001", "access": "rw------"}}
    return s
tr = lambda i, n: ([{"property": "NAME", "locale": "fr", "value": n + " (fr)"}] if i % 2 else [])
m = {
  "options": [{"id": "Crp0Opt%04d" % i, "code": "OPT%d" % i, "name": "Corpus option %d" % i,
               "sortOrder": i % 3 + 1, "optionSet": {"id": "Crp0OSet%03d" % (i // 3 + 1)}}
              for i in range(6)],
  "optionSets": [{"id": "Crp0OSet%03d" % k, "name": "Corpus option set %d" % k, "valueType": "TEXT",
                  "sharing": sh(k), "translations": tr(k, "Corpus option set %d" % k),
                  "options": [{"id": "Crp0Opt%04d" % i} for i in range(6) if i // 3 + 1 == k]}
                 for k in (1, 2)],
  "legendSets": [{"id": "Crp0LSet001", "name": "Corpus legend set", "sharing": sh(0),
                  "legends": [{"id": "Crp0Lgnd%03d" % i, "name": "Legend %d" % i,
                               "startValue": i * 10, "endValue": i * 10 + 10, "color": "#00%02d00" % i}
                              for i in range(3)]}],
  "categoryOptions": [{"id": "Crp0COpt%03d" % i, "name": "Corpus category option %d" % i,
                       "shortName": "CCO %d" % i, "sharing": sh(i)} for i in range(4)],
  "categories": [{"id": "Crp0Cat0%03d" % k, "name": "Corpus category %d" % k, "shortName": "CCat %d" % k,
                  "dataDimensionType": "DISAGGREGATION", "sharing": sh(k),
                  "categoryOptions": [{"id": "Crp0COpt%03d" % i} for i in range(4) if i % 2 == k - 1]}
                 for k in (1, 2)],
  "categoryCombos": [{"id": "Crp0CCmb001", "name": "Corpus category combo", "dataDimensionType": "DISAGGREGATION",
                      "sharing": sh(0), "categories": [{"id": "Crp0Cat0001"}, {"id": "Crp0Cat0002"}]}],
  "dataElements": [{"id": "Crp0DEl%04d" % i, "name": "Corpus data element %02d" % i, "shortName": "CDE %02d" % i,
                    "code": "CDE_%02d" % i if i % 3 else None, "formName": "Form %d" % i if i % 2 else None,
                    "valueType": ["NUMBER", "INTEGER", "TEXT", "BOOLEAN"][i % 4],
                    "aggregationType": ["SUM", "AVERAGE", "NONE", "COUNT"][i % 4], "domainType": "AGGREGATE",
                    "zeroIsSignificant": i % 2 == 0, "sharing": sh(i),
                    "translations": tr(i, "Corpus data element %02d" % i),
                    **({"categoryCombo": {"id": "Crp0CCmb001"}} if i % 3 == 0 else {}),
                    **({"optionSet": {"id": "Crp0OSet001"}, "valueType": "TEXT"} if i == 5 else {}),
                    **({"legendSets": [{"id": "Crp0LSet001"}]} if i % 4 == 1 else {})}
                   for i in range(12)],
  "dataElementGroups": [{"id": "Crp0DEGr%03d" % k, "name": "Corpus data element group %d" % k,
                         "shortName": "CDEG %d" % k, "sharing": sh(k),
                         "dataElements": [{"id": "Crp0DEl%04d" % i} for i in range(12) if i % k == 0]}
                        for k in (1, 2, 3)],
  "indicatorTypes": [{"id": "Crp0IndT001", "name": "Corpus per cent", "factor": 100}],
  "indicators": [{"id": "Crp0Ind%04d" % i, "name": "Corpus indicator %d" % i, "shortName": "CInd %d" % i,
                  "indicatorType": {"id": "Crp0IndT001"}, "numerator": "#{Crp0DEl0000}", "denominator": "1",
                  "sharing": sh(i), "legendSets": [{"id": "Crp0LSet001"}] if i == 1 else []}
                 for i in range(3)],
  "organisationUnits": [{"id": "Crp0OrgU001", "name": "Corpus land", "shortName": "Corpus land", "openingDate": "2000-01-01"}]
     + [{"id": "Crp0OrgU%03d" % i, "name": "Corpus district %d" % i, "shortName": "CD %d" % i,
         "openingDate": "2001-02-03", "parent": {"id": "Crp0OrgU001"}} for i in (2, 3, 4)],
  "organisationUnitGroups": [{"id": "Crp0OUGr001", "name": "Corpus org unit group", "shortName": "COUG",
                              "organisationUnits": [{"id": "Crp0OrgU002"}, {"id": "Crp0OrgU003"}]}],
}
for de in m["dataElements"]:
    for k in [k for k, v in de.items() if v is None]: del de[k]
print(json.dumps(m))
PY
)
post "$core"
