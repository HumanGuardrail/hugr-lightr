# Build Spec — Ship + VM + Views wave (FROZEN)

- **Status:** FROZEN (owner "fazer tudo em paralelo" mandate, 2026-06-12).
  Additive; all prior surfaces unchanged. 5 disjoint WPs.
- **Honesty law (inviolable):** views (W5) and the vz boot (W3/W4) cannot be
  runtime-validated on this Intel box. Same bar as `lightr-init`/`vz`:
  code-complete + host-testable seams + compiles + lints, with runtime
  validation packaged/gated for an ARM target. NO unmeasured runtime claim.

## W1 — Release pipeline + signing gates (`.github/` + `packaging/`)

Goal: Product A can be cut into a release the moment the GTM gate clears —
no core work, only release engineering.
- `.github/workflows/release.yml`: trigger on tag `v*`. Matrix build
  (macos-14 arm64, macos-13 x86_64, ubuntu x86_64) → `cargo build --release`
  → strip → `lightr-<version>-<os>-<arch>.tar.gz` + sha256 → upload as a
  GitHub Release. **Signing/notarization steps are present but GATED behind
  secrets** (`APPLE_CERT`, `AC_API_KEY`…): if absent, the step prints
  "signing skipped — secrets not set (owner provides Apple Developer creds)"
  and proceeds with an UNSIGNED artifact clearly named `-unsigned`. Never
  fail-silent, never fake-signed.
- Wire `packaging/release.sh` as the local equivalent the workflow calls.
- Update `packaging/lightr.rb` (brew formula) url/sha256 to read from the
  release tag pattern (still placeholder until a real tag exists; documented).
- A `release` step MUST be a no-op-publish unless a real tag is pushed — the
  workflow only runs on tag push, so this is structural.
- Validate: `yaml.safe_load` parses the workflow; `bash -n` the scripts.

## W2 — Naming verification (`docs/NAMING.md`)

Resolve MVP open-decision #4. Check availability of `lightr` / `hugr-lightr`
on: crates.io, Homebrew core + popular taps, npm (CLI namespace squat risk),
and a basic trademark sanity (USPTO/general web). Use WebSearch/WebFetch.
Produce `docs/NAMING.md`: a table (name | registry | status | link) + a
DECISION line (recommended crate name + binary name) + any conflict found.
Pre-decided constraint: binary stays `lightr`; crate is `hugr-lightr` if
`lightr` is taken on crates.io (it is — confirm + cite). No code changes.

## W3 — Real Linux kernel pack pipeline (`scripts/` + `crates/lightr-engine`)

Goal: give `lightr engine install-pack` a real pack to install, and a
reproducible recipe to build one. The cpio assembler already exists
(`assemble_pack`); this adds the kernel-sourcing + a structurally-validated
end-to-end pack build.
- `scripts/build-linux-pack.sh`: a documented recipe that (a) obtains a
  minimal Linux kernel suitable for Virtualization.framework (DOCUMENT the
  source: Apple Containerization kernel config OR a pinned minimal build;
  the script fetches/builds it), (b) builds the `lightr-init` binary for the
  guest target (`aarch64-unknown-linux-musl` / `x86_64-unknown-linux-musl`),
  (c) calls the pack assembler to produce `kernel` + `initrd` (init as
  `/init`), (d) emits a `pack.json` manifest (arch, kernel version, init
  digest). The kernel BUILD step may require a cross-toolchain not on this
  Mac — if so, the script DETECTS its absence and prints exactly what to
  install, and the STRUCTURAL assembly (given a kernel + init binary) is
  what's tested here. No fake kernel.
- `crates/lightr-engine`: extend the `pack` module if needed so
  `assemble_pack` + a new `verify_pack(dir) -> Result<PackInfo>` validate a
  pack's structure (kernel present, initrd is a valid cpio whose `/init` is
  executable, pack.json parses). `engine install-pack` already exists — wire
  `verify_pack` into it so a malformed pack is rejected loudly.
- Tests (host): assemble a pack from a FAKE kernel file + the REAL built
  `lightr-init` (or a stand-in) → `verify_pack` accepts it; a pack missing
  kernel/init or with a non-cpio initrd → `verify_pack` rejects. Structure,
  not boot.

## W4 — S5 boot runbook + harness (`spikes/s5-vz-boot/`)

The complete, runnable package to validate the real microVM boot on a rented
ARM Mac — so the owner just provisions and runs.
- `spikes/s5-vz-boot/README.md`: provisioning checklist (AWS EC2
  `mac2.metal` M1/M2 or MacStadium dedicated; macOS 14+; Xcode for swiftc),
  the exact commands, expected output, pass/fail criteria, cost estimate.
- `spikes/s5-vz-boot/run-s5.sh`: on an ARM Mac — build `--features vz`,
  build + install a linux pack (via W3's script), import a tiny alpine OCI
  image as a ref, `lightr run --engine vz @img/alpine -- /bin/echo
  s5-boot-ok`, and ASSERT: (a) exit code 0 returned via the real vsock chain
  (not the 255 no-report fallback), (b) stdout contains `s5-boot-ok`, (c)
  a non-zero command (`-- /bin/sh -c 'exit 7'`) returns 7 (proves the real
  exit code flows, not a hardcoded value). Print a clear PASS/FAIL summary.
- `spikes/s5-vz-boot/EXPECTED.md`: what each assertion proves (closes the
  parity-audit F-205/F-206 gates when green on ARM).
- This WP writes the harness; it does NOT run it (no ARM here). `bash -n`
  validates the scripts.

## W5 — Views layer (`crates/lightr-views/`, NEW)

The O(1) materialization layer (ADR-0013), the other half of the perf
headline. Host-testable logic behind seams; the real mount is cfg/seamed for
a target.
- New crate `lightr-views`. Frozen surface:
  ```rust
  /// A plan to present a manifest as a virtual view (O(1) appearance) plus
  /// a solidifier policy that promotes hot files to real CoW clones.
  pub struct ViewPlan { /* entries by path, lazy */ }
  pub fn plan_view(manifest: &lightr_core::Manifest) -> ViewPlan;

  /// OS actions a view backend performs — seamed for host testing.
  /// Real impls: composefs/EROFS (Linux), NFS-loopback (macOS, EdenFS-proven).
  pub trait ViewBackend {
      fn mount(&mut self, plan: &ViewPlan, at: &std::path::Path) -> std::io::Result<()>;
      fn fault_in(&mut self, path: &str) -> std::io::Result<()>; // lazy load one entry
      fn unmount(&mut self, at: &std::path::Path) -> std::io::Result<()>;
  }

  /// Solidifier: promote-on-access policy. Given an access trace, decide
  /// which entries to CoW-clone to real disk; when all hot entries are solid,
  /// signal the mount can evaporate. Pure + fully unit-tested.
  pub struct Solidifier { /* ... */ }
  impl Solidifier {
      pub fn new(plan: &ViewPlan) -> Self;
      pub fn record_access(&mut self, path: &str);
      pub fn next_to_promote(&mut self) -> Option<String>; // priority order
      pub fn is_fully_solid(&self) -> bool;
  }
  ```
- Real backends are `#[cfg(...)]`-gated stubs marked `// VIEW-PATH (S1/S3)`
  (NFS-loopback server skeleton on macOS, composefs/EROFS layout on Linux) —
  they COMPILE; runtime validation is the S1/S3 spike on a target box.
- A `FakeBackend` + the pure `ViewPlan`/`Solidifier` logic are FULLY tested
  on the host: plan covers every manifest entry; solidifier promotes in a
  sane priority order (manifest order + access frequency), `is_fully_solid`
  flips only after all accessed entries promoted; fault_in is idempotent.
- Deps: `lightr-core` (+ `lightr-store` for the CoW clone the solidifier
  drives, if needed). No wiring into `hydrate`/CLI in this WP (additive
  follow-up) — keep it a clean library so it stays disjoint.

## Gates (every WP) & integration

Per-WP: `cargo fmt --check` · `clippy -p <crate> --all-targets -D warnings`
· `cargo test -p <crate>` (or `bash -n`/`yaml` validate for script/CI WPs).
Integration: merge DAG (any order — disjoint), full `cargo test --workspace`
3× (no flakes), `clippy --workspace -D warnings`, `--features vz` still
compiles+lints, post-flight sweep (no writes outside owner-globs), opus
cold-critic. Lead owns all integration fixes.

## Lead scaffold (pre-wire, before dispatch)
- Add `crates/lightr-views` to workspace members + `lightr-views = { path }`
  in `[workspace.dependencies]` (the only shared-file touch).
- Create the `lightr-views` skeleton (frozen surface above + `todo!()`).
- Create empty `scripts/`, `spikes/s5-vz-boot/` dirs as needed.
