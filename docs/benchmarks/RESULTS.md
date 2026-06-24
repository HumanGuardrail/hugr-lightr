# lightr vs Docker / Podman / containerd — adversarial benchmark

**Honest, reproducible, head-to-head on identical hardware.** Run via
`.github/workflows/benchmark.yml` (`workflow_dispatch`) on a GitHub-hosted
`ubuntu-latest` runner — public, documented hardware that anyone can re-run.

## Method
- All four runtimes on the **same runner, same job, back-to-back**, N iterations,
  wall-clock averaged.
- **Isolation level (NOT identical — read carefully):** lightr `--engine ns
  --rootfs <ref>` = user+mount+pid namespaces + `pivot_root` into a CAS-materialized
  rootfs, **rootless** (unprivileged user namespace). IMPORTANT: it does **NOT**
  create a network namespace (`ns.rs` unshares `CLONE_NEWUSER|NEWNS|NEWPID`, no
  `CLONE_NEWNET` — the container shares the host network), whereas
  Docker/Podman/containerd DO isolate the network (veth/bridge). So this is **not
  full isolation parity**: part of lightr's cold-start advantage is skipping
  network-namespace + veth setup. A same-isolation comparison would run the
  competitors with `--network=host` (and/or add `CLONE_NEWNET` to lightr-ns).
- Competitors run the equivalent `alpine` workload; lightr materializes the same
  alpine rootfs from its content-addressed store (the 29 ms figure **includes**
  that CoW hydrate).

## Results (ubuntu-latest, 20 iterations; representative run 2026-06-23)

| Dimension | **lightr** | Docker | Podman | containerd |
|---|---|---|---|---|
| Cold-start (lightr = **lighter** isolation, see note) | **~29 ms** | ~213 ms | ~253 ms | ~110 ms |
| Idle / daemon RAM | **0 MB** | ~155 MB | 0 MB | (part of Docker) |
| Re-run real build (20k-fn C compile, memoized) | **~11 ms** | ~20,380 ms | n/a | n/a |
| Cold-start, native (no isolation — trusted/CI) | ~10 ms | — | — | — |

**Headline (honest):** lightr cold-starts far faster than the daemon-based
runtimes — ~29 ms vs containerd ~110 ms (**~3.8×**, the fastest competitor and
the fairest single baseline), Docker ~213 ms, Podman ~253 ms. **This is NOT yet a
same-isolation number:** lightr-ns omits the network namespace (see note) and ran
rootless while the others ran rootful, so part of the gap is lighter isolation +
no daemon, not pure runtime speed. A clean same-isolation comparison (competitors
`--network=host`, equal privilege, `+CLONE_NEWNET` in lightr-ns) is **pending**.
The **unambiguous** wins: **daemonless (0 MB idle vs ~155 MB)** and **memoized
re-builds ~1,900×** — the latter being a lightr *feature* Docker lacks, not a
runtime-vs-runtime measurement.

## Honest caveats (read these)
1. **Microbenchmark of runtime overhead.** Cold-start uses `true`; a heavy app's
   own startup is identical across runtimes, so the *percentage* win shrinks on
   heavy apps — but the runtime overhead difference is real and is what's measured.
2. **Scope:** n=20, one runner, one day. Trend is strong and consistent across
   repeated runs; not yet hundreds-of-runs across multiple machines.
3. **Linux isolation is new.** lightr's `ns` engine had a real uid/gid-map bug
   (fixed here — read uid/gid before `unshare`); it is fast and correct in this
   benchmark but not yet battle-tested in production at scale.
4. **Runner hardening:** ubuntu-24.04 GH runners restrict unprivileged user
   namespaces (AppArmor); we set `kernel.apparmor_restrict_unprivileged_userns=0`
   (the default on most Linux hosts) so lightr's rootless path runs.
5. **Not yet measured:** lazy image pull (HelloBench, lightr's CAS turf) and
   Phoronix real-app throughput. Steady-state app throughput is expected ~equal
   (same app); lightr's edge is overhead, footprint, and memoization.

## Reproduce
`gh workflow run benchmark.yml --ref <branch>` → see the run's job summary, or
download the `bench-results` artifact. SOTA references: HelloBench / Slacker
(FAST'16) for lazy startup; the public 6-dimension runtime comparison for the
cold-start / footprint methodology.
