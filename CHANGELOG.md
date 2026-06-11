# Changelog — hugr-lightr

## [Unreleased] — Omni cross-platform wave (2026-06-12)

One product, every desktop — macOS (Intel + Apple Silicon), Linux (x86_64 +
aarch64), Windows (x86_64). The daemonless core is portable behind `cfg`; each OS
gets the lightest isolation it natively offers. Code-complete + host-green +
cross-compile-clean; runtime on foreign hardware is a one-command runbook
(ADR-0017; docs/spec/build-spec-omni.md).

**vz un-gated from Apple Silicon (the correction):**
- Virtualization.framework runs Linux guests on Intel Macs too — `--features vz`
  **compiles + links on this Intel box** (verified, exit 0). Only VZ save/restore
  (F-406) and Rosetta-in-VM (F-208) are genuinely arm64-only.
- `packaging/vz.entitlements` (the `com.apple.security.virtualization` entitlement;
  ad-hoc codesign for local, Developer ID for releases) — the gap that would have
  blocked vz on ANY Mac. `spikes/s5-vz-boot/run-s5.sh` is now arch-aware
  (Intel→x86_64 / ASi→arm64) and codesigns before any VM run; arm64 sibling at
  `spikes/s5-vz-boot-arm64/`.

**Windows tier (NEW — zero `cfg(windows)` existed before):**
- Native core port, additive behind `#[cfg(windows)]`: file locks→`LockFileEx`,
  fsync→`FlushFileBuffers` (dir-fsync = documented no-op), control socket→named
  pipe (JSON protocol unchanged), CoW ladder gains a `RefsBlockClone` rung
  (FSCTL_DUPLICATE_EXTENTS_TO_FILE, best-effort → `std::fs::copy` fallback),
  symlinks→`symlink_file`+copy-fallback, perms→`cfg(unix)`.
- **`wsl` isolation engine** — runs the `ns` model inside WSL2's OS-managed VM
  ("no daemon" holds); honest probe when WSL2 is absent.
- `windows-sys` target-gated (never on unix builds). `cargo check --target
  x86_64-pc-windows-gnu` (lib+bins + all-targets): **0 errors**.

**Distribution + CI:** `release.yml` = 5-target matrix (macOS arm64/x86_64, Linux
x86_64/aarch64 cross-linked, Windows x86_64 `.zip`) → SHA256SUMS + Release;
`ci.yml` gate on ubuntu/macos/windows + an aarch64 cross-check (installs CC +
linker for blake3/ring C deps). macOS release signing applies the vz entitlement.

**Gates:** host `cargo test --workspace` **408/0**, clippy `-D`, fmt clean; Windows
cross-check 0 errors. Delivered via a 7-WP disjoint-by-crate fleet (git worktrees,
**zero merge conflicts**) + cold opus critic. Every Windows runtime path is
`// WIN-PATH` with an honest probe/error + a correct fallback; validation on
Windows/ARM hardware ships as runbooks — nothing claimed validated until green.

## [Unreleased] — Ship + VM + Views wave (2026-06-12)

Three parallel tracks toward true SOTA, all code-complete + host-tested
(runtime validation packaged/gated where it needs an ARM target).

**Ship (Product A):**
- **Release pipeline** — `.github/workflows/release.yml`: tag-triggered
  (`v*`) matrix build (macOS arm64/x86_64, Linux x86_64) → tarballs +
  SHA256SUMS → GitHub Release. macOS signing/notarization steps present but
  gated behind owner secrets; unsigned artifacts clearly labeled `-unsigned`,
  never fake-signed. Nothing publishes without a deliberate tag.
- **Naming resolved** (`docs/NAMING.md`): `lightr` and `hugr-lightr` both
  FREE on crates.io; no brew/CLI collision → crate `hugr-lightr`, binary
  `lightr`. (Apache-2.0 already set, ADR-0008.)

**VM foundation (Product B, ARM-validatable):**
- **Kernel-pack pipeline** — `scripts/build-linux-pack.sh` (kernel = Apple
  Containerization config, pinned + sha-verified; builds `lightr-init` for
  the guest target; assembles kernel+initrd+pack.json). `verify_pack`
  structurally validates a pack (cpio initrd, `/init` executable, non-empty
  kernel) and is now **wired into `engine install-pack`** — malformed packs
  rejected loudly.
- **S5 boot runbook** — `spikes/s5-vz-boot/` (README provisioning + `run-s5.sh`
  harness + EXPECTED): on a rented ARM Mac, build `--features vz`, install a
  pack, `lightr run --engine vz alpine` and assert the REAL exit code flows
  via vsock (0 on success, 7 on `exit 7` — never the 255 fallback, never a
  fake 0). Closes F-205/F-206 when green on ARM.

**Views (the O(1) materialization headline):**
- **`lightr-views` crate** — `ViewPlan` + `Solidifier` (promote-on-access:
  hot entries first, manifest order tiebreak, `is_fully_solid` only after all
  files confirmed) are pure and **fully host-tested**; composefs (Linux) /
  NFS-loopback (macOS, EdenFS-proven) backends are compile-only skeletons
  marked `// VIEW-PATH (S1/S3)` — runtime validation on a target box.

## [Unreleased] — Production hardening + VM foundation (2026-06-12)

379 tests, 0 failures, clippy -D clean. Two parallel tracks.

**Track A — toward shippable (local product):**
- **Registry pull hardened:** private-registry auth (`~/.docker/config.json`
  + `LIGHTR_REGISTRY_AUTH`), retry/backoff on 429/5xx (honors `Retry-After`),
  blobs streamed to disk (no OOM), typed HTTP status (`LightrError::Registry`
  — 401/403/404/429/5xx distinct, never collapsed to Io), host-arch image
  selection with fallback.
- **Crash durability:** every atomic write now `fsync`s the file and its
  parent directory; **gc takes an exclusive flock while writers take a shared
  one** — gc can no longer sweep an object a concurrent write is publishing.
- **CI:** `.github/workflows/ci.yml` — fmt/clippy -D/test + bench, honoring
  `rust-toolchain.toml` on GitHub runners (no founder-Mac proxy workaround).
- **Outward honesty:** README "Honest status" box + whitepaper §1 aspirational
  marker now match `parity-audit.md` (no fabricated `brew`/transcript).

**Track B — VM foundation (progresses on Intel; boot validated by S5/ARM):**
- **`lightr-init` crate** — the Linux guest PID1: real exit-code reporting
  through an `ExitSink` seam (host-tested), syscalls behind `cfg(linux)`.
- **The fake `exitCode = 0` is dead** — `vz` now returns the guest's real
  exit code via a vsock receiver (Rust owns the code; Swift shim returns only
  VM-lifecycle status; source-level invariant test proves no hardcoded path).
- **cpio pack assembly** — `assemble_pack(kernel, init, out)` builds a real
  initrd with `lightr-init` as `/init`, giving `engine install-pack` content.
- **Packaging prepared, license-gated:** `packaging/` (install.sh, brew
  formula, release.sh) — all fail loudly until ADR-0008 is Accepted.

## [Unreleased] — R1→R4 (2026-06-12): the full local product

338 tests, 0 failures, clippy -D clean, bench --check green. 9 crates.

**R1 — runtime parity + time axis + agent surface.** `ps/logs/stop/exec`
(daemonless supervisor: run owns its control socket, dies together), `gc`
mark/sweep (dry-run default, min-age), `-d` detach, `--mount` (CoW ref →
target), `undo`/`diff`/`bisect` over a reflog (the time axis), `plan`
dry-run, `--events` ndjson, **`lightr mcp`** (the runtime is an MCP server:
5 tools over JSON-RPC stdio). A9–A16.

**R2 — the Linux tier.** `oci import` (OCI layout + docker-save tar,
**sha256-verified fail-closed**, whiteouts/opaque/hardlink per spec) and
`oci pull` (registry, sha256-verified); `Engine` trait with `native` (real),
`ns` (Linux code, honest probe on macOS), `vz` (Swift shim + minimal kernel
behind a build feature, boot path for spike S5 — capability-probed, never a
silent skip); `run --engine/--rootfs`, `engine ls/install-pack`. A17–A21.

**R3 — ecosystem.** `lightr build` (Dockerfile-compatible, every step
content-keyed → Bazel-class incrementality on a plain Dockerfile),
`compose up/down` (lazy: listeners bound, services start on first connect,
ephemeral TTL supervisor), `docker` CLI compat (build/run/pull/images/ps/
compose; unsupported → exit 2, never silent). A22–A26.

**R4 — beyond + polish.** `run --deep-memo` (opt-in; honest probe + whole-run
fallback, no faked sub-memoization), `lightr schema` (versioned JSON Schema
per verb, machine-checked against real output), bench B9–B11 (oci-import,
build-cached **24.8 ms << build-cold 108.6 ms** — incrementality measured,
compose-up). `docs/spec/parity-audit.md` (every feature-tree F-id mapped to
status + evidence) + `json-schemas.md`. A27–A30.

## [Unreleased] — R0 "the warp core" (2026-06-11, overnight wave)

First working product: a 1.9 MB daemonless binary.

- `lightr snapshot/hydrate/status/run` — content-addressed workspace store
  (BLAKE3 file-level CAS, CoW clonefile ladder), git-style stat-index,
  memoized execution (exit-0-only, 5 MiB caps), `--json` + `--explain` on
  every verb, `hydrate --verify` paranoid path.
- `lightr bench [--check|--vs-docker|--json]` — the indicator table,
  measured on the user's machine; CI budget gate (all green on the Intel
  dev box; see spikes/RESULTS.md).
- Acceptance suite A1–A8 green end-to-end against the real binary
  (roundtrip, memo, fail-not-memoized, no-daemon, status, offline,
  integrity fail-closed a/b, agent JSON surface).
- Spec stack: whitepaper v2 (working backwards), feature tree F-001…F-605,
  ADRs 0001–0016, build-spec v2, decisions log (owner mandate + lead
  amendments), spike S4 results.
