//! Container wait/readiness helpers — exit polling, exec-readiness gate, and the
//! start-failure persist, plus the resolv.conf synth + container-init detector.
//!
//! Extracted from `container.rs` (behavior-preserving split). `wait_until_exited`
//! is target-agnostic; `wait_exec_ready` / `persist_exec_failed` and the free fns
//! `synth_resolv_conf` / `pid_is_container_init` are `linux` only. Callers live in
//! `container.rs` (stop/start core flow, `container_pid1`) and `container_setup.rs`
//! (`build_ns_plan` uses `synth_resolv_conf`).

#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
use crate::util::now_nanos;
#[cfg(target_os = "linux")]
use crate::vocab::{BackendError, Result};
use crate::vocab::{ContainerId, ContainerState};
use crate::LightrBackend;

impl LightrBackend {
    /// Poll the cache until the container is no longer Running (its reaper has
    /// recorded the terminal state), or `timeout` elapses. The reaper owns the
    /// real exit code; `stop` only waits for it to land so the call is
    /// synchronous to the caller. Transcribed from the fake.
    pub(crate) fn wait_until_exited(&self, id: &ContainerId, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let cache = self.cache();
                match cache.containers.get(&id.0) {
                    Some(r) if r.state != ContainerState::Running => return true,
                    None => return true,
                    _ => {}
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // ── WP-#102: exec-readiness wait for the NS path (linux only) ─────────────

    /// Block on the exec-readiness pipe READ end `rd` until the container's PID 1
    /// `execv`'s, distinguishing three outcomes:
    ///   • EOF (`read` returns 0) ⇒ a SUCCESSFUL `execv` auto-closed the CLOEXEC
    ///     write end ⇒ the workload is running ⇒ `Ok(())` (caller persists `Running`).
    ///   • BYTES (`read` returns N) ⇒ the ns engine wrote an `execv`-failure message ⇒
    ///     reap the shim, persist `Exited`/exec-failed (message = the bytes), `Err`.
    ///   • TIMEOUT ⇒ `child.kill()` + best-effort `cgroup.kill` on the leaf, reap,
    ///     persist `Exited`/start-timeout, `Err`.
    /// Deadline = `LIGHTR_CRI_START_TIMEOUT_MS` (default 30000ms). `rd` is always
    /// closed before returning. Fail-closed: any non-EOF outcome fails the start so a
    /// container is NEVER reported `Running` unless its workload actually `execv`'d.
    #[cfg(target_os = "linux")]
    pub(crate) fn wait_exec_ready(
        &self,
        id: &ContainerId,
        child: &mut std::process::Child,
        rd: std::os::unix::io::RawFd,
        cgroup_name: &str,
    ) -> Result<()> {
        let timeout_ms: i64 = std::env::var("LIGHTR_CRI_START_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(30_000);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);

        // poll(rd, POLLIN) to the deadline, retrying EINTR with the remaining budget.
        let readable = loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                break false; // timed out
            }
            let remaining_ms = (deadline - now).as_millis().min(i32::MAX as u128) as libc::c_int;
            let mut pfd = libc::pollfd {
                fd: rd,
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, remaining_ms) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue; // interrupted — retry with the (shrunk) remaining budget
                }
                break false; // genuine poll error → handle as timeout (fail-closed)
            }
            if n == 0 {
                continue; // slice elapsed; the deadline check above ends the loop
            }
            break true; // POLLIN/POLLHUP/POLLERR — go read to classify
        };

        if readable {
            let mut buf = [0u8; 256];
            let n = unsafe { libc::read(rd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            unsafe { libc::close(rd) };
            if n == 0 {
                return Ok(()); // EOF ⇒ execv SUCCEEDED
            }
            let message = if n > 0 {
                String::from_utf8_lossy(&buf[..n as usize]).into_owned()
            } else {
                format!(
                    "read exec-readiness pipe: {}",
                    std::io::Error::last_os_error()
                )
            };
            let _ = child.wait(); // reap the shim
            self.persist_exec_failed(id, 127, "exec-failed", &message);
            return Err(BackendError::Internal(format!(
                "container {} failed to start (exec failed): {message}",
                id.0
            )));
        }

        // Timeout / poll error: tear the whole subtree down and record the failure.
        unsafe { libc::close(rd) };
        let _ = child.kill();
        if !cgroup_name.is_empty() {
            let leaf = std::path::Path::new("/sys/fs/cgroup").join(cgroup_name);
            Self::cgroup_force_kill(&leaf, &leaf.join("cgroup.kill"));
        }
        let _ = child.wait();
        let message = format!("container did not signal exec readiness within {timeout_ms}ms");
        self.persist_exec_failed(id, -1, "start-timeout", &message);
        Err(BackendError::Internal(format!(
            "container {} start timed out after {timeout_ms}ms",
            id.0
        )))
    }

    /// WP-#102: record a start-time terminal failure (exec-failed / start-timeout)
    /// onto the container record. Mirrors the spawn-failed persist; best-effort.
    #[cfg(target_os = "linux")]
    fn persist_exec_failed(&self, id: &ContainerId, exit_code: i32, reason: &str, message: &str) {
        let mut cache = self.cache();
        if let Some(entry) = cache.containers.get_mut(&id.0) {
            entry.state = ContainerState::Exited;
            entry.finished_at_nanos = now_nanos();
            entry.exit_code = exit_code;
            entry.reason = reason.to_string();
            entry.message = message.to_string();
            let snap = entry.clone();
            drop(cache);
            let _ = self.persist(&snap);
        }
    }
}

/// WP-#107 (CRI GAP 2, "DNS config"): synthesize `/etc/resolv.conf` content from a
/// CRI `DnsConfig`. Standard resolv.conf format — one `nameserver <s>` line per
/// server, a single `search <a b c>` line, a single `options <a b c>` line (what
/// Docker/runc write). Returns `None` when ALL three lists are empty (so a
/// `DnsConfig::default()` leaves the image's resolv.conf untouched rather than
/// truncating it to an empty file). The trailing newline keeps it a well-formed file.
#[cfg(target_os = "linux")]
pub(crate) fn synth_resolv_conf(dns: &crate::vocab::DnsConfig) -> Option<String> {
    if dns.servers.is_empty() && dns.searches.is_empty() && dns.options.is_empty() {
        return None;
    }
    let mut out = String::new();
    for s in &dns.servers {
        out.push_str("nameserver ");
        out.push_str(s);
        out.push('\n');
    }
    if !dns.searches.is_empty() {
        out.push_str("search ");
        out.push_str(&dns.searches.join(" "));
        out.push('\n');
    }
    if !dns.options.is_empty() {
        out.push_str("options ");
        out.push_str(&dns.options.join(" "));
        out.push('\n');
    }
    Some(out)
}

/// True iff host `pid`'s `/proc/<pid>/status` `NSpid:` line has innermost (last)
/// field == 1 — i.e. it is PID 1 inside its own pid namespace (the container init).
#[cfg(target_os = "linux")]
pub(crate) fn pid_is_container_init(pid: u32) -> bool {
    let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s,
        Err(_) => return false, // raced away — not it
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            // Fields are tab/space separated host→innermost; the LAST is the pid
            // in the deepest pid namespace. Setup has a single field (host only).
            if let Some(innermost) = rest.split_whitespace().next_back() {
                return innermost == "1";
            }
        }
    }
    false
}
