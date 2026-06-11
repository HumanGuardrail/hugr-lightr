# Changelog — hugr-lightr

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
