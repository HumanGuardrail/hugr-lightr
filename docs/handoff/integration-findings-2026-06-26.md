# Integration findings (WP-#97) — the seam COMPOSES; 2 cross-repo items to land it

- **From:** hugr-lightr TL · **To:** lightr-cri TL
- **Date:** 2026-06-26 · **Re:** composing `LightrBackend` into `lightr-cri-server` after your PR #1
- **Status:** report + 2 requests (cross-repo). Lives in hugr-lightr; I don't touch lightr-cri.

## What landed (proven)
I composed the real backend end-to-end: a new opt-in crate `crates/lightr-cri-serve`
(excluded from the hugr-lightr default workspace, so normal builds stay decoupled
from your repo) with an `Adapter(LightrBackend)` that impls your canonical
`cri_canon::CriBackend` (the one `run_blocking<B: CriBackend>` binds) by converting
the parallel-transcribed vocab types and delegating to `LightrBackend`. **It builds
green** → `Adapter(LightrBackend)` satisfies `lightr_cri_server::run_blocking` → the
swap composes with the REAL backend. All **23 trait methods are method-identical**;
conversions were mechanical 1:1 (both vocab crates are faithful transcriptions). Kept
on branch `worktree-agent-ac70bb39fd9480b0a` (commit `288458f`), NOT merged — it
can't build standalone until the two items below are resolved.

## 2 cross-repo items needed to LAND it (yours)

### 1. Resolve the `lightr-cri-backend` name+version collision
Both repos ship a crate literally named **`lightr-cri-backend v0.1.0`** (yours = the
canonical trait+vocab; mine = `LightrBackend` + its transcription). Cargo refuses two
identically-named+versioned packages in one dependency graph (`package collision in
the lockfile`). I validated locally against a throwaway version-bumped copy of your
crate. To land for real (and identically in CI git-deps), one side must disambiguate.
**Cleanest from your side:** rename the canonical trait+vocab crate (e.g.
`lightr-cri-seam` or `lightr-cri-contract`) OR bump its version distinctly — your call,
it's your crate. Tell me the final name/version and I'll point `cri-canon` at it.
(If you'd rather I rename MY impl crate instead, say so — but yours is the one many
consumers will import as "the seam", so renaming it to a seam-y name reads better.)

### 2. Transcribe the v1.2 security-context onto the canonical seam (blocks KPI 4)
The canonical `ContainerConfig` is still **v1.1** — no security field. My local
`ContainerConfig` carries the owner-approved **v1.2 `security: Option<SecurityContext>`**
(apparmor/seccomp/capabilities; see `shell-swap-followup-2026-06-25.md §1` for the exact
shape). The adapter therefore **drops `security`** (canon↔local), so **KPI 4 (AppArmor)
is UNREACHABLE through the composed path until the canonical seam is bumped to v1.2.**
Please transcribe the v1.2 shape (from the followup doc) into your `ContainerConfig` +
contract + the proto→field mapping in `create_container`, and add a shared vector
exercising `security.apparmor`. KPI 3 (cold-start) does NOT need this; KPI 4 does.

## Owner-gated (not yours, noted for completeness)
CI for KPI 3/4 needs the linux-validation job to fetch your PRIVATE repo (git-dep /
cross-repo checkout) → a fine-grained read-only PAT secret on hugr-lightr
(`LIGHTR_CRI_CI_TOKEN`). Owner is setting that up.

## On delivery
Give me (1) the disambiguated crate name/version and (2) the v1.2 transcription +
vector. I rebase the `lightr-cri-serve` scaffold onto them, switch path-deps → git-deps
(pinned), wire the CI job with the PAT, run critest-real against the composed backend,
and sign KPI 3 (+ KPI 4 once v1.2 lands). I won't touch lightr-cri — reply via your
channel.

— hugr-lightr TL

---

## UPDATE (2026-06-26, later) — integration CI-PROVEN ✅
`crates/lightr-cri-serve` landed on hugr-lightr main with GIT deps (pinned rev
17008ca) — the name collision is resolved my-side via distinct source (git vs my
local path), so the lightr-cri rename (item 1 above) is now **optional, not
blocking**. The `cri-serve-smoke` CI job is GREEN: it builds the composed binary
and `crictl version` returns **`RuntimeName: lightr, RuntimeApiVersion: v1`** —
the real `LightrBackend` answers CRI through your shell, in CI. Item 2 (v1.2
security-context transcription) is still needed for KPI 4; KPI 3 cold-start is the
next step on top of this smoke and needs nothing from you.
