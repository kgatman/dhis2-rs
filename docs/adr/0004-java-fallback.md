# ADR 0004: Rust handlers may hand a request back to Java

- Status: accepted
- Date: 2026-10-15

## Context

Phase 2 slice 1 (`/api/system/{uid,id,uuid}`) showed how much of a Java response comes from
outside the controller. Some examples, all measured:

- `Accept: text/csv` or `application/xml` → Tomcat's HTML 406 page, because Spring's
  `WebMessage` for the error cannot be rendered in the requested type;
- unauthenticated → `302 Location: /login/`; bad credentials → 401 JSON from Spring Security;
- wrong method → 405 `WebMessage`;
- CORS preflight/`Origin` handling in `DhisCorsProcessor`.

Reproducing every such branch before an endpoint can move would slow the migration a lot, and
an approximation would break the drop-in guarantee.

## Decision

1. **Route rules list methods.** Methods not listed stay on Java, so Java answers them (405,
   handlers not ported yet).
2. **Authentication failures go to Java.** If the delegated `/api/me` check does not return 200,
   the gateway replays the original request to Java, which produces its exact 302/401/403.
3. **A handler may return `JavaFallback { reason }`** for any branch it does not implement:
   content negotiation outside the modelled cases, `Origin`/CORS requests, unported
   parameters or representations. The gateway replays the *original* request (same path,
   headers and body; the body is buffered for this) to Java.
4. **Decision order follows Spring.** Handler selection (negotiation) happens before parameter
   binding, so a fallback is decided before any 400 is produced (`text/csv` + `limit=abc` is
   Java's 406, not our 400).
5. **Fallbacks are counted** by reason (`Gateway::fallback_counts`, gateway log `fallback to
   Java`). A route counts as fully ported only when its fallback count is zero on the full
   corpus.
6. **Shadow mode is skipped on fallbacks**, so no comparison is made against a response that
   Java made anyway.

## Consequences

- An endpoint can move as soon as its common paths are exact; the long tail stays correct
  because Java answers it.
- At cutover (phase 9) every fallback must be gone. The counters show what is left.
- On fallback after successful delegated authentication, a basic-auth request creates one
  extra Java session (from the `/api/me` call). Fallback responses carry Java's own
  `Set-Cookie`, so clients see no difference; the orphan session expires normally.
- Shadow runs also authenticate through `/api/me`, so they add a session (basic auth) and an
  authentication event per shadowed request. That's acceptable for a validation mode; don't
  leave shadow on for heavy production traffic.
