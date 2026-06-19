//! Capability probes — side-effect-free availability checks per engine kind.

use super::kind::{EngineCaps, EngineKind};
use std::path::PathBuf;

// ── pack_dir helper ───────────────────────────────────────────────────────────

/// Returns the linux pack directory: $LIGHTR_LINUX_PACK, else
/// $LIGHTR_HOME/packs/linux, else ~/.lightr/packs/linux — the SAME root the
/// CLI's `lightr_home()` installs into, so install-pack (writer) and
/// probe_vz/vz_impl (readers) agree. (Bare $HOME mismatched ~/.lightr and hid
/// the installed pack — surfaced by the Intel vz boot bring-up.)
// Used by probe_vz (cfg macos+vz) and vz_impl — suppress dead_code on non-vz builds.
#[allow(dead_code)]
pub(crate) fn pack_dir() -> PathBuf {
    if let Ok(v) = std::env::var("LIGHTR_LINUX_PACK") {
        return PathBuf::from(v);
    }
    let base = std::env::var("LIGHTR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs_home()
                .map(|h| h.join(".lightr"))
                .unwrap_or_else(|| PathBuf::from("/tmp/lightr"))
        });
    base.join("packs").join("linux")
}

/// Minimal fallback for home dir — avoids pulling in a dep.
#[allow(dead_code)]
fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

// ── probe ─────────────────────────────────────────────────────────────────────

/// Probe WITHOUT side effects (build-spec-r2 §2).
pub fn probe(kind: EngineKind) -> EngineCaps {
    match kind {
        EngineKind::Native => EngineCaps {
            available: true,
            detail: "native process execution (no isolation — not a sandbox)".to_string(),
        },
        EngineKind::Ns => probe_ns(),
        EngineKind::Vz => probe_vz(),
        EngineKind::Wsl => probe_wsl(),
    }
}

/// Convenience alias for the CLI: `lightr engine ls` can call probe(Vz) via this.
pub fn pack_status() -> EngineCaps {
    probe(EngineKind::Vz)
}

#[cfg(target_os = "linux")]
fn probe_ns() -> EngineCaps {
    // ns runbook: this probe is a *compile-time* "we are on Linux" claim — it
    // does NOT prove the kernel grants unprivileged user namespaces (the
    // CLONE_NEWUSER unshare in `ns_impl::run_in_namespaces` can still EPERM on
    // hosts with `kernel.unprivileged_userns_clone=0`, inside restrictive
    // containers, or under a seccomp/AppArmor policy). The honest end-to-end
    // proof is runtime: the Linux CI/runbook MUST exercise a real isolated run
    // (e.g. `lightr run --engine ns` asserting a fresh PID namespace — PID 1
    // inside, distinct mount tree via pivot_root, uid 0↔caller uid map) and
    // assert correct exit-code/signal mapping. `unshare` failure surfaces as a
    // real non-zero error from `run`, never a fabricated success.
    EngineCaps {
        available: true,
        detail: "linux namespaces".to_string(),
    }
}

// Honest non-Linux arm: ns is a Linux-only model. We do NOT overclaim on macOS
// (or any non-Linux host) — namespaces don't exist there, so the probe reports
// unavailable with the host OS named, and `engine_for(Ns)` fails closed.
#[cfg(not(target_os = "linux"))]
fn probe_ns() -> EngineCaps {
    let os = std::env::consts::OS;
    EngineCaps {
        available: false,
        detail: format!("ns engine requires Linux (this host is {os})"),
    }
}

#[cfg(all(target_os = "macos", feature = "vz"))]
fn probe_vz() -> EngineCaps {
    let dir = pack_dir();
    let kernel = dir.join("kernel");
    let initrd = dir.join("initrd");
    match (kernel.exists(), initrd.exists()) {
        (true, true) => EngineCaps {
            available: true,
            detail: format!("vz engine ready (pack: {})", dir.display()),
        },
        (false, _) => EngineCaps {
            available: false,
            detail: format!(
                "vz engine: missing kernel at {} — run 'lightr engine install-pack <dir>'",
                kernel.display()
            ),
        },
        (true, false) => EngineCaps {
            available: false,
            detail: format!(
                "vz engine: missing initrd at {} — run 'lightr engine install-pack <dir>'",
                initrd.display()
            ),
        },
    }
}

#[cfg(not(all(target_os = "macos", feature = "vz")))]
fn probe_vz() -> EngineCaps {
    EngineCaps {
        available: false,
        detail: "vz engine requires macOS + the 'vz' build feature + a linux pack \
                 — see 'lightr engine install-pack'"
            .to_string(),
    }
}

// ── probe_wsl (Windows isolation = WSL2) ────────────────────────────────────

/// WSL2 probe (Windows). Honest, side-effect-free: detect that WSL2 exists and
/// has at least one distro by invoking `wsl.exe`. We never fake a success — if
/// WSL2 is absent we report unavailable with the install instruction.
///
/// "No daemon" still holds: the WSL2 utility VM is the OS's lightweight VM (a
/// Microsoft-managed Hyper-V utility VM, shared & on-demand), not a daemon we
/// run. We just exec into the user's default distro, the Windows analog of
/// `vz` booting a guest on macOS or `ns` unsharing namespaces on Linux.
#[cfg(target_os = "windows")]
fn probe_wsl() -> EngineCaps {
    // WIN-PATH: only meaningfully exercised on a real Windows box with WSL.
    // `wsl.exe -l -q` lists installed distros, one per line; a non-empty list
    // means WSL is installed *and* a distro is registered (so `wsl.exe -- …`
    // can actually run). We prefer this over `--status` because it tells us a
    // distro exists, not merely that the WSL feature is present.
    match wsl_list_distros() {
        Ok(distros) if !distros.is_empty() => EngineCaps {
            available: true,
            detail: format!(
                "WSL2 ready (default distro runs the ns model); distros: {}",
                distros.join(", ")
            ),
        },
        Ok(_) => EngineCaps {
            available: false,
            detail: "WSL2 has no distro registered — run `wsl --install` (or \
                     `wsl --install -d <distro>`) to add one"
                .to_string(),
        },
        Err(reason) => EngineCaps {
            available: false,
            detail: format!("WSL2 not installed/enabled — run `wsl --install` ({reason})"),
        },
    }
}

/// List registered WSL distros via `wsl.exe -l -q`. Returns the trimmed,
/// non-empty distro names, or an error string describing why detection failed.
///
/// WIN-PATH: validatable only on a real Windows box. `wsl.exe` emits UTF-16LE
/// on older builds; we decode leniently (strip NULs, then `from_utf8_lossy`)
/// so a `\0`-interleaved name still parses to its ASCII distro id.
#[cfg(target_os = "windows")]
fn wsl_list_distros() -> std::result::Result<Vec<String>, String> {
    let output = std::process::Command::new("wsl.exe")
        .args(["-l", "-q"])
        .output()
        .map_err(|e| format!("wsl.exe not found: {e}"))?;
    if !output.status.success() {
        return Err(format!("wsl.exe -l -q exited {:?}", output.status.code()));
    }
    let raw: Vec<u8> = output.stdout.into_iter().filter(|&b| b != 0).collect();
    let text = String::from_utf8_lossy(&raw);
    let distros: Vec<String> = text
        .lines()
        .map(|l| l.trim().trim_matches('\r').to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(distros)
}

/// Honest non-Windows arm: WSL2 is a Windows-only engine.
#[cfg(not(target_os = "windows"))]
fn probe_wsl() -> EngineCaps {
    let os = std::env::consts::OS;
    EngineCaps {
        available: false,
        detail: format!("wsl engine requires Windows + WSL2 (this host is {os})"),
    }
}
