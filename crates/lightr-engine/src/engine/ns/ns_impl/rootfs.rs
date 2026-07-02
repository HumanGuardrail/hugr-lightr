//! ns_impl::rootfs — PID-1 rootfs file writers (/etc/resolv.conf, /etc/hostname,
//! /etc/hosts) and the extracted rootfs-setup + pivot phase
//! (`setup_rootfs_and_pivot`). All items live inside the Linux-gated `ns_impl`
//! module (see `ns/mod.rs`).

use super::mounts::{
    apply_bind_mount, mount_proc, remount_root_readonly, setup_minimal_dev, setup_shm, setup_tmpfs,
};
use super::signal::signal_setup_failed;
use crate::engine::spec::{BindMount, TmpfsMount};
use std::ffi::CString;

/// WP-#107 (CRI GAP 2/3): write `content` to `<rootfs>/<rel>` (e.g.
/// `etc/resolv.conf`, `etc/hostname`) from PID 1 BEFORE pivot_root. `mkdir -p`s
/// the parent (`etc/`) first — the CAS rootfs may lack it. Returns an honest
/// `io::Error` so the caller fails closed. Uses std file I/O, consistent with the
/// other PID-1 setup steps (`create_dir_all`, `symlink`); the child is
/// single-threaded post-fork.
pub(super) fn write_rootfs_file(
    rootfs: &std::path::Path,
    rel: &str,
    content: &[u8],
) -> std::io::Result<()> {
    let target = rootfs.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, content)
}

/// `--add-host`: APPEND `content` to `<rootfs>/<rel>` (e.g. `etc/hosts`) from PID
/// 1 BEFORE pivot_root, preserving any existing content (the image's
/// `127.0.0.1 localhost`). `mkdir -p`s the parent (`etc/`) and CREATEs the file
/// if missing — mirrors `write_rootfs_file` but opens append-or-create instead of
/// truncating. Returns an honest `io::Error` so the caller fails closed.
pub(super) fn append_rootfs_file(
    rootfs: &std::path::Path,
    rel: &str,
    content: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    let target = rootfs.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)?;
    f.write_all(content)
}

/// PID-1 rootfs setup + pivot phase, extracted verbatim from `run_in_namespaces`.
/// Runs the mount sequence — MS_PRIVATE, bind rootfs, fresh /proc, resolv.conf,
/// hostname (+ sethostname), /etc/hosts, CRI volume binds, the EARLY seccomp
/// COMPILE (while the host profile path is still visible pre-pivot), pivot_root,
/// chdir /, minimal /dev (+ devpts), /dev/shm, `--tmpfs`, put_old detach, and the
/// optional read-only remount — in the SAME order as before. We are PID 1
/// post-fork, so every failure signals the exec-readiness pipe with bytes and
/// `libc::_exit(1)`s INSIDE this fn (byte-identical fail-closed behavior; nothing
/// is converted to a `return`). On success returns the compiled seccomp filter
/// (the ONE local that crosses back to the caller for the LATE install), which
/// stays `None` for `None`/"unconfined".
#[allow(clippy::too_many_arguments)]
pub(super) fn setup_rootfs_and_pivot(
    rootfs: &std::path::Path,
    read_only: bool,
    shm_size: Option<u64>,
    tmpfs: &[TmpfsMount],
    bind_mounts: &[BindMount],
    resolv_conf: Option<&str>,
    hostname: Option<&str>,
    add_host: &[(String, String)],
    seccomp: Option<&str>,
    exec_ready_fd: Option<libc::c_int>,
) -> Option<super::SeccompFilter> {
    // Build the rootfs CString (rebuilt here; the setup-process copy is gone).
    let rootfs_c = match CString::new(rootfs.as_os_str().as_encoded_bytes()) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("lightr-engine ns: bad rootfs path");
            signal_setup_failed(exec_ready_fd, "bad rootfs path"); // WP-#104
            unsafe { libc::_exit(1) }
        }
    };
    let none = CString::new("none").unwrap();
    let empty = CString::new("").unwrap();

    // 1. Make root mount private
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
        signal_setup_failed(exec_ready_fd, "MS_PRIVATE on / failed"); // WP-#104
        unsafe { libc::_exit(1) };
    }

    // 2. Bind-mount rootfs onto itself so it becomes a mountpoint for pivot_root
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
        signal_setup_failed(exec_ready_fd, "bind-mount rootfs failed"); // WP-#104
        unsafe { libc::_exit(1) };
    }

    // 3. WP-#96: mount a FRESH procfs at <rootfs>/proc — BEFORE pivot_root,
    // while the host /proc is still fully-visible (the crux of the fix) and PID
    // 1 is in the new pid ns. Best-effort (log + continue), but it should now
    // succeed; the CI hard-requires it.
    mount_proc(&rootfs.join("proc"));

    // 3b. WP-#107 (CRI GAP 2, "DNS config"): write the synthesized
    // /etc/resolv.conf into the rootfs BEFORE pivot_root (Docker/runc do this),
    // overwriting whatever the image carried. We write through the
    // still-unpivoted rootfs path (`<rootfs>/etc/resolv.conf`). `None` ⇒ skip
    // entirely (image resolv.conf untouched). Fail-closed: a requested DNS
    // config that cannot be written is a real error (the kubelet asked for
    // specific resolvers — silently dropping them is a conformance lie).
    if let Some(content) = resolv_conf {
        if let Err(e) = write_rootfs_file(rootfs, "etc/resolv.conf", content.as_bytes()) {
            eprintln!("lightr-engine ns: write /etc/resolv.conf failed: {e}");
            signal_setup_failed(exec_ready_fd, "write /etc/resolv.conf failed"); // WP-#107
            unsafe { libc::_exit(1) };
        }
    }

    // 3c. WP-#107 (CRI GAP 3, "set hostname"): write /etc/hostname into the
    // rootfs BEFORE pivot_root (runc writes BOTH the file and calls
    // sethostname). The kernel-level `sethostname` is done just below (we are
    // in the new UTS ns from the unshare). `None` ⇒ skip. Fail-closed: a
    // requested hostname that cannot be written is a real error.
    if let Some(name) = hostname {
        let mut line = name.as_bytes().to_vec();
        line.push(b'\n');
        if let Err(e) = write_rootfs_file(rootfs, "etc/hostname", &line) {
            eprintln!("lightr-engine ns: write /etc/hostname failed: {e}");
            signal_setup_failed(exec_ready_fd, "write /etc/hostname failed"); // WP-#107
            unsafe { libc::_exit(1) };
        }
        // sethostname in the new UTS ns (we hold CAP_SYS_ADMIN in the userns).
        // Fail-closed: a requested hostname that cannot be set is a real error.
        let bytes = name.as_bytes();
        let r = unsafe { libc::sethostname(bytes.as_ptr() as *const libc::c_char, bytes.len()) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("lightr-engine ns: sethostname({name:?}) failed: {e}");
            signal_setup_failed(exec_ready_fd, "sethostname failed"); // WP-#107
            unsafe { libc::_exit(1) };
        }
    }

    // 3c2. `--add-host` (Docker parity): APPEND each `(hostname, ip)` entry to
    // <rootfs>/etc/hosts as a `"<ip>\t<hostname>"` line, BEFORE pivot_root
    // (Docker/runc write the container's /etc/hosts host-side). APPEND — we
    // preserve any /etc/hosts the image carried (e.g. the default
    // `127.0.0.1 localhost`); the file (and /etc) is created if missing.
    // Mirrors the resolv.conf/hostname writes above but uses an APPEND helper.
    // Empty ⇒ skip (image /etc/hosts untouched). Fail-closed: a requested
    // host mapping that cannot be written is a real error (silently dropping
    // it would be a parity lie).
    if !add_host.is_empty() {
        let mut block = Vec::new();
        for (host, ip) in add_host {
            block.extend_from_slice(ip.as_bytes());
            block.push(b'\t');
            block.extend_from_slice(host.as_bytes());
            block.push(b'\n');
        }
        if let Err(e) = append_rootfs_file(rootfs, "etc/hosts", &block) {
            eprintln!("lightr-engine ns: write /etc/hosts failed: {e}");
            signal_setup_failed(exec_ready_fd, "write /etc/hosts failed");
            unsafe { libc::_exit(1) };
        }
    }

    // 3d. WP-#107 (CRI GAP 1, "starting container with volume"): apply the CRI
    // volume bind mounts. MUST be done BEFORE pivot_root: the `host_path` is a
    // HOST path, only reachable while the host fs is still mounted — after
    // pivot_root + the put_old MNT_DETACH it is gone (the same reason the /dev
    // binds reference the old root pre-unmount). We bind into the rootfs at
    // `<rootfs>/<container_path>` (still the pre-pivot path), `mkdir -p`ing the
    // target first (the image may lack it). `host_path` is already realpath'd
    // host-side in build_ns_plan (the symlink-host-path spec). Fail-closed: a
    // volume that cannot be applied aborts the start (a missing volume is a real
    // error). Empty ⇒ skipped (unchanged behavior).
    for m in bind_mounts {
        if let Err(e) = apply_bind_mount(rootfs, m) {
            eprintln!(
                "lightr-engine ns: bind mount {:?} -> {:?} failed: {e}",
                m.host_path, m.container_path
            );
            signal_setup_failed(exec_ready_fd, "volume bind mount failed"); // WP-#107
            unsafe { libc::_exit(1) };
        }
    }

    // WP-#108 (seccomp), EARLY half: COMPILE the OCI seccomp profile NOW —
    // BEFORE pivot_root, while the HOST profile path is still reachable (after
    // the put_old MNT_DETACH it is gone, exactly like the CRI volume host_paths
    // above). The compiled cBPF program (a plain Vec) is held in a local and
    // INSTALLED late (after the apparmor apply, right before execv), so the
    // filter is armed last — never restricting PID 1's own rootfs setup. A
    // `Some("unconfined")` profile compiles to None (explicit no-op). Fail-
    // closed: an unreadable/unparseable/unsupported profile `_exit`s rather
    // than exec unfiltered (the same discipline as #106 AppArmor). `None` ⇒
    // byte-identical to the pre-#108 path.
    // `default` ⇒ the BUILT-IN curated allow-list (compile_default,
    // embedded). `unconfined` ⇒ no filter. Anything else is a PATH to an
    // OCI profile. `None` ⇒ byte-identical to the pre-#108 path.
    // seccomp COMPILE (host path visible, pre-pivot) → the filter carried to the
    // late install. x86_64-linux-only; fails closed on other arches (see seccomp_ns).
    let compiled_seccomp: Option<super::SeccompFilter> =
        super::seccomp_ns::compile_seccomp(seccomp, exec_ready_fd);

    // 4. Create put_old dir inside rootfs, then pivot_root
    let put_old = rootfs.join(".put_old");
    if std::fs::create_dir_all(&put_old).is_err() {
        eprintln!("lightr-engine ns: cannot create .put_old");
        signal_setup_failed(exec_ready_fd, "cannot create .put_old"); // WP-#104
        unsafe { libc::_exit(1) };
    }

    let put_old_c = match CString::new(put_old.as_os_str().as_encoded_bytes()) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("lightr-engine ns: bad put_old path");
            signal_setup_failed(exec_ready_fd, "bad put_old path"); // WP-#104
            unsafe { libc::_exit(1) }
        }
    };

    let r = unsafe { libc::syscall(libc::SYS_pivot_root, rootfs_c.as_ptr(), put_old_c.as_ptr()) };
    if r != 0 {
        eprintln!(
            "lightr-engine ns: pivot_root failed: {}",
            std::io::Error::last_os_error()
        );
        signal_setup_failed(exec_ready_fd, "pivot_root failed"); // WP-#104
        unsafe { libc::_exit(1) };
    }

    // chdir to new root
    if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
        eprintln!("lightr-engine ns: chdir / failed");
        signal_setup_failed(exec_ready_fd, "chdir / failed"); // WP-#104
        unsafe { libc::_exit(1) };
    }

    // #91: give the container a minimal /dev. The CAS-materialized rootfs has
    // an EMPTY /dev (snapshot carries file content, not device nodes), so
    // programs that need /dev/null (e.g. any shell job-control `cmd &`),
    // /dev/zero, /dev/urandom, … fail. Rootless cannot `mknod` (no CAP_MKNOD in
    // the init userns), so we BIND-mount the host's device nodes — still
    // reachable at /.put_old/dev/* until put_old is unmounted just below. A
    // tmpfs at /dev gives a clean, writable surface for the bind targets
    // without mutating the rootfs. Best-effort: a device we can't wire is
    // skipped (a missing optional node must not fail an otherwise-good run;
    // pre-#91 there was no /dev at all).
    setup_minimal_dev();

    // #92: mount a tmpfs at /dev/shm (Docker's POSIX shared-memory mount). The
    // CAS-materialized rootfs has none, so programs that need /dev/shm (e.g.
    // Python multiprocessing, many DB clients) otherwise fail. /dev is the
    // tmpfs from setup_minimal_dev, so the /dev/shm dir is created there. A
    // default 64 MiB mount (Docker's default) is best-effort; an EXPLICIT
    // `--shm-size` that cannot be applied is fail-closed (`_exit(1)`) — a
    // requested size silently dropped would be a parity lie.
    let shm_bytes = shm_size.unwrap_or(64 * 1024 * 1024);
    if let Err(e) = setup_shm(shm_bytes, shm_size.is_some()) {
        eprintln!("lightr-engine ns: /dev/shm mount failed: {e}");
        signal_setup_failed(exec_ready_fd, "/dev/shm mount failed"); // WP-#104
        unsafe { libc::_exit(1) };
    }

    // `--tmpfs` (Docker parity): mount a fresh tmpfs at each requested target.
    // Done AFTER /dev/shm and BEFORE the rootfs read-only remount, so each
    // tmpfs is an independent submount (the NON-recursive RO remount of `/`
    // leaves it writable — same property as /dev/shm). We are POST-pivot, so a
    // target is `/<target>` in the new root (mirrors the bind/shm targets).
    // `MS_NOSUID|MS_NODEV` matches Docker's `--tmpfs` default (exec ALLOWED — no
    // MS_NOEXEC). Fail-closed: a requested tmpfs that cannot be mounted
    // `_exit`s rather than exec without it. Empty ⇒ no-op (pre-feature path).
    for t in tmpfs {
        if let Err(e) = setup_tmpfs(t) {
            eprintln!("lightr-engine ns: tmpfs {:?} mount failed: {e}", t.target);
            signal_setup_failed(exec_ready_fd, "tmpfs mount failed");
            unsafe { libc::_exit(1) };
        }
    }

    // Unmount put_old (AFTER /dev binds + the proc mount are established;
    // MNT_DETACH is lazy so the already-bound mounts — including /proc — survive).
    let inner_put_old = CString::new("/.put_old").unwrap();
    let _ = unsafe { libc::umount2(inner_put_old.as_ptr(), libc::MNT_DETACH) };

    // #92: `--read-only` ⇒ remount the rootfs READ-ONLY. Done LAST — after the
    // /proc + /dev + /dev/shm mounts — and NON-recursively, so only the `/`
    // mount (the rootfs bind) flips to RO; the /proc + /dev + /dev/shm
    // SUBMOUNTS are independent mount points and keep their flags. Net effect:
    // rootfs immutable, /proc + /dev + /dev/shm intact (the key correctness
    // point — a container with a RO root still needs a live /proc + writable
    // shared memory). Fail-closed: if the remount fails we `_exit(1)` rather
    // than exec a writable root the user asked to be read-only.
    if read_only {
        if let Err(e) = remount_root_readonly() {
            eprintln!("lightr-engine ns: read-only remount failed: {e}");
            signal_setup_failed(exec_ready_fd, "read-only remount failed"); // WP-#104
            unsafe { libc::_exit(1) };
        }
    }

    compiled_seccomp
}
