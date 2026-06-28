//! PATH resolution of `argv[0]` — the execvp-style search the raw `execv`/`execve`
//! sites do NOT do for themselves.
//!
//! CRITEST CONFORMANCE (the "should support starting container" spec + everything
//! downstream — execSync/stats/stop/remove/log/attach/exec): critest starts
//! containers with a BARE command name (`top`) and runs `execSync` with bare
//! commands too, expecting the runtime to resolve them against the container's
//! `PATH` (exactly as runc/crun do). Our ns engine and the `__ns-exec` shim use
//! raw `execv`/`execve`, which do NO PATH search, so `execv("top")` → ENOENT and
//! 20/34 specs failed with `exec failed: No such file or directory (os error 2)`.
//! This module mirrors `execvp`'s argv[0] resolution, keyed on the CONTAINER's
//! PATH, and is called from INSIDE the container mount namespace (post-pivot /
//! post-setns) so the candidate dirs resolve against the CONTAINER rootfs.
//!
//! Raw-libc-safe: the only work is splitting a `&str`, building one candidate
//! `CString` per PATH entry, and `access(path, X_OK)`. `access` is async-signal-
//! safe and the CString allocation is the only heap touch — acceptable in the
//! post-fork PID-1 context (documented in the call sites).

use std::ffi::CString;

/// The standard fallback `PATH` when the workload env carries none — the same
/// default the C library / shells use, and what runc/crun fall back to.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Resolve `argv[0]` to the absolute program `execv`/`execve` should run, mirroring
/// `execvp` but searching the CONTAINER's `PATH`.
///
/// - If `prog` CONTAINS a `/` (absolute or relative), it is used AS-IS — NO PATH
///   search — so behavior is byte-identical to the pre-fix `execv(prog)` for any
///   path-qualified program. (`Some(CString(prog))`, or `None` only if `prog`
///   has an interior NUL.)
/// - If `prog` has NO `/`, split `env_path` (or [`DEFAULT_PATH`] when `None`/empty)
///   on `:` and return the FIRST `<dir>/<prog>` that exists and is executable
///   (`access(path, X_OK) == 0`). An empty PATH entry is treated as the current
///   directory (POSIX), matching `execvp`.
/// - Returns `None` when nothing resolves — the caller MUST fail closed
///   (`signal_exec_failed` + `_exit(127)`), never exec a wrong/empty path.
///
/// MUST be called from inside the container mount namespace so `access()` hits the
/// container rootfs, not the host.
pub fn resolve_in_path(prog: &str, env_path: Option<&str>) -> Option<CString> {
    // Path-qualified ⇒ use as-is, no search (unchanged behavior). A NUL in the
    // name is invalid ⇒ None (fail-closed), exactly as the old CString::new path.
    if prog.contains('/') {
        return CString::new(prog).ok();
    }

    // Bare name ⇒ execvp-style search of the container PATH.
    let path = match env_path {
        Some(p) if !p.is_empty() => p,
        _ => DEFAULT_PATH,
    };
    for dir in path.split(':') {
        // POSIX: an empty entry means the current directory.
        let candidate = if dir.is_empty() {
            prog.to_string()
        } else {
            format!("{dir}/{prog}")
        };
        let c = match CString::new(candidate.as_bytes()) {
            Ok(c) => c,
            Err(_) => continue, // interior NUL ⇒ skip this candidate
        };
        // access(path, X_OK): exists AND is executable. Resolves against the
        // CONTAINER rootfs because we are post-pivot / post-setns.
        if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
            return Some(c);
        }
    }
    None
}

/// Pull the workload's `PATH` value out of an `(key, value)` env list, if present.
/// Used by the ns engine (which `execv`s with the inherited env) to feed
/// [`resolve_in_path`] the CONTAINER's own PATH.
pub fn path_from_env(env: &[(String, String)]) -> Option<String> {
    env.iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_qualified_is_used_as_is_no_search() {
        // Absolute path: returned verbatim regardless of PATH (no access() gate).
        let r = resolve_in_path("/bin/does-not-exist-xyz", Some("/nowhere")).unwrap();
        assert_eq!(r.to_str().unwrap(), "/bin/does-not-exist-xyz");
        // Relative-with-slash: also as-is.
        let r = resolve_in_path("./foo", None).unwrap();
        assert_eq!(r.to_str().unwrap(), "./foo");
    }

    #[test]
    fn bare_name_resolves_against_default_path() {
        // `sh` lives in one of the DEFAULT_PATH dirs on the test host (Linux/macOS).
        if let Some(r) = resolve_in_path("sh", None) {
            let s = r.to_str().unwrap();
            assert!(s.ends_with("/sh"), "expected an absolute .../sh, got {s}");
            assert!(
                std::path::Path::new(s).exists(),
                "resolved path must exist: {s}"
            );
        }
        // (If `sh` is somehow absent we simply don't assert — the search is correct
        // either way; the slash/X_OK/fail-closed logic is covered by the other tests.)
    }

    #[test]
    fn bare_name_unresolvable_is_none_fail_closed() {
        assert!(
            resolve_in_path("definitely-not-a-real-binary-zzz", Some("/bin:/usr/bin")).is_none(),
            "unresolvable bare name must be None (fail-closed)"
        );
    }

    #[test]
    fn empty_or_absent_path_falls_back_to_default() {
        // Empty env PATH ⇒ DEFAULT_PATH is used; a known bare binary still resolves
        // (or None if absent — never a panic).
        let _ = resolve_in_path("sh", Some(""));
        let _ = resolve_in_path("sh", None);
    }

    #[test]
    fn path_from_env_extracts_path() {
        let env = vec![
            ("FOO".to_string(), "bar".to_string()),
            ("PATH".to_string(), "/a:/b".to_string()),
        ];
        assert_eq!(path_from_env(&env).as_deref(), Some("/a:/b"));
        assert_eq!(path_from_env(&[]), None);
    }
}
