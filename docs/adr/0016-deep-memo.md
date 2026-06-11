# ADR-0016 — Deep-memo: the filesystem view is the tracer

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review)
- **Date:** 2026-06-11

One line: beyond whole-`run` memoization, Lightr memoizes **inside** the
process tree — every build step keyed by what it actually read — with the
dependency oracle coming from our own filesystem view (we see every open),
making sccache-grade incrementality automatic for any tool; opt-in
(`--deep-memo`, the nitro switch) until maturity flips the default.

## Context
BuildXL proved generic process-tree caching (Detours; internal, unadopted);
tup proved FUSE-based dependency tracking; sccache/ccache prove per-tool
demand. The SOTA-review synthesis: owning the view layer (ADR-0013) gives
read-set observation for free on Linux (mount-ns attribution per run).
macOS NFS path lacks caller PID attribution — spawn-shim interposition is
the macOS route (fragile with static binaries/SIP: hence opt-in nitro).

## Decision
1. Memo key per child process: (argv, env-subset, cwd-rel, read-set file
   digests, platform). Read-set from: Linux = our FS view + per-run mount
   ns; macOS = spawn-shim (`DYLD`-interpose where allowed) — degraded
   honestly to whole-run memo when interposition fails.
2. Writes captured to the upper layer become the step's output objects;
   replay materializes outputs + stdout/stderr.
3. Non-deterministic step detection (clock/random/net reads observed) ⇒
   step marked non-memoizable, loudly, in `--explain`.
4. `lightr build` (Dockerfile-compat) is the first consumer (each
   instruction = a deep-memo'd run); generic `--deep-memo` on any `run`
   follows.
5. With Stage 2 (ADR-0011), step records sync via the AC — the team
   shares incrementality. Tense law applies.

## Consequences
Bazel-class incrementality on dumb Makefiles, zero configuration. The
fragile part (interception) is quarantined behind an explicit flag with
graceful degradation; the robust part rides the view layer we own anyway.
