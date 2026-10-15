# dhis2-rust

A Rust reimplementation of [DHIS2 core](https://github.com/dhis2/dhis2-core), built as a drop-in
replacement for the Java server: same HTTP API, same PostgreSQL schema, same `dhis.conf`.

The Java source is the specification. The charter is
[`dhis2-rust-port-prompt.md`](dhis2-rust-port-prompt.md); progress is in
[`PORT_STATUS.md`](PORT_STATUS.md).

## Layout

| Path | What |
|---|---|
| `crates/gateway` | Reverse proxy in front of Java; `config/routes.toml` moves endpoints to Rust one at a time |
| `crates/diffcheck` | Replays a request corpus against Java and Rust and diffs normalised responses |
| `crates/dhis-config` | `DHIS2_HOME/dhis.conf` loading, identical to Java |
| `crates/dhis-db` | Postgres pool from `dhis.conf` |
| `crates/dhis-*` | Domain crates mirroring the Java modules (mostly empty so far) |
| `tools/inventory` | Generates `docs/inventory/*.csv` (the progress ledger) from the Java source |
| `docs/adr`, `docs/deviations.md` | Decisions, and every intentional difference from Java |
| `ci/` | Pinned dhis2-core commit, e2e-through-gateway wiring |

## Prerequisites

- Rust 1.98 (`cargo`)
- A dhis2-core checkout at the commit in `ci/dhis2-core.ref`, next to this repo
  (`../dhis2-core`) or pointed to by `DHIS2_CORE_DIR`. It is read, never modified.
- Docker, for the e2e suite.

## Common commands

```sh
cargo test --workspace                    # unit + integration tests
tools/gen-inventory.sh                    # regenerate docs/inventory (add --check in CI)

# Run the gateway in front of a Java server on :8081
cargo run -p gateway -- --listen 0.0.0.0:8080 --java-upstream http://localhost:8081 \
  --routes config/routes.toml

# Compare Java and Rust on a corpus
cargo run -p diffcheck -- --java http://localhost:8081 --rust http://localhost:8090 corpus.jsonl

# dhis2-core's e2e suite through the gateway (needs the dhis2/core-dev:local image)
ci/e2e/run.sh
```
