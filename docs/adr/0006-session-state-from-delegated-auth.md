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

The delegated authentication call already asks Java's `/api/me` about the requesting user on
every request routed to Rust. It now also requests `settings`
(`/api/me?fields=id,username,authorities,settings`), which Java renders from the same
`ThreadUserSettings` / session state it would use for the request itself
(`UserSettings.toJson(false)`). Rust keeps that object verbatim (member order and JSON text), as
`CurrentUser::settings`, and builds the settings responses from it. Per-request overrides
(`translate=true&locale=`, `UserContextInterceptor`) are applied by Rust, as the auth call does
not carry the client's query.

No extra round trip; the snapshot is exactly what Java would use at that moment.

What the snapshot cannot tell (raw stored strings behind typed values, the empty-settings case)
makes the handler fall back to Java (`user setting value`).

## Consequences

- `/api/me/settings` and `/api/me/settings/{key}` match Java including its staleness rules.
- Confidential settings are never in the snapshot (`toJson(false)`); `/api/me`'s
  `settings[...]` (which includes confidential values) will need another source or a fallback.
- With native sessions (phase 7) Rust owns this state and must reproduce
  `SessionUserSettings`' refresh rules itself.
