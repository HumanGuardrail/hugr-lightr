//! `lightr supervise` handler — OS-supervisor unit generation (F-308).
//!
//! build-spec-parity.md §A0.5 freezes the CLI surface + dispatch; **WP-A2 fills
//! the bodies** (launchd plist / systemd unit generators + install/uninstall/
//! list). A0 ships honest stubs: they return an `Unsupported` I/O error and
//! route it through `die_lightr` (exit 1, `lightr: …` on stderr) — they NEVER
//! print fake success. (`lightr-core::LightrError` has no `Unsupported` variant,
//! so the honest closest is `Io(ErrorKind::Unsupported)`.)

use crate::exit::die_lightr;

fn not_yet(feature: &str) -> i32 {
    let e = lightr_core::LightrError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{feature}: not yet implemented (WP-A2)"),
    ));
    die_lightr(&e)
}

/// `lightr supervise install --name N --restart P --dir D -- CMD`.
pub fn install(name: &str, restart: &str, dir: &str, command: &[String]) -> i32 {
    let _ = (name, restart, dir, command);
    not_yet("supervise install")
}

/// `lightr supervise uninstall --name N`.
pub fn uninstall(name: &str) -> i32 {
    let _ = name;
    not_yet("supervise uninstall")
}

/// `lightr supervise list`.
pub fn list() -> i32 {
    not_yet("supervise list")
}
