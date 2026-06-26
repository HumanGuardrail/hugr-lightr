# lightr vs Docker / Podman — adversarial benchmark

**Honest, reproducible, head-to-head on identical hardware.** Run via
`.github/workflows/benchmark.yml` (`workflow_dispatch`) on a GitHub-hosted
`ubuntu-latest` runner — public, documented hardware that anyone can re-run.

> **Scope:** this file is the **Linux runtime** benchmark (`ns` engine cold-start,
> footprint, memoization). The **macOS app-level** numbers (vz cold-run, install,
> idle) live in [`../spec/benchmark-results.md`](../spec/benchmark-results.md);
> the clonefile micro-spike in `../../spikes/RESULTS.md`. Different hardware +
> different layers — they do not overlap.

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

## CAS dedup KPIs (CI-signed, ubuntu-latest, 2026-06-25)

The content-addressed store is the moat. Two KPI probes (`ci/linux-kpis/`) run in
the `cas-kpis` job and **gate for real** (the job fails on a missed bar). Metric =
bytes in the CAS object plane (`du -sb $LIGHTR_HOME/store/objects`); there is no
network bytes-in counter, so the signed claim is **bytes written to the CAS**, not
network bytes.

**KPI 1 — pull dedup:**

| Step | New CAS bytes | Bar | Result |
|---|---|---|---|
| Cold pull `alpine:latest` (B_A1) | 12,280,424 | > 0 | PASS |
| Re-pull same image, new ref (B_A2) | **0** | == 0 | **PASS** |
| Import B = `FROM alpine +1 layer` (B_B) | 8,738,785 | 0 < B_B < B_A1 | PASS |

Re-pulling content already in the CAS writes **zero** new bytes. (B_B is not tiny:
a docker-save tar carries the base as a compressed blob that lightr retains as a
new object, but the per-file snapshot content dedups — so B_B < a fresh cold pull.)

**KPI 2 — disk dedup ratio (N=4 images sharing an alpine base):**

| Quantity | Bytes | Bar | Result |
|---|---|---|---|
| Σ standalone (no sharing) | 68,539,410 | — | — |
| S_lightr (combined store) | 17,257,368 | — | — |
| **dedup_ratio = Σ / S_lightr** | **3.97×** | > 1 | **PASS** |
| S_containerd (content store) | 8,716,874 | S_lightr ≤ S_containerd | INFO (not PASS) |

**Honest on-disk note:** lightr's combined store (17.3 MB) is **larger** than
containerd's content store (8.7 MB) for the same 4 images, because lightr stores
**decompressed, per-file** CAS objects (ready to run — no unpack step) while
containerd holds **compressed** layer blobs that must still be unpacked to a
snapshot before running. Different trade-off; reported as INFO, never gated. The
hard, signed claim is the dedup ratio (3.97×) and the 0-byte re-pull.

## KPI 3 — CRI cold-start + footprint vs containerd (CI-signed, ubuntu-latest, 2026-06-26)

Both runtimes are driven through the SAME path — `crictl` → gRPC CRI →
RunPodSandbox (shared CNI bridge) → create+start → poll `CONTAINER_RUNNING` — on
the same `alpine:latest`, same runner, in the `cri-kpi3` job (`KPI3_AB=SIGNED`).
lightr runs the REAL alpine rootfs under the `ns` engine joined into the pod netns
(CI-proven: PID 1 + `/etc/alpine-release` + the container's net-ns inode == the
pod's pinned netns); containerd runs the same image under runc.

| Metric | lightr | containerd | Ratio |
|---|---|---|---|
| Cold-start mean (n=5, ms) | **91.0** (90–92) | 119.2 (110–136) | 1.31× faster |
| Resident RSS (KB) | **7,064** (cri-serve) | 65,860 (daemon) | **9.32× smaller** |
| Per-container shim RSS (KB) | **0** (no shim) | 14,960 (one `containerd-shim-runc-v2` per container) | — |

*(Measured execv-aligned post-WP-#102, run `8b4fec4`, ubuntu-latest; both arms report Running at the same lifecycle point — the workload executing — via crictl/gRPC.)*

**The robust, signed headline is FOOTPRINT: lightr's resident process is ~9×
smaller** (7 MB vs 62 MB) AND there is **no per-container shim** — daemonless +
shimless. That is the structural result: containerd carries a ~62 MB always-on
daemon plus a ~15 MB shim per running container; lightr carries a ~7 MB server and
the container is its own supervised process tree.

**Cold-start is now milestone-aligned (WP-#102).** An earlier measurement read
69 ms because lightr used to persist `CONTAINER_RUNNING` right after spawning the
`__ns-run` shim (before the in-namespace `pivot_root`/`exec` finished) — an
undercount vs containerd's "task up" milestone. WP-#102 (CLOEXEC exec-success
pipe) made lightr persist `Running` only AFTER the workload `execv`s; the 91 ms
above is that honest, execv-aligned figure (it rose ~22 ms, as expected, and is
still 1.31× under containerd at the SAME milestone). **The headline remains the
FOOTPRINT: ≈9× smaller resident RSS and zero per-container shim — daemonless +
shimless.**

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
