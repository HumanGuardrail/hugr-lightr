# 30 ms cold starts and ~2,000× memoized re-runs: benchmarking a container runtime, honestly

*July 2026 · [lightr](https://github.com/HumanGuardrail/hugr-lightr) — a daemonless, CAS-native container runtime in Rust*

I'm building a container runtime on one thesis: **don't run containers faster — make most of the work other runtimes do cease to exist.** No daemon (nothing runs when nothing runs). No images (content-addressed files, hydrated lazily, copy-on-write). And memoization first: if the action cache has seen this exact work before, the answer returns without the work happening.

The numbers that came out of that thesis look, frankly, suspicious:

| | lightr | baseline | |
|---|---|---|---|
| Cold start, full rootless isolation | **30.8 ms** | 124.9 ms (rootless podman) | 4.05× |
| Re-run of a 20k-function C build | **10.3 ms** | 20,633 ms (Docker) | ~2,000× |
| Idle / daemon RAM | **0 MB** | 127 MB (dockerd) | ∞ |
| CRI server resident | **7.1 MB** | 65.9 MB (containerd) | 9.3× |

Numbers like these are exactly the kind that benchmark posts inflate. So this post is less about the numbers and more about the harness that keeps them honest — including the two times honesty made our numbers *worse*, and the real bugs the harness caught in our own code.

All figures are from CI-signed runs on public GitHub-hosted `ubuntu-latest` runners (Linux runtime numbers, n=100, [ledger](../benchmarks/RESULTS.md)) or a named Intel Mac (macOS app-level numbers, [ledger](../spec/benchmark-results.md)), each with a reproduction path. House rule: a number may be claimed only with its run context attached; anything unmeasured is a target and is labeled as one.

## Why it's fast: the work isn't optimized, it's absent

**Cold start.** A cold `lightr run` under the `ns` engine does: clone into a full namespace set (`CLONE_NEWUSER|NEWNS|NEWPID|NEWNET`, rootless), hydrate an alpine rootfs from the content-addressed store via copy-on-write, `pivot_root`, bring up loopback, exec. That's the whole list. There is no daemon to round-trip, no graph driver, no image layers to mount, no shim to spawn. 30.8 ms *includes* materializing the rootfs.

**Re-runs.** Runs are keyed by the content hash of their inputs (rootfs, command, mounts, env). A hit in the action cache returns recorded outputs and exit code without provisioning anything — the 10.3 ms is the lookup, not a faster compile. Docker has no equivalent, which is precisely the point: the 2,000× is a *feature comparison*, not a fair race, and the ledger says so in those words.

**Footprint.** The same shape shows up at the Kubernetes layer. Our CRI implementation is a stateless listener over kernel + disk — `kill -9` it mid-operation and nothing is lost, because there was never an in-memory mirror to reconcile. 7.1 MB resident, no per-container shim; containerd carries a ~66 MB always-on daemon plus ~15 MB of shim per running container.

## The harness rules that keep us honest

**Same isolation, same privilege, or it doesn't count.** The headline comparison is rootless lightr vs *rootless podman*, both with the full namespace set including network. Rootful Docker is reported as a reference and labeled "not the same privilege class."

**n=100 with variance, not a lucky average.** Every cold-start row publishes mean, p50, p95, sd, min/max. lightr's sd is 2.4 ms; if the distribution were wide, you'd see it.

**Caveats are first-class output.** The ledger's caveat section states plainly: cold-start is a microbenchmark of *runtime overhead* (a heavy app's own startup is identical everywhere, so percentage wins shrink); `--network=none` isolates the runtime variable and says nothing about network provisioning; rootless user namespaces are not a hostile-tenant boundary; ubuntu-24.04 runners need an AppArmor sysctl for *any* rootless runtime to work, ours included.

**Gates, not press releases.** The CAS dedup claims run as CI jobs that fail on a missed bar: re-pulling an image already in the store writes exactly **0 bytes** (gated `== 0`), and four alpine-family images dedup at **3.97×** (gated `> 1`).

## Twice, honesty made the number worse. We published the worse number.

**The network-namespace audit (#82).** An early reviewer worry: "part of lightr's speed is skipping the network namespace." Instead of arguing, we measured it — adding `CLONE_NEWNET` + loopback cost ~1–2 ms. The worry was refuted, but the entire benchmark was re-run at full isolation anyway, and the headline moved from ~29 ms to 30.8 ms. The gap was real overhead, not skipped work — now that's *demonstrated*, not asserted.

**The milestone alignment (WP-#102).** Our CRI cold-start first measured 69 ms. Too good, and it was: lightr persisted `CONTAINER_RUNNING` right after spawning its namespace shim, *before* the in-namespace `pivot_root`/`exec` completed — an undercount versus containerd's "task up" milestone. We added a CLOEXEC exec-success pipe so `Running` is persisted only after the workload actually `execv`s. The honest number is **91 ms** — 22 ms slower than our own earlier claim, still 1.31× under containerd at the same milestone, and the ledger documents the correction rather than hiding it.

There's a third entry in the same spirit: for the same four images, containerd's *on-disk* content store is **smaller** than ours (8.7 MB compressed blobs vs our 17.3 MB decompressed, per-file, ready-to-run objects). Different trade-off — we report it as INFO in our own benchmark, against ourselves.

## What the harness caught in our own code

Adversarial CI turns out to be a bug-finder, not just a truth-keeper. Running the suite on real Linux exposed claims that were *wired but non-functional* in the rootless engine: a cgroup limit that was applied after `pivot_root` and therefore never took effect, and a `--pids-limit` that parsed cleanly and did nothing. Each one became a failing validation gate before it became a fix. A benchmark you can't lie to is also a benchmark your own code can't lie to.

## Why this is an LLM-infrastructure problem

I build lightr because of agents. Agent fleets re-execute the world constantly — install, build, test, retry — and mostly re-execute *the same* world. Two consequences:

- **Cold start is the serving problem.** Sandbox-per-task means the runtime's overhead is paid thousands of times a day. The serverless platforms that cracked this (Lambda's on-demand container loading, Modal's content-addressed filesystem) did it with exactly this shape: content-addressed storage, lazy hydration, aggressive dedup. Those are cited here as precedent, not as claims about lightr at their scale.
- **Memoization is the agent economy.** An agent loop that re-runs a test suite it already ran should pay 10 ms, not 20 seconds. Keying runs by input content makes that automatic — and a fleet sharing one CAS pays for each unit of work once, globally.

That's the layer lightr occupies: the runtime under agent sandboxes, where redundant work goes to not happen.

## Reproduce it

```
gh workflow run benchmark.yml --ref main -f iterations=100
```

on [the repo](https://github.com/HumanGuardrail/hugr-lightr) — results land in the job summary and the `bench-results` artifact. The Linux ledger is [docs/benchmarks/RESULTS.md](../benchmarks/RESULTS.md); macOS app-level numbers (install footprint 452×, 1 GB hydrate 119×, measured on a named Intel Mac) are in [docs/spec/benchmark-results.md](../spec/benchmark-results.md). If you find a hole in the method, open an issue — the harness exists to be attacked.
