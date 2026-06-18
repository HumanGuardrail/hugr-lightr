# build-spec-omni — full cross-platform delivery

**Frozen:** 2026-06-12 · **Lead:** TechLead · **Owner mandate (verbatim):**
"Fecha logo tudo irmão, windows, mac intel e mac silicon. Ai eu posso ate pedir
pra amigos ou provisionar maquinas. Mas tu tem que entregar o produto full."

The product ships on **every** desktop platform. Validation on hardware the lead
does not have is **not a blocker** — it is a one-command runbook the owner (or a
friend, or a provisioned box) triggers. The lead delivers: code-complete +
host-green + cross-compile-clean + per-platform runbook. No overclaim.

## Platform × engine matrix

| Platform | native core (CAS/memo/run/build) | isolation engine | CoW rung | validated by |
|---|---|---|---|---|
| macOS Intel x86_64 | ✅ here | **vz** (guest x86_64) | clonefile (APFS) | **lead, on this box** |
| macOS Apple Silicon arm64 | cross-build | vz (guest arm64) | clonefile (APFS) | runbook (ARM Mac) |
| Linux x86_64 + aarch64 | cross-build | **ns** (namespaces) | FICLONE / copy_file_range | CI + runbook (Linux) |
| Windows x86_64 | **NEW — native `.exe`** | **WSL2** (runs `ns` inside) | ReFS block-clone / copy | runbook (Windows) |

Future rings (named, not claimed): Hyper-V microVM (Windows vz-analog), fc/cloud,
Rosetta-in-VM (arm64), VZ save/restore (arm64), views runtime (S1/S3).

## Frozen invariants — bind EVERY WP (non-negotiable)

1. **Additive, behind cfg.** No existing unix/macos/linux path changes behavior.
   New code sits behind `#[cfg(windows)]`; existing `use std::os::unix::…`
   imports get `#[cfg(unix)]`. The unix build is untouched.
2. **Host stays green.** `cargo test -p <crate>` on this macOS host stays 100%
   green (the 403/0 invariant). A changed host test = the WP is wrong.
3. **Cross-check gate (objective DoD).** `cargo check --target
   x86_64-pc-windows-gnu -p <crate>` passes clean. This typechecks the whole
   Windows path with no Windows box. Done = this is green, not "looks done".
4. **windows-sys, target-gated.** FFI via `windows-sys` (already in
   `[workspace.dependencies]`). Each crate adds, in ITS OWN Cargo.toml:
   `[target.'cfg(windows)'.dependencies]` → `windows-sys.workspace = true`.
   Never a plain dependency (keeps it off unix builds).
5. **Honest markers, no fabrication.** A runtime path validatable only on a
   Windows/ARM box is marked `// WIN-PATH` (cf. `// BOOT-PATH`, `// VIEW-PATH`).
   Capability probes return an honest "unavailable + reason"; never a fake
   success, never a silent skip.
6. **Correctness path required; fast path best-effort.** Where a Windows
   fast-path is fiddly (ReFS block-clone), the **copy fallback is the
   required-correct path**; the fast-path must gracefully fall back, never
   hard-fail.
7. **Protocol stable, transport cfg-split.** Where IPC changes (unix domain
   socket → named pipe), the JSON wire protocol is unchanged; only transport is
   cfg-split.
8. **Symbol-anchored.** Contracts name function/type SYMBOLS (stable), not line
   numbers.

## WP table — disjoint by crate → fully parallel (worktree-isolated)

| WP | owner files (disjoint) | model | gate |
|---|---|---|---|
| WP-WIN-STORE | `crates/lightr-store/**` + `crates/lightr-index/**` | sonnet | host test -p (both) + win cross-check -p (both) |
| WP-WIN-RUN | `crates/lightr-run/**` | opus | host test -p + win cross-check -p |
| WP-WIN-FS | `crates/lightr-oci/**` + `crates/lightr-build/**` | sonnet | host test -p (both) + win cross-check -p (both) |
| WP-ENGINE-PLAT | `crates/lightr-engine/**` | opus | host test -p + win cross-check -p + linux/ns notes |
| WP-VIEWS-PLAT | `crates/lightr-views/**` | sonnet | host test -p + win cross-check -p |
| WP-ARM-RUNBOOK | `spikes/s5-vz-boot-arm64/**` (new) | sonnet | shellcheck + bash -n |
| WP-WSLRUNBOOK | `spikes/wsl-run/**` (new) | sonnet | bash -n (owner/HW-gated: requires Windows+WSL2) |

## Per-WP frozen contracts (código-âncora = symbols)

### WP-WIN-STORE — `lightr-store` + `lightr-index`
- **Locks:** `WriteGuard`/`GcGuard` drop paths use `libc::flock(LOCK_SH|LOCK_EX)`
  → `#[cfg(windows)]` `LockFileEx`/`UnlockFileEx` (exclusive =
  `LOCKFILE_EXCLUSIVE_LOCK`; shared = 0; non-blocking add
  `LOCKFILE_FAIL_IMMEDIATELY`). Handle via `AsRawHandle`.
- **Durability:** `fsync_dir` + `atomic_write`'s `sync_all` → `#[cfg(windows)]`
  `FlushFileBuffers` for files; **dir-fsync is a documented no-op on Windows**
  (NTFS has no portable directory fsync — state the weaker guarantee in a
  comment, do not pretend). Mirror in `lightr-index` (`fsync_dir`, pre-rename
  `sync_all`).
- **CoW:** `try_ladder_probe`/`try_cow_at_rung` — add a Windows rung: best-effort
  `DeviceIoControl(FSCTL_DUPLICATE_EXTENTS_TO_FILE, DUPLICATE_EXTENTS_DATA)` on
  ReFS, **graceful fall-through to `std::fs::copy`** (required path) on
  NTFS/failure.
- **Perms:** `PermissionsExt`/`from_mode` usage → `#[cfg(unix)]`; Windows skips
  mode bits.

### WP-WIN-RUN — `lightr-run` (the hard one)
- **Control socket → named pipe.** `ctl_sock_path`, `send_ctl_op`, `supervise`
  use `UnixListener`/`UnixStream`. cfg-split the **transport only**: Windows uses
  a named pipe (`\\.\pipe\lightr-<id>`, `CreateNamedPipeW` server /
  `CreateFileW` client). **JSON protocol unchanged.** `use std::os::unix::net::*`
  → `#[cfg(unix)]`.
- **Liveness:** `pid_alive` (`libc::kill(pid,0)`) → `#[cfg(windows)]`
  `OpenProcess` + `GetExitCodeProcess` (alive = `STILL_ACTIVE`).
- **Signals:** SIGTERM/SIGKILL via `libc::kill` → `#[cfg(windows)]`
  `TerminateProcess`.
- **Process group:** `pre_exec(setsid)` → `#[cfg(windows)]` no-op (job objects =
  future; mark `// WIN-PATH`).
- Exit-code mapping already has `#[cfg(unix)]`/`#[cfg(not(unix))]` — keep correct.

### WP-WIN-FS — `lightr-oci` + `lightr-build`
- **Symlinks:** `std::os::unix::fs::symlink` → `#[cfg(windows)]`
  `std::os::windows::fs::symlink_file` (note: needs Dev Mode/admin; mark
  `// WIN-PATH`, fall back to copy if symlink creation errors so import never
  hard-fails).
- **Perms/mode bits:** `PermissionsExt`/`from_mode`/`mode() & 0o777` →
  `#[cfg(unix)]`; Windows uses `set_readonly` semantics or skips.
- `pre_exec` in `lightr-build` → `#[cfg(unix)]` (Windows no-op).

### WP-ENGINE-PLAT — `lightr-engine`
- **Windows isolation = WSL2 engine.** New `#[cfg(windows)]` engine: probe for
  WSL2 (`wsl.exe --status` / distro presence); when present, run the workload via
  the `ns` model inside WSL2; when absent, **honest probe → unavailable with a
  reason** (install WSL2). Mark runtime path `// WIN-PATH`. Hyper-V microVM =
  future ring.
- **Linux ns harden.** `ns_impl`/`probe_ns` stay `#[cfg(target_os="linux")]`;
  tighten probe truthfulness; add notes the Linux runbook/CI will exercise.
- Keep `exit_code`/signal mapping correct across `cfg`.
- Engine registry: Windows selects `wsl` where macOS selects `vz`, Linux `ns`;
  `engine ls` shows honest per-platform availability.

### WP-VIEWS-PLAT — `lightr-views`
- Pure `ViewPlan`/`Solidifier` stay host-tested + portable.
- composefs (`#[cfg(target_os="linux")]`) / nfsloopback
  (`#[cfg(target_os="macos")]`) backends unchanged.
- Add a `#[cfg(target_os="windows")]` backend **skeleton** (ProjFS / ReFS
  overlay), compile-only, marked `// VIEW-PATH (S1/S3)` — no fake mount.

### WP-ARM-RUNBOOK — `spikes/s5-vz-boot-arm64/` (new dir, fully disjoint)
- Sibling of `spikes/s5-vz-boot/` but `--arch aarch64`. README (provision ARM
  Mac) + `run-s5-arm64.sh` (build `--features vz`, codesign w/ entitlement, build
  pack arm64, install-pack, `lightr run --engine vz alpine`, assert exit 0
  `s5-boot-ok` + exit 7, never 255). shellcheck + `bash -n` clean.

### WP-WSLRUNBOOK — `spikes/wsl-run/` (new dir, fully disjoint)
- Windows WSL2 engine press-go runbook. Owner/hardware-gated: requires Windows
  10/11 + WSL2 + a registered distro. `run-wsl.sh` (locate `lightr.exe`,
  `engine ls` must show `wsl available`, import Alpine, assert A1: `lightr run
  --engine wsl --rootfs alpine -- /bin/echo wsl-ok` → exit 0 + stdout `wsl-ok`
  + not 255; assert A2: `lightr run --engine wsl --rootfs alpine -- /bin/sh -c
  'exit 7'` → exit 7; print_summary). `bash -n` clean. WSL runbook now lives at
  `spikes/wsl-run/run-wsl.sh`.

## Lead-owned (NOT delegated)

Root `Cargo.toml` (windows-sys — done) · `.github/workflows/{ci,release}.yml`
(5-target matrix: macos x86_64/arm64, linux x86_64/aarch64, windows x86_64; zip
on Windows, tarball unix; checksums; signing gated) · `lightr-cli` touch-ups ·
`lightr-acceptance` test cfg-guards (symlink/perms test sites → `#[cfg(unix)]`) ·
workspace-wide windows cross-check · **vz entitlement + ad-hoc codesign**
(`packaging/vz.entitlements` + codesign step in run-s5/release — required by vz on
ANY Mac) · vz boot validation on Intel · ADRs · parity-audit · CHANGELOG · cold
critic.

## Merge order / DAG

All 6 WPs branch from the wave base (windows-sys + this spec committed) and run in
parallel worktrees. Each: edits disjoint files → host `cargo test -p` green → `cargo
check --target x86_64-pc-windows-gnu -p` clean → commit on `wp/<name>` → return a
card (branch, SHA, files, gate output, `// WIN-PATH` markers). Lead merges all six
(disjoint files → conflict-free) → adds acceptance/cli cfg-guards → workspace-wide
host gate (403/0) + workspace-wide windows cross-check → CI matrix → cold critic →
parity-audit/CHANGELOG/ADR → report.
