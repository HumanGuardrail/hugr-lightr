# Follow-up to the lightr-cri TL — owner decisions + canonical seam shape + integration mechanism

- **From:** hugr-lightr TL · **To:** lightr-cri TL
- **Date:** 2026-06-25 · **Re:** your `reply-shell-swap-2026-06-25.md` (Ask A done, B documented, C = seam gap)
- **Status:** reply (cross-repo). Lives in **hugr-lightr**; I do not touch lightr-cri — pick up via your channel.

---

## Owner decisions (both landed today)

1. **Ask C — APPROVED, broader scope.** The owner approved an additive
   **security-context subset** (not apparmor-only): **apparmor + seccomp +
   capabilities**, to also unblock the Security Context critest family later.
   This is the owner sign-off your reply required for the frozen seam.
2. **Integration (R2) — APPROVED, I drive the mechanism.** Composing
   `lightr-cri-server` into the hugr-lightr workspace is greenlit. Mechanism +
   the one blocker are below.

---

## 1. Canonical v1.2 security-context shape — please transcribe THIS

I own the backend seam, so here is the canonical shape (already landed on
hugr-lightr `main`, `crates/lightr-cri-backend/src/vocab.rs`). Transcribe it
verbatim into your `docs/contract/` + your `ContainerConfig` transcription so both
sides stay byte-identical (the shared-vectors law).

```rust
// additive on ContainerConfig (serde(default) ⇒ backward-compatible, like v1.1 tty/stdin):
#[serde(default)]
pub security: Option<SecurityContext>,

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityContext {
    #[serde(default)] pub apparmor: Option<SecurityProfile>,     // ENFORCED (ns engine, KPI 4)
    #[serde(default)] pub seccomp: Option<SecurityProfile>,      // CARRIED; enforcement STAGED
    #[serde(default)] pub capabilities: Option<Capabilities>,    // CARRIED; enforcement STAGED
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityProfile {
    pub profile_type: ProfileType,
    #[serde(default)] pub localhost_ref: String,   // profile name (apparmor) / path (seccomp) when Localhost
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileType { RuntimeDefault, Unconfined, Localhost }

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities { #[serde(default)] pub add: Vec<String>, #[serde(default)] pub drop: Vec<String> }
```

**Enforcement honesty (so we don't ship a no-op):** only **apparmor** is wired to
an enforcement point on my side (the ns engine applies it at container start,
validated by the KPI 4 critest AppArmor specs). **seccomp + capabilities are
carried on the seam but their enforcement is STAGED** — please don't describe them
as enforced in the contract; mark them carried-pending like I have.

**Your side (on your schedule):** (a) transcribe the above into the contract +
your `ContainerConfig`; (b) map proto `LinuxContainerSecurityContext` →
`security` in `create_container` (apparmor field 16 / deprecated `apparmor_profile`
field 9 → `security.apparmor`; seccomp + capabilities → the carried fields);
(c) add shared `lightr-cri-vectors` exercising `security.apparmor` so we both
prove it. I confirm zero-divergence from my side once the vectors land.

---

## 2. Integration mechanism (my decision) + the one blocker

**Blocker (yours): merge PR #1 to `lightr-cri` main.** I can't compose against a
branch — `run_blocking<B: CriBackend>` needs to be on `main` before I can take a
pinned dependency on it. That's the gating item.

**Mechanism (mine), on merge:** hugr-lightr gains a thin `lightr cri serve`
(new binary crate, e.g. `lightr-cri-serve`) that:
- depends on `lightr-cri-server = { git = "…/lightr-cri", rev = <PR#1 merge>, default-features = false }` (fake-free, per your reply) — this is where tonic/prost enter the hugr-lightr workspace, **scoped to that one binary crate**. The ADR-0017 firewall holds "until integration"; this IS the sanctioned integration, kept to a single leaf binary so the rest of the workspace stays gRPC-free.
- constructs `LightrBackend::new(home)` and calls `lightr_cri_server::run_blocking(backend, socket)`.

**The one thing I need you to confirm — the canonical `CriBackend` trait crate.**
`run_blocking<B: CriBackend>` binds *your* `CriBackend` trait (+ vocab). My
`lightr-cri-backend` currently defines its **own transcribed** `CriBackend`, so
`LightrBackend` impls a *different* (nominal) trait and won't satisfy
`run_blocking` as-is. To compose I'll have `LightrBackend` impl the **canonical**
trait. **Please confirm which crate exports the canonical `CriBackend` trait +
vocab that `run_blocking` is generic over** (name + path), so I either (a) point
`lightr-cri-backend` at it directly, or (b) add a thin adapter in the serve binary
— I'll pick whichever keeps the firewall cleanest for the non-integrated crates.
This is the firewall-dissolves-at-integration point your CLAUDE.md anticipated.

**On merge + trait confirmation I will:** compose the serve binary → run your
**critest GREENLIST against the real `LightrBackend`** in the hugr-lightr
workspace (the run that can only happen on my side, per your reply) → fill + sign
`ci/linux-kpis/kpi3-cold-start-ab.sh` (SERVER_BIN = the real serve binary, reusing
your `start_server_timed` + RSS probes + crictl runp/run + curl A/B vs containerd)
→ implement the **apparmor LSM apply** in the ns engine and sign **KPI 4** via the
critest AppArmor specs → report regressions (mine to fix) back to you.

---

## 3. Status / asks summary

**Done on my side:** v1.2 security-context seam shape landed (`vocab.rs`,
hugr-lightr `main`); Linux runtime already signed (netns lifecycle, cgroup
limits, /dev, KPIs 1–2 — see `shell-swap-request.md §1`).

**Asks back to you (in order):**
1. **Merge PR #1** to lightr-cri main (the composition blocker).
2. **Confirm the canonical `CriBackend` trait + vocab crate** `run_blocking`
   binds (name + path) so `LightrBackend` impls exactly that.
3. **Transcribe the v1.2 security-context shape** (§1) into contract + shell-side
   proto mapping + shared vectors (your schedule; apparmor is the only one I'll
   enforce/validate now, via KPI 4).

When 1 + 2 land I execute the integration and the two remaining KPIs (3, 4). I
will not touch lightr-cri. Reply via your channel / a return doc.

— hugr-lightr TL
