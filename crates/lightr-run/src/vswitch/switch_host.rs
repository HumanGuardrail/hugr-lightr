//! Cross-process switch lifecycle (WP-C9, ADR-0018) — productionizes the
//! `s6-xproc-switch` spike into the real `attach`/`detach` API the detached `vz`
//! supervisor calls.
//!
//! Production spawns each container as a SEPARATE detached supervisor (both
//! `run` and `compose` detach), so the per-network L2 [`super::VSwitch`] cannot
//! live inside any one supervisor — it must be a process they all ATTACH to. The
//! switch host is that process: a small re-exec of the current binary that wraps
//! one `VSwitch` + a `ctl.sock` accept loop, and self-stops when the last member
//! leaves. Each supervisor does:
//!
//! ```text
//!   socketpair(AF_UNIX, SOCK_DGRAM) → (guest_fd, host_fd)
//!   attach():  connect ctl.sock  ──or──  flock-elect → birth switch host
//!              send_fd(host_fd) + meta  (SCM_RIGHTS, one atomic message)
//!   keep guest_fd  → ExecSpec.net_fd  (the guest's mesh NIC, eth1)
//! ```
//!
//! ## The four S6 risks, addressed
//!
//! 1. **CMSG_SPACE not const (macOS EINVAL).** Reuses [`super::passfd`]'s runtime
//!    `CMSG_SPACE` fix verbatim — `send_fd`/`recv_fd` already get the controllen
//!    exactly right; this module just calls them.
//! 2. **Birth race (two supervisors start at once).** A `flock(switch.lock)`
//!    ELECTS exactly one birther; the loser falls through to connect the socket
//!    the winner just bound.
//! 3. **Refcount teardown (who stops the switch?).** The switch host SELF-watches
//!    the registry refcount under the registry lock and self-stops at 0 — no
//!    supervisor has to decide; `detach` just `registry.leave`s.
//! 4. **Stale ctl.sock (a crashed prior switch left the file).** `attach` tries
//!    connect FIRST; a refused connect (no live listener) → flock-elect → birth,
//!    which removes the stale node before binding. Connect-then-birth fallback.
//!
//! Unix-only: `socketpair`/`SCM_RIGHTS`/`flock` are POSIX, and the whole vz path
//! is unix. The module is `#[cfg(unix)]` at its `pub mod` site (`vswitch/mod.rs`)
//! so the windows `--all-targets` clippy gate never sees it (8a: honest cfg-out).

use crate::network::{Member, NetworkRegistry};
use crate::vswitch::passfd::{recv_fd, send_fd};
use crate::vswitch::VSwitch;
use std::io;
use std::net::Ipv4Addr;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The argv-0 marker a consumer (the CLI/NET3, or the integration example) keys
/// on to dispatch a re-exec into [`run_switch_host`]. `attach` spawns
/// `current_exe()` with `[SWITCH_HOST_ARGV, <home>, <network_id>]`; the binary's
/// entry must recognise it and call `run_switch_host(home, id)`.
pub const SWITCH_HOST_ARGV: &str = "__vswitch-host";

/// How long `attach` waits for a freshly-birthed switch host to bind its
/// `ctl.sock` before giving up (cold process spawn + bind).
const BIRTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll cadence for the switch host's refcount self-watch.
const REFCOUNT_POLL: Duration = Duration::from_millis(250);

/// `<home>/net/<id>` — the per-network dir (mirrors `NetworkRegistry`'s layout).
fn net_dir(home: &Path, network_id: &str) -> PathBuf {
    home.join("net").join(network_id)
}

fn ctl_sock_path(home: &Path, network_id: &str) -> PathBuf {
    net_dir(home, network_id).join("ctl.sock")
}

fn switch_lock_path(home: &Path, network_id: &str) -> PathBuf {
    net_dir(home, network_id).join("switch.lock")
}

// ── attach metadata: the member payload carried alongside the passed fd ───────
//
// Fixed layout: 6 MAC | 4 IP | 1 name-len | name bytes (matches the s6 spike,
// the proven wire shape). Sent in the SAME SCM_RIGHTS message as host_fd so the
// switch never sees a torn attach.

fn encode_meta(mac: [u8; 6], ip: Ipv4Addr, name: &str) -> Vec<u8> {
    let nb = name.as_bytes();
    let mut v = Vec::with_capacity(11 + nb.len());
    v.extend_from_slice(&mac);
    v.extend_from_slice(&ip.octets());
    v.push(nb.len() as u8);
    v.extend_from_slice(nb);
    v
}

fn decode_meta(buf: &[u8]) -> Option<([u8; 6], Ipv4Addr, String)> {
    if buf.len() < 11 {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&buf[0..6]);
    let ip = Ipv4Addr::new(buf[6], buf[7], buf[8], buf[9]);
    let nlen = buf[10] as usize;
    let name = String::from_utf8(buf.get(11..11 + nlen)?.to_vec()).ok()?;
    Some((mac, ip, name))
}

// ── flock election (S6 risk #2) ──────────────────────────────────────────────

/// Minimal RAII exclusive `flock` over `switch.lock`, used to elect the single
/// birther. (The `network` module's `FlockGuard` is private to that module; the
/// switch lock is a DISTINCT file with its own short-lived guard.)
struct ElectionLock {
    file: std::fs::File,
}

impl ElectionLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: `file` owns a valid fd for the lifetime of this guard.
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ElectionLock { file })
    }
}

impl Drop for ElectionLock {
    fn drop(&mut self) {
        // SAFETY: `file` owns the fd; release the advisory lock explicitly.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

// ── attach: connect-or-birth, then pass the host fd ──────────────────────────

/// Births-or-connects the per-network switch host and returns the GUEST end of a
/// fresh `socketpair` (for `ExecSpec.net_fd` — the mesh NIC `eth1`). The switch
/// host receives the HOST end via `SCM_RIGHTS` and calls
/// [`VSwitch::add_member`](super::VSwitch::add_member) with `member`'s
/// registry-assigned MAC/IP/name.
///
/// Connect-then-birth (S6 risks #2/#4): try the existing `ctl.sock` first; if no
/// live listener answers, take the `switch.lock` to elect a single birther,
/// re-try the connect under the lock (the winner may already be up), and only
/// then spawn the switch host and wait for its socket.
pub fn attach(home: &Path, network_id: &str, member: &Member) -> io::Result<OwnedFd> {
    std::fs::create_dir_all(net_dir(home, network_id))?;
    let ctl = ctl_sock_path(home, network_id);

    // Fresh guest NIC pair. Keep `guest`; hand `host` to the switch.
    let (guest, host) = UnixDatagram::pair()?;
    let meta = encode_meta(member.mac.0, member.ip, &member.name);

    // 1. Fast path: a live switch host is already serving — connect + pass.
    if let Ok(stream) = UnixStream::connect(&ctl) {
        return pass_and_ack(stream, host, &meta, guest);
    }

    // 2. No live listener. Elect the single birther under the switch lock.
    let _election = ElectionLock::acquire(&switch_lock_path(home, network_id))?;

    // 2a. Re-try the connect under the lock: another supervisor may have birthed
    //     the switch between our failed connect and acquiring the lock.
    if let Ok(stream) = UnixStream::connect(&ctl) {
        return pass_and_ack(stream, host, &meta, guest);
    }

    // 2b. We are the birther. Remove any stale socket node (S6 risk #4), spawn
    //     the switch host process, and wait for it to bind `ctl.sock`.
    let _ = std::fs::remove_file(&ctl);
    spawn_switch_host(home, network_id)?;

    let stream = connect_with_retry(&ctl, BIRTH_CONNECT_TIMEOUT)?;
    pass_and_ack(stream, host, &meta, guest)
}

/// One byte the switch host writes back AFTER `add_member` succeeds, so `attach`
/// returns only once the member's NIC is live in the switch (no caller-visible
/// race between attach returning and the switch consuming the passed fd).
const ATTACH_ACK: u8 = 0x01;

/// Send `host`'s fd + `meta` over `stream`, then block for the switch's
/// post-`add_member` ACK before handing back the `guest` end. Synchronizing on
/// the ACK is what makes membership ESTABLISHED on return — the supervisor can
/// boot the VM knowing eth1 is already wired into the switch. `host` is dropped
/// here (the switch owns its dup); a missing ACK fails closed.
fn pass_and_ack(
    mut stream: UnixStream,
    host: UnixDatagram,
    meta: &[u8],
    guest: UnixDatagram,
) -> io::Result<OwnedFd> {
    use std::io::Read;
    send_fd(&stream, host.as_raw_fd(), meta)?;
    drop(host); // the switch owns its own dup now
    stream.set_read_timeout(Some(BIRTH_CONNECT_TIMEOUT))?;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack)?;
    if ack[0] != ATTACH_ACK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "switch host returned a bad attach ack",
        ));
    }
    Ok(OwnedFd::from(guest))
}

/// Spawn the switch host as a DETACHED child process (a re-exec of the current
/// binary with [`SWITCH_HOST_ARGV`]), so it outlives the birthing supervisor and
/// every member can attach to it. `setsid` detaches it from our session, exactly
/// like the run supervisor (`launch_supervisor`).
fn spawn_switch_host(home: &Path, network_id: &str) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(SWITCH_HOST_ARGV)
        .arg(home)
        .arg(network_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setsid` in the child after fork detaches the switch host into its
    // own session so a signal to the launcher does not tear it down.
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

/// Connect to `ctl` retrying until `timeout` elapses (a freshly-spawned switch
/// host needs a moment to bind). Fails closed with `TimedOut` if it never binds.
fn connect_with_retry(ctl: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(ctl) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "switch host did not bind {} within {:?}: {e}",
                        ctl.display(),
                        timeout
                    ),
                ));
            }
        }
    }
}

// ── detach: leave the registry; the switch self-stops at refcount 0 ──────────

/// Remove `name` from the network registry (S6 risk #3). The switch host's own
/// refcount self-watch observes the empty membership and self-stops — no
/// supervisor decides the switch's lifetime. Idempotent: leaving an absent
/// member returns Ok.
pub fn detach(home: &Path, network_id: &str, name: &str) -> io::Result<()> {
    let reg = NetworkRegistry::open(home, &network_id.to_string())?;
    reg.leave(name)?;
    Ok(())
}

// ── run_switch_host: the switch process entrypoint ───────────────────────────

/// The switch host process body: open the network, start one [`VSwitch`], serve
/// `ctl.sock` (recv each member's passed fd + meta → `add_member`), and SELF-stop
/// when the registry refcount reaches 0 (S6 risk #3). Returns when the switch has
/// stopped cleanly (socket + threads reclaimed). A consumer's binary entry
/// dispatches here on [`SWITCH_HOST_ARGV`].
pub fn run_switch_host(home: &Path, network_id: &str) -> io::Result<()> {
    let id = network_id.to_string();
    let reg = NetworkRegistry::open(home, &id)?;
    let switch = Arc::new(VSwitch::start(&id, reg.subnet())?);

    let ctl = ctl_sock_path(home, network_id);
    let _ = std::fs::remove_file(&ctl);
    let listener = UnixListener::bind(&ctl)?;
    listener.set_nonblocking(true)?;

    let stop = Arc::new(AtomicBool::new(false));
    let acc_switch = Arc::clone(&switch);
    let acc_stop = Arc::clone(&stop);
    let accept = std::thread::Builder::new()
        .name(format!("vswitch-accept-{id}"))
        .spawn(move || accept_loop(&listener, &acc_switch, &acc_stop))?;

    // Refcount self-watch: poll members.json (under the registry lock). The
    // switch is born BEFORE the first member's attach completes, so wait for the
    // first member to appear, then stop once the count returns to 0.
    let mut seen_member = false;
    let watch_deadline = Instant::now() + BIRTH_CONNECT_TIMEOUT;
    loop {
        let count = reg.members().map(|m| m.len()).unwrap_or(0);
        if count > 0 {
            seen_member = true;
        } else if seen_member {
            break; // last member left → self-stop
        } else if Instant::now() >= watch_deadline {
            // No member ever attached (birther died before passing its fd). Stop
            // rather than linger as a daemon — "nothing runs when nothing runs".
            break;
        }
        std::thread::sleep(REFCOUNT_POLL);
    }

    // Stop: signal the accept loop, join it (drops its VSwitch clone), then take
    // the sole VSwitch and shut it down (joins every member thread). Remove the
    // socket so the next birth starts clean.
    stop.store(true, Ordering::SeqCst);
    let _ = accept.join();
    if let Ok(vswitch) = Arc::try_unwrap(switch) {
        vswitch.shutdown()?;
    }
    let _ = std::fs::remove_file(&ctl);
    Ok(())
}

/// Accept attach connections, recv the `SCM_RIGHTS` fd + meta, and call
/// [`VSwitch::add_member`](super::VSwitch::add_member) verbatim (the proven
/// path). The switch owns each passed fd from here. Exits when `stop` is set.
fn accept_loop(listener: &UnixListener, switch: &Arc<VSwitch>, stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((mut conn, _)) => {
                conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let (fd, meta) = match recv_fd(&conn) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some((mac, ip, name)) = decode_meta(&meta) else {
                    // SAFETY: close the orphaned fd so a bad attach cannot leak.
                    unsafe { libc::close(fd) };
                    continue;
                };
                // add_member takes ownership of `fd` (wraps it before spawning),
                // so even on a spawn failure there is nothing to close here. ACK
                // only on success, so the member's `attach` returns exactly when
                // its NIC is live (no caller-visible race).
                if switch.add_member(fd, mac, ip, &name).is_ok() {
                    use std::io::Write;
                    let _ = conn.write_all(&[ATTACH_ACK]);
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
#[path = "switch_host_tests.rs"]
mod tests;
