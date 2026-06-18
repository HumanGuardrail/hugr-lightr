# Build-spec — Parity debt + the humiliation bench (Wave A + Wave C)

- **Status:** FROZEN contract (tech-lead, 2026-06-18). Closes the buildable parity
  debt the owner ruled in-scope ("expansão = dívida; deveria estar entregue") and
  builds the real head-to-head benchmark. Code is written ONLY against this spec.
- **Canon:** docs/spec/feature-parity.md (R1/R3 rows) · performance-bar.md (8
  indicators) · ADR-0012 (bench = CI gate, tense law) · parity-audit.md (F-ids).
- **Rule:** zero debt, no stub shipped to main, root-cause only. Per-platform
  capability that needs HW/caps it can't get is an **honest error/skip**, never a
  silent no-op. No unmeasured number claimed (tense law).

This wave closes **F-203** (resource limits), **F-308** (restart via OS
supervisor), **F-309** (healthcheck/secrets/configs), and builds **bench-compare**
(the real vs-Docker/OrbStack/Apple-container harness).

---

## §0 — Memo-key law (LEAD decision, do not relitigate)

The memo key identifies a run's deterministic output. Therefore:

| Field | In key? | Why |
|---|---|---|
| `limits` (cpu/mem) | **NO** | resource caps don't change deterministic output; an OOM-kill is an environmental failure, not a cached result. Docker doesn't key on them. |
| `secrets` | **YES** | store-backed inputs; a different secret ⇒ different run ⇒ must not share a cache entry. Contribute like `mounts` (ordered: ref-name + resolved manifest digest). |
| `configs` | **YES** | same as secrets. |
| `healthcheck` | **NO** | post-result probe; never part of the command's output. |
| `restart` | **NO** | OS-supervisor concern; not a run input. |

Consequence: `limits` are threaded as a **separate exec parameter**, NOT a RunSpec
field → the 16 existing `RunSpec {…}` sites stay untouched. `secrets`/`configs`
DO become RunSpec fields and DO update `build_key`/`assemble_key`.

---

## §1 — WP-A0: the contract freeze (GATING; lead-owned scaffold)

A0 makes every shared-surface edit and wires each seam to a **stub fn in a new
per-feature file**, so A1/A2/A3/C each own one file and never collide. A0 must end
**`cargo check` + `cargo test` green** (stubs are honest no-ops / `Unsupported`,
never `panic!`/`unimplemented!`). Branch base for A1/A2/A3/C is the A0 commit.

### A0.1 — `ResourceLimits` in lightr-core (pure type + parser; safe)

`crates/lightr-core/src/lib.rs` — add (core is `#![forbid(unsafe_code)]`, so TYPE
+ PARSE only; application lives in run/engine):

```rust
/// Resource caps for a run. None = unlimited (parity default). NOT in the memo key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    /// CPU as milli-CPUs: 1000 = one full core. 500 = half. None = unlimited.
    pub cpu_millis: Option<u64>,
}

impl ResourceLimits {
    pub fn is_unlimited(&self) -> bool { self.memory_bytes.is_none() && self.cpu_millis.is_none() }

    /// Parse Docker-style strings. memory: "512m" "1g" "2048k" "1073741824".
    /// cpus: "0.5" "2" "1.5". Err on malformed input (fail closed).
    pub fn parse(memory: Option<&str>, cpus: Option<&str>) -> Result<Self> { /* WP-A1 */ }
}
```
(`parse` body is WP-A1; A0 ships it returning `Ok(Self::default())` ONLY as a
compiling placeholder IFF flags are absent — but A0 wires the call so a flag
present routes to `parse`. Simplest A0: implement `parse` fully here since it's
pure + safe + unit-testable — see A1 §2.1. **A0 implements parse** to keep the
freeze self-consistent.)

### A0.2 — `RunSpec` gains secrets/configs (in-key inputs)

`crates/lightr-run/src/lib.rs:10` — extend:

```rust
pub struct RunSpec {
    pub cwd: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
    pub mounts: Vec<Mount>,
    pub secrets: Vec<StoreFile>,   // F-309 — hydrated to <cwd>/.lightr/secrets/<name> (0600)
    pub configs: Vec<StoreFile>,   // F-309 — hydrated to <cwd>/.lightr/configs/<name> (0644)
}

/// A store-backed file injected into a run. ref_name resolves via lightr_index.
pub struct StoreFile { pub name: String, pub ref_name: String }
```

- Update **all 16** `RunSpec {…}` sites (grep `RunSpec {`): add `secrets: vec![], configs: vec![]`.
- `build_key` (lib.rs:197) + `assemble_key`: after the mount contribution, call
  `crate::secrets::contribute_to_key(&mut hasher, &spec.secrets, b"secret\0")` and
  `…(&spec.configs, b"config\0")` (stub in new file `secrets.rs`, A0 = no-op that
  still compiles; WP-A3 fills it). Because A0's stub is a no-op, existing keys are
  unchanged until A3 lands → existing tests stay green in A0.
- `run_memoized` miss path (after mount hydrate, ~line 291): call
  `crate::secrets::hydrate(&spec.cwd, store, &spec.secrets, &spec.configs)?;`
  (stub Ok(()) in A0).

### A0.3 — `limits` as a separate exec parameter (NOT in RunSpec)

`crates/lightr-run/src/lib.rs` — add the wrapper, keep the old fn delegating:

```rust
pub mod limits;            // new file (A0 creates it)

pub fn run_memoized(spec: &RunSpec, store: &Store) -> Result<RunOutcome> {
    run_memoized_with(spec, store, &lightr_core::ResourceLimits::default())
}

pub fn run_memoized_with(spec: &RunSpec, store: &Store, limits: &lightr_core::ResourceLimits)
    -> Result<RunOutcome> { /* body = today's run_memoized, but the spawn applies limits */ }
```
- Move today's `run_memoized` body into `run_memoized_with`; at the native spawn
  (line 300) build the `Command`, then call
  `limits::apply_native(&mut cmd, limits)?` **before** `.output()`.
  `limits::apply_native` stub in A0 = `Ok(())` if `limits.is_unlimited()` else `Ok(())`
  (WP-A1 fills the real `setrlimit`). 16 existing callers unchanged (they call the
  `run_memoized` wrapper).

### A0.4 — `ExecSpec` gains owned `limits`

`crates/lightr-engine/src/lib.rs:280`:
```rust
pub struct ExecSpec<'a> {
    pub cwd: &'a Path,
    pub command: &'a [String],
    pub rootfs: Option<&'a Path>,
    pub limits: lightr_core::ResourceLimits,   // Copy; default = unlimited
}
```
- Update the **5** `ExecSpec {…}` sites: add `limits: Default::default()`.
- `NativeEngine::run` (line 318): after building `Command`, call
  `crate::limits::apply_native(&mut cmd, &spec.limits)?` before `.status()`.
- `run_in_namespaces` (ns, ~409): after unshare/pre-exec, call
  `crate::limits::apply_cgroup(&spec.limits)?`. vz: thread `spec.limits` into the
  shim call. A0 creates `crates/lightr-engine/src/limits.rs` with stub
  `apply_native`/`apply_cgroup` (= `Ok(())`), `pub mod limits;`. (Two limits.rs
  files — run's applies to a std Command pre-output; engine's to engine spawns.
  Or share one in core? No — apply needs `unsafe`, forbidden in core. Keep one per
  crate; WP-A1 owns BOTH.)

### A0.5 — CLI surface (all of Wave A + C; lead owns main.rs)

`crates/lightr-cli/src/main.rs`:
- `Run {…}` (line 245): add
  `#[arg(long, value_name="SIZE")] memory: Option<String>,`
  `#[arg(long, value_name="N")] cpus: Option<String>,`
  `#[arg(long, value_name="NAME=REF")] secret: Vec<String>,`
  `#[arg(long, value_name="NAME=REF")] config: Vec<String>,`
  `#[arg(long, value_name="CMD")] health_cmd: Option<String>,`
  `#[arg(long, default_value_t=30)] health_interval: u64,`
  `#[arg(long, default_value_t=3)] health_retries: u32,`
- New top-level subcommands:
  ```rust
  /// Generate an OS-supervisor unit (launchd/systemd) for a restart policy — no daemon of ours.
  Supervise { #[command(subcommand)] subcmd: SuperviseCmd },
  /// Head-to-head benchmark vs Docker/OrbStack/Apple container on identical workloads.
  BenchCompare {
      #[arg(long, value_delimiter=',', default_value="docker,orbstack,container")] vs: Vec<String>,
      #[arg(long, default_value="all")] workload: String,
      #[arg(long)] json: bool,
  },
  ```
  `SuperviseCmd::{Install { name, restart, dir, command }, Uninstall { name }, List}`.
- Dispatch arms route to `handlers::supervise::*` and `handlers::bench_compare::run`
  (A0 creates these handler files with a stub that returns `Ok(())` printing
  "not yet implemented (WP-A2/WP-C)"… NO — honest: A0 stub returns
  `Err(LightrError::Unsupported("…"))` so it never lies about success).
- `crates/lightr-cli/src/handlers/run.rs:168` + `:143`: parse the new flags →
  build `ResourceLimits::parse(memory.as_deref(), cpus.as_deref())?`, the
  `secrets`/`configs` vecs (split `NAME=REF`), pass `limits` to `run_memoized_with`
  / into `ExecSpec.limits`; healthcheck only on `-d`/detached (WP-A3 wires probe).
- `plan.rs:154`, `mcp.rs:337`, `build/lib.rs:1194`: add `secrets: vec![], configs: vec![]`
  to their `RunSpec`; `build/lib.rs:509` + engine test sites: add `limits: Default::default()`.

### A0 DoD
`cargo fmt` clean · `cargo clippy --all-targets -- -D warnings` (default + `--features vz`) ·
`cargo test --workspace` GREEN (all 411 still pass — stubs are inert no-ops) ·
new files created with honest stubs · `git commit` on `wave/zero-debt`.

---

## §2 — WP-A1: resource limits impl (F-203). Owns `*/limits.rs` + shim.

**Files:** `crates/lightr-core/src/lib.rs` (`ResourceLimits::parse` body + unit tests),
`crates/lightr-run/src/limits.rs`, `crates/lightr-engine/src/limits.rs`,
`crates/lightr-engine/shim/vz.swift` (+ the vz FFI call site in `vz_impl`).

2.1 **`ResourceLimits::parse`** — memory suffixes k/m/g (1024-based) + bare bytes;
cpus float→`round(f*1000)` milli. Reject negative/zero/garbage with `InvalidRef`. Unit tests.

2.2 **`apply_native` + early `check_native_support`** — AMENDED 2026-06-18 (lead).
The original premise ("memory IS enforced natively on macOS") was FACTUALLY WRONG:
macOS (Darwin) ignores `RLIMIT_AS`/`RLIMIT_DATA` (verified — `setrlimit` returns
`EINVAL`; a 256 MB alloc under a 64 MB cap succeeds), and there is no stable PUBLIC
macOS mechanism (jetsam/Mach footprint are private — no gambiarra). WP-A1 correctly
STOPPED and surfaced this rather than ship a contradiction (AP-3). Honest matrix:
- **Linux native:** `memory_bytes` → `RLIMIT_AS`+`RLIMIT_DATA` in a `pre_exec` hook
  (`#[cfg(target_os="linux")]`). Enforced.
- **macOS/Windows native + `memory_bytes`:** honest `Err(InvalidRef("memory limits
  are not enforceable on the native engine on this OS; use --engine vz (macOS) for a
  hard cap"))`. The macOS hard memory cap IS the vz engine (VM RAM — Docker's own
  mechanism on Mac; on-thesis per feature-parity.md physics note).
- **`cpu_millis` on native (any OS):** honest `Err` → `--engine ns` (cgroup) / `vz`
  (vcpu). (`RLIMIT_CPU` caps total cpu-seconds, not a share.)
- **Validate EARLY:** `check_native_support(limits)?` at the TOP of `run_memoized_with`
  (before the AC lookup) so a cache-HIT can't bypass the honest error (memo key
  excludes limits, §0). Fail closed, hit or miss.
- Tests: Linux-gated "over-cap child killed"; off-Linux unit asserts the honest
  `Err`; cpu-share honest `Err` (all platforms).

2.3 **`apply_cgroup`** (ns, Linux): write a transient cgroup v2 dir under the
caller's delegated subtree — `memory.max` ← bytes, `cpu.max` ← `"<millis*100> 100000"`
(quota/period). If cgroup v2 unavailable or write denied (no `CAP_SYS_RESOURCE`/
delegation) → honest `Err(Unsupported(detail))`, never silent. Linux-gated test
(`#[cfg(target_os="linux")]`, skip-with-note if not delegated in CI).

2.4 **vz**: extend `vz.swift` FFI to take `memory_mb: u64, cpu_count: u64`; set
`VZVirtualMachineConfiguration.memorySizeInBytes` / `.cpuCount`. cpus→`ceil(millis/1000)`
vcpus (min 1); memory→bytes (min the VZ floor, else honest error). Thread
`spec.limits` from `vz_impl` into the call. Validatable on Intel vz (F-205 path).

2.5 Acceptance: `crates/lightr-acceptance/tests/` — `--memory` caps a real run
(native, memory); `--cpus` on native errors honestly; (ns/vz limit tests gated).

2.6 Update **parity-audit.md** F-203 → ✅ (native mem) / 🟡 honest per-engine notes.

---

## §3 — WP-A2: restart via OS supervisor (F-308). All new files + `supervise.rs`.

**Files:** `crates/lightr-run/src/restart.rs` (pure unit-file templates),
`crates/lightr-cli/src/handlers/supervise.rs` (the verb).

3.1 `RestartPolicy::{No, Always, OnFailure{max:u32}, UnlessStopped}` + parse
("no"/"always"/"on-failure[:N]"/"unless-stopped").
3.2 Pure generators (no I/O): `launchd_plist(label, program, args, dir, policy) -> String`
(macOS: `KeepAlive`/`KeepAlive{SuccessfulExit:false}` maps the policy; `RunAtLoad`),
`systemd_unit(...)->String` (Linux user unit: `Restart=always|on-failure`, `ExecStart`).
3.3 `lightr supervise install --name N --restart P --dir D -- CMD`: write the unit
to `~/.lightr/units/<name>.{plist|service}`, print the exact `launchctl bootstrap`/
`systemctl --user enable --now` command (DO NOT auto-load — the user opts in; we
ship NO daemon). `uninstall`: remove the unit + print the unload command. `list`:
enumerate `~/.lightr/units/`.
3.4 Honest: this **integrates the OS supervisor**; Lightr ships no resident
process (feature-parity.md R3). Windows: honest `Unsupported` for now (Task
Scheduler = future).
3.5 Acceptance: install writes a shell-valid unit (parse/lint), correct policy
mapping, uninstall removes it; `ps` still shows zero Lightr daemons (A4 invariant holds).
3.6 parity-audit F-308 → ✅ (macOS/Linux), Windows 🟡.

---

## §4 — WP-A3: healthcheck / secrets / configs (F-309). `secrets.rs` + `healthcheck.rs`.

**Files:** `crates/lightr-run/src/secrets.rs`, `crates/lightr-run/src/healthcheck.rs`
(+ fill the A0 seams in build_key/run_memoized/spawn_detached — those CALL sites are
A0-wired; A3 fills the fn bodies in its own files).

4.1 **secrets/configs** (`secrets.rs`):
- `contribute_to_key(hasher, &[StoreFile], domain)`: for each (sorted by name),
  hash name + `\0` + resolved ref's manifest digest (resolve via lightr_index).
  This is the in-key contribution (§0).
- `hydrate(cwd, store, secrets, configs)`: materialize each ref via
  `lightr_index::hydrate` into `<cwd>/.lightr/secrets/<name>` (chmod 0600) /
  `<cwd>/.lightr/configs/<name>` (0644). **Honest boundary:** no daemon/tmpfs, so
  secrets land on disk under the run dir at 0600 (single-user local) — documented,
  not hidden. Fail closed if a ref is missing.
4.2 **healthcheck** (`healthcheck.rs`): `Healthcheck{cmd, interval_s, retries}`;
`probe(&Healthcheck, cwd) -> Health::{Healthy,Unhealthy}` runs the cmd, retries.
Wire into the **detached supervisor** (`spawn_detached`): after spawn, a probe loop
writes health state to the run's control dir (`~/.lightr/run/<id>/health`); `ps`
surfaces it. Not in the memo key. (Foreground runs: `--health-cmd` runs one probe
post-exit, reports; no loop.)
4.3 compose (`lightr-build`): parse `healthcheck:`/`secrets:`/`configs:` in
compose.yml into the RunSpec/service spec (schema extension).
4.4 Acceptance: secret/config hydrated at right path+mode; changing a secret ref
changes the memo key (cache miss); healthcheck flips Healthy→Unhealthy; secret
absent → fail closed.
4.5 parity-audit F-309 → ✅.

---

## §5 — WP-C: the humiliation benchmark. `handlers/bench_compare.rs` (one file).

**File:** `crates/lightr-cli/src/handlers/bench_compare.rs` (+ A0 wired the
subcommand + dispatch). Reuses fixtures from `bench.rs` (extract shared if needed,
but prefer self-contained to avoid touching bench.rs).

5.1 **Workloads** (identical bytes through each runtime; this is the head-to-head):
- `materialize`: a **1 GB** tree (scale the bench fixture up — current ~10 MB is too
  small for indicator #3) — Lightr `hydrate` vs Docker `pull`+unpack of an equiv image.
- `cold-run`: cold pull+run a tiny image (e.g. alpine `echo`) — Lightr import+run vs `docker run`.
- `re-run`: same job twice — Lightr memo-hit vs Docker re-run.
- `idle`: idle RSS/processes after the tool is "installed but idle" — Lightr (0, `ps` proves) vs Docker (dockerd+VM).
- `build`: the 3-step Dockerfile — Lightr `build` (memoized 2nd) vs `docker build`.
5.2 **Runtimes**: detect on PATH — `docker`, `orbstack`/`orb`, Apple `container`.
**Tense law:** an absent runtime is a printed **SKIP** row, never a fabricated
number. Measure Lightr always; others only if present. Each timing = median-of-N
after warmup (mirror bench.rs methodology); RSS via `ps`/`/usr/bin/time`.
5.3 **Output**: side-by-side table (`indicator | lightr | docker | orbstack | container | factor`)
+ `--json`. Factor = competitor/lightr (the "humiliation multiple"), only where both measured.
5.4 No budgets/CI-gate here (that's `bench`); this is the marketing/proof harness.
Honest header: machine class + which runtimes were present + "numbers measured on
THIS box; Apple-Silicon headline binds when run on AS".
5.5 parity-audit F-602 note: bench-compare added; `--vs-docker` deepened or
superseded.

---

## §6 — WP DAG, disjointness, merge order, return shape

```
WP-A0 (freeze, lead)  ──┬──► WP-A1 (limits)     [core parse, run/limits.rs, engine/limits.rs, vz.swift]
   gate-green base      ├──► WP-A2 (restart)    [run/restart.rs, handlers/supervise.rs]
                        ├──► WP-A3 (health/sec) [run/secrets.rs, run/healthcheck.rs, build compose]
                        └──► WP-C  (bench-cmp)  [handlers/bench_compare.rs]
```
- **Disjointness:** after A0, A1/A2/A3/C touch **disjoint files** (their own new
  files + parity-audit rows). The only shared touch is `parity-audit.md` (each adds
  its row) → resolve by lead at merge (union, trivial) OR each appends a distinct
  section. A1 & A3 both reference (not edit) A0-frozen seams.
- **Worktree isolation:** each WP in its own worktree off the A0 commit.
- **Merge order:** A0 → (A1, A2, A3, C any order) → lead cold-reviews each SEAL →
  cherry-pick linear → gate green after each → final parity-audit + CHANGELOG truth-up.
- **Return shape (each agent):** `WP-<id> SEAL | branch <name> @ <sha> | files: <list> |
  tests: <added N, all green> | gate: fmt✓ clippy-D✓ test✓ | honest-boundaries: <list> |
  parity-audit row updated: <F-id→status> | NO decisions taken (or: BLOCKED on <x>)`.
- **DoD (all):** no stub on main; honest per-platform errors (never silent no-op);
  tense law; real tests; gate green; parity-audit truth-synced.
```
