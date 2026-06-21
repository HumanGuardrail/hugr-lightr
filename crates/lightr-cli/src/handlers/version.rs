//! `lightr version` handler — docker-version parity, daemonless-honest.
//!
//! Docker prints a Client block AND a Server (Engine) block. lightr has NO
//! server component (principle #1: "No daemon, ever"), so we print the client
//! facts and state that fact rather than fabricating a Server section.
//!
//! Human output:
//! ```text
//! lightr version 0.1.0
//!  Git commit:  abc1234
//!  Built:       2026-06-19
//!  OS/Arch:     macos/aarch64
//!  Server:      none (daemonless — no server component)
//! ```
//! `--json` output (stable keys):
//! ```json
//! {"version":"0.1.0","git_commit":"abc1234","built":"2026-06-19",
//!  "os":"macos","arch":"aarch64","daemonless":true,"server":null}
//! ```

use serde::Serialize;

#[derive(Serialize)]
struct VersionJson {
    /// Semver from CARGO_PKG_VERSION.
    version: &'static str,
    /// Short git SHA captured at build time (or "unknown").
    git_commit: &'static str,
    /// Build date YYYY-MM-DD (or "unknown").
    built: &'static str,
    /// Target OS the running binary was compiled for.
    os: &'static str,
    /// Target architecture the running binary was compiled for.
    arch: &'static str,
    /// Principle #1: lightr never runs a daemon. Always true.
    daemonless: bool,
    /// There is no server component — honestly `null`, never fabricated.
    server: Option<()>,
}

/// Run `lightr version`. Always exits 0 (a pure report).
pub fn run(json: bool) -> i32 {
    let v = VersionJson {
        version: env!("CARGO_PKG_VERSION"),
        git_commit: env!("LIGHTR_GIT_SHA"),
        built: env!("LIGHTR_BUILD_DATE"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        daemonless: true,
        server: None,
    };

    if json {
        println!("{}", serde_json::to_string(&v).expect("serialize version"));
    } else {
        println!("lightr version {}", v.version);
        println!(" Git commit:  {}", v.git_commit);
        println!(" Built:       {}", v.built);
        println!(" OS/Arch:     {}/{}", v.os, v.arch);
        println!(" Server:      none (daemonless — no server component)");
    }
    0
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
