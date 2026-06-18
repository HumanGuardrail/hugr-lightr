//! Build script: capture best-effort git SHA + build date for `--version`.
//!
//! Never fails the build. If git is absent or not a repo, the SHA falls back
//! to "unknown". Emits two env vars consumed via `env!` in `main.rs`:
//!   LIGHTR_GIT_SHA  — short commit hash (or "unknown")
//!   LIGHTR_BUILD_DATE — UTC build date YYYY-MM-DD (or "unknown")

use std::process::Command;

fn main() {
    let sha = git_short_sha().unwrap_or_else(|| "unknown".to_string());
    let date = build_date().unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=LIGHTR_GIT_SHA={sha}");
    println!("cargo:rustc-env=LIGHTR_BUILD_DATE={date}");

    // Re-run when HEAD moves so the SHA stays fresh, best-effort.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}

/// `git rev-parse --short HEAD`, or None if git is missing / not a repo.
fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// UTC build date as YYYY-MM-DD, derived from SOURCE_DATE_EPOCH if set (for
/// reproducible builds) else the current system time. No external crates.
fn build_date() -> Option<String> {
    let secs: u64 = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(v) => v.trim().parse().ok()?,
        Err(_) => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    };
    Some(civil_date_from_unix(secs))
}

/// Convert a Unix timestamp (UTC) to "YYYY-MM-DD" using the civil-from-days
/// algorithm (Howard Hinnant). Pure integer math; no chrono dependency.
fn civil_date_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    // Shift epoch to 0000-03-01 so leap-day handling is uniform.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
