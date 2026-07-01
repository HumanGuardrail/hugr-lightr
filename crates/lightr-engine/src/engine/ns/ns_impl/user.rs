//! ns_impl::user — `--user` (uid/gid switch, Docker parity): resolve the spec
//! against the CONTAINER /etc/passwd|/etc/group and drop privilege in PID 1.
//! All items live inside the Linux-gated `ns_impl` module (see `ns/mod.rs`).

use super::signal::signal_setup_failed;

/// `--user`: drop the EXECing process to the requested uid/gid. Called post-fork,
/// AFTER caps/apparmor and BEFORE seccomp — at this point we are post-pivot (so
/// `/etc/passwd`/`/etc/group` are the CONTAINER files) and still hold
/// CAP_SETUID/CAP_SETGID from the userns baseline. `None` ⇒ no-op (byte-identical
/// to the pre-feature path). Like the other PID-1 setup steps this is FAIL-CLOSED:
/// an unresolvable/malformed spec or any failing setgroups/setgid/setuid signals
/// the exec-readiness pipe with bytes (so the kernel-closed fd is NOT misread as
/// EOF ⇒ a false `Running`) and `_exit(1)`s rather than exec with the WRONG identity
/// (running the workload as root when a non-root user was requested is a SECURITY
/// bug, worse than an error).
pub(super) fn apply_user_if_any(
    user: Option<&str>,
    exec_ready_fd: Option<libc::c_int>,
    use_range: bool,
) {
    let spec = match user {
        None => return,
        Some(s) => s,
    };
    let (uid, gid) = match resolve_user(spec) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("lightr-engine ns: --user: {e}");
            signal_setup_failed(exec_ready_fd, &format!("--user: {e}"));
            unsafe { libc::_exit(1) };
        }
    };
    // WP-#114: RANGE path — the outside parent installed a subuid RANGE map, so the
    // target uid/gid IS mapped. Drop privilege FOR REAL: setgroups([gid]) → setgid
    // → setuid (the canonical order: shed supplementary groups + gid while still
    // privileged, then uid last). Fail-closed at EVERY step — exec'ing with the
    // WRONG identity (e.g. still-root when a non-root user was asked for) is a
    // security bug, worse than an error. `setgroups` is usable because the range
    // path never wrote setgroups=deny (newgidmap wrote gid_map with privilege).
    // This also covers a root target on the range path (the calls are no-ops).
    if use_range {
        let groups = [gid as libc::gid_t];
        if unsafe { libc::setgroups(1, groups.as_ptr()) } != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("lightr-engine ns: --user: setgroups([{gid}]) failed: {e}");
            signal_setup_failed(exec_ready_fd, "--user: setgroups failed");
            unsafe { libc::_exit(1) };
        }
        if unsafe { libc::setgid(gid as libc::gid_t) } != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("lightr-engine ns: --user: setgid({gid}) failed: {e}");
            signal_setup_failed(exec_ready_fd, "--user: setgid failed");
            unsafe { libc::_exit(1) };
        }
        if unsafe { libc::setuid(uid as libc::uid_t) } != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("lightr-engine ns: --user: setuid({uid}) failed: {e}");
            signal_setup_failed(exec_ready_fd, "--user: setuid failed");
            unsafe { libc::_exit(1) };
        }
        return;
    }
    // v1 SCOPE (honest boundary): the ns userns uses a SINGLE-uid map
    // (`"0 <outer> 1"`) with `setgroups=deny` (see the uid_map/gid_map writes
    // above), so the ONLY identity that exists inside the container is
    // container-root (uid/gid 0). A switch to ANY other uid/gid would EPERM (the
    // target is unmapped, and setgroups is forbidden). Therefore:
    //   - `(0,0)` (root / `--user 0`) ⇒ NO-OP (already root; we must NOT call
    //     setgroups — it is denied — so just return).
    //   - any NON-root target ⇒ HONEST-ERROR (this REPLACES the prior SILENT
    //     run-as-root, a security footgun: a workload that asked for uid 1000 must
    //     never silently run as root). Real non-root support needs a rootless
    //     subuid RANGE mapping (newuidmap + /etc/subuid) so the target uid exists
    //     inside the userns — tracked as #115.
    if (uid, gid) == (0, 0) {
        return;
    }
    eprintln!(
        "lightr-engine ns: --user {spec:?} (uid={uid} gid={gid}): running as a \
         non-root in-container user is not yet supported on the rootless ns engine \
         (its single-uid userns maps only container-root; a subuid RANGE mapping is \
         required) — run as root, or use --engine native (host-mapped uid/gid)"
    );
    signal_setup_failed(
        exec_ready_fd,
        "--user: non-root uid/gid not supported on the ns engine (subuid range required)",
    );
    unsafe { libc::_exit(1) };
}

/// Resolve a `--user` spec (`uid[:gid]` or `name[:group]`) to numeric `(uid, gid)`
/// against the CONTAINER `/etc/passwd`/`/etc/group` (we are post-pivot, so the bare
/// paths are the container's). Resolution rules (Docker parity):
///   - uid part all-digits ⇒ parse u32; else a NAME ⇒ look it up in `/etc/passwd`
///     (`name:passwd:uid:gid:...`), taking the uid AND its primary gid.
///   - gid part present + all-digits ⇒ parse; present + a NAME ⇒ look it up in
///     `/etc/group` (`name:passwd:gid:...`).
///   - gid ABSENT: numeric uid ⇒ gid 0 (Docker: `--user 1000` ⇒ gid 0); NAME uid ⇒
///     the primary gid from that user's `/etc/passwd` entry.
/// Any unresolvable name / malformed value ⇒ `Err` (caller fails closed).
fn resolve_user(spec: &str) -> std::result::Result<(u32, u32), String> {
    if spec.is_empty() {
        return Err("empty user spec".to_string());
    }
    let (uid_part, gid_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };
    if uid_part.is_empty() {
        return Err(format!("malformed user spec {spec:?}"));
    }

    // Resolve the uid part. `passwd_gid` is the user's primary gid from
    // /etc/passwd, used only when the gid part is absent AND the uid was a name.
    let (uid, passwd_gid): (u32, Option<u32>) = if uid_part.bytes().all(|b| b.is_ascii_digit()) {
        let uid = uid_part
            .parse::<u32>()
            .map_err(|_| format!("invalid numeric uid {uid_part:?}"))?;
        (uid, None)
    } else {
        let (uid, pgid) = passwd_lookup(uid_part)
            .map_err(|e| format!("reading /etc/passwd: {e}"))?
            .ok_or_else(|| format!("user {uid_part:?} not found in container /etc/passwd"))?;
        (uid, Some(pgid))
    };

    // Resolve the gid part.
    let gid: u32 = match gid_part {
        Some(g) if g.is_empty() => return Err(format!("malformed user spec {spec:?}")),
        Some(g) if g.bytes().all(|b| b.is_ascii_digit()) => g
            .parse::<u32>()
            .map_err(|_| format!("invalid numeric gid {g:?}"))?,
        Some(g) => group_lookup(g)
            .map_err(|e| format!("reading /etc/group: {e}"))?
            .ok_or_else(|| format!("group {g:?} not found in container /etc/group"))?,
        // gid absent: numeric uid ⇒ 0 (Docker default); name uid ⇒ its primary gid.
        None => passwd_gid.unwrap_or(0),
    };

    Ok((uid, gid))
}

/// Look up a user NAME in the container `/etc/passwd`. Returns `(uid, primary_gid)`
/// or `None` if absent. Lines: `name:passwd:uid:gid:gecos:home:shell`. Malformed /
/// non-numeric id fields are skipped (lenient, matching libc getpwnam). A missing
/// file is an I/O error ⇒ fail-closed.
fn passwd_lookup(name: &str) -> std::io::Result<Option<(u32, u32)>> {
    let content = std::fs::read_to_string("/etc/passwd")?;
    for line in content.lines() {
        let mut f = line.split(':');
        if f.next() != Some(name) {
            continue;
        }
        let _passwd = f.next();
        let uid = match f.next().and_then(|s| s.parse::<u32>().ok()) {
            Some(u) => u,
            None => continue,
        };
        let gid = match f.next().and_then(|s| s.parse::<u32>().ok()) {
            Some(g) => g,
            None => continue,
        };
        return Ok(Some((uid, gid)));
    }
    Ok(None)
}

/// Look up a group NAME in the container `/etc/group`. Returns the gid or `None` if
/// absent. Lines: `name:passwd:gid:members`. A missing file is an I/O error ⇒
/// fail-closed.
fn group_lookup(name: &str) -> std::io::Result<Option<u32>> {
    let content = std::fs::read_to_string("/etc/group")?;
    for line in content.lines() {
        let mut f = line.split(':');
        if f.next() != Some(name) {
            continue;
        }
        let _passwd = f.next();
        if let Some(gid) = f.next().and_then(|s| s.parse::<u32>().ok()) {
            return Ok(Some(gid));
        }
    }
    Ok(None)
}
