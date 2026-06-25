# lightr vs Docker / Podman — adversarial benchmark

**Honest, reproducible, head-to-head on identical hardware.** Run via
`.github/workflows/benchmark.yml` (`workflow_dispatch`) on a GitHub-hosted
`ubuntu-latest` runner — public, documented hardware that anyone can re-run.

## Method
- All runtimes on the **same runner, same job, back-to-back**, n=100 iterations
  for cold-start (variance reported, not just an average).
- **Isolation level — now SAME-ISOLATION (the #82 honest re-run).** Every
  cold-start variant isolates a **full namespace set including the network
  namespace**:
  - **lightr** `--engine ns --net=none` → `CLONE_NEWUSER|NEWNS|NEWPID|NEWNET`
    + `pivot_root` into a CAS-materialized rootfs, **rootless**, loopback up.
  - **podman** `--network=none` → **rootless**, same namespace set. This is the
    **fairest apples-to-apples** baseline: same privilege class (rootless) and
    same isolation (full ns incl. network).
  - **docker** `--network=none` → **rootful** daemon. Reported as a *reference*,
    explicitly **not** the same privilege class.
- The earlier audit worry — "part of lightr's speed is skipping the network
  namespace" — is now **measured and refuted**: adding `CLONE_NEWNET` + loopback
  cost only **~1–2 ms** (host-net ns was ~29 ms; `--net=none` is ~30.8 ms). The
  net-ns is cheap in the rootless path; it was never the source of the gap.
- Competitors run the equivalent `alpine` workload; lightr materializes the same
  alpine rootfs from its content-addressed store (the ~31 ms **includes** that
  CoW hydrate).

## Results (ubuntu-latest, n=100 cold-start; representative run 2026-06-25)

**Cold-start, SAME-ISOLATION (full ns incl. network), per-iteration variance:**

| Variant | Privilege | mean | p50 | p95 | sd | min / max |
|---|---|---|---|---|---|---|
| **lightr ns `--net=none`** | rootless | **30.8 ms** | 30.4 | 31.2 | 2.4 | 29.7 / 51.2 |
| podman `--network=none` | rootless | 124.9 ms | 125.3 | 137.4 | 7.3 | 110.1 / 150.5 |
| docker `--network=none` | rootful (ref) | 143.5 ms | 141.7 | 156.0 | 6.7 | 132.7 / 160.4 |
| lightr native (no isolation) | — | 10.2 ms | 10.1 | 10.7 | 0.3 | 9.7 / 10.9 |

**Other dimensions:**

| Dimension | **lightr** | Docker |
|---|---|---|
| Idle / daemon RAM | **0 MB** | 127 MB |
| Re-run real build (20k-fn C compile, memoized; n=10) | **10.3 ms** | 20,633 ms |

**Headline (honest, same-isolation):** at the **same isolation (full namespaces
incl. network) and the same privilege class (rootless)**, lightr cold-starts in
**~30.8 ms vs rootless podman ~124.9 ms — ~4.05× faster** (p50 30.4 vs 125.3,
~4.12×), with a far tighter distribution (sd 2.4 vs 7.3). Against rootful Docker
`--network=none` it is **~4.66×**. The audit's "lighter-isolation" caveat is
resolved: the network namespace adds only ~1–2 ms, so the gap is real runtime
overhead, not skipped work. The **unambiguous structural wins** stand:
**daemonless (0 MB idle vs 127 MB)** and **memoized re-builds ~2,000×** (a lightr
*feature* Docker lacks — `10.3 ms` vs `20,633 ms`).

## Honest caveats (read these)
1. **Microbenchmark of runtime overhead.** Cold-start uses `true`; a heavy app's
   own startup is identical across runtimes, so the *percentage* win shrinks on
   heavy apps — but the runtime overhead difference is real and is what's measured.
2. **Scope:** n=100 cold-start, one runner, one day. Distribution is tight and
   consistent; not yet hundreds-of-runs across multiple machines/kernels.
3. **`--network=none` is the honest same-isolation knob, not the everyday one.**
   Real workloads usually need a routable network (veth/bridge/CNI), whose setup
   cost is comparable across runtimes and is *not* measured here. This isolates
   the runtime-overhead variable; it does not claim lightr provisions networking
   faster (it provisions *none*, like the competitors here).
4. **lightr-ns is rootless** — same as the rootless podman baseline. It is fast
   and correct in this benchmark but rootless user namespaces are **not** a
   hostile-tenant boundary; hardware isolation (`fc`/VM) is the answer there.
5. **Runner hardening:** ubuntu-24.04 GH runners restrict unprivileged user
   namespaces (AppArmor); we set `kernel.apparmor_restrict_unprivileged_userns=0`
   (the default on most Linux hosts) so **both** lightr's rootless path and
   rootless podman run. Many enterprise hosts keep it ON → both fail there.
6. **Not yet measured:** lazy image pull (HelloBench, lightr's CAS turf) and
   Phoronix real-app throughput. Steady-state app throughput is expected ~equal
   (same app); lightr's edge is overhead, footprint, and memoization.

## Reproduce
`gh workflow run benchmark.yml --ref main -f iterations=100` → see the run's job
summary, or download the `bench-results` artifact. SOTA references: HelloBench /
Slacker (FAST'16) for lazy startup; the public 6-dimension runtime comparison for
the cold-start / footprint methodology.
