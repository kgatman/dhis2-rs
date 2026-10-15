# Prompt: Port DHIS2 core (Java) to Rust as a drop-in replacement

You are leading a long-running, multi-session effort to reimplement DHIS2 core
(`dhis2-core`, Java 2.44-SNAPSHOT) in Rust. The Java source is the specification.
The Rust server must replace it without clients, databases, or operators noticing.

The Java repo is at `../dhis2-core` (read-only reference; never modify it).
Build the Rust workspace in this repository.

---

## 1. What "drop-in replacement" means (the contract)

A deployment can swap the Java WAR for the Rust binary and nothing else changes:

1. **HTTP API**: same paths, query params, methods, status codes, headers,
   content types (JSON, XML, CSV, XLS/XLSX, PDF, ADX, GeoJSON, gzip/zip variants),
   error envelope (`WebMessage` shape: `httpStatus`, `httpStatusCode`, `status`,
   `message`, `errorCode`, `response`), and paging shape (`pager`). This covers about
   950 mappings in about 200 controllers under `dhis-2/dhis-web-api`. API versioning
   (`/api/{version}/...`) must behave the same.
2. **Database**: the same PostgreSQL + PostGIS schema, read and written the same way.
   Java Flyway (`dhis-support-db-migration`, about 390 migrations) stays the source of
   truth for the schema. The Rust server must start on a DB that Java has migrated, and
   Java must start on a DB that Rust has written to. Do not add or alter tables unless
   a Java migration exists for it.
3. **Configuration**: reads `DHIS2_HOME/dhis.conf` with the same keys and defaults
   (see `ConfigurationKey.java`, about 300 keys), the same env vars, and the same
   `system settings` and `user settings` semantics.
4. **Auth**: existing users can log in with existing credentials. This covers
   BCrypt password hashes, basic auth, session cookies, personal access tokens (PATs),
   OAuth2/OIDC (provider and client), 2FA (TOTP and email), LDAP, account lockout,
   and the password policy.
5. **Semantics**: authorities, sharing/ACL (the JSONB `sharing` column), org-unit
   hierarchy scoping (capture/search/view), category option restrictions, data
   approval, locking/expiry, audit tables, and the `fields=` / `filter=` / `order=`
   query DSL. These must all produce identical results.
6. **Side effects**: scheduled jobs (`JobType`), notifications, outbound messaging
   (email/SMS), event hooks, file resources (filesystem and S3 via the jclouds-compatible
   layout), app install and serving, and cache invalidation must all behave the same.

If behaviour is ambiguous, **the Java implementation wins**. That includes bugs that
clients depend on. Write down every intentional deviation in `docs/deviations.md`,
and each one needs a justification.

---

## 2. Strategy: strangler fig with differential testing

Do NOT attempt a big-bang rewrite. The Java code is about 640k LOC of main code
plus about 2,000 test files.

- **Gateway**: build a thin Rust reverse proxy first (`gateway` crate). Every request
  goes to Java by default. A route table moves endpoints to the Rust handlers one at a
  time. Rollback is a config flip.
- **Shared DB**: both servers point at the same Postgres while the migration runs.
  Rust must be safe to run beside Java. Respect Java's caches: write through the same
  tables and trigger whatever cache invalidation Java expects. Study the Debezium-based
  `dhis-support-cache-invalidation` module and the Hibernate second-level cache usage.
- **Shared auth during transition**: early on, Rust can delegate authentication to
  Java. Forward the cookie or token to `GET /api/me`, then cache the resolved user
  for a short TTL. Native Rust auth is its own later milestone.
- **Differential oracle**: build a `diffcheck` tool. It replays the same request
  against Java and Rust on identical DB snapshots and diffs the responses after
  normalisation (ignore volatile fields such as `lastUpdated`, generated UIDs,
  timing, and key order where JSON order is not significant). Also diff the DB rows
  touched by write requests. An endpoint is "ported" only when `diffcheck` passes on
  its whole request corpus.
- **Existing black-box tests are the acceptance suite**: `dhis-test-e2e` (about 330
  RestAssured test files) talks HTTP only. It must run green against the gateway with
  any mix of routes. Wire it into CI against the Rust gateway early. Mine
  `dhis-test-web-api` (about 316 controller tests) for request/response fixtures.
- **Test data**: use the Sierra Leone demo DB (the same dump `docker-compose.yml`
  downloads) for realistic coverage.

---

## 3. Target architecture (defaults; propose changes via ADR before deviating)

Cargo workspace, crates roughly mirroring the Java modules:

| Rust crate | Java counterpart |
|---|---|
| `dhis-core-model` | `dhis-api` domain types (UIDs, IdentifiableObject, sharing, periods) |
| `dhis-db` | Hibernate mappings (108 `*.hbm.xml`) translated to explicit SQL via `sqlx` (Postgres) |
| `dhis-schema` | `dhis-service-schema` (property introspection backing `/api/schemas`, field filtering, validation) |
| `dhis-query` | the `fields=`, `filter=`, `order=` DSL and the metadata query engine |
| `dhis-acl` | `dhis-service-acl`, authorities, org-unit scoping |
| `dhis-metadata` | metadata CRUD, `dhis-service-dxf2` metadata import/export (preheat, object bundle, validation, atomic modes) |
| `dhis-aggregate` | data values, data value sets (JSON/XML/CSV/ADX), complete registrations, approval, locking |
| `dhis-tracker` | `dhis-tracker` (import/export, ownership, program rules via an equivalent of `rule-engine-jvm`) |
| `dhis-analytics` | analytics table generation + query engine; SQL dialects for Postgres, ClickHouse, Doris |
| `dhis-expression` | DHIS2 expression/indicator/predictor/validation-rule language (ANTLR grammar in Java; reimplement the grammar faithfully, e.g. with a hand-written parser or `pest`, and keep the same error messages) |
| `dhis-jobs` | scheduler and `JobType` implementations, job progress API |
| `dhis-auth` | sessions, PAT, OAuth2/OIDC, LDAP, 2FA |
| `dhis-files` | file resources, apps, static content, icons |
| `dhis-reporting` | pivot/data set reports, PDF/XLS output (JasperReports and POI have no Rust equivalent; byte-identical PDFs are not required, but structure and content must match) |
| `dhis-web` | `axum` routers, one module per Java controller, content negotiation, `WebMessage` errors |
| `gateway`, `diffcheck` | migration tooling |

Stack defaults: `tokio`, `axum`/`tower`, `sqlx` (Postgres, compile-time-checked
queries), `serde`/`serde_json`, `quick-xml`, `csv`, `rust_xlsxwriter`/`calamine`,
`geo`/`geozero` (PostGIS), `tracing`, `bcrypt`, `openidconnect`, `ldap3`, `totp-rs`,
`redis` (only where Java uses Redis), and `lettre` (email). Prefer mature crates
and pin exact versions. No `unsafe` without an ADR.

Mirror Java package and class names in module/doc comments
(`// Ported from org.hisp.dhis.dataset.DataSetController`). That keeps traceability
mechanical.

---

## 4. Phased plan (each phase ends with green e2e + diffcheck, before moving on)

0. **Inventory (automated, before writing server code)**:
   - generate `docs/inventory/endpoints.csv`: every Spring mapping with controller,
     method, path, params, produces/consumes, required authority;
   - `docs/inventory/tables.csv` from the migrated schema plus the hbm.xml files;
   - `docs/inventory/config-keys.csv`;
   - `docs/inventory/jobs.csv`.

   Derive these by parsing the Java source or by introspecting a running Java
   instance (`/api/openapi.json`, `/api/schemas`). Do not hand-write them.
   These files are the progress ledger.
1. **Foundations**: workspace, config loading (`dhis.conf`), DB pool, gateway,
   diffcheck, CI running e2e through the gateway (all routes → Java).
2. **Read-only metadata**: `/api/schemas`, generic metadata GET for all schema types
   with full `fields`/`filter`/`order`/paging/sharing semantics, `/api/me`,
   `/api/system/info`, settings reads. This is the highest-leverage slice:
   field filtering and the query engine sit under hundreds of endpoints.
3. **Metadata writes**: CRUD, JSON Patch, metadata import/export (`/api/metadata`),
   validation, audit, cache invalidation.
4. **Aggregate data**: data entry APIs, data value sets, approval, locking, ADX.
5. **Tracker**: `/api/tracker` import/export, ownership/audit, program rules,
   notifications.
6. **Expressions and analytics**: parser first (with exhaustive grammar tests ported
   from Java), then analytics table export jobs, then `/api/analytics*` queries per DB
   dialect.
7. **Native auth**: sessions, PATs, OAuth2/OIDC, LDAP, 2FA. After this, Rust no
   longer depends on Java for login.
8. **Long tail**: jobs, messaging, SMS, reports/PDF, apps, data exchange, event hooks,
   remaining endpoints in the inventory.
9. **Cutover**: Rust runs alone, with Java shut off. Then run the full e2e suite,
   the performance suite (`dhis-test-performance`, Gatling) with no regression,
   and a soak test against a production-sized DB.

---

## 5. How to work (per slice)

For each endpoint group:

1. Read the Java controller, its services, the Hibernate mapping, and its tests.
   Note the authority checks, sharing checks, and every error branch.
2. Capture a request corpus. Take requests from `dhis-test-web-api`/`dhis-test-e2e`
   tests and add edge cases: missing params, bad UIDs, forbidden user, paging limits,
   every `Accept` type.
3. Write failing Rust tests from that corpus (TDD), then implement.
4. Run diffcheck against Java. Fix diffs until it passes. Log any intentional
   difference in `docs/deviations.md`.
5. Flip the gateway route, run e2e, and update `endpoints.csv` status
   (`java` → `rust-shadow` → `rust`).
6. Commit in small, reviewable steps, one controller or service per commit,
   with conventional commit messages.

Keep `PORT_STATUS.md` updated at the end of every session. It should hold the
current phase, ported/total counts from the inventory, open problems, and the
next three concrete tasks. Read it first at the start of every session.
Record architectural decisions as ADRs in `docs/adr/`.

---

## 6. Rules

- Never guess Java behaviour. Read the code or run it against a live Java instance
  (`docker compose up` in `../dhis2-core`). When unsure, measure.
- Never edit the database schema outside Java Flyway migrations.
- Never weaken a test, normaliser, or diffcheck rule to make something pass.
  If a diff is genuinely irrelevant, add the normalisation with a comment explaining why.
- Security parity is non-negotiable. Every handler must apply the same authority,
  sharing, and org-unit checks as Java. Add negative tests (wrong user/authority)
  for every write endpoint.
- Performance: no endpoint may be slower than Java at p95 on the demo DB. Watch for
  N+1 queries that Hibernate batching used to hide.
- Ask me before you add a new external service dependency, change the phase order,
  or declare any part of the API out of scope.
- If a session is getting long, stop at a clean commit boundary and update
  `PORT_STATUS.md` rather than leaving work half-done.

## 7. First session deliverables

1. The Cargo workspace skeleton with the crates above (empty but compiling).
2. Phase 0 inventory files, generated by a script checked into `tools/`.
3. `dhis.conf` loader with tests against `../dhis2-core/dhis-2/dhis-test-e2e/config/dhis2_home/dhis.conf`.
4. Gateway that proxies 100% to Java, plus a CI job running `dhis-test-e2e` through it.
5. `PORT_STATUS.md` with a sized estimate per phase based on the inventory.
