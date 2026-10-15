# ADR 0002: Gateway design

- Status: accepted
- Date: 2026-10-15

## Context

The migration is a strangler fig: a Rust gateway sits in front of Java and moves endpoints one at
a time. The gateway must be invisible. Clients, Java and the e2e suite must not be able to tell it
is there while it routes 100% to Java.

## Decision

1. **Single binary, Rust handlers in-process.** The gateway mounts `dhis_web::router()` and
   dispatches to it with `tower::ServiceExt::oneshot`. No second Rust service and no extra hop.
2. **Route table in TOML (`config/routes.toml`).** It is first-match-wins and defaults to Java.
   Routing uses the path after Java's own normalisation (`ApiVersionFilter`'s `/api/{28..44}/`
   strip, plus `CustomRequestMappingHandlerMapping`'s single trailing slash and registered
   media-type extension). So one rule covers every spelling Java treats as the same endpoint.
   `SIGHUP` reloads the table; a reload that fails keeps the old one. Rollback is a file edit.
3. **Transparency rules for the Java path:**
   - pass method, path and query byte for byte, and keep `Host` (Java builds hrefs from it);
   - preserve header-name case and Tomcat's empty reason phrase (hyper `preserve_header_case`);
   - stream bodies both ways, with no buffering and no timeouts;
   - drop only RFC 9110 hop-by-hop headers (plus those named in `Connection`) and `Expect`
     (hyper answers `100-continue` itself);
   - answer in the client's HTTP version;
   - `X-Forwarded-For` is added only when the client sent none. Java's `getRemoteAddr()` sees the
     gateway, but its XFF-aware paths (API-token IP allowlist, auth audit) still see the client.
     This can be turned off.
4. **Upstream is plain HTTP**, scheme and authority only. A Java context path such as `/dhis`
   must already be in the client URL, because the gateway forwards paths as-is.
5. **e2e wiring** (`ci/e2e/docker-compose.gateway.yml`): the gateway runs in the test container's
   network namespace and `web` resolves to `127.0.0.1` there. The tests keep their URLs and Host
   header. Java (alias `java`) still sees connections from `172.30.0.10`, so the impersonation IP
   allowlist in the upstream `dhis.conf` keeps working without edits.

## Consequences

- IP-sensitive Java code that calls `getRemoteAddr()` sees the gateway's address in a real
  deployment: impersonation allowlist, `MeController`, reCAPTCHA. Operators must allowlist the
  gateway's IP, as they already do for nginx. This is recorded in `docs/deviations.md`.
- Shadow mode (serve Java's response, compare with Rust off the request path) is not built yet.
  `diffcheck` covers the comparison offline for now.
- No WebSocket/SSE support is needed: `dhis-web-api` has neither (checked 2026-10-15).
