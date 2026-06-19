//! NsEngine — Linux user+mount+pid namespace isolation via unshare + pivot_root.

use super::spec::ExecSpec;
use super::Engine;
// Used only by the non-Linux stub below; the Linux `ns_impl` has its own import.
#[cfg(not(target_os = "linux"))]
use lightr_core::{LightrError, Result};

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
            let limits = spec.limits;

            // Fork so the child becomes PID 1 in the new PID namespace.
            // Safety: standard fork+exec pattern; we exec immediately in child.
            let pid = unsafe { libc::fork() };
            match pid {
                -1 => Err(LightrError::Io(std::io::Error::last_os_error())),
                0 => {
                    // ── child ──────────────────────────────────────────────
                    let rc = run_in_namespaces(&rootfs_path, &cwd_str, &command, &limits);
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

    fn run_in_namespaces(
        rootfs: &std::path::Path,
        cwd: &str,
        command: &[String],
        limits: &lightr_core::ResourceLimits,
    ) -> i32 {
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
        let empty = CString::new("").unwrap();

        // Make root mount private
        let r = unsafe {
            libc::mount(
                none.as_ptr(),
                c"/".as_ptr(),
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
        if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
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

        // F-203: apply cgroup v2 caps before exec. A0 stub is Ok(()); WP-A1 fills
        // it (honest Unsupported if cgroup v2 is unavailable / not delegated).
        if let Err(e) = crate::limits::apply_cgroup(limits) {
            eprintln!("lightr-engine ns: apply_cgroup failed: {e}");
            return 1;
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
        let argv_c: Vec<CString> = command
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
pub(super) fn ns_engine_box() -> Box<dyn Engine> {
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
pub(super) fn ns_engine_box() -> Box<dyn Engine> {
    Box::new(NsEngineStub)
}
