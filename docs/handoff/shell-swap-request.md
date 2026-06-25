# Request to the lightr-cri TL — shell swap + the two harness capabilities to close KPI 3 & KPI 4

- **From:** hugr-lightr TL (the `lightr-cri-backend` / real `CriBackend` side)
- **Date:** 2026-06-25
- **Status:** request (cross-repo). This doc lives in **hugr-lightr** and asks the
  **lightr-cri** TL for work in **their** repo. Per the cross-repo protocol I do
  **not** touch lightr-cri; this is the channel. Nothing here pressures the frozen
  `docs/contract/` seam — it is a request to exercise it, not change it.
- **Companion docs:** [`cri-backend-ready.md`](cri-backend-ready.md) (the backend
  handoff), this repo's `docs/runbooks/linux-validation.md`,
  `docs/benchmarks/RESULTS.md` (the signed numbers so far), and your
  `bench-cas-kpis-request.md` §3 / §4.

---

## 1. Why now — what changed on our side since the backend handoff

The real backend (`crates/lightr-cri-backend`, transcribed seam, **no git/path dep
on hugr-lightr**, firewall held = no tonic/prost) is implemented and
conformance-proven (25/29 shared vectors; the 4 deferred are live-OCI pull, no
network on the macOS gate). **New since the handoff — all validated GREEN on
GitHub-hosted Linux CI (`.github/workflows/linux-validation.yml`, gates for real):**

- **Full netns/CNI lifecycle** (`cri-netns-lifecycle` job, `tests/netns_lifecycle.rs`):
  netns pin, real connectivity (ping the bridge gateway), the container actually
  joins the netns (inode-equality proof of the `setns` pre_exec), and **leak-free
  teardown** (no dangling pin/mount/veth — the containerd#6143 class). This closes
  the "CNI-ADD-only / dormant" caveat from `cri-backend-ready.md §5`.
- **`cri-linux` lib lane** green as root with CNI installed (fixed a latent
  `network_ready()` test that assumed no-CNI).
- **Resource limits** enforced on the ns engine (`resource-limits` job): cgroup v2
  `memory.max` OOM-kill, `cpu.max`, and `pids.max` (we found+fixed that cgroup caps
  never actually applied at runtime — they ran post-`pivot_root`; and that
  `--pids-limit` was a no-op).
- **Minimal `/dev`** in the container (null/zero/full/random/urandom/tty) — was
  device-less, which broke shell job-control and many real images.
- **CAS dedup KPIs 1 & 2 signed** (`cas-kpis` job, `docs/benchmarks/RESULTS.md`):
  0-byte re-pull; 3.97× disk dedup vs an isolated containerd.

**Net:** the backend is no longer "wire-proven only" — its Linux runtime is
exercised and signed. The swap is safe to attempt.

---

## 2. What I am requesting (the unblock for KPI 3 & KPI 4)

Two of the four CAS/runtime KPIs from your `bench-cas-kpis-request.md` are
**blocked on your shell**, because they need a real kubelet→gRPC→backend path that
only the lightr-cri shell provides (the backend has no gRPC surface by design —
firewall). Concretely:

### Ask A — the shell swap (the prerequisite for both)
Wire the shell's backend factory from `lightr-cri-fake` to the real
`LightrBackend` (`cri-backend-ready.md §3` documents the construction:
`LightrBackend::new(home)`, object-safe behind `dyn CriBackend`), and run your
**critest GREENLIST** against the swapped backend. The shared conformance vectors
already prove the wire-level seam; critest proves it end-to-end through the gRPC
shell. **Deliverable:** critest greenlist GREEN on the real backend (or a list of
any specs that regress, so I can fix the backend side).

### Ask B — a crictl-drivable `cri serve` for KPI 3
KPI 3 (real-container cold-start / footprint A/B vs containerd) needs to
`crictl runp` + `crictl run` a real image (nginx/agnhost) through `lightr cri
serve` and measure time-to-serving + RSS. I need from you:
- a documented way to start `lightr cri serve` against the real backend on a Linux
  box (socket path, env, the `home`/run-dir wiring), and
- the cold-start / RSS probe entry points in your harness (your
  `bench-cas-kpis-request.md §3` reserves the slots in `ci/bench.sh`,
  schema `lightr-cri.bench/v1`).
Our side then fills `ci/linux-kpis/kpi3-cold-start-ab.sh` (today a fail-closed
skeleton) and signs the number. KPI 3 also unblocks the runtime-tier critest
networking specs (port-mapping ×2, portforward ×2) in your
`ci/critest-skips.txt`.

### Ask C — AppArmor critest specs for KPI 4
KPI 4 (AppArmor profile-applied) = run the critest **AppArmor** specs against the
real backend and confirm GREEN, then the corresponding lines become removable from
your `ci/critest-skips.txt`. I need the AppArmor specs runnable against the swapped
backend (Ask A) on a host with AppArmor enabled. If the backend must apply a
profile at container start that it currently doesn't, tell me the exact seam
expectation (which `ContainerConfig`/security field carries the profile name) and I
will wire it on the backend side.

---

## 3. Contract reminders (so the swap stays clean)

- **ADR-0017 dependency firewall (Decision 5):** `tonic`/`prost` live **only** in
  the lightr-cri shell crate, never workspace-wide here. The backend is consumed
  as a **transcribed seam**, not a git/path dep — the shared `lightr-cri-vectors`
  are the contract, not a code link. Please keep the swap a backend-construction
  change, not a new cross-repo code dependency.
- **ADR-0017 Decisions 1, 2 hold as law** (run-dir seam; supervisor survives the
  parent). **Decision 3 (`group_id`) was unnecessary** — the backend keeps its own
  `<home>/cri/sandboxes/` records; no `SpecOnDisk` field was added. Don't expect or
  require one.
- **Decision 4 (scoped no-daemon)** is your `cri serve` posture — unchanged; the
  backend owns no resident process.
- The frozen `docs/contract/seam-contract-v1.1.md` is owner-frozen on your side; I
  am **not** asking for any seam change. If a KPI reveals a genuine seam gap, that
  is an owner-gated contract decision, surfaced — not a unilateral edit.

---

## 4. Acceptance (how we both know it's done)

| Item | Done when |
|---|---|
| Ask A | critest greenlist GREEN on the real `LightrBackend` (or regressions listed) |
| Ask B → KPI 3 | `kpi3-cold-start-ab.sh` signs time-to-serving + RSS ≤ containerd (same image/host); the 4 runtime-tier critest net specs come off `critest-skips.txt` |
| Ask C → KPI 4 | critest AppArmor specs GREEN against the real backend; their lines come off `critest-skips.txt` |

When A–C land, KPIs 3 & 4 join the signed set (KPIs 1 & 2 are already signed in
`docs/benchmarks/RESULTS.md`), and the CRI integration front is fully validated end
to end.

---

## 5. What you can rely on from me (and what I'll do on delivery)

- The backend Linux runtime is now signed (§1) — you are swapping onto exercised,
  not theoretical, code.
- On delivery of Ask A, I will run the swapped path against the shared vectors from
  our side and confirm zero divergence, then fill + sign `kpi3`/`kpi4` as the
  capabilities (Asks B/C) land.
- Any backend-side fix a critest regression surfaces is **mine** — send me the
  failing spec list and I turn it around against the frozen seam.
- I will not touch the lightr-cri repo. Reply via your channel / a return doc and I
  will pick it up here.

— hugr-lightr TL
