# ADR 0007: Registered schemas are captured from the pinned Java build

- Status: accepted
- Date: 2026-10-15

## Context

`/api/schemas` renders `SchemaService`'s registered schemas (116 at the pinned ref). Java builds
them at startup from the schema descriptors, Jackson annotations and the **Hibernate mappings**
(persisted, length, unique, many-to-many roles, ...). The dynamic schemas used so far could be
computed by running Java's own introspectors in jshell (`tools/gen-dynamic-schemas.sh`), but
the Hibernate introspector needs a running session factory, i.e. the whole server.

The output also has request- and JVM-dependent parts, all measured:

- links (`href`, `apiEndpoint`, and `relativeApiEndpoint` on reference properties) are written
  into the shared `Schema`/`Property` objects by each `getSchemas`/`getSchema` request, from that
  request's API path; `getSchemaProperty` shows whatever the last such request left;
- the order of schemas with equal `order`, of each schema's `references` (a set of `Class`) and
  of `properties` (a `HashMap` with ties in reflection order) changes between JVM starts. The
  content does not (verified across a restart).

## Decision

1. `tools/gen-registered-schemas.sh` captures `GET /api/schemas` (default `fields=*`, which
   equals `:all` for `Schema`) from a server at the pinned ref into
   `crates/dhis-schema/data/registered/schemas.json`, with `Host: rust-capture.invalid` so the
   links carry a recognisable placeholder base. Regenerate it whenever the ref moves.
2. Rust parses it once into field-filter nodes (`Schema`/`Property`/`Authority`/`GistPreferences`
   described by their dynamic schemas) and serves `getSchemas`, `getSchema` and
   `getSchemaProperty`, replacing the placeholder with the request's API path.
3. The per-schema "last links" state is kept in Rust and updated exactly where Java updates it.
   All three handlers move together; otherwise Java's state would stop being updated.
4. Orders are those of the captured run, which is one of the orders Java produces. diffcheck
   compares those three arrays as sets (documented rule), since Java itself reorders them on
   restart.

## Consequences

- Responses match Java byte for byte for the JVM the capture came from, and as sets otherwise.
- The capture must be refreshed when the pinned ref changes. A CI check against a fresh server
  is still to do (it needs the e2e stack).
- Link state starts empty when the gateway starts. It matches Java's only if both started
  together or a schema request has been served since (deviation M3).
- At cutover (phase 9) the data stays generated, so Rust does not need Hibernate.
