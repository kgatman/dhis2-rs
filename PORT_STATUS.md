# Port status

_Read this first at the start of every session; update it at the end._

- **Last updated:** 2026-10-15 (session 1, continued)
- **Java reference:** dhis2-core `2a7855b69e` (2.44-SNAPSHOT), pinned in `ci/dhis2-core.ref`
- **Current phase:** 2 (read-only metadata). Slice 1 (`/api/system/{uid,id,uuid}`) is routed to
  Rust.
- **CI:** GitHub `kgatman/dhis2-rs`. `ci` and `e2e-through-gateway` were green at `70dc1df`.

## Ledger

| Inventory | Total | `java` | `rust-shadow` | `rust` |
|---|---:|---:|---:|---:|
| Endpoint rows (method × path), `docs/inventory/endpoints.csv` | 3092 | 3087 | 5 | 0 |
| — distinct handler methods | 770 | 770 | 0 | 0 |
| — controllers | 195 | 195 | 0 | 0 |
| Job types, `docs/inventory/jobs.csv` | 43 | 43 | 0 | 0 |
| Config keys read by Rust, `docs/inventory/config-keys.csv` | 189 | — | — | 189 loadable |
| Tables present after migrations, `docs/inventory/tables.csv` | 309 | — | — | 0 read/written |

Regenerate the inventory with `tools/gen-inventory.sh`. CI fails if it is stale.

## Phase 2, slice 1 (session 2): `/api/system/{uid,id,uuid}`

Tracer bullet that built the shared Web API layer every later endpoint reuses:

- `dhis-auth`: `CurrentUser`; authentication **delegated to Java** (`/api/me`), relaying Java's
  `Set-Cookie`s (Java creates a session per basic-auth request and refreshes `SESSION_EXPIRE`).
- `dhis-web`: Spring `Integer` binding (whitespace stripping, hex, Unicode digits; JDK messages,
  fixtures generated with jshell), `WebMessage`, security headers (`CspFilter` incl. the CORS
  whitelist in Java `HashSet` order), shallow ETag filter, conservative content negotiation.
- Gateway Rust pipeline: Java path normalisation, delegated auth, `JavaFallback` replay with
  counters (ADR 0004), Tomcat request-target and parameter-decoding checks (send Java-rejected
  requests to Java), empty reason phrase like Tomcat.
- Evidence: diffcheck `corpus/phase2/system-uid.jsonl` **84/84** against Java (67 answered by
  Rust); shadow **67 match / 0 mismatch**; raw request-target comparison (13 cases, 4 remaining
  gateway-level differences documented as G4/G6).
- Ledger: 5 rows `rust-shadow`. They become `rust` once `e2e-through-gateway` is green with the
  routes flipped (`config/routes.toml`). Note that e2e no longer calls these endpoints, so the
  evidence is diffcheck + shadow.
- Local Java reference: `dev/java-local.sh start` (host JVM, PostGIS in Docker, :8081).

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

## Done later in session 1

- Java image built locally at the pinned ref (`dhis2/core-dev:local`, Maven+Jib, 13 min).
- **Live cross-checks** (against Java at the pinned ref):
  - `tables.csv` vs `information_schema` of the Java-migrated DB: **309/309** after two fixes
    (tables created only inside `DO $$` blocks are now `conditional`; Flyway/PostGIS tables are
    listed).
  - `endpoints.csv` vs `/api/openapi.json`: 2462 common, **0 unexplained** rows. Fixing this
    found a generator bug (same-class overloads were dropped; +6 rows). The new `openapi` column
    marks 226 rows Spring routes but the docs hide (`@OpenApi.Ignore`/`@OpenApi.Filter`). 2
    OpenAPI-only paths are RouteController's overridden collection handlers, a docs artifact
    (to be confirmed with a live request).
- Gateway **shadow mode** (GET/HEAD only, Rust compared off the request path, no added latency).
- Phase-2 seed corpus `corpus/phase2/*.jsonl` (40 read-only cases).
- **e2e through the gateway: green.** Full `dhis-test-e2e` at the pinned ref, all routes to Java
  through the gateway: **589 run, 0 failures, 0 errors, 14 skipped**. The direct-to-Java baseline
  gives the identical result (589/0/0/14), so the gateway is transparent to the suite and there
  are no known upstream failures at this ref. Java saw the test container IP (`172.30.0.10`) on
  805 auth events. The single other IP is RouteTest's Java-to-itself call, which bypasses the
  gateway in both setups. Logs: `target/e2e/` (not committed).
- Gateway now exits on SIGTERM (it was killed with 137 on `docker stop`).

## Local e2e notes

- The e2e stack needs about 5.5 GB of Docker memory. On this machine (7.75 GB VM), stop other
  stacks for the run (about 25 min per run). `E2E_LOW_MEMORY=1` alone was not enough while
  ~5.5 GB was taken by other containers.
- Use a clone of dhis2-core at `ci/dhis2-core.ref` for `DHIS2_CORE_DIR`; Maven/compose write
  build output there. The `../dhis2-core` checkout stays untouched.

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
   canonicalisation, no XML/CSV normalisers yet. Shadow mode covers reads.
6. **`dhis-test-e2e` upstream flakiness is unknown.** Run `e2e.yml` with `baseline: true` once to
   record which tests fail without the gateway.

## Next three tasks (from session 2)

1. **Confirm CI at `4ba1e23`**, then mark the 5 SystemController rows `rust`.
2. **Field filtering (`fields=`) engine in `dhis-query`**, with `/api/system/info` as its first
   consumer (`SystemController#getSystemInfo`: `FieldFilterService` over `SystemInfo`, sensitive
   fields removed for non-superusers, `setNoStore`). Needs `SystemService#getSystemInfo` (system
   settings, DB info, build info) and the `fields` grammar (`*`, `:all`, `:owner`, `[...]`,
   `!exclusions`, transformers). Port `FieldFilterParser` tests from Java first.
3. **`/api/me`** (MeController GET, e2e uses it on almost every test) on top of the same engine,
   then `/api/schemas`.

## Previous next-tasks (session 1, superseded)

1. **Push to a remote** so `ci.yml`/`e2e.yml` run on every change. They have only been exercised
   locally so far (same commands).
2. **Start phase 2 with `/api/system/info` and `/api/schemas`.** Read `SystemController`,
   `SchemaController`, `dhis-service-schema` and their tests. Build `dhis-schema` and the
   `WebMessage` error envelope in `dhis-web`. Mount them as `shadow` routes and drive them with
   `corpus/phase2`.
3. **Sierra Leone DB** for diffcheck/shadow runs (realistic data), and confirm the two
   RouteController OpenAPI-only paths with live requests. DB snapshot/restore for write
   comparisons is deferred to the start of phase 3, since phase 2 is read-only.

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
