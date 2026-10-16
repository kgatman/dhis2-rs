# Deviations from the Java behaviour

Every intentional difference between the Rust port and Java 2.44-SNAPSHOT
(`ci/dhis2-core.ref`) is listed here with a justification. Anything missing from this list is
treated as a bug.

Entries marked **transition** go away at cutover (phase 9).

## Configuration (`dhis-config`)

| # | Java | Rust | Why |
|---|---|---|---|
| C1 | DHIS2_HOME can come from the JNDI context variable `dhis2-home` or the JVM system property `dhis2.home` | No JNDI. `--dhis2-home` (where a binary exposes it) stands in for `-Ddhis2.home`. `DHIS2_HOME` and the `/opt/dhis2` default behave the same. | There is no servlet container or JVM. The precedence order is unchanged. |
| C2 | `\uXXXX` escapes in `dhis.conf` may produce unpaired UTF-16 surrogates | Unpaired surrogates become U+FFFD | A Rust `String` cannot hold them. No valid configuration value contains one. |
| C3 | `Integer.parseInt` / `Long.parseLong` accept any Unicode decimal digits (e.g. Arabic-Indic) | ASCII digits only | Writing `connection.pool.max_size = ٨٠` is not a realistic configuration. Revisit if anyone reports it. |
| C4 | `dhis-google-auth.json` is turned into a Google credential (Earth Engine tokens) | Only `client_id` is copied into the properties, as Java does. The credential itself is not built yet. | Earth Engine token endpoints are phase 8. |
| C5 | `getEncryptionStatus()` can return `MISSING_JCE_POLICY` | Never returned | Java 17+ always has the unlimited JCE policy, so Java never returns it either. |

**Bugs kept on purpose** (Java behaviour reproduced, not a deviation, listed so nobody "fixes" it):

- `CspConstants.EXTERNAL_STATIC_CONTENT_URL_PATTERNS` is `List.of(P_1, P_2, P_2, P_4)`: the
  `fileResources/{uid}/data` pattern (`P_3`) is never applied, so those responses get no
  `script-src 'none'` CSP header.
- `ConfigurationKey` alias lookup uses `Properties.contains(alias)`, which checks **values**,
  not keys. Setting `audit.inmemory_queue.enabled` has no effect. A property whose value equals
  an alias name changes the result.
- `getIntProperty` passes `NumberUtils.isParsable` and then calls `Integer.parseInt`, so `1.5`,
  ` 7` and `1e3` make Java throw rather than return `-1`.
- A `dhis-google-auth.json` without `client_id` makes Java fail at startup
  (`NullPointerException` from `Properties.put`). The Rust loader also fails.

## Query DSL (`dhis-query`)

| # | Java | Rust | Why |
|---|---|---|---|
| Q1 | A `fields=` parse error at a supplementary character (e.g. `𝔸`) prints the lone high surrogate (`charAt`) in the message | Prints U+FFFD | A Rust `String` cannot hold a lone surrogate. Position numbers are UTF-16 based, as in Java. |

**Gist bugs kept on purpose** (ADR 0008): in single-field lists (`unwrap`) json-tree prints
doubles with up to three decimals through a shortcut that drops leading zeros of the decimals and
the sign between -1 and 0 (`0.05` → `0.5`, `-0.5` → `0.5`); `name:gt:5` on a text property
compares the **length** (`isStringLengthFilter`); `order=name` sorts by the translated name; a nested `ref.displayName` is translated from
the root object's own translations and name (`GistBuilder#createTranslatedFieldHQL` always
uses `e.`); nested fields inner-join their reference, dropping rows without it from the page.
Requests on which Java fails (`fields=code` with a null code: 500; `pageSize=0`: 500) go to Java.

**Metadata query engine bugs kept on purpose** (`dhis_query::metadata`, lists the Gist bridge
leaves to `QueryService`): `like`-family filters strip `%` from the value but leave `_` (and `\`)
as SQL wildcards, while `query=` keeps `%`; `order=code:iasc` does not ignore case, because only
TEXT, EMAIL and USERNAME properties are lowered and `code`/`id` are IDENTIFIERs; any order on a
translatable property (`name`, `shortName`, ...) sorts by the value translated into the user's
database locale; a direction other than `asc`/`desc`/`iasc`/`idesc`, or a third `:` part
(`name:desc:x`), sorts ascending; explicit orders get no `id` tiebreaker, so ties come back in
whatever order PostgreSQL returns them; a page past the end returns no items with
`pager.page` clamped to the last page.

**Bugs kept on purpose:** `Fields.of("a::rename(x)::rename(y)")` yields no field at all, and an
inherited rename plus one transform drops the transform (`a::rename(x)[b::size]` → `a.b` without
`size`). See `crates/dhis-query/tests/fixtures/fields.tsv`.

## Database (`dhis-db`)

| # | Java | Rust | Why |
|---|---|---|---|
| D1 | Hikari `connection.pool.preferred.test.query` replaces the validity check | sqlx always uses its ping | sqlx has no custom test-query hook. Both detect dead connections. |
| D2 | Hikari `keepaliveTime` (`connection.pool.keep_alive_time_seconds`) | Not applied | sqlx has no keep-alive pinging. `idle_timeout`/`max_lifetime` still recycle connections. |
| D3 | pgjdbc multi-host URLs (`//a,b/db`, failover) | Rejected at startup with a clear error | Not used by the reference deployments. Add it if someone needs it. |
| D4 | pgjdbc honours many URL parameters | `sslmode`, `ssl`, `sslrootcert`, `sslcert`, `sslkey`, `ApplicationName`, `currentSchema`, `options`, `user`, `password`. Others are logged and ignored. | Covers DHIS2 deployment docs. Extend on demand. |
| D5 (transition) | One pool of `connection.pool.max_size` | The Rust pool can be capped separately (`max_size_override`) | Java and Rust share one Postgres during migration and must fit `max_connections` together. |

## Gateway (transition)

| # | Java direct | Through the gateway | Why |
|---|---|---|---|
| G1 | `request.getRemoteAddr()` is the client (or nginx) | It is the gateway | Unavoidable for a proxy, since Java has no RemoteIpValve. Affects the impersonation IP allowlist (`switch_user_allow_listed_ips`), `MeController`'s IP use and reCAPTCHA. Operators allowlist the gateway IP, as they do for nginx. XFF-aware code still sees the client (ADR 0002). |
| G2 | — | Upstream must be plain `http://` | Java behind the gateway runs on a private network. TLS terminates in front. |
| G3 | Tomcat closes the connection after some error responses (`Connection: close` on 400) | The gateway keeps client connections alive | Hop-by-hop behaviour of the transport, not of the API. No client relies on connection closing. |
| G4 | A request target containing raw `"`, `<` or `>` gets Tomcat's HTML 400 | hyper rejects it before routing with an empty `400 Bad Request` (any route) | hyper's request parser; same status, no client sends these unencoded. Other invalid targets (raw non-ASCII, `^`, `` ` ``, `[` `]` in the path, ...) and undecodable query parameters (Java's 409) are sent to Java and match exactly (`gateway::tomcat`). |
| G6 | A `#fragment` in the request target gets Tomcat's HTML 400 | `http::Uri` drops the fragment, so the request is served as if it had none (any route) | Clients never send fragments (RFC 9110 §4.2.4 forbids them in the request target). |
| G5 | — | A basic-auth request answered by Rust after a `JavaFallback` leaves one extra (unused) Java session; so can a Gist request with an expired session cookie (both authentication calls run at once and each may start a session) | Delegated authentication calls Java first (ADR 0004, 0006). The sessions expire normally. |
| G7 | — | A Rust-served request costs one extra cheap Java request (delegated authentication, ADR 0004/0006) plus the proxy hop: about 1–2 ms more than Java at p95 for the smallest responses (release build, session cookie: `/api/system/uid` 2.1 vs 0.8 ms, `/api/me/settings` 2.7 vs 0.7 ms); larger responses are as fast or faster (`/api/schemas` 30 vs 37 ms, `/api/schemas/dataElement` 2.1 vs 2.8 ms) | Inherent while Java authenticates. Gist lists need two authentication calls (authorities and settings, ADR 0008), at once on a session: `/api/constants` 2.8 vs 2.4 ms, `/api/constants/gist?fields=id,name` 2.3 vs 1.9 ms, a filtered and ordered page of 60 with total 3.3 vs 8.3 ms. Goes away with native auth (phase 7); the cutover gate (phase 9) measures p95 without it. |
| G8 | — | Requests by non-superusers to Rust routes that need the session's identity (Gist lists) go to Java when the gateway did not see the session being authenticated (sessions from before a gateway restart, from another gateway instance or made directly on Java) or carry tokens: same responses, plus the delegated authentication calls (about 1 ms) | The identity is only known for sessions the gateway saw created (ADR 0009). Goes away with native sessions (phase 7). |

## Mirrored from Java (transition)

| # | Java | Rust | Why |
|---|---|---|---|
| M1 | `/api/system/info` JVM/build values (`version`, `revision`, `buildTime`, `systemId`, `jasperReportsVersion`, `java*`, `os*`, `memoryInfo`, `cpuCores`) computed at JVM start | Read once from Java's own `/api/system/info` and cached | No JVM in Rust; identical output while both run (ADR 0005). If Java restarts, Rust keeps the old snapshot until it restarts. Native values are due at cutover. |
| M3 | `/api/schemas/{type}/{property}` shows the links (`href`, `apiEndpoint`, `relativeApiEndpoint`) the last `/api/schemas[/{type}]` request wrote into the shared schema objects; none since JVM start | Same state, kept by the gateway's Rust side: empty when the gateway starts, updated by every schema request it receives (including those it falls back on). XML requests for a property fall back and show Java's own state | Matches while Java and the gateway start together and all traffic goes through one gateway. Different only after a restart of just one of them, until a schema request is served (ADR 0007). |
| M5 | User and group display names in `sharing` come from caches (`userDisplayNameCache`, `userGroupNameCache`; a group's name is cached in the locale of the first request) | Read from the database per request (group names translated to the requester's locale) | Same values unless a name changed while cached or users with different locales asked first; the cache's staleness is not reproducible from outside the JVM. |
| M4 | The order of `/api/schemas` entries with equal `order`, of `references`, and of `properties` changes on every JVM start (identity hashes, reflection order). Gist presets (`fields=*`, `:all`, `:persisted`, `:owner`) list fields in that property order within each property type | The order of the run the data was captured from (`tools/gen-registered-schemas.sh`) | One of Java's own orders; content verified identical across restarts (ADR 0007, ADR 0008). For Gist only the member order of the objects is affected. |

## Java runtime

| # | Java | Rust | Why |
|---|---|---|---|
| R1 | DHIS2 2.44 runs on JDK 17 or 21, and some output depends on which: `HashSet(Collection)` sizing (key order of `/api/me/settings?key=...` with 12, 24, 48, ... keys), `Double.toString`, time-zone display names | Reproduces JDK 21, the runtime of the `dhis2/core` image (fixtures generated with it: `tools/gen-hashset-fixtures.sh`, `tools/gen-tz-names.sh`) | One reference runtime is needed; the official image is the drop-in target. `dev/java-local.sh` runs on JDK 17 and agrees on everything in the corpus. |

## HTTP responses produced by Rust

| # | Java | Rust | Why |
|---|---|---|---|
| H1 | Header names as Tomcat writes them (`X-XSS-Protection`) | HTTP/1 title case from hyper (`X-Xss-Protection`) | hyper has no public API to set the case of headers it generates. Header names are case-insensitive (RFC 9110 §5.1). Status line and reason phrase do match (`HTTP/1.1 200 `). |
| H2 | Header order as Tomcat emits it | Different order | Order of distinct header fields is not significant (RFC 9110 §5.3); repeated fields such as `Set-Cookie` keep their relative order. |
