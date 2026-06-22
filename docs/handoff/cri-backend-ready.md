# Handoff — CRI backend ready: ask #1 delivered + conformance-proven

**From:** Lightr (real backend) TL · **To:** lightr-cri (shell) TL
**Re:** onboarding ask **A** — *implement `CriBackend` over Lightr's CAS crates*
(`lightr-cri/docs/handoff/lightr-tl-onboarding.md` §5.A, §8)
**Repo:** hugr-lightr · **Relay:** owner

The real backend exists, is wired across every plane, and passes the shared
conformance vectors. You can swap your fake. This note states **exactly** what
is proven, what is deferred (and why), and how to consume it — no overclaim.

---

## 1. What's delivered

A new crate **`crates/lightr-cri-backend`** in the hugr-lightr workspace: the
real `CriBackend` implementation, **`LightrBackend`**, over the Lightr/CoreLink
CAS crates (`lightr-core`, `lightr-store`, `lightr-oci`, `lightr-engine`,
`lightr-run`).

- **Transcribed seam, not a dependency.** The `CriBackend` trait and every
  vocabulary type are transcribed wire-for-wire from your frozen contract
  (`lightr-cri/.../src/{lib.rs,vocab.rs}` + `docs/contract/seam-contract-v1.1.md`).
  There is **no git/path dep on lightr-cri** — the house seam pattern
  (`crates/lightr-cri-backend/src/lib.rs:1–22`). Drift is caught by the shared
  vectors, never by a crate import.
- **Dependency firewall held.** Zero `tonic`/`prost`/gRPC anywhere in the
  hugr-lightr workspace — verified: the only match in any `Cargo.toml` is the
  comment forbidding it (`crates/lightr-cri-backend/Cargo.toml:14`). gRPC stays
  in front of the seam, in your shell.
- **All planes wired** (each delegates from the trait impl to a per-concern
  module, `lib.rs:210–305`):
  - **sandbox / pod** — state machine + persistent records; `cfg(linux)`
    netns + CNI executor (`sandbox.rs`, `sandbox_net.rs`).
  - **container lifecycle** — create / start / stop (SIGTERM→SIGKILL grace) /
    remove / status / list, crash-only state re-derived from disk
    (`container.rs`, `container_query.rs`).
  - **exec_sync** (`exec.rs`).
  - **images** — pull / status / list / remove / fs_info, honoring the lazy-pull
    law (`pull_image` does not move file bytes) (`images.rs`).
  - **stats** — `container_stats` / `list_container_stats` (`stats.rs`).
  - **streaming** — `open_exec` / `open_attach` over a live stdio side-table,
    real waiters, unix-only / fail-closed elsewhere (`stream.rs`, `stream_io.rs`).
  - **network_ready** — probe-truthful override of the trait default
    (`lib.rs:284`).

---

## 2. Proof

Source of truth: `crates/lightr-cri-backend/tests/vectors.rs` (the shared
conformance vectors, transcribed from `lightr-cri` @ seam-contract-v1.1; run
directly against the **real** `LightrBackend`, no scaffold).

- **25 / 29 shared conformance vectors PASS** against `LightrBackend`
  (`vectors.rs:135` locks `run_pass == 25`). Zero divergences, zero source
  massaging — the vectors drive the real backend through the trait object.
- **4 deferred**, gated out and **logged, never silently skipped**
  (`vectors.rs:101–123,136`): the `DeferNet` class — live-OCI **image-content**
  pull. The fake fabricates the record in memory; the real backend performs a
  live network pull, and **there is no network on the macOS gate**.

**macOS-gate caveat (read this).** The `cfg(linux)` netns/CNI **runtime** is
NOT exercised on our macOS CI. What *is* exercised on macOS: the sandbox
**state machine** and the **pure helpers**. Probe-truthful by construction — on
macOS there is no CNI, so `pod_ip = None` and no vector asserts a CNI-assigned
IP (`vectors.rs:17–22`). The Linux netns/CNI runtime lane is **dormant /
queued**, to be tracked in `docs/runbooks/linux-validation.md` (to be authored
as part of the Linux-validation work — see §5). Nothing about CNI on a real
netns is claimed as proven yet.

---

## 3. How you consume it — the swap

This is the **contract-swap**, not a copy:

1. In your shell's backend factory, construct **`LightrBackend`** instead of the
   fake — `LightrBackend::new(home) -> impl CriBackend` (`lib.rs:171`), bound as
   `Box<dyn CriBackend>` (object-safe — `lib.rs:375`).
2. Nothing else in the shell, streaming, or vector harness changes: the
   wire-level seam is guaranteed by the shared **`lightr-cri-vectors`** — your
   backend passes the same vectors the fake passes (your §6.1 bar).

**Trait surface you bind to** (`CriBackend`, `lib.rs:65–132`):

- *sandbox:* `run_sandbox`, `stop_sandbox`, `remove_sandbox`, `sandbox_status`,
  `list_sandboxes`
- *container:* `create_container`, `start_container`, `stop_container`,
  `remove_container`, `container_status`, `list_containers`,
  `container_stats`, `list_container_stats`
- *exec:* `exec_sync`
- *images:* `pull_image`, `image_status`, `list_images`, `remove_image`,
  `image_fs_info`, `pull_image_with_auth` (additive, v1.1)
- *streaming / net:* `open_exec`, `open_attach`, `network_ready`

`StreamSession` carries the stdio/pty + a real waiter; `LightrBackend` hands
back live fds from the side-table populated at `start_container`. Attach after a
restart is unavailable and surfaces **honestly** (`NotFound`), since the fds are
process-local (`lib.rs:153–161`).

---

## 4. ADR-0017 status (`lightr-cri/docs/handoff/ADR-0017-cri-ready-not-cri-now.md`)

- **Decision 1 (run-dir seam):** holds as law. Per-instance disk state is the
  source of truth; the backend re-derives its view on construction (crash-only,
  `lib.rs:182–186`).
- **Decision 2 (supervisor survives the parent):** holds as law — the lifecycle
  and `ctl.sock` control plane are inherited from `lightr-run`; the backend
  consumes them, the listener owns nothing.
- **Decision 3 (`group_id` on `SpecOnDisk`):** turned out **UNNECESSARY**.
  `LightrBackend` keeps its own sandbox records under
  `<home>/cri/sandboxes/` (`lib.rs:174–207`), so the pod/group concept lives in
  the backend's own state — **`lightr-run` needed no `SpecOnDisk` refactor and
  no new field.** Confirmed: `group_id` is absent from the entire crate.
- **Decision 5 (dependency firewall):** held — see §1.

(Decision 4, scoped no-daemon, is your shell's `cri serve` posture; unchanged.)

---

## 5. What's still on each side

**Ours (Lightr / backend):**
- Linux runtime validation of netns/CNI on a real netns (needs a Linux box;
  the lane is dormant — to be driven from `docs/runbooks/linux-validation.md`).
- The **4 CAS / runtime KPIs** from your `bench-cas-kpis-request.md` (pull
  dedup, disk-for-N-images, real-container A/B vs containerd, AppArmor) — each
  becomes a *signed* number once exercised on Linux. Nothing claimed until a run
  signs it (tense discipline).

**Yours (lightr-cri / shell):**
- Wire the backend factory to `LightrBackend` (the swap, §3).
- Run your **critest GREENLIST** against the swapped backend — the conformance
  parity bar (your §6.1). The shared vectors already prove the wire-level seam;
  critest proves it end-to-end through the gRPC shell.

---

## 6. One line

Ask #1 is delivered: the real `CriBackend` over CAS, 25/29 shared vectors green
with zero divergence, firewall and ADR-0017 laws held, `group_id` proven
unnecessary. Swap the fake, run your greenlist. Linux runtime + the 4 KPIs are
ours to sign next.
