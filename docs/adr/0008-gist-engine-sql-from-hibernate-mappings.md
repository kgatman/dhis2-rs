# ADR 0008: The Gist engine writes PostgreSQL from generated Hibernate mappings

- Status: accepted
- Date: 2026-10-15

## Context

Metadata lists are served by two Java engines. `AbstractFullReadOnlyController#getObjectList`
first asks `GistBridge` whether the Gist engine (`org.hisp.dhis.gist`) can answer the request;
with the default `fields` (`id,displayName`) and simple fields, filters and orders it can, so most
list requests by clients (`/api/constants`, ...) are Gist responses (they carry `"gist":true`).
The `/api/{type}/gist` endpoints use the same engine directly. Everything else (`fields=*`,
presets, `query`, `paging=false`, ...) goes to the metadata query engine.

The Gist engine plans the fields (`GistPlanner`), validates (`GistValidator`), writes HQL
(`GistBuilder`), lets Hibernate run it and streams the rows as JSON with json-tree
(`GistOutput`, not Jackson). HQL names entity properties; Hibernate maps them to tables and
columns from `*.hbm.xml` files and JPA annotations.

## Decision

1. `dhis-inventory` generates `crates/dhis-schema/data/hibernate/hbm-entities.json` from the
   `*.hbm.xml` files of the pinned source: per class its table, identifier column and, per
   property, its column, type, relation target, plus inheritance flags. CI checks it for drift
   like the other inventory outputs. JPA-annotated (`@Entity`) classes are added in the same
   shape from their annotations (JPA defaults where no name is given). All 2549 recorded columns
   were checked against a Java-migrated database (all present). Where the mapping and the
   database could disagree (not-null flags), the SQL follows Hibernate's own form instead of
   relying on the flag. The SQL forms for collections and references were taken from the
   statements Hibernate 5 actually sends (PostgreSQL statement log).
2. `dhis_query::gist` ports the engine for list queries and writes the SQL Hibernate's
   translation amounts to (`e.<property>` → `e.<column>`, entity comparison → identifier
   columns, `setFirstResult`/`setMaxResults` → `offset`/`limit`, HQL functions DHIS2 registers
   are PostgreSQL functions of the same name). The bridge decision (`GistBridge`) is ported too;
   only a definite "Gist" answer is acted on.
3. The port covers part of the engine (simple and translated fields, `href`, references with
   `apiEndpoints`, presets other than `:identifiable`/`:nameable`, filters and orders on simple
   persisted properties, paging, totals, pager links, `headless`, `unwrap`, `absoluteUrls`,
   `references`, `auto`, `locale`). Anything else returns `Unsupported` and the request goes to
   Java (ADR 0004). So does every request Java answers with an error: Rust never writes a Gist
   error response, and Java's quirks there (500s on single null columns, `pageSize=0`) stay
   Java's.
4. json-tree's own output rules are reproduced: its escaping, null members left out, and its
   `TextBuilder` number shortcut in single-field lists, which prints `0.05` as `0.5`
   (fixtures from the server image: `tools/gen-json-tree-fixtures.sh`).
5. **Superusers only** for now. For other users the query needs a sharing filter on the user's
   id and group ids from the session's `UserDetails`; delegated authentication does not provide
   them and no cheap Java endpoint returns them (`/api/me` builds the whole `MeDto`, 2.5–8 ms).
   Non-superuser requests fall back. The routes stay in shadow mode until this is settled
   (PORT_STATUS.md, open problem).
6. The translation locale is the session's database locale, so Gist routes authenticate with
   both probes (`Probe::AuthoritiesAndSettings`, ADR 0006): at once on an existing session;
   for basic auth the settings call rides on the session the first call created, so Java still
   sees one authentication.

## Consequences

- Adding a metadata type is mostly data: a registered schema, an hbm mapping of plain shape, and
  a corpus. Types with inheritance, JPA mappings, collections or JSONB-mapped values
  (`sharing`, `translations`, `attributeValues`) need more of the engine.
- SQL is built from mapping identifiers and validated literals (locales, translation keys);
  values are bound. Anything unusual in a literal position falls back.
- `fields=*` and the other presets list properties in schema order, whose ties are Java's
  `HashMap` order of one JVM run (deviation M4).
