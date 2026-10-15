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

- `ConfigurationKey` alias lookup uses `Properties.contains(alias)`, which checks **values**,
  not keys. Setting `audit.inmemory_queue.enabled` has no effect. A property whose value equals
  an alias name changes the result.
- `getIntProperty` passes `NumberUtils.isParsable` and then calls `Integer.parseInt`, so `1.5`,
  ` 7` and `1e3` make Java throw rather than return `-1`.
- A `dhis-google-auth.json` without `client_id` makes Java fail at startup
  (`NullPointerException` from `Properties.put`). The Rust loader also fails.

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
