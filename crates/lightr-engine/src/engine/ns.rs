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
            // WP-NET-ISO: `--net=none` ⇒ create a network namespace (CLONE_NEWNET)
            // so the container gets an isolated, empty net stack (loopback only).
            let net_isolate = spec.net_isolate;

            // Fork so the child becomes PID 1 in the new PID namespace.
            // Safety: standard fork+exec pattern; we exec immediately in child.
            let pid = unsafe { libc::fork() };
            match pid {
                -1 => Err(LightrError::Io(std::io::Error::last_os_error())),
                0 => {
                    // ── child ──────────────────────────────────────────────
                    let rc =
                        run_in_namespaces(&rootfs_path, &cwd_str, &command, &limits, net_isolate);
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
        net_isolate: bool,
    ) -> i32 {
        // Capture the REAL outer uid/gid BEFORE unsharing the user namespace.
        // After unshare(CLONE_NEWUSER) and before a map is written, the process
        // has no mapping, so getuid()/getgid() return the overflow id (65534);
        // writing "0 65534 1" to uid_map is rejected by the kernel (the map must
        // target the writer's real id in the parent userns). Reading first fixes
        // the "uid/gid map failed" that broke the ns engine on Linux.
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        // unshare user+mount+pid namespaces. WP-NET-ISO: with `--net=none`, also
        // unshare the network namespace (CLONE_NEWNET) so the container starts
        // with an isolated, empty net stack (loopback only); host interfaces and
        // ports are invisible. When net_isolate=false the flags are byte-identical
        // to before (share host network — zero regression).
        let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID;
        if net_isolate {
            flags |= libc::CLONE_NEWNET;
        }
        if unsafe { libc::unshare(flags) } != 0 {
            eprintln!(
                "lightr-engine ns: unshare failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }

        // Map uid 0 inside → real outer uid (captured above)
        if write_map("/proc/self/uid_map", &format!("0 {} 1\n", uid)).is_err()
            || write_map("/proc/self/setgroups", "deny\n").is_err()
            || write_map("/proc/self/gid_map", &format!("0 {} 1\n", gid)).is_err()
        {
            eprintln!("lightr-engine ns: uid/gid map failed");
            return 1;
        }

        // F-203 (#90): apply cgroup v2 caps (memory.max / cpu.max / pids.max) HERE
        // — after the uid/gid map, but **before** the mount sequence + pivot_root.
        // The mount namespace is unshared yet still carries the host mounts, so the
        // host's cgroup-v2 hierarchy is still visible at `/sys/fs/cgroup`. After
        // pivot_root the root is the container rootfs whose `/sys/fs/cgroup` is an
        // empty dir, so apply_cgroup there always failed "cgroup v2 not mounted"
        // (the Linux-CI resource-limits job exposed this — the caps never actually
        // applied). cgroup membership survives the later mount-ns pivot, and exec
        // inherits it. Fail closed: any error returns 1.
        if let Err(e) = crate::limits::apply_cgroup(limits) {
            eprintln!("lightr-engine ns: apply_cgroup failed: {e}");
            return 1;
        }

        // WP-NET-ISO: with `--net=none` the new netns starts with `lo` DOWN, so
        // even loopback traffic fails. Bring `lo` up here — we hold CAP_NET_ADMIN
        // in the new userns, so this works rootless. MUST run AFTER the uid/gid
        // map is written (the cap is only effective once the map exists) and
        // BEFORE pivot_root/exec. Fail closed: any error returns 1.
        if net_isolate {
            if let Err(e) = bring_up_loopback() {
                eprintln!("lightr-engine ns: bring up loopback failed: {e}");
                return 1;
            }
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

    // WP-NET-ISO: bring the loopback interface up inside the new netns. Opens a
    // DGRAM socket, reads `lo`'s flags (SIOCGIFFLAGS), ORs in IFF_UP|IFF_RUNNING,
    // writes them back (SIOCSIFFLAGS), and closes the socket. Returns an honest
    // io::Error on any failure (the caller fails closed). We define a minimal
    // `ifreq` whose first union member is the 16-bit flags field — the only field
    // these two ioctls touch — laid out to match the C `struct ifreq`.
    fn bring_up_loopback() -> std::io::Result<()> {
        // `struct ifreq` on Linux: char ifr_name[IFNAMSIZ=16] followed by a union
        // whose largest member is 16 bytes (sockaddr/etc.). For the FLAGS ioctls
        // only `ifr_flags` (a `short` at the start of the union) is read/written.
        #[repr(C)]
        struct IfReq {
            ifr_name: [libc::c_char; libc::IFNAMSIZ],
            ifr_flags: libc::c_short,
            // Pad the union out to its full size so the struct matches the kernel's
            // `struct ifreq` layout (the ioctls copy a full ifreq in/out).
            _pad: [u8; 22],
        }

        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut req = IfReq {
            ifr_name: [0; libc::IFNAMSIZ],
            ifr_flags: 0,
            _pad: [0; 22],
        };
        // Copy "lo" into ifr_name (NUL-terminated; the array is zero-initialized).
        let lo = b"lo";
        for (i, &b) in lo.iter().enumerate() {
            req.ifr_name[i] = b as libc::c_char;
        }

        // SIOCGIFFLAGS: read current flags.
        if unsafe { libc::ioctl(fd, libc::SIOCGIFFLAGS, &mut req) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }

        // OR in IFF_UP | IFF_RUNNING.
        req.ifr_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;

        // SIOCSIFFLAGS: write them back.
        if unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS, &req) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }

        unsafe { libc::close(fd) };
        Ok(())
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
