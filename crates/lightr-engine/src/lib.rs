//! lightr-engine — frozen contract: build-spec-r2.md §2 (bodies: WP R2-W2).

use lightr_core::{LightrError, Result};
use std::path::{Path, PathBuf};

// ── EngineKind ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Native,
    Ns,
    Vz,
}

impl std::str::FromStr for EngineKind {
    type Err = LightrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "native" => Ok(EngineKind::Native),
            "ns" => Ok(EngineKind::Ns),
            "vz" => Ok(EngineKind::Vz),
            _ => Err(LightrError::InvalidRef(format!("unknown engine: {s}"))),
        }
    }
}

// ── EngineCaps ────────────────────────────────────────────────────────────────

pub struct EngineCaps {
    pub available: bool,
    pub detail: String,
}

// ── pack_dir helper ───────────────────────────────────────────────────────────

/// Returns the linux pack directory: $LIGHTR_LINUX_PACK, else $LIGHTR_HOME/packs/linux.
// Used by probe_vz (cfg macos+vz) and vz_impl — suppress dead_code on non-vz builds.
#[allow(dead_code)]
fn pack_dir() -> PathBuf {
    if let Ok(v) = std::env::var("LIGHTR_LINUX_PACK") {
        return PathBuf::from(v);
    }
    let base = std::env::var("LIGHTR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home().unwrap_or_else(|| PathBuf::from("/tmp/lightr")));
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
    }
}

/// Convenience alias for the CLI: `lightr engine ls` can call probe(Vz) via this.
pub fn pack_status() -> EngineCaps {
    probe(EngineKind::Vz)
}

#[cfg(target_os = "linux")]
fn probe_ns() -> EngineCaps {
    EngineCaps {
        available: true,
        detail: "linux namespaces".to_string(),
    }
}

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
    use std::ffi::CString;

    extern "C" {
        /// C ABI exposed by shim/vz.swift (compiled to static lib by build.rs).
        ///
        /// BOOT NOTE (S5 spike): this extern fn signature is frozen contract.
        /// The actual microVM boot has NOT been validated on Intel x86_64 —
        /// Apple's VZ save/restore is arm64-only; the boot path itself (cold
        /// kernel start) may work on x86 with a suitable kernel+initrd pack,
        /// but is only validated by the S5 owner spike when the pack exists.
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
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            let dir = pack_dir();
            let kernel = dir.join("kernel");
            let initrd = dir.join("initrd");
            let rootfs = spec.rootfs.ok_or_else(|| {
                LightrError::InvalidRef("vz engine requires a rootfs".to_string())
            })?;

            let kernel_c = path_to_cstr(&kernel)?;
            let initrd_c = path_to_cstr(&initrd)?;
            let rootfs_c = path_to_cstr(rootfs)?;
            // store path: not yet wired to a live store handle in R2;
            // pass empty string — the Swift shim treats "" as no store mount.
            let store_c = CString::new("").unwrap();

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

            // BOOT PATH (S5): see BOOT NOTE above — validated with a real pack.
            let rc = unsafe {
                lightr_vz_run(
                    kernel_c.as_ptr(),
                    initrd_c.as_ptr(),
                    rootfs_c.as_ptr(),
                    store_c.as_ptr(),
                    argv_ptrs.len() as libc::c_int - 1, // exclude null sentinel
                    argv_ptrs.as_ptr(),
                )
            };
            Ok(rc as i32)
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
}
