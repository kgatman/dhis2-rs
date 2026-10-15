# ADR 0005: Mirror Java's startup-time values during the transition

- Status: accepted (decided with the project owner, 2026-10-15)
- Date: 2026-10-15

## Context

`/api/system/info` (`DefaultSystemService`) returns values that Java computes **once at JVM
start** and that describe the JVM or its build: `version`, `revision`, `buildTime` and
`jasperReportsVersion` (from `build.properties`), `javaVersion`, `javaVendor`, `javaOpts`,
`osName`, `osArchitecture`, `osVersion` (JVM system properties), `memoryInfo` (heap at start),
`cpuCores`, and `systemId` (read from the `configuration` row at start, so `null` until a restart
when Java created the row after reading it).

The Rust server has no JVM. While both servers run, clients must not see different values
depending on which server answered.

## Decision

While Java runs alongside, Rust **mirrors** these values from Java:

- They are read from Java's own `/api/system/info?fields=...` with the **requesting user's
  credentials** (no stored service account), once, and cached for the life of the Rust process.
  Java does the same (computes them once).
- Public values (`version`, `revision`, `buildTime`, `systemId`) are fetched on the first
  request. Superuser-only values (`jasperReportsVersion`, `java*`, `os*`, `memoryInfo`,
  `cpuCores`) are fetched on the first superuser request. Java hides those from other users
  anyway.
- If Java cannot be reached, the request falls back to Java (ADR 0004), which then fails the
  same way it would without the gateway.

Values Rust can compute identically (settings, configuration, database info, clock, time zone,
request headers) are computed natively.

## Consequences

- Responses are identical to Java's (verified: diffcheck 48/48 including non-superusers;
  shadow 43 matches, 0 mismatches).
- If Java restarts, its new `memoryInfo`/`systemId` are not picked up until Rust restarts.
- The first mirror call creates one Java session for a basic-auth user (as in ADR 0004).
- **At cutover (phase 9)** these fields need Rust-native values: build metadata from the Rust
  build, OS details from the host, and a decision on the JVM-only fields (`javaVersion`,
  `javaVendor`, `javaOpts`, `memoryInfo`, `jasperReportsVersion`). That decision will be logged
  in `docs/deviations.md` then.
