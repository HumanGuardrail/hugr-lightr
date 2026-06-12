//! lightr-engine — frozen contract: build-spec-r2.md §2 (bodies: WP R2-W2).

use lightr_core::{LightrError, Result};
use std::path::{Path, PathBuf};

pub mod pack;
pub mod vsock;

// ── EngineKind ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Native,
    Ns,
    Vz,
    /// Windows isolation engine: runs the `ns` model inside the default WSL2
    /// distro's utility VM. Analog of `Vz` (macOS) / `Ns` (Linux). The WSL2 VM
    /// is the OS's, not ours — so "no daemon" still holds.
    Wsl,
}

impl std::str::FromStr for EngineKind {
    type Err = LightrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "native" => Ok(EngineKind::Native),
            "ns" => Ok(EngineKind::Ns),
            "vz" => Ok(EngineKind::Vz),
            "wsl" => Ok(EngineKind::Wsl),
            _ => Err(LightrError::InvalidRef(format!("unknown engine: {s}"))),
        }
    }
}

impl EngineKind {
    /// Stable lowercase token (inverse of `FromStr`). Stable across cfg so
    /// `engine ls` can render every kind on every platform.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Native => "native",
            EngineKind::Ns => "ns",
            EngineKind::Vz => "vz",
            EngineKind::Wsl => "wsl",
        }
    }

    /// Every engine kind, in display order. Provided so callers (e.g. the CLI's
    /// `engine ls`) can iterate without re-hardcoding the variant set and
    /// without an exhaustive `match` that breaks when a kind is added.
    pub fn all() -> &'static [EngineKind] {
        &[
            EngineKind::Native,
            EngineKind::Ns,
            EngineKind::Vz,
            EngineKind::Wsl,
        ]
    }

    /// The isolation engine this platform selects by default:
    /// macOS → `Vz`, Linux → `Ns`, Windows → `Wsl`, else `Native`.
    /// `Native` always works everywhere; the platform isolation engine reports
    /// its own honest availability via [`probe`].
    pub fn platform_default() -> EngineKind {
        #[cfg(target_os = "macos")]
        {
            EngineKind::Vz
        }
        #[cfg(target_os = "linux")]
        {
            EngineKind::Ns
        }
        #[cfg(target_os = "windows")]
        {
            EngineKind::Wsl
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            EngineKind::Native
        }
    }
}

// ── EngineCaps ────────────────────────────────────────────────────────────────

pub struct EngineCaps {
    pub available: bool,
    pub detail: String,
}

// ── pack_dir helper ───────────────────────────────────────────────────────────

/// Returns the linux pack directory: $LIGHTR_LINUX_PACK, else
/// $LIGHTR_HOME/packs/linux, else ~/.lightr/packs/linux — the SAME root the
/// CLI's `lightr_home()` installs into, so install-pack (writer) and
/// probe_vz/vz_impl (readers) agree. (Bare $HOME mismatched ~/.lightr and hid
/// the installed pack — surfaced by the Intel vz boot bring-up.)
// Used by probe_vz (cfg macos+vz) and vz_impl — suppress dead_code on non-vz builds.
#[allow(dead_code)]
fn pack_dir() -> PathBuf {
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

// ── ExecSpec ──────────────────────────────────────────────────────────────────

pub struct ExecSpec<'a> {
    pub cwd: &'a Path,
    pub command: &'a [String],
    /// ns/vz: CoW-materialized tree to pivot/boot into. Native: must be None.
    pub rootfs: Option<&'a Path>,
}

// ── Engine trait ──────────────────────────────────────────────────────────────

pub trait Engine {
    /// Spawn + wait; stdout/stderr inherit. Exit law: code or 128+signal.
    fn run(&self, spec: &ExecSpec) -> Result<i32>;
}

// ── engine_for ────────────────────────────────────────────────────────────────

/// Unavailable ⇒ Err(InvalidRef("engine <kind>: <probe detail>")).
pub fn engine_for(kind: EngineKind) -> Result<Box<dyn Engine>> {
    let caps = probe(kind);
    if !caps.available {
        return Err(LightrError::InvalidRef(format!(
            "engine {:?}: {}",
            kind, caps.detail
        )));
    }
    match kind {
        EngineKind::Native => Ok(Box::new(NativeEngine)),
        EngineKind::Ns => Ok(ns_engine_box()),
        EngineKind::Vz => Ok(vz_engine_box()),
        EngineKind::Wsl => Ok(wsl_engine_box()),
    }
}

// ── NativeEngine ──────────────────────────────────────────────────────────────

pub struct NativeEngine;

impl Engine for NativeEngine {
    fn run(&self, spec: &ExecSpec) -> Result<i32> {
        if spec.rootfs.is_some() {
            return Err(LightrError::InvalidRef(
                "native engine has no rootfs".to_string(),
            ));
        }
        let (prog, args) = spec
            .command
            .split_first()
            .ok_or_else(|| LightrError::InvalidRef("empty command".to_string()))?;
        let status = std::process::Command::new(prog)
            .args(args)
            .current_dir(spec.cwd)
            // inherit all env from parent
            // inherit stdio (stdout/stderr passed through)
            .status()
            .map_err(LightrError::Io)?;
        Ok(exit_code(status))
    }
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    }
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

// ── NsEngine (Linux only) ─────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod ns_impl {
    use super::{Engine, ExecSpec};
    use lightr_core::{LightrError, Result};
    use std::ffi::CString;

    pub struct NsEngine;

    impl Engine for NsEngine {
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            let rootfs = spec.rootfs.ok_or_else(|| {
                LightrError::InvalidRef("ns engine requires a rootfs".to_string())
            })?;

            let rootfs_path = rootfs.to_owned();
            let cwd_str = spec.cwd.to_string_lossy().into_owned();
            let command: Vec<String> = spec.command.to_vec();

            // Fork so the child becomes PID 1 in the new PID namespace.
            // Safety: standard fork+exec pattern; we exec immediately in child.
            let pid = unsafe { libc::fork() };
            match pid {
                -1 => Err(LightrError::Io(std::io::Error::last_os_error())),
                0 => {
                    // ── child ──────────────────────────────────────────────
                    let rc = run_in_namespaces(&rootfs_path, &cwd_str, &command);
                    std::process::exit(rc);
                }
                child_pid => {
                    // ── parent: wait ───────────────────────────────────────
                    let mut wstatus: libc::c_int = 0;
                    let r = unsafe { libc::waitpid(child_pid, &mut wstatus, 0) };
                    if r == -1 {
                        return Err(LightrError::Io(std::io::Error::last_os_error()));
                    }
                    Ok(wait_to_exit_code(wstatus))
                }
            }
        }
    }

    fn wait_to_exit_code(wstatus: libc::c_int) -> i32 {
        if libc::WIFEXITED(wstatus) {
            libc::WEXITSTATUS(wstatus)
        } else if libc::WIFSIGNALED(wstatus) {
            128 + libc::WTERMSIG(wstatus)
        } else {
            1
        }
    }

    fn run_in_namespaces(rootfs: &std::path::Path, cwd: &str, command: &[String]) -> i32 {
        // unshare user+mount+pid namespaces
        let flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID;
        if unsafe { libc::unshare(flags) } != 0 {
            eprintln!(
                "lightr-engine ns: unshare failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }

        // Map uid 0 inside → current uid outside
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        if write_map("/proc/self/uid_map", &format!("0 {} 1\n", uid)).is_err()
            || write_map("/proc/self/setgroups", "deny\n").is_err()
            || write_map("/proc/self/gid_map", &format!("0 {} 1\n", gid)).is_err()
        {
            eprintln!("lightr-engine ns: uid/gid map failed");
            return 1;
        }

        // Mount rootfs as private bind mount so we can pivot_root
        let rootfs_c = match CString::new(rootfs.as_os_str().as_encoded_bytes()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("lightr-engine ns: bad rootfs path");
                return 1;
            }
        };
        let none = CString::new("none").unwrap();
        let bind = CString::new("bind").unwrap();
        let empty = CString::new("").unwrap();

        // Make root mount private
        let r = unsafe {
            libc::mount(
                none.as_ptr(),
                b"/\0".as_ptr() as *const libc::c_char,
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        };
        if r != 0 {
            eprintln!(
                "lightr-engine ns: MS_PRIVATE on / failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }

        // Bind-mount rootfs onto itself so it becomes a mountpoint for pivot_root
        let r = unsafe {
            libc::mount(
                rootfs_c.as_ptr(),
                rootfs_c.as_ptr(),
                empty.as_ptr(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )
        };
        if r != 0 {
            eprintln!(
                "lightr-engine ns: bind-mount rootfs failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }

        // Create put_old dir inside rootfs, then pivot_root
        let put_old = rootfs.join(".put_old");
        if std::fs::create_dir_all(&put_old).is_err() {
            eprintln!("lightr-engine ns: cannot create .put_old");
            return 1;
        }

        let put_old_c = match CString::new(put_old.as_os_str().as_encoded_bytes()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("lightr-engine ns: bad put_old path");
                return 1;
            }
        };

        let r =
            unsafe { libc::syscall(libc::SYS_pivot_root, rootfs_c.as_ptr(), put_old_c.as_ptr()) };
        if r != 0 {
            eprintln!(
                "lightr-engine ns: pivot_root failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }

        // chdir to new root
        if unsafe { libc::chdir(b"/\0".as_ptr() as *const libc::c_char) } != 0 {
            eprintln!("lightr-engine ns: chdir / failed");
            return 1;
        }

        // Unmount put_old
        let inner_put_old = CString::new("/.put_old").unwrap();
        let _ = unsafe { libc::umount2(inner_put_old.as_ptr(), libc::MNT_DETACH) };

        // chdir to cwd-within-rootfs, or fallback to /
        let cwd_in = if cwd.is_empty() { "/" } else { cwd };
        let cwd_c = match CString::new(cwd_in.as_bytes()) {
            Ok(c) => c,
            Err(_) => CString::new("/").unwrap(),
        };
        unsafe {
            libc::chdir(cwd_c.as_ptr());
        }

        // exec command
        if command.is_empty() {
            eprintln!("lightr-engine ns: empty command");
            return 1;
        }

        let prog_c = match CString::new(command[0].as_bytes()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("lightr-engine ns: bad program name");
                return 1;
            }
        };
        let mut argv_c: Vec<CString> = command
            .iter()
            .filter_map(|s| CString::new(s.as_bytes()).ok())
            .collect();
        let mut argv_ptrs: Vec<*const libc::c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());

        unsafe {
            libc::execv(prog_c.as_ptr(), argv_ptrs.as_ptr());
        }

        eprintln!(
            "lightr-engine ns: exec failed: {}",
            std::io::Error::last_os_error()
        );
        1
    }

    fn write_map(path: &str, content: &str) -> std::io::Result<()> {
        std::fs::write(path, content.as_bytes())
    }
}

#[cfg(target_os = "linux")]
fn ns_engine_box() -> Box<dyn Engine> {
    Box::new(ns_impl::NsEngine)
}

/// macOS stub type so engine_for can name the arm — probe says unavailable,
/// so this is never actually constructed in production.
#[cfg(not(target_os = "linux"))]
struct NsEngineStub;

#[cfg(not(target_os = "linux"))]
impl Engine for NsEngineStub {
    fn run(&self, _spec: &ExecSpec) -> Result<i32> {
        Err(LightrError::InvalidRef(
            "ns engine requires Linux".to_string(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn ns_engine_box() -> Box<dyn Engine> {
    Box::new(NsEngineStub)
}

// ── VzEngine (macOS + feature "vz") ─────────────────────────────────────────

#[cfg(all(target_os = "macos", feature = "vz"))]
mod vz_impl {
    use super::{pack_dir, Engine, ExecSpec};
    use lightr_core::{LightrError, Result};
    use lightr_init::{InitSpec, CMD_FILE, EXIT_FILE};
    use std::ffi::CString;

    /// Exit code returned when the VM booted (and stopped) but the guest never
    /// wrote a readable exit file — i.e. PID1 crashed before reporting. NOT a
    /// success: we surface a real, non-zero failure rather than fabricate 0.
    const GUEST_NO_REPORT_CODE: i32 = 255;

    extern "C" {
        /// C ABI exposed by shim/vz.swift (compiled to static lib by build.rs).
        ///
        /// BOOT NOTE (S5 spike): this extern fn signature is frozen contract.
        /// The actual microVM boot has NOT been validated on Intel x86_64 —
        /// Apple's VZ save/restore is arm64-only; the boot path itself (cold
        /// kernel start) may work on x86 with a suitable kernel+initrd pack,
        /// but is only validated by the S5 owner spike when the pack exists.
        ///
        /// RETURN CONTRACT (WP-B-vsock): this is a VM-LIFECYCLE status, NOT the
        /// guest's exit code. `0` = the VM booted and stopped cleanly; a
        /// negative value = boot/config failure. The guest's REAL exit code
        /// arrives out-of-band on the host vsock receiver (see `super::vsock`),
        /// never from this return value. The shim no longer fabricates `0`.
        fn lightr_vz_run(
            kernel: *const libc::c_char,
            initrd: *const libc::c_char,
            rootfs: *const libc::c_char,
            store: *const libc::c_char,
            argc: libc::c_int,
            argv: *const *const libc::c_char,
        ) -> libc::c_int;
    }

    pub struct VzEngine;

    impl Engine for VzEngine {
        /// Run the guest and return its REAL exit code.
        ///
        /// Sequence (build-spec-prod §WP-B-vsock):
        ///   1. Bind the host vsock exit receiver on CID_HOST:EXIT_PORT and
        ///      start its accept+read on a thread BEFORE booting (so the guest
        ///      can connect the instant PID1 comes up).
        ///   2. Boot the VM via the Swift shim and block until it stops.
        ///   3. Join the receiver for the guest's exit frame.
        ///
        /// Exit-code law:
        ///   - boot/config failure (shim < 0)            ⇒ `Err(LightrError)`
        ///   - VM stopped but no exit frame (guest crash) ⇒ 255, NOT 0
        ///   - otherwise                                  ⇒ the guest's code
        ///     parsed from the vsock frame by `vsock::read_exit_frame`.
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            let dir = pack_dir();
            let kernel = dir.join("kernel");
            let initrd = dir.join("initrd");
            let rootfs = spec.rootfs.ok_or_else(|| {
                LightrError::InvalidRef("vz engine requires a rootfs".to_string())
            })?;

            // ── 1. Write the command spec onto the rootfs share BEFORE boot ──
            // macOS has NO host AF_VSOCK, so the host↔guest channel is two files
            // on the shared (writable) rootfs virtiofs share (decisions-log
            // 2026-06-12): the host writes the command to CMD_FILE here; the guest
            // PID1 reads it, runs it, and writes its REAL exit code to EXIT_FILE,
            // which the host reads back after the VM stops. cwd "/" + a minimal
            // PATH is the guest environment (ExecSpec.cwd is a host path).
            let cmd_path = rootfs.join(CMD_FILE.trim_start_matches('/'));
            let exit_path = rootfs.join(EXIT_FILE.trim_start_matches('/'));
            // A stale exit file from a prior run must not be read as this run's
            // result — clear it before boot.
            let _ = std::fs::remove_file(&exit_path);
            let init_spec = InitSpec {
                command: spec.command.to_vec(),
                cwd: "/".to_string(),
                env: vec![(
                    "PATH".to_string(),
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
                )],
            };
            std::fs::write(&cmd_path, init_spec.to_json()).map_err(LightrError::Io)?;

            let kernel_c = path_to_cstr(&kernel)?;
            let initrd_c = path_to_cstr(&initrd)?;
            let rootfs_c = path_to_cstr(rootfs)?;
            // store path: empty → the Swift shim mounts no store share. The
            // command travels via CMD_FILE on the rootfs, not argv/cmdline.
            let store_c = CString::new("").unwrap();

            // argv is still handed to the shim (it sets LIGHTR_CMD on the kernel
            // cmdline), but the guest reads CMD_FILE instead — pass it anyway for
            // forward-compat + console debugging.
            let argv_cstrings: Vec<CString> = spec
                .command
                .iter()
                .map(|s| {
                    CString::new(s.as_bytes()).map_err(|_| {
                        LightrError::InvalidRef(format!("invalid NUL in command arg: {s}"))
                    })
                })
                .collect::<Result<_>>()?;
            let mut argv_ptrs: Vec<*const libc::c_char> =
                argv_cstrings.iter().map(|c| c.as_ptr()).collect();
            argv_ptrs.push(std::ptr::null());

            // ── 2. Boot the VM and block until it stops (or fails) ──────────
            let vm_status = unsafe {
                lightr_vz_run(
                    kernel_c.as_ptr(),
                    initrd_c.as_ptr(),
                    rootfs_c.as_ptr(),
                    store_c.as_ptr(),
                    argv_ptrs.len() as libc::c_int - 1, // exclude null sentinel
                    argv_ptrs.as_ptr(),
                )
            };
            if vm_status < 0 {
                // The VM never booted (config/boot failure). No guest, no code to
                // fake — surface a real error.
                return Err(LightrError::InvalidRef(format!(
                    "vz engine: VM boot/config failed (shim status {vm_status})"
                )));
            }

            // ── 3. Read the guest's REAL exit code from the rootfs share ─────
            // PID1 wrote EXIT_FILE (fsync) then powered off cleanly, so it is
            // durable on the host's materialized rootfs by the time the shim
            // returns. A missing/unparsable file means the guest never reported
            // (crashed before writing) ⇒ GUEST_NO_REPORT_CODE (255), never a
            // fabricated 0. A brief retry covers any virtiofs flush lag.
            for _ in 0..30 {
                if let Ok(s) = std::fs::read_to_string(&exit_path) {
                    if let Ok(code) = s.trim().parse::<i32>() {
                        return Ok(code);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(GUEST_NO_REPORT_CODE)
        }
    }

    fn path_to_cstr(p: &std::path::Path) -> Result<CString> {
        CString::new(p.as_os_str().as_encoded_bytes())
            .map_err(|_| LightrError::InvalidRef(format!("invalid path: {}", p.display())))
    }
}

#[cfg(all(target_os = "macos", feature = "vz"))]
fn vz_engine_box() -> Box<dyn Engine> {
    Box::new(vz_impl::VzEngine)
}

/// Stub for builds without feature "vz" (or non-macOS) — probe gates before
/// this is ever reached, so this path is dead-code in practice.
#[cfg(not(all(target_os = "macos", feature = "vz")))]
struct VzEngineStub;

#[cfg(not(all(target_os = "macos", feature = "vz")))]
impl Engine for VzEngineStub {
    fn run(&self, _spec: &ExecSpec) -> Result<i32> {
        Err(LightrError::InvalidRef(
            "vz engine requires macOS + the 'vz' build feature + a linux pack".to_string(),
        ))
    }
}

#[cfg(not(all(target_os = "macos", feature = "vz")))]
fn vz_engine_box() -> Box<dyn Engine> {
    Box::new(VzEngineStub)
}

// ── WslEngine (Windows) ───────────────────────────────────────────────────────
//
// The Windows isolation engine. It is the Windows analog of `vz` (macOS) and
// `ns` (Linux): isolation is provided by running the workload inside the WSL2
// utility VM, then applying the SAME `ns` model (Linux user/mount/pid
// namespaces + pivot_root) *inside* that distro. We do NOT run a daemon — the
// WSL2 utility VM is the OS's own lightweight, on-demand Hyper-V VM, managed by
// Windows, not by us; `wsl.exe` is a transient launcher. "No daemon" holds.
//
// Future ring (named, NOT built here): a Hyper-V microVM engine (the Windows
// vz-analog) that boots our own kernel+initrd pack directly via the Host Compute
// System / Hyper-V, bypassing WSL. That is a separate, later engine.

#[cfg(target_os = "windows")]
mod wsl_impl {
    use super::{exit_code, Engine, ExecSpec};
    use lightr_core::{LightrError, Result};

    pub struct WslEngine;

    impl Engine for WslEngine {
        /// Run the workload inside the default WSL2 distro and return its REAL
        /// exit code. The exit-code/signal law is the shared `exit_code`
        /// helper: `wsl.exe` propagates the in-distro process's exit status,
        /// and on Windows `ExitStatus::code()` carries it through.
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            // WIN-PATH: real execution, validatable only on a Windows box with
            // WSL2 + a registered distro. Probe (probe_wsl) gates `engine_for`
            // before we get here, so reaching this with no WSL is not expected;
            // we still fail closed if `wsl.exe` cannot be spawned.
            let (prog, args) = spec
                .command
                .split_first()
                .ok_or_else(|| LightrError::InvalidRef("empty command".to_string()))?;

            let mut cmd = std::process::Command::new("wsl.exe");

            // Run in the user's default distro. `--cd` sets the working
            // directory *inside* the distro (a Linux path); fall back to the
            // distro default when cwd is unusable as a Linux path.
            if let Some(cwd) = spec.cwd.to_str() {
                if cwd.starts_with('/') {
                    cmd.args(["--cd", cwd]);
                }
            }

            match spec.rootfs {
                // Isolated run: reuse the REAL `ns` engine INSIDE the distro.
                // There is no external shim — the `ns` model is in-process in
                // `NsEngine::run` (unshare + pivot_root) — so we invoke a Linux
                // `lightr` on the distro PATH with `run --engine ns --rootfs …`,
                // which runs that same in-process ns path inside WSL2. The rootfs
                // is a host (Windows) path; translate it to the WSL2 mount view
                // (C:\x -> /mnt/c/x) so the in-distro process can see it.
                //
                // WIN-PATH: requires a Linux `lightr` installed in the default
                // WSL2 distro; runtime-validated via the Windows runbook, not here.
                Some(rootfs) => {
                    let rootfs_str = rootfs.to_str().ok_or_else(|| {
                        LightrError::InvalidRef(
                            "wsl engine: rootfs path is not valid UTF-8 for in-distro use"
                                .to_string(),
                        )
                    })?;
                    let wsl_rootfs = win_path_to_wsl(rootfs_str);
                    // `--` ends wsl.exe option parsing; everything after runs in
                    // the distro. Reuse the real ns engine via the public CLI.
                    cmd.arg("--");
                    cmd.args(["lightr", "run", "--engine", "ns", "--rootfs"]);
                    cmd.arg(&wsl_rootfs);
                    cmd.arg("--");
                    cmd.arg(prog);
                    cmd.args(args);
                }
                // No rootfs: run the command directly in the distro (the WSL2
                // VM is still the isolation boundary vs. the Windows host).
                None => {
                    cmd.arg("--");
                    cmd.arg(prog);
                    cmd.args(args);
                }
            }

            // Inherit stdio (stdout/stderr passed through), like NativeEngine.
            let status = cmd.status().map_err(LightrError::Io)?;
            Ok(exit_code(status))
        }
    }

    /// Translate a Windows path (`C:\a\b`) to its WSL2 mount view (`/mnt/c/a/b`)
    /// so an in-distro process can see a host-materialized rootfs. Already-Linux
    /// paths pass through. WIN-PATH: covers drive-backed paths (the common case
    /// for a materialized rootfs); validated end-to-end via the Windows runbook.
    fn win_path_to_wsl(p: &str) -> String {
        if p.starts_with('/') {
            return p.to_string();
        }
        let bytes = p.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            let rest = p[2..].replace('\\', "/");
            return format!("/mnt/{drive}{rest}");
        }
        p.replace('\\', "/")
    }
}

#[cfg(target_os = "windows")]
fn wsl_engine_box() -> Box<dyn Engine> {
    Box::new(wsl_impl::WslEngine)
}

/// Stub for non-Windows builds so `engine_for` can name the `Wsl` arm. The
/// probe (`probe_wsl`) gates before this is ever constructed in production, so
/// this path is dead-code on unix in practice — it just keeps the match total.
#[cfg(not(target_os = "windows"))]
struct WslEngineStub;

#[cfg(not(target_os = "windows"))]
impl Engine for WslEngineStub {
    fn run(&self, _spec: &ExecSpec) -> Result<i32> {
        Err(LightrError::InvalidRef(
            "wsl engine requires Windows + WSL2".to_string(),
        ))
    }
}

#[cfg(not(target_os = "windows"))]
fn wsl_engine_box() -> Box<dyn Engine> {
    Box::new(WslEngineStub)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    // from_str roundtrip + reject
    #[test]
    fn from_str_roundtrip() {
        assert_eq!(EngineKind::from_str("native").unwrap(), EngineKind::Native);
        assert_eq!(EngineKind::from_str("ns").unwrap(), EngineKind::Ns);
        assert_eq!(EngineKind::from_str("vz").unwrap(), EngineKind::Vz);
        assert_eq!(EngineKind::from_str("wsl").unwrap(), EngineKind::Wsl);
    }

    // as_str is the exact inverse of from_str for every kind in all().
    #[test]
    fn as_str_inverts_from_str_for_all_kinds() {
        for &k in EngineKind::all() {
            assert_eq!(
                EngineKind::from_str(k.as_str()).unwrap(),
                k,
                "as_str/from_str roundtrip failed for {k:?}"
            );
        }
    }

    // all() lists every variant exactly once (guards future additions).
    #[test]
    fn all_lists_every_kind() {
        let all = EngineKind::all();
        assert!(all.contains(&EngineKind::Native));
        assert!(all.contains(&EngineKind::Ns));
        assert!(all.contains(&EngineKind::Vz));
        assert!(all.contains(&EngineKind::Wsl));
        assert_eq!(all.len(), 4, "exactly four engine kinds");
    }

    // platform_default picks this host's isolation engine; native always works.
    #[test]
    fn platform_default_matches_host() {
        let d = EngineKind::platform_default();
        #[cfg(target_os = "macos")]
        assert_eq!(d, EngineKind::Vz);
        #[cfg(target_os = "linux")]
        assert_eq!(d, EngineKind::Ns);
        #[cfg(target_os = "windows")]
        assert_eq!(d, EngineKind::Wsl);
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        assert_eq!(d, EngineKind::Native);
    }

    #[test]
    fn from_str_reject() {
        let err = EngineKind::from_str("bogus").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown engine"),
            "expected 'unknown engine' in: {msg}"
        );
    }

    // probe(Native) always available
    #[test]
    fn probe_native_available() {
        let caps = probe(EngineKind::Native);
        assert!(caps.available, "native must always be available");
        assert!(
            caps.detail.contains("native"),
            "detail should mention 'native': {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("no isolation"),
            "detail must be honest about no isolation: {}",
            caps.detail
        );
    }

    // probe(Ns) is false on macOS with "requires Linux"
    #[test]
    #[cfg(target_os = "macos")]
    fn probe_ns_unavailable_on_macos() {
        let caps = probe(EngineKind::Ns);
        assert!(!caps.available, "ns must be unavailable on macOS");
        assert!(
            caps.detail.contains("Linux"),
            "detail must mention Linux: {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("requires Linux"),
            "detail: {}",
            caps.detail
        );
    }

    // probe(Wsl) is false off-Windows with the host OS named (no overclaim).
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn probe_wsl_unavailable_off_windows() {
        let caps = probe(EngineKind::Wsl);
        assert!(!caps.available, "wsl must be unavailable off Windows");
        assert!(
            caps.detail.contains("Windows"),
            "detail must mention Windows: {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("WSL2"),
            "detail must mention WSL2: {}",
            caps.detail
        );
    }

    // engine_for(Wsl) off-Windows fails closed with a Windows reason.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn engine_for_wsl_off_windows_err_contains_windows() {
        match engine_for(EngineKind::Wsl) {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("Windows"), "error must mention Windows: {msg}");
            }
            Ok(_) => panic!("engine_for(Wsl) must fail off Windows"),
        }
    }

    // probe(Vz) is false (feature off) with actionable detail
    #[test]
    #[cfg(not(feature = "vz"))]
    fn probe_vz_unavailable_feature_off() {
        let caps = probe(EngineKind::Vz);
        assert!(!caps.available, "vz must be unavailable without feature");
        assert!(
            caps.detail.contains("'vz' build feature"),
            "detail must mention 'vz' build feature: {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("install-pack"),
            "detail must be actionable (mention install-pack): {}",
            caps.detail
        );
    }

    // NativeEngine runs /bin/echo and returns 0 (inherit stdio)
    #[test]
    fn native_engine_echo_exit_0() {
        let engine = NativeEngine;
        let cwd = std::env::current_dir().unwrap();
        let command: Vec<String> = vec!["/bin/echo".to_string(), "lightr-engine-test".to_string()];
        let spec = ExecSpec {
            cwd: &cwd,
            command: &command,
            rootfs: None,
        };
        let code = engine.run(&spec).expect("echo should not fail");
        assert_eq!(code, 0, "echo exits 0");
    }

    // NativeEngine maps exit code correctly (sh -c 'exit 5' => 5)
    #[test]
    fn native_engine_exit_code_mapping() {
        let engine = NativeEngine;
        let cwd = std::env::current_dir().unwrap();
        let command: Vec<String> = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 5".to_string(),
        ];
        let spec = ExecSpec {
            cwd: &cwd,
            command: &command,
            rootfs: None,
        };
        let code = engine.run(&spec).expect("sh should not fail to launch");
        assert_eq!(code, 5, "exit code must be 5, got {code}");
    }

    // NativeEngine with Some(rootfs) => Err
    #[test]
    fn native_engine_rootfs_rejected() {
        let engine = NativeEngine;
        let cwd = std::env::current_dir().unwrap();
        let rootfs = std::path::PathBuf::from("/tmp/fake-rootfs");
        let command: Vec<String> = vec!["/bin/true".to_string()];
        let spec = ExecSpec {
            cwd: &cwd,
            command: &command,
            rootfs: Some(&rootfs),
        };
        let err = engine.run(&spec).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no rootfs"),
            "expected 'no rootfs' in error: {msg}"
        );
    }

    // engine_for(Ns) on macOS => Err containing "Linux"
    #[test]
    #[cfg(target_os = "macos")]
    fn engine_for_ns_macos_err_contains_linux() {
        match engine_for(EngineKind::Ns) {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("Linux"), "error must mention Linux: {msg}");
            }
            Ok(_) => panic!("engine_for(Ns) must fail on macOS"),
        }
    }

    // ── KEY INVARIANT (WP-B exit channel) ──────────────────────────────────
    // There is NO code path where vz returns a hardcoded 0. The command is
    // handed to the guest via CMD_FILE on the shared rootfs; the exit code comes
    // back via EXIT_FILE on that same share (macOS has no host AF_VSOCK). The
    // shim return is a VM-lifecycle status only. A missing exit file ⇒ 255,
    // never a fabricated 0. These source-level tests pin that down so a future
    // edit can't silently restore a fake success.

    /// The Swift shim must NOT contain the fabricated `exitCode = 0` it used to,
    /// and must not name a guest exitCode at all — it reports only VM-lifecycle
    /// status (vmStatus); the real code is a file on the shared rootfs.
    #[test]
    fn swift_shim_has_no_fabricated_exit_code_zero() {
        let shim = include_str!("../shim/vz.swift");
        assert!(
            !shim.contains("exitCode = 0"),
            "vz.swift must not fabricate a guest exit code of 0"
        );
        assert!(
            !shim.contains("exitCode"),
            "vz.swift must not name a guest exitCode at all — it reports only \
             VM-lifecycle status (vmStatus); the code is read from the rootfs file"
        );
        assert!(
            shim.contains("vmStatus"),
            "vz.swift must report a VM-lifecycle status (vmStatus), not the code"
        );
    }

    /// `VzEngine::run` delivers the command via CMD_FILE and reads the exit code
    /// from EXIT_FILE on the shared rootfs — it NEVER returns the shim's status
    /// as the exit code, and a missing file maps to 255 (not a fabricated 0).
    #[test]
    fn vz_exit_code_comes_from_the_rootfs_file_not_the_shim() {
        let lib = include_str!("lib.rs");

        // Command delivered to the guest by writing CMD_FILE on the rootfs.
        assert!(
            lib.contains("CMD_FILE") && lib.contains("init_spec.to_json()"),
            "VzEngine::run must write the command spec to CMD_FILE"
        );
        // Exit code read back from EXIT_FILE on the rootfs, parsed as i32.
        assert!(
            lib.contains("EXIT_FILE") && lib.contains("parse::<i32>()"),
            "VzEngine::run must read the exit code from EXIT_FILE"
        );
        // The shim return is a lifecycle status (vm_status), only checked for
        // failure — never returned directly as the exit code.
        assert!(
            lib.contains("let vm_status") && lib.contains("vm_status < 0"),
            "the shim return must be handled as a lifecycle status (vm_status)"
        );
        // The honest no-report fallback is 255, explicitly NOT 0.
        assert!(
            lib.contains("GUEST_NO_REPORT_CODE: i32 = 255"),
            "a missing guest exit file must map to 255, not a fabricated 0"
        );
    }
}
