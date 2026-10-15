# ADR 0001: Workspace layout, dependency pinning, generated code

- Status: accepted
- Date: 2026-10-15

## Context

The charter (`dhis2-rust-port-prompt.md` §3) proposes one crate per Java module group. Three
practical questions came up in session 1:

1. Where does `dhis.conf` loading live? It is needed by the gateway, the DB pool and every later
   crate. The charter's table has no crate for it.
2. What language should the phase-0 inventory generator be written in?
3. How should we keep Rust copies of Java enums (e.g. `ConfigurationKey`) from drifting?

## Decision

- **Add `crates/dhis-config`**, the counterpart of `dhis-support-external`
  (`org.hisp.dhis.external.conf`, `org.hisp.dhis.external.location`). It has no dependencies on
  other workspace crates, so anything can depend on it. This is the only crate added beyond the
  charter's table.
- **Write the generator in Rust** (`tools/inventory`, a workspace member) using `tree-sitter-java`.
  CI then needs a single toolchain, and the parser handles modern Java (records, switch
  expressions, text blocks) that Java-8-era Python parsers do not.
- **Generate the Rust copies of Java tables from the Java source.**
  `crates/dhis-config/src/keys_generated.rs` is written by `tools/inventory` together with the
  CSVs. CI runs `dhis-inventory --check` against the pinned dhis2-core commit (`ci/dhis2-core.ref`)
  and fails if anything changed.
- **Pin every dependency exactly** (`=x.y.z` in `[workspace.dependencies]`) and commit
  `Cargo.lock`. Upgrades are explicit commits.
- `unsafe_code = "forbid"` is set workspace-wide. Lifting it for a crate needs its own ADR.

## Consequences

- Moving `ci/dhis2-core.ref` to a newer Java commit is how we "rebase" onto upstream. CI then
  shows exactly which endpoints, keys, jobs and tables changed (CSV diff).
- `crates/*` are workspace members by glob, so a new crate needs no root `Cargo.toml` edit.
