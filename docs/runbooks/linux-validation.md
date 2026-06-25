# Runbook — Linux validation lane

**Status: DORMANT.** Everything here is wired but inert until a Linux
self-hosted runner is registered. The lane lives in
`.github/workflows/linux-validation.yml` (trigger: `workflow_dispatch` ONLY) and
the KPI probes in `ci/linux-kpis/`. It never runs on push/PR and cannot break
the green macOS gate (`.github/workflows/ci.yml`) until a runner exists.

## Why this lane exists

The CI gate runs on a self-hosted **macOS Intel** runner. Two things are
Linux-gated and unvalidated there:

1. **CRI sandbox netns-pin + CNI ADD/DEL runtime.** The state machine and the
   pure CNI helpers are proven on macOS, but the actual netns join
   (`setns(CLONE_NEWNET)` via `pre_exec`) and the CNI executor are `cfg(linux)`
   — see `crates/lightr-cri-backend/src/{sandbox.rs,sandbox_net.rs}`. The
   Linux-only helper tests are behind `#[cfg(all(test, target_os = "linux"))]`
   in `crates/lightr-cri-backend/src/sandbox_net_tests.rs`.
2. **The 4 CAS / runtime KPIs** handed off by lightr-cri — see
   `../lightr-cri/docs/handoff/bench-cas-kpis-request.md` (sibling repo,
   read-only). They are properties of the **real CAS backend**, not measurable
   against the fake backend without measuring the fake.

## 1. Register a Linux self-hosted runner (the label)

The workflow jobs target `runs-on: [self-hosted, linux]`. The `linux` label
does not exist yet, so jobs queue without scheduling until a runner provides it.

One-time setup (owner):

1. Repo → **Settings → Actions → Runners → New self-hosted runner → Linux**.
2. Follow the registration steps. The runner self-labels `self-hosted`, `Linux`,
   `X64`. GitHub's `Linux` label matches the workflow's `linux` (label match is
   case-insensitive); no extra label is needed.
3. Provision the runner box (mirrors the macOS box, swapping the triple):
   - **Pinned toolchain** at
     `$HOME/.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu` (the
     `linux-gnu` triple — the macOS gate uses `apple-darwin`). Install once:
     `rustup toolchain install 1.96.0` then `rustup component add clippy
     rustfmt`. CI never calls `rustup` (proxy-immune pinned-bin PATH, same as
     `ci.yml`).
   - For the netns/CNI runtime + KPIs: `iproute2` (`ip netns`), CNI plugins
     (`/opt/cni/bin`), `containerd` + `crictl` (for the A/B benches), AppArmor
     in-kernel (`aa-status` should report enabled), and `curl`.
   - The persistent `target/` keeps runs fast; no cache action (self-hosted
     keeps state — caching can fight the persistent target).

The moment the runner is online with the `Linux` label, dispatching the workflow
schedules both jobs.

## 2. Trigger the workflow

```sh
# from the repo root, with the gh CLI authenticated:
gh workflow run linux-validation.yml
# or with the optional reason input:
gh workflow run linux-validation.yml -f reason="R2 netns/CNI validation"

# watch it:
gh run watch
```

Manual dispatch is always allowed; before a runner exists the jobs simply queue.

## 3. What each job proves

| Job | runs-on | Proves |
| --- | --- | --- |
| `cri-linux` | `[self-hosted, linux]` | The `cfg(linux)` netns-pin + CNI ADD/DEL runtime actually executes on a real kernel: builds `lightr-cri-backend`, runs the `#[cfg(all(test, target_os = "linux"))]` CNI helper tests + the backend vectors that are macOS-deferred (no `setns` there) and un-deferred on Linux. |
| `cas-kpis` | `[self-hosted, linux]` (`needs: cri-linux`) | The 4 deferred KPIs from the lightr-cri handoff, measured against the **real CAS backend** (see §4). |

## 4. The 4 KPIs — measurement method + pass bar

All four are specced in `../lightr-cri/docs/handoff/bench-cas-kpis-request.md`
(read-only sibling). The lightr-cri bench harness
(`../lightr-cri/ci/bench.sh`, schema `lightr-cri.bench/v1`) reserves the slots in
`out_of_scope.deferred_kpis`; this lane promotes them to `in_scope` once the
backend capability lands. **Tense discipline:** every probe is fail-closed — it
refuses to emit a number it did not actually measure (set
`KPI_BACKEND_READY=1` only when the capability is real).

| # | KPI | Probe script | Measurement | Pass bar | Status |
| - | --- | --- | --- | --- | --- |
| 1 | Pull dedup (0-byte re-pull) | `ci/linux-kpis/kpi1-pull-dedup.sh` | pull A cold, re-pull A, import B = `FROM A +1 layer`; CAS object-plane bytes delta (no network bytes-in counter exists — measures bytes WRITTEN to CAS) | re-pull == 0 new bytes; 0 < B_B < B_A1 | ✅ **VALIDATED 2026-06-25** (`cas-kpis` job): B_A2=**0**, ratio in `docs/benchmarks/RESULTS.md` |
| 2 | Disk dedup ratio (N similar images) | `ci/linux-kpis/kpi2-disk-dedup.sh` | import N overlapping images; `du -sb` CAS objects vs an isolated containerd content store | dedup ratio > 1 (HARD); CAS on-disk vs containerd = INFO | ✅ **VALIDATED 2026-06-25**: **ratio 3.97×**. NOTE: S_lightr (decompressed, run-ready) > S_containerd (compressed blobs) — honest INFO, not a fail |
| 3 | Real-container cold-start / footprint A/B | `ci/linux-kpis/kpi3-cold-start-ab.sh` | drive a real `crictl run` (nginx/agnhost) + curl via the lightr-cri harness cold-start/RSS probes | time-to-serving + RSS <= containerd, same image + host | ⛔ **BLOCKED** on the lightr-cri shell swap (needs crictl→CRI server; other TL's repo) — §3 |
| 4 | AppArmor profile applied | `ci/linux-kpis/kpi4-apparmor.sh` | run critest AppArmor specs against the real backend | critest AppArmor specs GREEN; lines removable from `../lightr-cri/ci/critest-skips.txt` | ⛔ **BLOCKED** on the lightr-cri critest harness (other TL's repo) — §4 |

KPI 3 also unblocks the runtime-tier critest networking specs (port-mapping ×2,
portforward ×2) listed in `../lightr-cri/ci/critest-skips.txt` — they need a real
image serving HTTP in the pod netns.

## 5. Run it locally on a Linux box (by hand)

Put the pinned toolchain on PATH (proxy-immune, same as CI):

```sh
export PATH="$HOME/.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"
```

### netns / CNI tests (job `cri-linux`)

```sh
cargo build -p lightr-cri-backend
# runs the cfg(linux) CNI helper tests + the un-deferred-on-linux vectors:
cargo test -p lightr-cri-backend -- --nocapture
```

### KPI benches (job `cas-kpis`)

Build the real backend, then run each probe. Until the real CAS-backend
capability is wired, each script fails closed; set `KPI_BACKEND_READY=1` only
once the capability is real, and fill in the probe body documented at the top of
each script.

```sh
cargo build --release -p lightr-cli

./ci/linux-kpis/kpi1-pull-dedup.sh     # pull dedup, 0-byte re-pull
./ci/linux-kpis/kpi2-disk-dedup.sh     # disk dedup ratio, N similar images
./ci/linux-kpis/kpi3-cold-start-ab.sh  # real cold-start/RSS A/B vs containerd
./ci/linux-kpis/kpi4-apparmor.sh       # AppArmor profile applied (critest)
```

For KPI 3 the probe drives the sibling lightr-cri harness **read-only** (never
edit the sibling repo):

```sh
BACKEND=lightr bash ../lightr-cri/ci/bench.sh   # in_scope real workload, signs lightr-cri.bench/v1 JSON
```

## Guards

- **Sibling repo `lightr-cri` is read-only.** This lane cites
  `../lightr-cri/ci/bench.sh`, `../lightr-cri/ci/critest-skips.txt`, and the
  handoff doc, and invokes the harness read-only — it never edits them.
- **`.github/workflows/ci.yml` is untouched.** This lane is a separate
  `workflow_dispatch`-only file; the green macOS gate is unaffected.
- **Fail-closed / tense discipline.** No KPI emits a number it did not measure;
  Lightr remains design-phase until a real CI run signs each KPI.
