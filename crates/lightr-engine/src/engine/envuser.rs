//! WP-IMG-ENVUSER — apply an `ExecSpec`'s `env` + `user` to a native child
//! `Command`, Docker-faithful.
//!
//! These are the RUNTIME apply of an image's recorded `ENV`/`USER` (recorded by
//! the build into `.lightr-image.json` via DF-IMGCFG, merged with the CLI's
//! `-e`/`-u` overrides by the run handler before construction). The engine is the
//! single place a process is spawned, so it is where the values take effect.
//!
//! - **`apply_env`**: the spec `env` (already `image ENV < CLI -e`-merged by the
//!   caller — CLI keys win) is added ON TOP of the inherited parent env. An
//!   EMPTY env slice is a NO-OP (the child inherits exactly the parent env, as
//!   before this WP — behavior-preserving for images with no `ENV` and runs with
//!   no `-e`). `std::process::Command::envs` overlays each pair onto the inherited
//!   set, matching Docker (image/CLI env augments, it does not clear the base).
//!
//! - **`apply_user`** (cfg(unix)): the spec `user` (`image USER < CLI -u`-merged
//!   by the caller) sets the child uid/gid. `None` is a NO-OP (the child runs as
//!   the current user — behavior-preserving). `Some(spec)` is `uid[:gid]`
//!   (numeric) or `name[:group]`; a non-numeric name resolves against the host
//!   `/etc/passwd`/`/etc/group` (cfg(unix)). Setting a uid different from the
//!   current process needs root; lightr native is not a root daemon, so the
//!   kernel's `EPERM` surfaces HONESTLY at spawn (never silently ignored). On
//!   non-unix a POSIX uid has no meaning ⇒ `Some(_)` is an honest error.

use lightr_core::{LightrError, Result};
use std::process::Command;

/// Overlay the resolved `env` pairs onto the child's inherited environment.
/// Empty ⇒ no-op (child inherits the parent env unchanged — behavior-preserving).
pub(crate) fn apply_env(cmd: &mut Command, env: &[(String, String)]) {
    if env.is_empty() {
        return;
    }
    cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
}

/// Apply the resolved `user` spec to the native child uid/gid before exec.
/// `None` ⇒ no-op (current user). See module docs for the full contract.
pub(crate) fn apply_user(cmd: &mut Command, user: Option<&str>) -> Result<()> {
    let spec = match user {
        None => return Ok(()),
        Some("") => {
            return Err(LightrError::InvalidRef(
                "invalid user value: empty".to_string(),
            ))
        }
        Some(s) => s,
    };

    let (uid_part, gid_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let uid = resolve_uid(uid_part)?;
    let gid = match gid_part {
        Some(g) => Some(resolve_gid(g)?),
        None => None,
    };

    apply_ids(cmd, uid, gid)
}

#[cfg(unix)]
fn apply_ids(cmd: &mut Command, uid: u32, gid: Option<u32>) -> Result<()> {
    use std::os::unix::process::CommandExt;
    cmd.uid(uid);
    if let Some(gid) = gid {
        cmd.gid(gid);
    }
    // EPERM (setting a different uid as non-root) surfaces at spawn as an honest
    // io::Error — lightr native is not a root daemon.
    Ok(())
}

#[cfg(not(unix))]
fn apply_ids(cmd: &mut Command, uid: u32, gid: Option<u32>) -> Result<()> {
    // POSIX uid/gid has no meaning on windows. Consume the bindings (no unused-
    // var on the windows clippy gate) and fail HONESTLY.
    let _ = (cmd, uid, gid);
    Err(LightrError::InvalidRef(
        "USER (POSIX uid/gid) is not supported on this host".to_string(),
    ))
}

/// Resolve a uid component: numeric ⇒ that id; a name ⇒ looked up in the host
/// `/etc/passwd` (cfg(unix)). On non-unix a non-numeric name is an honest error.
fn resolve_uid(part: &str) -> Result<u32> {
    if let Ok(n) = part.parse::<u32>() {
        return Ok(n);
    }
    resolve_name_uid(part)
}

/// Resolve a gid component: numeric ⇒ that id; a name ⇒ looked up in the host
/// `/etc/group` (cfg(unix)). On non-unix a non-numeric name is an honest error.
fn resolve_gid(part: &str) -> Result<u32> {
    if let Ok(n) = part.parse::<u32>() {
        return Ok(n);
    }
    resolve_name_gid(part)
}

#[cfg(unix)]
fn resolve_name_uid(name: &str) -> Result<u32> {
    let body = std::fs::read_to_string("/etc/passwd").map_err(|e| {
        LightrError::InvalidRef(format!("USER {name:?}: cannot read /etc/passwd: {e}"))
    })?;
    parse_passwd_id(&body, name).ok_or_else(|| {
        LightrError::InvalidRef(format!("USER {name:?}: no such user in /etc/passwd"))
    })
}

#[cfg(unix)]
fn resolve_name_gid(name: &str) -> Result<u32> {
    let body = std::fs::read_to_string("/etc/group").map_err(|e| {
        LightrError::InvalidRef(format!("USER group {name:?}: cannot read /etc/group: {e}"))
    })?;
    parse_passwd_id(&body, name).ok_or_else(|| {
        LightrError::InvalidRef(format!("USER group {name:?}: no such group in /etc/group"))
    })
}

#[cfg(not(unix))]
fn resolve_name_uid(name: &str) -> Result<u32> {
    Err(LightrError::InvalidRef(format!(
        "USER {name:?}: name resolution (/etc/passwd) is unix-only"
    )))
}

#[cfg(not(unix))]
fn resolve_name_gid(name: &str) -> Result<u32> {
    Err(LightrError::InvalidRef(format!(
        "USER group {name:?}: name resolution (/etc/group) is unix-only"
    )))
}

/// Parse a colon-delimited `passwd`/`group` body for `name`, returning its
/// numeric id (3rd field — uid in passwd, gid in group). The 2 files share the
/// `name:x:ID:...` shape for these columns, so one parser serves both. Unix-only
/// (the only place a name needs host-DB resolution).
#[cfg(unix)]
fn parse_passwd_id(body: &str, name: &str) -> Option<u32> {
    for line in body.lines() {
        let mut cols = line.split(':');
        if cols.next() == Some(name) {
            // skip the password placeholder (2nd col), take the id (3rd col)
            let _ = cols.next();
            if let Some(id) = cols.next() {
                return id.parse::<u32>().ok();
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "envuser_tests.rs"]
mod tests;
