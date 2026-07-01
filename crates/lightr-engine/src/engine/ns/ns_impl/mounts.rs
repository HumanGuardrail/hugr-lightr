//! ns_impl::mounts — the individual mount/fs leaf helpers used by PID-1 rootfs
//! setup: fresh /proc, minimal /dev (+ devpts), /dev/shm, `--tmpfs`, CRI volume
//! binds, the read-only remount, plus the netns join, loopback bring-up, and the
//! uid/gid-map writer. The pivot orchestration itself lives in `rootfs.rs`. All
//! items live inside the Linux-gated `ns_impl` module (see `ns/mod.rs`).

use crate::engine::spec::{BindMount, TmpfsMount};

pub(super) fn write_map(path: &str, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content.as_bytes())
}

/// WP-#99: open the pinned netns at `path` `O_RDONLY` and `setns` into it
/// (CLONE_NEWNET). MUST be called while still real root in the HOST init userns
/// (BEFORE `unshare(CLONE_NEWUSER)`) — see the call-site ordering note. Returns
/// an honest `io::Error` on open/setns failure so the caller fails closed.
pub(super) fn setns_netns(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().read(true).open(path)?;
    let rc = unsafe { libc::setns(f.as_raw_fd(), libc::CLONE_NEWNET) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(()) // `f` drops here, closing the fd after the successful setns.
}

// WP-NET-ISO: bring the loopback interface up inside the new netns. Opens a
// DGRAM socket, reads `lo`'s flags (SIOCGIFFLAGS), ORs in IFF_UP|IFF_RUNNING,
// writes them back (SIOCSIFFLAGS), and closes the socket. Returns an honest
// io::Error on any failure (the caller fails closed). We define a minimal
// `ifreq` whose first union member is the 16-bit flags field — the only field
// these two ioctls touch — laid out to match the C `struct ifreq`.
pub(super) fn bring_up_loopback() -> std::io::Result<()> {
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

/// WP-#96: mount a fresh `proc` at `target` (`<rootfs>/proc`) from PID 1, BEFORE
/// pivot_root. Two conditions make a rootless fresh procfs mount legal and correct,
/// and BOTH hold only at this call site. First, the host `/proc` (inherited via
/// CLONE_NEWNS) is STILL fully-visible, so the kernel's
/// `mount_too_revealing`/`fs_fully_visible` check passes (post-pivot it would be
/// gone → EPERM, the #95 bug this WP fixes). Second, the caller is PID 1 in the NEW
/// pid namespace, so the mounted proc reflects that ns (`cat /proc/self/status` ⇒
/// `Pid: 1`). Best-effort: mkdir the target first (the CAS rootfs may lack `/proc`);
/// if the mount fails we log + continue (it should now succeed; CI hard-requires
/// it). `MS_NOSUID|MS_NODEV|MS_NOEXEC` matches the runc/youki hardening for `/proc`.
pub(super) fn mount_proc(target: &std::path::Path) {
    use std::ffi::CString;
    let _ = std::fs::create_dir_all(target);
    let tgt = match CString::new(target.as_os_str().as_encoded_bytes()) {
        Ok(c) => c,
        Err(_) => return,
    };
    let (src, fstype) = (CString::new("proc").unwrap(), CString::new("proc").unwrap());
    let r = unsafe {
        libc::mount(
            src.as_ptr(),
            tgt.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        )
    };
    if r != 0 {
        eprintln!(
            "lightr-engine ns: /proc mount failed (continuing): {}",
            std::io::Error::last_os_error()
        );
    }
}

/// #91: populate the container's /dev with the standard device nodes by
/// BIND-mounting the host's (rootless cannot `mknod`). Called after pivot_root
/// + `chdir /` but BEFORE `/.put_old` is unmounted, while the host nodes are
/// still reachable at `/.put_old/dev/*`. A fresh tmpfs at /dev gives a clean,
/// writable surface for the bind targets without mutating the rootfs. Entirely
/// best-effort: any step that fails is skipped (a device we can't wire must not
/// fail an otherwise-good run — pre-#91 there was no /dev at all).
pub(super) fn setup_minimal_dev() {
    use std::ffi::CString;
    let _ = std::fs::create_dir_all("/dev");
    // Fresh tmpfs at /dev (tmpfs is mountable in a user namespace).
    if let (Ok(src), Ok(tgt), Ok(fstype), Ok(opts)) = (
        CString::new("tmpfs"),
        CString::new("/dev"),
        CString::new("tmpfs"),
        CString::new("mode=0755"),
    ) {
        unsafe {
            libc::mount(
                src.as_ptr(),
                tgt.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NOSUID,
                opts.as_ptr() as *const libc::c_void,
            );
        }
    }
    // Bind each standard node from the still-mounted old root.
    for name in ["null", "zero", "full", "random", "urandom", "tty"] {
        let src = format!("/.put_old/dev/{name}");
        let dst = format!("/dev/{name}");
        if std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&dst)
            .is_err()
        {
            continue;
        }
        if let (Ok(s), Ok(d)) = (CString::new(src), CString::new(dst)) {
            unsafe {
                libc::mount(
                    s.as_ptr(),
                    d.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                );
            }
        }
    }
    // #105: give the container a PRIVATE devpts at /dev/pts (Docker mounts one).
    // The /dev tmpfs above is writable, so mkdir the mountpoint, then mount a
    // `devpts` with `newinstance` — a private pts namespace, isolated from the
    // host's (and any other container's) ptys. `ptmxmode=0666` makes the ptmx
    // node world-rw so unprivileged programs can allocate a pty WITHOUT relying on
    // the `gid=5` (tty group) option: that gid does NOT map inside the container's
    // user namespace (rootless/userns), so passing it would EINVAL — we omit gid
    // by design and rely on ptmxmode. MS_NOSUID|MS_NOEXEC matches Docker/runc
    // hardening. Mountable here because PID 1 holds CAP_SYS_ADMIN in the new
    // user+mount ns. Entirely best-effort (eprintln on failure): #103 `exec -it`
    // already works via isatty WITHOUT a devpts, so a devpts that won't mount must
    // NOT kill the container — it only adds the /dev/pts NAME surface.
    let _ = std::fs::create_dir_all("/dev/pts");
    if let (Ok(src), Ok(tgt), Ok(fstype), Ok(opts)) = (
        CString::new("devpts"),
        CString::new("/dev/pts"),
        CString::new("devpts"),
        CString::new("newinstance,ptmxmode=0666"),
    ) {
        let r = unsafe {
            libc::mount(
                src.as_ptr(),
                tgt.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NOSUID | libc::MS_NOEXEC,
                opts.as_ptr() as *const libc::c_void,
            )
        };
        if r != 0 {
            eprintln!(
                "lightr-engine ns: /dev/pts devpts mount failed (continuing): {}",
                std::io::Error::last_os_error()
            );
        } else {
            // #105: /dev/ptmx → pts/ptmx (Docker's layout). With `newinstance` the
            // multiplexor lives at /dev/pts/ptmx; a relative symlink is the simplest
            // rootless-safe way to expose it at the conventional /dev/ptmx path.
            let _ = std::os::unix::fs::symlink("pts/ptmx", "/dev/ptmx");
        }
    }
    // Convenience symlinks programs expect (harmless if /proc isn't mounted —
    // creating the link always succeeds; only following it would need /proc).
    let _ = std::os::unix::fs::symlink("/proc/self/fd", "/dev/fd");
    let _ = std::os::unix::fs::symlink("/proc/self/fd/0", "/dev/stdin");
    let _ = std::os::unix::fs::symlink("/proc/self/fd/1", "/dev/stdout");
    let _ = std::os::unix::fs::symlink("/proc/self/fd/2", "/dev/stderr");
}

/// #92: mount a tmpfs at `/dev/shm` sized to `bytes` (`mode=1777`, Docker's
/// `/dev/shm`). `/dev` is the tmpfs from [`setup_minimal_dev`], so the dir is
/// created there first. `explicit` is true for a user `--shm-size`: such a
/// mount is fail-closed (an `Err` is returned, the run aborts) — a requested
/// size silently dropped is a parity lie. The default 64 MiB mount
/// (`explicit=false`) is best-effort: `/dev/shm` should always exist, but a
/// default that cannot mount must not fail an otherwise-good run.
pub(super) fn setup_shm(bytes: u64, explicit: bool) -> std::io::Result<()> {
    use std::ffi::CString;
    let _ = std::fs::create_dir_all("/dev/shm");
    let opts = format!("mode=1777,size={bytes}");
    let (src, tgt, fstype, opts_c) = match (
        CString::new("tmpfs"),
        CString::new("/dev/shm"),
        CString::new("tmpfs"),
        CString::new(opts),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        _ => return Err(std::io::Error::other("bad /dev/shm mount arg")),
    };
    let r = unsafe {
        libc::mount(
            src.as_ptr(),
            tgt.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            opts_c.as_ptr() as *const libc::c_void,
        )
    };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        if explicit {
            return Err(e);
        }
        // Best-effort default: note it and continue (no /dev/shm is degraded,
        // not fatal, for a run that did not request a specific size).
        eprintln!("lightr-engine ns: default /dev/shm tmpfs mount failed (continuing): {e}");
    }
    Ok(())
}

/// `--tmpfs` (Docker parity): mount a fresh tmpfs at `t.target` (POST-pivot, so
/// `/<target>` in the new root). Mirrors `setup_shm`'s `libc::mount` shape: same
/// `MS_NOSUID|MS_NODEV` flags (exec ALLOWED — Docker's `--tmpfs` default; NO
/// MS_NOEXEC) and the same `mode=...,size=...` option string (size omitted when
/// `None` ⇒ the kernel default). `mkdir -p`s the target first (the image may lack
/// it). Fail-closed: any error is returned so the caller `_exit`s (a requested
/// tmpfs silently dropped would be a parity lie).
pub(super) fn setup_tmpfs(t: &TmpfsMount) -> std::io::Result<()> {
    use std::ffi::CString;
    // Strip a leading '/' so a POST-pivot absolute target stays the in-root path
    // when joined to "/" (an empty/relative target falls back to the verbatim
    // value); the mount target itself is the absolute container path.
    let target = &t.target;
    std::fs::create_dir_all(target)?;
    // Mode is always present (defaulted to 1777 by the CLI); size only when set.
    let opts = match t.size {
        Some(bytes) => format!("mode={},size={}", t.mode, bytes),
        None => format!("mode={}", t.mode),
    };
    let (src, tgt, fstype, opts_c) = match (
        CString::new("tmpfs"),
        CString::new(target.as_bytes()),
        CString::new("tmpfs"),
        CString::new(opts),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        _ => return Err(std::io::Error::other("bad tmpfs mount arg")),
    };
    let r = unsafe {
        libc::mount(
            src.as_ptr(),
            tgt.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            opts_c.as_ptr() as *const libc::c_void,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// WP-#107 (CRI GAP 1): bind-mount one CRI volume into the rootfs BEFORE
/// pivot_root. The target is `<rootfs>/<container_path>` (leading `/` stripped so
/// `join` stays inside the rootfs); `mkdir -p` it first (the image may lack it).
/// The source `host_path` is already host-side realpath'd in build_ns_plan (the
/// symlink-host-path spec), so we bind it verbatim. `MS_BIND|MS_REC` mounts the
/// dir; when `readonly`, a second `MS_BIND|MS_REMOUNT|MS_RDONLY` makes it RO (the
/// canonical two-step). Returns an honest `io::Error` so the caller fails closed.
pub(super) fn apply_bind_mount(rootfs: &std::path::Path, m: &BindMount) -> std::io::Result<()> {
    use std::ffi::CString;
    // Strip a leading '/' so `container_path` joins INSIDE the rootfs.
    let rel = m.container_path.trim_start_matches('/');
    let target = rootfs.join(rel);
    std::fs::create_dir_all(&target)?;

    let src = CString::new(m.host_path.as_bytes())
        .map_err(|_| std::io::Error::other("bind mount host_path has interior NUL"))?;
    let tgt = CString::new(target.as_os_str().as_encoded_bytes())
        .map_err(|_| std::io::Error::other("bind mount target has interior NUL"))?;

    let r = unsafe {
        libc::mount(
            src.as_ptr(),
            tgt.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }

    if m.readonly {
        let r = unsafe {
            libc::mount(
                std::ptr::null(),
                tgt.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// #92: remount `/` (the pivoted rootfs bind) READ-ONLY. NON-recursive on
/// purpose: it flips ONLY the `/` mount, leaving the /dev + /dev/shm tmpfs
/// submounts (independent mount points) writable. `MS_BIND | MS_REMOUNT |
/// MS_RDONLY` is the canonical incantation to make a bind mount read-only; it
/// works rootless because we hold CAP_SYS_ADMIN in the new user+mount ns.
pub(super) fn remount_root_readonly() -> std::io::Result<()> {
    let r = unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
