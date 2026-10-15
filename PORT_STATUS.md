# Port status

_Read this first at the start of every session; update it at the end._

- **Last updated:** 2026-10-15 (session 1)
- **Java reference:** dhis2-core `2a7855b69e` (2.44-SNAPSHOT), pinned in `ci/dhis2-core.ref`
- **Current phase:** 1 (Foundations). Phase 0 (inventory) is done.

## Ledger

| Inventory | Total | `java` | `rust-shadow` | `rust` |
|---|---:|---:|---:|---:|
| Endpoint rows (method × path), `docs/inventory/endpoints.csv` | 3086 | 3086 | 0 | 0 |
| — distinct handler methods | 764 | 764 | 0 | 0 |
| — controllers | 195 | 195 | 0 | 0 |
| Job types, `docs/inventory/jobs.csv` | 43 | 43 | 0 | 0 |
| Config keys read by Rust, `docs/inventory/config-keys.csv` | 189 | — | — | 189 loadable |
| Tables present after migrations, `docs/inventory/tables.csv` | 309 | — | — | 0 read/written |

Regenerate the inventory with `tools/gen-inventory.sh`. CI fails if it is stale.

## Done in session 1

- Cargo workspace with every crate from the charter (`crates/*`), plus `dhis-config` (ADR 0001).
- `tools/inventory`: generates the four CSVs and `dhis-config`'s key table from the Java source
  (ADR 0003).
- `dhis-config`: `dhis.conf` loader matching Java: Latin-1 properties parsing, env
  substitution, DHIS2_HOME resolution, aliases, confidential map. Tested against
  `dhis-test-e2e/config/dhis2_home/dhis.conf`.
- `dhis-db`: pgjdbc-compatible URL parsing, Hikari-equivalent sqlx pool settings (lazy connect).
- `gateway`: transparent proxy plus route table with Java's path normalisation. Byte-level tests
  cover header case, Host, hop-by-hop, streaming, 502, versions and Rust dispatch (ADR 0002).
- `diffcheck`: corpus replay, rules that require reasons, structural JSON diff.
- CI: `ci.yml` (fmt, clippy, tests, inventory drift check); `e2e.yml` (builds Java at the pinned
  ref and runs `dhis-test-e2e` through the gateway, with an optional direct baseline).

## Not yet verified

- **The e2e suite has not been run through the gateway yet.** That needs the Java image (Maven
  build) and roughly an hour, either in CI (`e2e.yml`) or locally (`ci/e2e/run.sh`). The compose
  wiring was checked with `docker compose config` only.
- The inventory has not been cross-checked against a live Java instance (`/api/openapi.json`,
  `information_schema`).

## Open problems / risks

1. **Non-controller surface is missing from `endpoints.csv`**: Spring Security filter chains
   (`/oauth2/*` authorization server, form login, logout, `/api/auth/login` filters), static
   resources and bundled apps in `dhis-web-server`. They need their own inventory pass (phase 7/8).
2. **Shared-DB cache coherence.** Java caches metadata in the Hibernate L2 cache (Ehcache) and
   Redis. Rust writes must invalidate them. Study `dhis-support-cache-invalidation` (Debezium) and
   Redis invalidation before any Rust write route is flipped (phase 3 blocker).
3. **Connection budget.** Java's pool (80) plus Rust's must fit Postgres `max_connections`.
   `dhis-db` has an override; this needs an operator-facing setting before phase 2 ships.
4. **IP-sensitive Java code sees the gateway IP** (deviation G1). This is fine in e2e thanks to
   the network-namespace wiring, but must be in deployment docs.
5. **diffcheck gaps**: no DB-row diff for writes, no snapshot/restore orchestration, no UID
   canonicalisation, no XML/CSV normalisers yet.
6. **`dhis-test-e2e` upstream flakiness is unknown.** Run `e2e.yml` with `baseline: true` once to
   record which tests fail without the gateway.

## Next three tasks

1. **Get e2e green through the gateway.** Run `.github/workflows/e2e.yml` (or `ci/e2e/run.sh`
   with a locally built `dhis2/core-dev:local`), once with `baseline: true`. Record failures that
   also fail on the baseline as known upstream issues in this file.
2. **Live Java cross-check.** Start Java at the pinned ref on the Sierra Leone dump. Diff
   `endpoints.csv` against `/api/openapi.json` and `tables.csv` against `information_schema`.
   Fix the generator. Add a `--from-openapi` check mode to `tools/inventory`.
3. **diffcheck for phase 2.** Add DB snapshot/restore (pg_dump template DB per run) and gateway
   shadow mode. Seed the first corpus for `/api/schemas`, `/api/system/info` and `/api/me` from
   `dhis-test-web-api` (`SchemaControllerTest`, `SystemControllerTest`, `MeControllerTest`) and
   the e2e tests.

## Phase plan with sizes

Sizes come from the inventory (handler methods = distinct Java methods with a mapping;
rows = method × path after inheritance). Person-weeks assume one engineer working with an AI
agent. They are rough (±50%) and should be re-estimated at each phase start.

| Phase | Scope (from inventory) | Units | Est. person-weeks | Main risk |
|---|---|---|---:|---|
| 0 Inventory | 4 CSVs + generator | — | done | — |
| 1 Foundations | config ✓, pool ✓, gateway ✓, diffcheck ✓; still to do: e2e green, live cross-check, shadow mode, snapshots | — | 1–2 left | e2e flakiness |
| 2 Read-only metadata | generic GET/gist on 85 controllers, `/api/schemas`, `/api/me`, system info, settings reads; fields/filter/order DSL, paging, sharing read ACL | 1053 rows / 129 handlers / 84 controllers | 10–14 | Schema introspection must reproduce Java's `@JsonProperty`-driven property model for ~85 types |
| 3 Metadata writes | CRUD/patch/sharing/translations on 81 CRUD controllers, `/api/metadata` import/export (preheat, bundle, validation, atomic modes), audit, cache invalidation | 1057 rows / 122 handlers / 72 controllers | 14–20 | dxf2 import pipeline size; cache coherence with Java |
| 4 Aggregate | data values/sets (JSON/XML/CSV/ADX), complete registrations, approval, locking, min/max, data entry | 171 rows / 103 handlers / 21 controllers | 10–14 | Approval + locking rules; streaming formats |
| 5 Tracker | `/api/tracker` import/export, ownership, audit, program rules, notifications | 195 rows / 84 handlers / 15 controllers | 16–24 | Program rule engine (`rule-engine-jvm`) parity |
| 6 Expressions + analytics | expression grammar, validation/predictors, analytics tables + queries (Postgres/ClickHouse/Doris) | 286 rows / 164 handlers / 28 controllers | 20–30 | Analytics SQL generation across 3 dialects |
| 7 Native auth | sessions, PATs, OAuth2 AS + OIDC client, LDAP, 2FA, lockout, password policy; plus filter-based endpoints not in the CSV | 88 rows / 56 handlers / 10 controllers + filters | 8–12 | Spring Authorization Server parity |
| 8 Long tail | maintenance, dataStore, scheduling + 43 job types, apps, files/icons, messaging/SMS, reports/PDF, remaining endpoints | 236 rows / 204 handlers / 52 controllers; 43 jobs | 16–24 | Breadth; PDF/XLS structure parity |
| 9 Cutover | Rust alone, full e2e, Gatling no-regression, soak test | — | 4–6 | Performance (N+1 hidden by Hibernate batching) |
| **Total** | | | **~100–145** | |

The phase-to-endpoint mapping above is a path/controller heuristic made for sizing. Do the real
assignment in `endpoints.csv` when each phase starts.
