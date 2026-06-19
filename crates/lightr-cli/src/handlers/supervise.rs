//! `lightr supervise` handler — OS-supervisor unit generation (F-308).
//!
//! build-spec-parity.md §3 / feature-parity.md R3 ("restart policies /
//! `--restart`"): **integrate the OS supervisor, ship NO daemon of our own.**
//! The OS already has a resident supervisor (launchd / systemd) — Lightr does
//! not ship a second one. `supervise install` GENERATES a unit file under
//! `~/.lightr/units/` and PRINTS the exact opt-in command the user runs to load
//! it; it never auto-loads anything (the supervisor is the user's, opt-in).
//!
//! Pure unit-file templates + `RestartPolicy` live in
//! `lightr_run::restart`; this file owns only the I/O + user-facing flow.
//!
//! Platform support: macOS (launchd) + Linux (systemd user units). Windows is
//! an honest `Unsupported` error (Task Scheduler = a future ring), never a
//! silent no-op.

use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use lightr_run::restart;
use lightr_run::restart::RestartPolicy;

use crate::exit::die_lightr;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::lightr_home;

fn io_err(kind: std::io::ErrorKind, msg: impl Into<String>) -> lightr_core::LightrError {
    lightr_core::LightrError::Io(std::io::Error::new(kind, msg.into()))
}

/// The `~/.lightr/units/` directory (uses the same home resolution as the rest
/// of the CLI — `$LIGHTR_HOME`, else `~/.lightr`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn units_dir() -> PathBuf {
    lightr_home().join("units")
}

/// Best-effort absolutization of the working directory (so the unit, loaded by
/// the supervisor from an unrelated cwd, points at the right place). Falls back
/// to the given string if the cwd can't be read.
fn absolutize(dir: &str) -> String {
    let p = Path::new(dir);
    if p.is_absolute() {
        return dir.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).to_string_lossy().into_owned(),
        Err(_) => dir.to_string(),
    }
}

/// Split a `-- CMD args…` slice into (program, args). The CLI enforces
/// `required = true`, so an empty command is a usage error here too.
fn split_command(command: &[String]) -> Result<(&str, &[String]), lightr_core::LightrError> {
    match command.split_first() {
        Some((prog, args)) => Ok((prog.as_str(), args)),
        None => Err(io_err(
            std::io::ErrorKind::InvalidInput,
            "supervise install requires a command after `--`",
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// install
// ─────────────────────────────────────────────────────────────────────────────

/// `lightr supervise install --name N --restart P --dir D -- CMD`.
///
/// Writes the unit to `~/.lightr/units/<name>.{plist|service}`, then prints the
/// exact opt-in command the user runs to load it under their OS supervisor.
/// Does NOT auto-load — we ship no daemon; loading is the user's explicit step.
pub fn install(name: &str, restart_str: &str, dir: &str, command: &[String]) -> i32 {
    let policy = match RestartPolicy::parse(restart_str) {
        Ok(p) => p,
        Err(e) => return die_lightr(&e),
    };
    let (program, args) = match split_command(command) {
        Ok(v) => v,
        Err(e) => return die_lightr(&e),
    };
    let work_dir = absolutize(dir);

    match install_impl(name, policy, program, args, &work_dir) {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}

#[cfg(target_os = "macos")]
fn install_impl(
    name: &str,
    policy: RestartPolicy,
    program: &str,
    args: &[String],
    work_dir: &str,
) -> Result<(), lightr_core::LightrError> {
    let label = format!("com.hugr.lightr.{name}");
    let dir = units_dir();
    std::fs::create_dir_all(&dir).map_err(lightr_core::LightrError::Io)?;
    let path = dir.join(format!("{name}.plist"));
    let unit = restart::launchd_plist(&label, program, args, work_dir, policy);
    std::fs::write(&path, unit).map_err(lightr_core::LightrError::Io)?;

    // `$(id -u)` is left for the user's shell to expand — we print the opt-in
    // command, we do not run launchctl (no daemon of ours, no auto-load).
    println!("wrote launchd unit: {}", path.display());
    println!("restart policy: {}", policy.as_str());
    println!();
    println!("to start it under your launchd (opt-in — Lightr loads nothing):");
    println!("  launchctl bootstrap gui/$(id -u) {}", path.display());
    println!("to stop it later:");
    println!("  launchctl bootout gui/$(id -u)/{label}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_impl(
    name: &str,
    policy: RestartPolicy,
    program: &str,
    args: &[String],
    work_dir: &str,
) -> Result<(), lightr_core::LightrError> {
    let dir = units_dir();
    std::fs::create_dir_all(&dir).map_err(lightr_core::LightrError::Io)?;
    let path = dir.join(format!("{name}.service"));
    let description = format!("lightr supervised run: {name}");
    let unit = restart::systemd_unit(&description, program, args, work_dir, policy);
    std::fs::write(&path, unit).map_err(lightr_core::LightrError::Io)?;

    println!("wrote systemd user unit: {}", path.display());
    println!("restart policy: {}", policy.as_str());
    println!();
    println!("to start it under your systemd --user (opt-in — Lightr loads nothing):");
    println!(
        "  cp {} ~/.config/systemd/user/{name}.service",
        path.display()
    );
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now {name}");
    println!("to stop it later:");
    println!("  systemctl --user disable --now {name}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_impl(
    _name: &str,
    _policy: RestartPolicy,
    _program: &str,
    _args: &[String],
    _work_dir: &str,
) -> Result<(), lightr_core::LightrError> {
    Err(unsupported_platform())
}

// ─────────────────────────────────────────────────────────────────────────────
// uninstall
// ─────────────────────────────────────────────────────────────────────────────

/// `lightr supervise uninstall --name N`: remove the unit + print the unload
/// command (so the user can detach it from their supervisor first if loaded).
pub fn uninstall(name: &str) -> i32 {
    match uninstall_impl(name) {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}

#[cfg(target_os = "macos")]
fn uninstall_impl(name: &str) -> Result<(), lightr_core::LightrError> {
    let path = units_dir().join(format!("{name}.plist"));
    if !path.exists() {
        return Err(io_err(
            std::io::ErrorKind::NotFound,
            format!(
                "no supervised unit named {name:?} under {}",
                units_dir().display()
            ),
        ));
    }
    let label = format!("com.hugr.lightr.{name}");
    // Print the unload command BEFORE removing the file (so a loaded unit can be
    // detached); then remove the generated file. We never call launchctl.
    println!("if it is loaded, stop it first (Lightr unloads nothing):");
    println!("  launchctl bootout gui/$(id -u)/{label}");
    std::fs::remove_file(&path).map_err(lightr_core::LightrError::Io)?;
    println!("removed launchd unit: {}", path.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_impl(name: &str) -> Result<(), lightr_core::LightrError> {
    let path = units_dir().join(format!("{name}.service"));
    if !path.exists() {
        return Err(io_err(
            std::io::ErrorKind::NotFound,
            format!(
                "no supervised unit named {name:?} under {}",
                units_dir().display()
            ),
        ));
    }
    println!("if it is loaded, stop it first (Lightr unloads nothing):");
    println!("  systemctl --user disable --now {name}");
    std::fs::remove_file(&path).map_err(lightr_core::LightrError::Io)?;
    println!("removed systemd user unit: {}", path.display());
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn uninstall_impl(_name: &str) -> Result<(), lightr_core::LightrError> {
    Err(unsupported_platform())
}

// ─────────────────────────────────────────────────────────────────────────────
// list
// ─────────────────────────────────────────────────────────────────────────────

/// `lightr supervise list`: enumerate generated units under `~/.lightr/units/`.
pub fn list() -> i32 {
    match list_impl() {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn list_impl() -> Result<(), lightr_core::LightrError> {
    let dir = units_dir();
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        // No units dir yet ⇒ nothing installed; not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(lightr_core::LightrError::Io(e)),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in rd {
        let entry = entry.map_err(lightr_core::LightrError::Io)?;
        let p = entry.path();
        let ext = p.extension().and_then(|x| x.to_str());
        if matches!(ext, Some("plist") | Some("service")) {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    for n in names {
        println!("{n}");
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn list_impl() -> Result<(), lightr_core::LightrError> {
    Err(unsupported_platform())
}

// ─────────────────────────────────────────────────────────────────────────────
// platform helpers
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported_platform() -> lightr_core::LightrError {
    io_err(
        std::io::ErrorKind::Unsupported,
        "supervise is not supported on this platform yet (macOS launchd / Linux systemd only; \
         Windows Task Scheduler is a future ring)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolutize_keeps_absolute() {
        assert_eq!(absolutize("/srv/app"), "/srv/app");
    }

    #[test]
    fn absolutize_relative_becomes_absolute() {
        let out = absolutize(".");
        assert!(Path::new(&out).is_absolute(), "{out} should be absolute");
    }

    #[test]
    fn split_command_splits_program_and_args() {
        let cmd = vec!["/bin/echo".to_string(), "a".to_string(), "b".to_string()];
        let (p, a) = split_command(&cmd).unwrap();
        assert_eq!(p, "/bin/echo");
        assert_eq!(a, &["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn split_command_empty_is_error() {
        assert!(split_command(&[]).is_err());
    }

    #[test]
    fn install_rejects_bad_policy() {
        // Bad policy must map to exit 2 (InvalidRef), never write a file.
        let code = install(
            "x",
            "definitely-not-a-policy",
            ".",
            &["/bin/true".to_string()],
        );
        assert_eq!(code, 2);
    }
}
