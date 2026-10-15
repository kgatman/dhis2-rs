# ADR 0003: Phase-0 inventory is derived from source; live introspection follows

- Status: accepted
- Date: 2026-10-15

## Context

The charter allows the inventory to come from parsing the Java source or from introspecting a
running Java instance (`/api/openapi.json`, `/api/schemas`, the migrated schema). No Java
instance was running in session 1.

## Decision

Generate everything from the pinned source now. Treat live introspection as the cross-check that
must agree before phase 2 starts.

| File | Source | Known limits |
|---|---|---|
| `endpoints.csv` | Spring annotations in `dhis-web-api`, expanded through the controller class hierarchy the way Spring registers handlers; method-less mappings default to GET (DHIS2's `CustomRequestMappingHandlerMapping`) | Spring Security filter endpoints (`/oauth2/*`, form login, logout), static resources and app serving are not controller mappings and are not listed. Overrides are matched by method name + arity. |
| `tables.csv` | `*.hbm.xml` + JPA `@Entity` classes, joined with a textual replay of `CREATE`/`DROP`/`ALTER … RENAME` in the Flyway base schema and migrations (SQL and Java) | Not a schema: no columns from Flyway, dynamic SQL in Java migrations can be missed. Runtime tables (`analytics_*`, `_*` resource tables) are not listed. |
| `config-keys.csv` | `ConfigurationKey` enum | Complete (it is the definition). |
| `jobs.csv` | `JobType` enum, its flag methods, `Job#getJobType()` implementations | Complete for the enum; implementation lookup is by `getJobType()`. |

The `status` column (`java` → `rust-shadow` → `rust`) is hand-maintained and kept when the files
are regenerated.

## Consequences

Next tasks (PORT_STATUS.md): start Java from `ci/dhis2-core.ref`, then (a) diff `endpoints.csv`
against `/api/openapi.json` and (b) diff `tables.csv` against `information_schema` of the migrated
DB. Fix the generator wherever they disagree.
