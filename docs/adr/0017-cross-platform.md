# ADR-0017 — One product, every desktop: cross-platform engines + portability seams

- **Status:** Accepted (owner mandate 2026-06-12, verbatim: "Fecha logo tudo
  irmão, windows, mac intel e mac silicon … tu tem que entregar o produto
  full"). Runtime validation on non-host platforms is **runbook-gated, not
  blocking**.
- **Date:** 2026-06-12

One line: Lightr ships native on **macOS (Intel + Apple Silicon)**, **Linux
(x86_64 + aarch64)**, and **Windows (x86_64)** — the daemonless content/run core
is portable Rust behind `cfg` seams, each OS gets the lightest isolation it
natively offers (`vz` / `ns` / `wsl`), and foreign-hardware validation is a
one-command runbook the owner triggers, never claimed as run.

## Context
The prior framing wrongly treated `vz` as Apple-Silicon-only and gated the VM
tier on renting an ARM Mac. Correction (this ADR's trigger): Virtualization.
framework runs Linux guests on **Intel** Macs too — guest arch = host arch (it
is virtualization, not emulation; OrbStack/Lima/Docker-vz prove it daily). Only
VZ **save/restore** (F-406) and **Rosetta-in-VM** (F-208) are genuinely
arm64-only. Separately, the product was macOS+Linux-shaped with **zero**
`cfg(windows)` anywhere. The store/index/run/build/memo logic is OS-portable
Rust; the only OS-specific surfaces are file locking, durability, CoW, IPC,
permissions, symlinks, and process control — a bounded seam set.

## Decision
1. **Engine per platform (lightest native isolation):** macOS→`vz`
   (Virtualization.framework), Linux→`ns` (namespaces), Windows→`wsl` (run the
   `ns` model inside WSL2's OS-managed utility VM); `native` everywhere
   (reproducibility, **not** a sandbox — stated loudly). `EngineKind::
   platform_default()` selects per host; an honest capability probe reports
   unavailable with a reason ("WSL2 not installed/enabled — run `wsl --install`"),
   never a silent skip.
2. **"No daemon, ever" holds on Windows:** the WSL2 utility VM is the OS's, not
   ours — identical posture to `vz` leaning on Apple's framework. Lightr starts
   and owns no background service. Hyper-V microVM (the Windows `vz`-analog) is a
   named future ring, not built now.
3. **Portability seams behind `cfg`, additive (unix path untouched):**
   `flock`→`LockFileEx`/`UnlockFileEx`; file `fsync`→`FlushFileBuffers`,
   **dir-fsync→documented no-op** (NTFS has no portable directory fsync — the
   weaker guarantee is written down, not hidden); the CoW ladder gains a
   `RefsBlockClone` rung (`FSCTL_DUPLICATE_EXTENTS_TO_FILE` on ReFS) with the
   **copy fallback as the required-correct path**; unix domain socket→**named
   pipe** (JSON wire protocol unchanged, transport only); unix symlink→
   `symlink_file` with **copy fallback**; permission mode-bits→`#[cfg(unix)]`
   (Windows skips them).
4. **FFI via `windows-sys`, target-gated per crate** (`[target.'cfg(windows)'.
   dependencies]`) so it never enters unix builds; prefer std (`symlink_file`,
   `sync_all`) over raw FFI where it suffices.
5. **Distribution = 5-target matrix:** macOS arm64 + x86_64 (x86 cross-built on
   the arm64 runner), Linux x86_64 + aarch64 (cross-linked), Windows x86_64
   (`.zip`). Ad-hoc local signing with the **virtualization entitlement**
   (`packaging/vz.entitlements`) runs `vz` locally with no Apple account.
   Carrying that entitlement on a Developer-ID-**notarized** release is a
   *restricted* entitlement that Apple must provision for the team — to be
   validated when signing secrets are configured (NOT asserted as working).
6. **Honesty law extends:** runtime paths validatable only on a foreign host are
   marked `// WIN-PATH` (cf. `// BOOT-PATH`, `// VIEW-PATH`). Two objective gates
   bind every `cfg` port — the host stays green **and** `cargo check --target
   <triple>` is clean. What needs Windows/ARM hardware ships as a one-command
   runbook the owner triggers; nothing is claimed validated until that runbook is
   green.

## Consequences
One codebase, every desktop. `vz` boot is validated on the owner's **Intel** Mac
(F-205/F-206, spike S5 here) — not gated on renting ARM. arm64 `vz`, Windows
`wsl` run, Linux `ns` isolation, and ReFS block-clone are code-complete +
cross-compile-clean + runbook-packaged, to be validated on hardware the owner
provisions. Genuinely-arm64-only features (VZ save/restore F-406, Rosetta F-208)
remain future rings. Supersedes the "needs an ARM Mac to proceed" framing in
prior status notes. (ADR number note: 0017 taken by this concrete, shipping
work; the discussed "CRI-ready" ADR takes the next free number when authored.)
