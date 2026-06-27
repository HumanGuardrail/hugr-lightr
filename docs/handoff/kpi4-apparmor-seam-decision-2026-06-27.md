# KPI 4 (AppArmor) — frozen-seam decision request + status refresh

- **From:** hugr-lightr TL (real `LightrBackend` side)
- **To:** the **owner** (the frozen-seam decision is yours) + the **lightr-cri TL**
  (the shell-side mapping is yours, on owner approval)
- **Date:** 2026-06-27 · **Re:** your `reply-shell-swap-2026-06-25.md` Ask C
- **Status:** cross-repo request. Lives in **hugr-lightr**; I do not touch
  lightr-cri. Nothing here edits the frozen `docs/contract/` — it asks the owner
  to approve ONE additive field, then the shell mapping is the lightr-cri TL's.

---

## Where we are (so the decision has full context)

| Ask (your reply) | State now |
|---|---|
| **A — swap-ready shell** (PR #1, `run_blocking<B: CriBackend>`) | ✅ **consumed** — `lightr-cri-serve` composes the real `LightrBackend` fake-free; `cri-serve-smoke` CI proves `crictl version` → `RuntimeName: lightr`. |
| **B — KPI 3 probes** | ✅ **KPI 3 landed + SIGNED** on my side (`cri-kpi3`, execv-aligned): footprint **9.32× smaller** (7 MB vs 66 MB, shimless) + cold-start **1.31×** (91 ms vs 119 ms). Your `out_of_scope.deferred_kpis` → `in_scope` flip is unblocked; the 4 runtime-tier net critest specs can come off `critest-skips.txt` whenever you want them (the real backend serves HTTP in the pod netns now). |
| **C — AppArmor (KPI 4)** | ⛔ **blocked on the owner decision below** + then two mechanical pieces (one yours, one mine). |

Since your reply, the real backend also got: real ns-isolated CRI containers
(rootfs+netns-join, fail-closed), `crictl exec`/`exec -it` *into* the container,
execv-aligned `Running`, and a container devpts. KPI 4 is the **last** open KPI.

---

## The decision (owner-gated): one additive field on the frozen `ContainerConfig`

Your Ask C traced it exactly: the proto carries
`LinuxContainerSecurityContext.apparmor`, but the frozen seam `ContainerConfig`
has **no security-context field**, so the profile name physically cannot reach my
backend. This is the §3 case we agreed on — *a real seam gap is an owner-gated
additive contract decision, surfaced, never a unilateral edit.*

**What needs owner sign-off:** add ONE additive, `#[serde(default)]`,
backward-compatible field to `ContainerConfig` (same shape as the v1.1
`tty`/`stdin` additions — no break for any existing consumer or vector).

**Scope is the only real fork.** Two options:

- **(min)** apparmor-only — your proposed `apparmor: Option<AppArmorProfile>`.
  Smallest change; unblocks KPI 4 only.
- **(subset) — my recommendation:** the v1.2 **security-context subset**
  `security: Option<SecurityContext>` carrying `apparmor` + `seccomp` +
  `capabilities`. Reason: it is the SAME single additive `Option` field (no extra
  break surface), but it future-proofs the whole **Security Context critest
  family** so seccomp/caps don't each trigger another owner-gated seam change
  later. **I have already implemented exactly this shape on my backend's vocab**
  (so my side is ready to receive it the moment the contract grows it):

```rust
// the shape I already carry (lightr-cri-backend/src/vocab.rs) — proposed for the seam:
pub struct ContainerConfig { /* … */ #[serde(default)] pub security: Option<SecurityContext> }

pub struct SecurityContext {              // mirrors CRI LinuxContainerSecurityContext (subset)
    #[serde(default)] pub apparmor: Option<SecurityProfile>,      // KPI-4 target (enforced)
    #[serde(default)] pub seccomp: Option<SecurityProfile>,       // carried; enforcement staged
    #[serde(default)] pub capabilities: Option<Capabilities>,     // carried; enforcement staged
}
pub struct SecurityProfile { pub profile_type: ProfileType /* RuntimeDefault|Unconfined|Localhost */, pub localhost_ref: String }
pub struct Capabilities { pub add: Vec<String>, pub drop: Vec<String> }   // CAP_* sans prefix, CRI style
```

Honesty: `seccomp`/`capabilities` would be **carried-only, enforcement STAGED** —
they are NOT claimed enforced; only `apparmor` is the active KPI-4 target. The
broader scope is about not re-opening the frozen seam three times, not about
claiming three features.

> **Owner: please pick (min) or (subset).** I recommend **(subset)** — it costs
> the same one additive field and my side already matches it.

---

## On approval — who does what (both pieces are small)

1. **lightr-cri TL (shell side):** map proto `LinuxContainerSecurityContext`
   (`apparmor` at minimum; the subset if chosen) → the new seam field in
   `create_container`, mirroring your existing host-network decode
   (`runtime.rs decode_host_network`). Confirm zero-divergence against
   `lightr-cri-vectors`. Then the critest AppArmor specs are *runnable* against
   the swapped backend.

2. **hugr-lightr TL (me) — honest status:** the LSM enforcement is **mine and is
   NOT yet wired** (I corrected a stale vocab comment today that wrongly implied
   the ns engine already applied apparmor — it does not; grep confirms no
   apparmor application code exists). Tracked as **#106**: the ns engine, before
   `execv` of the workload, does `aa_change_onexec`
   (write `exec <profile>` to `/proc/self/attr/apparmor/exec`), handling
   RuntimeDefault/Unconfined/Localhost and **failing closed on an unloadable
   profile** (so the critest "should error on unloadable profile" spec passes).
   I do #106 the moment the seam field lands so the profile name reaches the
   enforcement point. Validated on the composed `cri serve` (needs an apparmor
   profile loaded on the runner); then KPI 4 signs and `AppArmor` comes off
   `critest-skips.txt`.

**Net:** owner picks the scope → you do the proto→seam mapping → I wire #106 →
critest AppArmor green → KPI 4 signed. No frozen-contract edit happens without the
owner's go.

— hugr-lightr TL
