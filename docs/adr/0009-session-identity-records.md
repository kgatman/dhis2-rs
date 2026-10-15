# ADR 0009: The gateway records the identity behind each Java session

- Status: accepted (decided with the owner)
- Date: 2026-10-15

## Context

Lists of shareable metadata are filtered by sharing for every user but superusers. The filter
uses the user id and group ids of the session's `UserDetails`, which Java builds when the session
is authenticated and keeps in its in-memory session (Tomcat; Redis only when `redis.enabled`, and
then as serialised Java objects). Measured: after removing a user from a group, an existing
session still reads what the group was shared; a new session does not.

Rust cannot read that memory, and no cheap Java endpoint returns the snapshot: `/api/me` reads
the database and builds the whole `MeDto` (2.5–8 ms). Native sessions (Rust issuing and checking
them) cannot coexist with Java serving the rest of the API before cutover, so they stay in
phase 7. The owner chose to record the identities instead.

## Decision

1. `dhis_auth::sessions::SessionIdentities` maps a Java session id (`JSESSIONID`) to the user id
   and sorted group ids. Entries idle longer than `system.session.timeout` are dropped (the
   session is gone in Java), and the least recently used beyond a maximum
   (`--session-records-max`, default 100 000).
2. The gateway sees every request and response (`gateway::identity`):
   - Java sets a new session on a basic-auth request (no session sent): the user is looked up
     in the database (`dhis_db::users`, exact username as `UserStore#getUserByUsername`), which
     is what Java builds `UserDetails` from for that request;
   - Java sets a new session on any other request (form login, OAuth2, ...): `/api/me` on that
     session right after the response, before the user can change. A group change in those few
     milliseconds would be missed; there is no exact source;
   - requests to paths that authenticate, re-authenticate or end the authentication of a session
     without a new id (`/api/auth/` with impersonation, `/api/account`,
     `/dhis-web-commons-security/` with logout, `/oauth2/`, `/login/`): the record is dropped, and
     taken again from `/api/me` if the response succeeded.
   Recording runs off the request path.
3. Rust requests on routes that need it (`dhis_web::needs_identity`) get
   `CurrentUser::identity` after Java has authenticated them (every request still goes through the
   cheap delegated authentication, ADR 0004):
   - basic auth without a session cookie: the database, as above (and recorded for the session
     Java created);
   - a session cookie without credentials: the record, if any;
   - anything else (both, tokens, several session cookies, unknown sessions): none, and the
     handler falls back to Java.
4. Authentication itself is still not cached (`dhis_auth::delegated`): this records only what
   Java itself fixes per session.

## Consequences

- Non-superusers on known sessions and basic auth are served by Rust (sharing filter as in
  `JpaQueryUtils.generateHqlQueryForSharingCheck`).
- Sessions the gateway did not see created (created before it started, on another gateway
  instance, or directly on Java) and token requests go to Java: correct, with the cost of the
  delegated authentication added (deviation G8). After a gateway restart this holds for every
  session until its user logs in again.
- All traffic must pass through the gateway for the records to stay right (the same assumption
  as the rest of the transition; a direct request to Java that impersonates would not be seen).
- Removed at phase 7, when Rust owns sessions.
