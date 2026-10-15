# ADR 0006: Session-bound state comes from the delegated authentication call

- Status: accepted
- Date: 2026-10-15

## Context

Some responses depend on state that Java keeps **in memory per session or per user**, not in
the database:

- `UserSettings.getCurrentSettings()` (`/api/me/settings`, `/api/me`): `SessionUserSettings`
  holds one snapshot per username. It is refreshed on every successful authentication
  (`AuthenticationSuccessEvent`, so on each basic-auth request) and on user-setting writes,
  but not on system-setting writes, and it is cleared when any session of that user is
  destroyed. Recomputing from `usersetting` + `systemsetting` would differ whenever a cookie
  session predates a system-setting change, or after another session of the user expired.
- `UserDetails#getAllAuthorities()` (`/api/me/authorities`): the session's `UserDetails`, built
  at login.

Rust cannot see that memory while authentication is delegated to Java (ADR 0004).

## Decision

The delegated authentication call (ADR 0004) is the one request per Rust-served request that
Java answers from the session state. It asks for exactly what the handler needs
(`dhis_auth::delegated::Probe`, chosen by `dhis_web::auth_probe` from the path):

- `GET /api/me/authorities` (default): the session's `UserDetails#getAllAuthorities()`, in Java's
  own iteration order;
- `GET /api/me/settings` (for `/api/me/settings[/{key}]`): `UserSettings.toJson(false)`, rendered
  from the same `ThreadUserSettings` / session state Java would use for the request itself;
- both (`Probe::AuthoritiesAndSettings`, Gist lists: ADR 0008): at once when the request is on
  an existing session; otherwise the settings call follows on the session the authorities call
  created (`JSESSIONID`, credentials dropped), so Java sees one authentication. Credentials that
  create no session (tokens) get no settings, and the handler falls back.

Rust keeps the result verbatim (member order and JSON text) as `CurrentUser::authorities` /
`CurrentUser::settings`; a handler that needs the other one falls back to Java. Per-request
overrides (`translate=true&locale=`, `UserContextInterceptor`) are applied by Rust, as the auth
call does not carry the client's query.

Both endpoints are cheap (measured 0.4–0.7 ms; the first version of this ADR used
`/api/me?fields=...`, but `/api/me` builds the whole `MeDto` whatever `fields` asks: 2.5–8 ms per
request, which made small Rust responses several ms slower than Java at p95).

What the snapshot cannot tell (raw stored strings behind typed values, the empty-settings case)
makes the handler fall back to Java (`user setting value`).

## Consequences

- `/api/me/settings` and `/api/me/settings/{key}` match Java including its staleness rules.
- Confidential settings are never in the snapshot (`toJson(false)`); `/api/me`'s
  `settings[...]` (which includes confidential values) will need another source or a fallback.
- With native sessions (phase 7) Rust owns this state and must reproduce
  `SessionUserSettings`' refresh rules itself.
