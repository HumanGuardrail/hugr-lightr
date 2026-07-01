//! ns_impl::subid_ns — WP-#114: rootless subuid RANGE mapping for a real
//! non-root `--user`. The plan/handshake structs, the syntactic gate, the plan
//! builder, and the OUTSIDE-parent newuidmap/newgidmap dance. All items live
//! inside the Linux-gated `ns_impl` module (see `ns/mod.rs`).

use crate::engine::subid;

/// Everything the OUTSIDE parent (the shim) needs to map a subuid RANGE onto the
/// setup child via the setuid-root helpers. Computed pre-fork; `None` ⇒ no range
/// (the single-uid path runs).
pub(super) struct SubidPlan {
    pub(super) host_uid: u32,
    pub(super) host_gid: u32,
    pub(super) uid_range: subid::SubIdRange,
    pub(super) gid_range: subid::SubIdRange,
    pub(super) newuidmap: std::path::PathBuf,
    pub(super) newgidmap: std::path::PathBuf,
}

/// The two one-byte sync pipes for the dance, plus the plan. `ready_*` carries the
/// child's "userns created" signal (child WRITES, parent READS); `done_*` carries
/// the parent's "maps installed (0) / failed (1)" reply (parent WRITES, child READS).
pub(super) struct SubidSetup {
    pub(super) plan: SubidPlan,
    pub(super) ready_r: libc::c_int,
    pub(super) ready_w: libc::c_int,
    pub(super) done_r: libc::c_int,
    pub(super) done_w: libc::c_int,
}

/// Does this `--user` spec ask for a NON-root identity (so it needs a subuid
/// RANGE)? Syntactic — names other than `root` are assumed non-root (a name that
/// resolves to root still works on the range path, the drop is just a no-op). The
/// root forms (`0` / `root`, with an absent or root gid) stay on the byte-identical
/// single-uid path. A non-root gid alone (`0:5`) also needs the range.
pub(super) fn wants_subid_range(user: Option<&str>) -> bool {
    let s = match user {
        None => return false,
        Some(s) => s,
    };
    let (u, g) = match s.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (s, None),
    };
    let is_root = |p: &str| p == "0" || p == "root";
    let uid_root = is_root(u);
    // Absent gid follows the uid (numeric uid ⇒ gid 0; `root` ⇒ gid 0) ⇒ root iff
    // the uid is root. An explicit gid is judged on its own.
    let gid_root = match g {
        None => uid_root,
        Some(g) => is_root(g),
    };
    !(uid_root && gid_root)
}

/// Build the RANGE plan: the host uid/gid, their /etc/subuid + /etc/subgid
/// allocations, and the newuidmap/newgidmap helper paths. `None` if ANYTHING is
/// missing (no allocation, helpers not installed) ⇒ the caller falls back to the
/// single-uid path, where a non-root `--user` hits the #113 honest-error (no
/// silent root). Pure lookups (getuid/getpwuid + file reads); pre-fork.
pub(super) fn plan_subid_range() -> Option<SubidPlan> {
    let host_uid = unsafe { libc::getuid() } as u32;
    let host_gid = unsafe { libc::getgid() } as u32;
    let uname = host_username(host_uid)?;
    let uid_range = subid::lookup_subid("/etc/subuid", &uname, host_uid)?;
    let gid_range = subid::lookup_subid("/etc/subgid", &uname, host_gid)?;
    let newuidmap = subid::find_helper("newuidmap")?;
    let newgidmap = subid::find_helper("newgidmap")?;
    Some(SubidPlan {
        host_uid,
        host_gid,
        uid_range,
        gid_range,
        newuidmap,
        newgidmap,
    })
}

/// Resolve the calling uid to its login NAME via `getpwuid` (subid files are
/// usually keyed by name). `None` if the uid has no passwd entry.
pub(super) fn host_username(uid: u32) -> Option<String> {
    let pw = unsafe { libc::getpwuid(uid as libc::uid_t) };
    if pw.is_null() {
        return None;
    }
    let name = unsafe { (*pw).pw_name };
    if name.is_null() {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

/// The parent side of the dance: wait for the child's "userns created" signal,
/// install the RANGE maps via newuidmap/newgidmap, then ALWAYS reply on `done_w`
/// (status 0=ok / 1=fail) so the child never wedges. Closes both fds it owns.
pub(super) fn run_parent_subid_dance(
    child_pid: libc::pid_t,
    plan: &SubidPlan,
    ready_r: libc::c_int,
    done_w: libc::c_int,
) {
    let mut b = [0u8; 1];
    let n = unsafe { libc::read(ready_r, b.as_mut_ptr() as *mut libc::c_void, 1) };
    unsafe { libc::close(ready_r) };
    let ok = if n == 1 {
        let pid_s = child_pid.to_string();
        run_newidmap(&plan.newuidmap, &pid_s, plan.host_uid, plan.uid_range)
            && run_newidmap(&plan.newgidmap, &pid_s, plan.host_gid, plan.gid_range)
    } else {
        false
    };
    let status = [if ok { 0u8 } else { 1u8 }; 1];
    unsafe {
        libc::write(done_w, status.as_ptr() as *const libc::c_void, 1);
        libc::close(done_w);
    }
}

/// Run one helper: `newuidmap PID  0 <host_id> 1   1 <base> <count>` — i.e. map
/// container-root (intra 0) to the user's OWN id (always allowed, count 1), AND
/// container ids `1..=count` to the subordinate RANGE `base..`. Returns true on a
/// clean exit. The helper itself authorizes the range against /etc/sub{u,g}id.
fn run_newidmap(
    helper: &std::path::Path,
    pid_s: &str,
    host_id: u32,
    range: subid::SubIdRange,
) -> bool {
    use std::process::Command;
    matches!(
        Command::new(helper)
            .arg(pid_s)
            .args(["0", &host_id.to_string(), "1"])
            .args(["1", &range.base.to_string(), &range.count.to_string()])
            .status(),
        Ok(s) if s.success()
    )
}
