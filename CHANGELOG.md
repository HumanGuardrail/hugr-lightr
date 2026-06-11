# Changelog — hugr-lightr

## [Unreleased] — R1→R4 (2026-06-12): the full local product

338 tests, 0 failures, clippy -D clean, bench --check green. 9 crates.

**R1 — runtime parity + time axis + agent surface.** `ps/logs/stop/exec`
(daemonless supervisor: run owns its control socket, dies together), `gc`
mark/sweep (dry-run default, min-age), `-d` detach, `--mount` (CoW ref →
target), `undo`/`diff`/`bisect` over a reflog (the time axis), `plan`
dry-run, `--events` ndjson, **`lightr mcp`** (the runtime is an MCP server:
5 tools over JSON-RPC stdio). A9–A16.

**R2 — the Linux tier.** `oci import` (OCI layout + docker-save tar,
**sha256-verified fail-closed**, whiteouts/opaque/hardlink per spec) and
`oci pull` (registry, sha256-verified); `Engine` trait with `native` (real),
`ns` (Linux code, honest probe on macOS), `vz` (Swift shim + minimal kernel
behind a build feature, boot path for spike S5 — capability-probed, never a
silent skip); `run --engine/--rootfs`, `engine ls/install-pack`. A17–A21.

**R3 — ecosystem.** `lightr build` (Dockerfile-compatible, every step
content-keyed → Bazel-class incrementality on a plain Dockerfile),
`compose up/down` (lazy: listeners bound, services start on first connect,
ephemeral TTL supervisor), `docker` CLI compat (build/run/pull/images/ps/
compose; unsupported → exit 2, never silent). A22–A26.

**R4 — beyond + polish.** `run --deep-memo` (opt-in; honest probe + whole-run
fallback, no faked sub-memoization), `lightr schema` (versioned JSON Schema
per verb, machine-checked against real output), bench B9–B11 (oci-import,
build-cached **24.8 ms << build-cold 108.6 ms** — incrementality measured,
compose-up). `docs/spec/parity-audit.md` (every feature-tree F-id mapped to
status + evidence) + `json-schemas.md`. A27–A30.

## [Unreleased] — R0 "the warp core" (2026-06-11, overnight wave)

First working product: a 1.9 MB daemonless binary.

- `lightr snapshot/hydrate/status/run` — content-addressed workspace store
  (BLAKE3 file-level CAS, CoW clonefile ladder), git-style stat-index,
  memoized execution (exit-0-only, 5 MiB caps), `--json` + `--explain` on
  every verb, `hydrate --verify` paranoid path.
- `lightr bench [--check|--vs-docker|--json]` — the indicator table,
  measured on the user's machine; CI budget gate (all green on the Intel
  dev box; see spikes/RESULTS.md).
- Acceptance suite A1–A8 green end-to-end against the real binary
  (roundtrip, memo, fail-not-memoized, no-daemon, status, offline,
  integrity fail-closed a/b, agent JSON surface).
- Spec stack: whitepaper v2 (working backwards), feature tree F-001…F-605,
  ADRs 0001–0016, build-spec v2, decisions log (owner mandate + lead
  amendments), spike S4 results.
