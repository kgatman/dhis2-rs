# Port status

_Read this first at the start of every session; update it at the end._

- **Last updated:** 2026-10-15 (session 2, continued)
- **Java reference:** dhis2-core `2a7855b69e` (2.44-SNAPSHOT), pinned in `ci/dhis2-core.ref`
- **Current phase:** 2 (read-only metadata). Slices 1–3 (`/api/system/{uid,id,uuid}`,
  `/api/system/info` + the `fields=` engine, the `/api/me` authorities and settings sub-resources)
  and slice 4 (`/api/schemas`, `/{type}`, `/{type}/{property}`) are `rust`. Slice 5 (the Gist
  engine: `/api/constants` via the Gist bridge and `/api/constants/gist`) is routed to Rust for
  all users with a known session identity (ADR 0009), `rust-shadow` until CI e2e confirms.
- **CI:** GitHub `kgatman/dhis2-rs`. `ci` and `e2e-through-gateway` green at `b7f2d1f` (with the
  `/api/schemas` handlers on Rust).

## Ledger

| Inventory | Total | `java` | `rust-shadow` | `rust` |
|---|---:|---:|---:|---:|
| Endpoint rows (method × path), `docs/inventory/endpoints.csv` | 3092 | 3047 | 30 | 15 |
| — distinct handler methods | 770 | 757 | 2 | 11 |
| — controllers | 195 | 195 | 0 | 0 |
| Job types, `docs/inventory/jobs.csv` | 43 | 43 | 0 | 0 |
| Config keys read by Rust, `docs/inventory/config-keys.csv` | 189 | — | — | 189 loadable |
| Tables present after migrations, `docs/inventory/tables.csv` | 309 | — | — | 0 read/written |

Regenerate the inventory with `tools/gen-inventory.sh`. CI fails if it is stale.

## Phase 2, slice 5 (session 2): the Gist engine, on `/api/constants`

- Finding: `AbstractFullReadOnlyController#getObjectList` hands most list requests to the **Gist
  engine** (`GistBridge`; the default `fields=id,displayName` qualifies), so `/api/{type}` lists
  are usually Gist responses (`"gist":true`, json-tree JSON, not Jackson). Only `fields=*`,
  presets, `query`, `paging=false` and the like use the metadata query engine.
- `dhis-inventory` now also writes `crates/dhis-schema/data/hibernate/hbm-entities.json`
  (table, id column, column/relation per property, from the `*.hbm.xml` files; drift-checked);
  all 1694 columns verified against the Java-migrated DB. JPA-mapped classes not covered yet.
- `dhis-schema`: typed registered schemas (`registered::by_singular/by_klass`), the extra
  `Property` members (readable, sortable, translatable, relation flags, gist preferences), and
  the mappings (`hibernate::entity`).
- `dhis_query::gist` (ADR 0008): filter/order parsing, `GistBridge` decision (14 cases checked
  against Java), planner/validator/SQL builder for list queries, json-tree output (fixtures from
  the server image: `tools/gen-json-tree-fixtures.sh`, 441 doubles), Spring's pager links.
  Supported: simple/translated/`href`/reference fields, presets but `:identifiable`/`:nameable`,
  filters and orders on simple persisted properties, paging/totals/links, `headless`, `unwrap`,
  `absoluteUrls`, `references`, `auto`, `locale`. Everything else and every Java error response
  falls back.
- `dhis-web`: `metadata_controller` (generic, types listed in `GIST_TYPES`: `constant`), Spring
  binding of the Gist/list parameters (repeated string parameters joined with commas, other
  types strict). `dhis-auth`: `Probe::AuthoritiesAndSettings` (the translation locale is the
  session's `keyDbLocale`); at once on a session, sequential for basic auth (ADR 0006).
- Fixtures (`corpus/phase2/setup.sh`): 60 constants with codes, descriptions, translations and
  four sharing patterns, a user group, and a superuser `corpussuper` with database locale `fr`.
- Evidence: `corpus/phase2/constants.jsonl` **244/244** through rust routes (157 answered by
  Rust, the rest fall back by design: unported features, non-superusers, Java errors); full
  phase-2 corpus **1046/1046**; shadow **163 match / 0 mismatch**.
- Latency (release, session cookie, p95 Java vs Rust): `/api/constants` 2.4 vs 2.8 ms, 60 rows
  with 5 fields 2.7 vs 2.8, `/gist?fields=id,name` 1.9 vs 2.3, `/gist` (all fields) 2.2 vs 3.1,
  filtered+ordered page with total 8.3 vs 3.3 (G7).
- Then (same session, decided with the owner): **session identity records** (ADR 0009). Java
  fixes a session's user id and group ids at login (measured: a removed group member's old
  session still reads the group's objects); the gateway records them when it sees a session
  created (database lookup for basic auth, `/api/me` right after other logins) and drops them on
  impersonation, account creation and logout paths. Rust then applies the sharing filter
  (`JpaQueryUtils.generateHqlQueryForSharingCheck`) and the type's `READ` authorities.
  Native sessions were ruled out before cutover: Java keeps sessions in Tomcat memory.
- The mapping generator now also records collection joins (table, key and element columns; 225
  checked against the DB). `dhis_db::users::identity_by_username` is built from them.
- Evidence: corpus 259/259 with a non-superuser (basic auth) served by Rust; through the gateway
  on cookie sessions (form login and basic-auth-created sessions) served by Rust and equal to
  Java on the same session, including the stale-group case (old session 45 rows, new 30);
  sessions unknown to the gateway fall back and match. Latency, non-superuser cookie session, p95
  Java vs Rust: `/api/constants` 3.3 vs 3.1 ms, `/gist?fields=id,name` 2.8 vs 2.7, filtered page
  with total 3.5 vs 3.5; basic auth ~70 ms on both (password hashing).
- Routes `rust` in `config/routes.toml`.
- Then: **14 more types** on the same handlers (`GIST_TYPES`: dataElement, dataElementGroup,
  option, optionSet, legendSet, categoryOption, category, categoryCombo, indicator,
  indicatorType, organisationUnit, organisationUnitGroup, userGroup, userRole; their controllers
  were checked for list-hook overrides). Needed: **JPA-annotated entities** in the generated
  mappings (44 classes; fields, `@MappedSuperclass` chains, embedded `TranslationProperty`,
  `@JoinTable`/`mappedBy`/`@JoinColumn` collections, enums, secondary-table join columns,
  composite ids), hbm enum types, and in the planner: collection `size`/`ids`/`idObjects`
  (join tables, one-to-many, element collections; sharing-filtered items), required references
  as Hibernate's inner join, the Gist default order (`option`), `date` columns, enums stored by
  name. The SQL forms were taken from Hibernate's own statements (PostgreSQL statement log).
  All 2549 mapped columns verified against the DB; mapping not-null flags are not reliable (8
  differ from the DB), so nothing relies on them.
- Fixtures: option sets, a legend set, categories/combo, 12 data elements, groups, indicators,
  an org unit tree. Evidence: `corpus/phase2/gist-types.jsonl` **226/226**, with the constants
  corpus 485/485 (345 answered by Rust).
- Then: **JSONB values** (`dhis_core_model::jsonb`): `Sharing`, translation sets (Java
  `HashSet` order via `Translation#hashCode`, `java::hash::hash_order`; JPA's
  `TranslationProperty` wrapper), `AttributeValues`, read as DHIS2's Hibernate JSON types read
  them. Fixtures from DHIS2's own classes on the server image (`tools/gen-jsonb-fixtures.sh`, 88
  values incl. resizes and invalid locales). Findings: a stored access entry's `displayName` is
  never read (read-only for Jackson), unknown sharing members are ignored, invalid translation
  locales fail the request in Java. Corpus 539/539.
- Then: **nested fields of references** (`categoryCombo[id,name]`, `lastUpdatedBy.username`): one
  inner join per referenced property, as Hibernate renders the implicit joins (so rows whose
  reference is null drop out of the page but not of the total: Java's behaviour); nested
  `displayName` reproduces Java's bug of translating from the **root** object's columns; nested
  `href`; members grouped as json-tree's layout tree does; the referenced type's read access is
  checked (public `User` fields as Java lists them). Full phase-2 corpus **1364/1364**.
- Not yet (fall back): nested filters/orders, collection item fields (`legendSets.name`),
  unwrapped nested fields, `style`, `valueTypeOptions`, `geometry`, enum filters, custom user types
  (`category.dataDimensionType`), secondary-table joins (`categoryOptionCombos`), `access`,
  `isEmpty`/`isNotEmpty`/`member`, nested fields.

## Delegated auth cost (session 2)

- `/api/me?fields=...` as the auth call cost 2.5–8 ms per Rust request (Java builds the whole
  `MeDto`), making small Rust responses 2–5 ms slower than Java at p95. The call is now a cheap
  probe chosen per route (`dhis_auth::delegated::Probe`, `dhis_web::auth_probe`):
  `/api/me/authorities` by default, `/api/me/settings` for the settings endpoints (0.4–0.7 ms).
  Still one Java request per Rust request (no caching, ADR 0004); ADR 0006 updated.
- Release build, session cookie, p95 Java vs Rust: `/api/system/uid` 0.8 vs 2.1 ms,
  `/api/me/authorities/ALL` 1.6 vs 2.1, `/api/me/settings` 0.7 vs 2.7, `/api/system/info` 3.7 vs
  3.4, `/api/schemas/dataElement` 2.8 vs 2.1, `/api/schemas` 36.5 vs 30.0. Remaining gap: G7.
- `/api/me/authorities` now returns Java's own order (the probe is that endpoint), so M2 is gone.
  `CurrentUser` no longer carries `id`/`username` (unused).
- Full phase-2 corpus **802/802** after the change.

## Phase 2, slice 4 (session 2): `/api/schemas`

- Ported `SchemaController#getSchemas`, `getSchema`, `getSchemaProperty` (JSON; XML and field
  transformers fall back; `validateSchema` stays on Java).
- **Registered schemas captured from Java** (ADR 0007): `tools/gen-registered-schemas.sh` records
  `/api/schemas` of the pinned build (116 schemas, Hibernate-derived values included) with a
  placeholder host; `dhis-query::json_tree` loads it as field-filter beans described by the
  dynamic schemas of `Schema`/`Property`/`Authority`/`GistPreferences` (generated with jshell).
  Refresh the capture whenever the ref moves.
- Measured Java quirks reproduced: links are request-dependent and written into shared objects;
  `getSchemaProperty` shows the last request's links (none since startup) → kept as per-schema
  state in Rust (deviation M3). `ApiVersionFilter` strips `/api/{28..44}/` before
  `getApiPath`, so versioned requests get unversioned links. Lookup is by exact singular name;
  properties by `Property#key()` (collection name for collections).
- Java reorders schemas (ties in `order`), `references` and `properties` on every restart
  (content identical): Rust serves the captured order (M4); diffcheck compares these arrays as sets
  (new `[[unordered]]` rule, reasons in `rules.toml`). diffcheck rule paths now ignore the API
  version like Java.
- `WebMessage` gained `errorCode` (after the listed properties, as Jackson orders it): 404s
  carry `E1005`.
- Evidence: `corpus/phase2/schemas.jsonl` **333/333** (all 116 types, fields variants, links with
  `X-Forwarded-*`, link-state sequences) against a Java restarted after the capture; full phase-2
  corpus **802/802** through `config/routes.toml`; shadow **1216 match / 0 mismatch**. One shadow-mode diffcheck run had 25 failures that did not
  reproduce in four later runs (output not kept).
- Latency (release build, session cookie, 60 requests): full list p95 Rust 33 ms vs Java 37 ms;
  small responses were 2–5 ms slower until the auth call was made cheap (see "Delegated auth
  cost").

## Phase 2, slice 3 (session 2): `/api/me` authorities and settings

- Ported `MeController#getAuthorities`, `hasAuthority`, `getSettings`, `getSetting`
  (`/api/me/{authorities,authorization}[/{authority}]`, `/api/me/settings[/{key}]`).
- **Session state from the delegated auth call** (ADR 0006): Java keeps user settings per session
  in memory (`SessionUserSettings`, stale after system-setting changes until the next
  authentication), so the auth call now requests `settings` too and Rust replays Java's own
  rendering, applying the `translate=true&locale=` override itself.
- `dhis-core-model`: `java::hash` (`HashSet<String>` iteration order, JDK 21 sizing; 158
  fixtures from the image, `tools/gen-hashset-fixtures.sh`), `locale` (port of
  `org.hisp.dhis.common.Locale`; fixes `Settings::as_locale`, which only lower-cased),
  `java::json` (Jackson's escaping and `Double.toString`, as a serde_json formatter),
  `java::string::trim`.
- **Latent bug fixed:** serde_json writes `\u001f`, Jackson `\u001F` (measured), so
  `/api/system/info` and `WebMessage` bodies with control characters differed. All Rust JSON now
  goes through the Jackson formatter.
- `dhis-web`: Spring's reflected-file-download header (`Content-Disposition:
  inline;filename=f.txt` for unsafe path extensions; safe list computed from the image's classes,
  `tools/gen-rfd-safe-extensions.sh`); `Params::string_set` (Spring `Set<String>` binding: one
  value is split at commas and trimmed, several are not).
- Gateway: Spring Security's `StrictHttpFirewall` rules and Tomcat's malformed-`%` check send
  requests Java rejects (400) to Java (`gateway::firewall`).
- Evidence: diffcheck `corpus/phase2/me.jsonl` **328/328** (admin, plus `corpusbasic` with stored
  settings from `setup.sh`); full phase-2 corpus **483/483**; shadow **197 match / 0 mismatch**.
  diffcheck rule paths now match whole segments (`/api/me` no longer covers `/api/metadata`); new
  rule: `lastUpdated` under `/api/me` (Java stamps it on every basic-auth login).
  Fallbacks by design: non-JSON `Accept`/q-values, setting keys without the `key` prefix that
  resolve to a stored key, keys that need JSON escaping (Java writes them raw or fails with 500).
- Deviation R1 (JDK 21 is the reference runtime). (M2, the tie order of `/api/me/authorities`,
  was removed once the auth call started returning Java's own set; see "Delegated auth cost".)

## Phase 2, slice 2 (session 2): `fields=` engine and `/api/system/info`

- `dhis-query`: `PropertyPath` + `fields=` parser (101 fixtures generated from Java's `Fields`),
  and the field-filter engine (port of `FieldPathHelper#apply` + the Jackson property filter):
  presets (`:all`, `:simple`, `:owner`, `:persisted`, `:identifiable`, `:nameable`), default
  expansion of complex/reference properties, exclusions, maps always expanded, output in
  serialisation order. Field transformers not ported yet (fall back).
- `dhis-schema`: `Schema`/`Property` model; descriptors are generated from Java
  (`tools/gen-dynamic-schemas.sh`; registered `/api/schemas` descriptors still to come).
- `dhis-core-model`: system/user settings (`LazySettings` semantics; 157 + 7 definitions generated
  by `tools/gen-settings.sh`), Java date formatting/time zone (`jiff`), Joda `getPrettyInterval`
  (JDK fixtures), time-zone display names generated with the server image's Java 21
  (`tools/gen-tz-names.sh`; they differ between JDK 17 and 27).
- `dhis-db`: system settings and `DatabaseInfo` reads.
- `/api/system/info`: native except the JVM/build snapshot, which is mirrored from Java
  (ADR 0005, decided with the owner). diffcheck `corpus/phase2/system-info.jsonl` **48/48**
  (superuser + non-superuser via `corpus/phase2/setup.sh`); full phase-2 corpus **132/132**;
  shadow **43 match / 0 mismatch**. Fallbacks: transformers, JSONP/XML, non-PostgreSQL analytics.
- diffcheck: `--host` so both servers see the same `Host` (`contextPath` depends on it).
- Server time zone follows the JDK's lookup (`TZ`, `/etc/timezone`, `/etc/localtime`, else
  **`GMT`**). Both container images have none of these, so Java reports `GMT` there (measured)
  and Rust now does too; tzdb is bundled like the JVM's.

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
- Ledger: 5 rows `rust` (`e2e-through-gateway` green at `4ba1e23` with the routes flipped). Note that e2e no longer calls these endpoints, so the
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
7. **Delegated auth adds ~1–2 ms at p95** to the smallest Rust responses (one cheap Java request
   plus the proxy hop; deviation G7). Removed by native auth (phase 7).
9. **Latency evidence is informal** (owner decision, 2026-10-15: benchmarks after the porting
   work). The p95 figures in this file are one-off shell loops on a tiny local DB (Java on JDK
   17), not the charter's demo-DB comparison. To do later: a benchmark script with fixed
   endpoints and recorded environment, `docs/perf/latency.md` with history, and runs on the
   Sierra Leone demo DB before the cutover gate.
8. **Session identity records** (ADR 0009) depend on all traffic passing through one gateway and
   are lost on a gateway restart (those sessions fall back until their users log in again, G8).
   Token (PAT/OAuth2 bearer) requests always fall back on identity routes; supporting them needs
   the token's user (Java resolves it per request: a database lookup like basic auth).

## Next three tasks (end of session 2)

1. **CI e2e with the Gist routes on Rust**, then mark the two constants rows `rust`.
2. **Metadata query engine** (`fields=*`, presets, `GET /api/{type}/{uid}`): QueryService
   filters/orders/paging over DB-loaded entities rendered through the field filter, `href`,
   `access`, the Jackson forms of JSONB values (`AttributeValues` as an array there).
3. Gist extras: nested filters/orders, collection item fields, enum filters, `access`,
   single-object `/api/{type}/{uid}/gist`, more types. Then `/api/me` (`MeDto`).

## Earlier next-tasks (start of slice 5, done or superseded)

1. CI e2e with `/api/schemas` and the probe-based auth, then the three schema rows `rust` (done:
   green at `b7f2d1f`).
2. `/api/me` (MeController GET), moved behind the metadata engine.
3. Generic metadata reads for `/api/constants` (slice 5, in shadow).

## Earlier plan for slice 2 (done)

2. **Field filtering (`fields=`) engine in `dhis-query`**, with `/api/system/info` as its first
   consumer (`SystemController#getSystemInfo`: `FieldFilterService` over `SystemInfo`, sensitive
   fields removed for non-superusers, `setNoStore`). Needs `SystemService#getSystemInfo` (system
   settings, DB info, build info) and the `fields` grammar (`*`, `:all`, `:owner`, `[...]`,
   `!exclusions`, transformers). Port `FieldFilterParser` tests from Java first.
   Scope note: DHIS2 has **three** field-filter engines. Phase 2's consumers
   (`AbstractFullReadOnlyController`, `MeController`, `SchemaController`, `SystemController`)
   use `dhis-service-field-filtering` (`org.hisp.dhis.fieldfiltering`, Jackson-based, ~1.3k LOC
   + `FieldFilterParserTest`/`FieldFilterServiceTest`). The older `dhis-service-node`
   `fieldfilter` engine (12 controllers: audits, data approval, dimensions, ...) and tracker's
   `export.fieldfiltering` (20 files) come later with their phases.
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
