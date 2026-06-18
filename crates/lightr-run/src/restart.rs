//! Restart policy + OS-supervisor unit-file templates (F-308).
//!
//! build-spec-parity.md §3 / feature-parity.md R3 ("restart policies /
//! `--restart`"): the law is **integrate the OS supervisor, ship NO daemon of
//! our own**. This module is the PURE half — a `RestartPolicy` enum, its parser
//! (fail closed on garbage), and the unit-file template generators. Every
//! generator returns a `String` and performs **no I/O**, so it is fully
//! unit-testable. The I/O + opt-in flow lives in
//! `lightr-cli::handlers::supervise`.
//!
//! Policy → supervisor mapping (the Docker `--restart` ↔ launchd/systemd
//! translation; standard, not a design choice):
//!
//! | RestartPolicy   | launchd `KeepAlive`            | systemd `Restart=` |
//! |-----------------|--------------------------------|--------------------|
//! | `No`            | (omitted)                      | `no`               |
//! | `Always`        | `<true/>`                      | `always`           |
//! | `OnFailure{..}` | `{ SuccessfulExit = false; }`  | `on-failure`       |
//! | `UnlessStopped` | `<true/>`                      | `always`           |
//!
//! `unless-stopped` has no distinct launchd/systemd primitive (it differs from
//! `always` only in surviving a manual stop across host reboot, which the OS
//! supervisor's enable/disable state already governs), so it maps like
//! `always`. `OnFailure.max` (Docker's retry cap) has no portable
//! single-directive equivalent in either supervisor; it is preserved in the
//! type for fidelity and surfaced to the user, not encoded into the unit.

use lightr_core::{LightrError, Result};
use std::fmt::Write as _;

/// A container/run restart policy, mirroring Docker's `--restart`.
///
/// Not part of the memo key (build-spec-parity.md §0 — it is an OS-supervisor
/// concern, never a run input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Never restart (Docker `no`). Default-equivalent.
    No,
    /// Always restart (Docker `always`).
    Always,
    /// Restart only on non-zero exit (Docker `on-failure[:N]`); `max` = retry
    /// cap (`0` = unlimited, matching Docker's bare `on-failure`).
    OnFailure { max: u32 },
    /// Restart unless explicitly stopped (Docker `unless-stopped`).
    UnlessStopped,
}

impl RestartPolicy {
    /// Parse a Docker-style restart string. Fail closed on anything else.
    ///
    /// Accepts: `"no"`, `"always"`, `"unless-stopped"`,
    /// `"on-failure"` (cap = 0 = unlimited), `"on-failure:N"` (cap = N).
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "no" => Ok(Self::No),
            "always" => Ok(Self::Always),
            "unless-stopped" => Ok(Self::UnlessStopped),
            "on-failure" => Ok(Self::OnFailure { max: 0 }),
            other => {
                if let Some(n) = other.strip_prefix("on-failure:") {
                    let max: u32 = n.parse().map_err(|_| {
                        LightrError::InvalidRef(format!(
                            "restart policy on-failure:N requires a non-negative integer, got {n:?}"
                        ))
                    })?;
                    Ok(Self::OnFailure { max })
                } else {
                    Err(LightrError::InvalidRef(format!(
                        "unknown restart policy {s:?} (want: no | always | on-failure[:N] | unless-stopped)"
                    )))
                }
            }
        }
    }

    /// The canonical Docker-style string for this policy (round-trips `parse`).
    pub fn as_str(&self) -> String {
        match self {
            Self::No => "no".to_string(),
            Self::Always => "always".to_string(),
            Self::UnlessStopped => "unless-stopped".to_string(),
            Self::OnFailure { max: 0 } => "on-failure".to_string(),
            Self::OnFailure { max } => format!("on-failure:{max}"),
        }
    }

    /// systemd `Restart=` value for this policy.
    fn systemd_restart(&self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Always | Self::UnlessStopped => "always",
            Self::OnFailure { .. } => "on-failure",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// XML / shell escaping (pure)
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal XML text escaping for plist string values.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// launchd plist generator (macOS) — PURE
// ─────────────────────────────────────────────────────────────────────────────

/// Generate a launchd plist (macOS) for the given program/args under a restart
/// policy. Pure: returns the full XML document as a `String`, no I/O.
///
/// - `RunAtLoad` is always `<true/>` (the unit runs when the user opts in via
///   `launchctl bootstrap`).
/// - `KeepAlive` is mapped from the policy: `Always`/`UnlessStopped` →
///   `<true/>`; `OnFailure` → `{ SuccessfulExit = false; }`; `No` → omitted.
/// - `WorkingDirectory` ← `dir`. The full argv is `program` followed by `args`.
pub fn launchd_plist(
    label: &str,
    program: &str,
    args: &[String],
    dir: &str,
    policy: RestartPolicy,
) -> String {
    let mut prog_args = String::new();
    // First ProgramArguments entry is the program itself.
    let _ = writeln!(
        prog_args,
        "        <string>{}</string>",
        xml_escape(program)
    );
    for a in args {
        let _ = writeln!(prog_args, "        <string>{}</string>", xml_escape(a));
    }

    let keep_alive = match policy {
        RestartPolicy::No => String::new(),
        RestartPolicy::Always | RestartPolicy::UnlessStopped => {
            "    <key>KeepAlive</key>\n    <true/>\n".to_string()
        }
        RestartPolicy::OnFailure { .. } => "    <key>KeepAlive</key>\n    <dict>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n".to_string(),
    };

    // Concatenate explicit lines so the indentation is preserved (a `\`
    // line-continuation inside the string literal would eat the leading
    // whitespace of each source line). The plist is human-readable + valid.
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n");
    out.push_str("<dict>\n");
    out.push_str("    <key>Label</key>\n");
    let _ = writeln!(out, "    <string>{}</string>", xml_escape(label));
    out.push_str("    <key>ProgramArguments</key>\n");
    out.push_str("    <array>\n");
    out.push_str(&prog_args);
    out.push_str("    </array>\n");
    out.push_str("    <key>WorkingDirectory</key>\n");
    let _ = writeln!(out, "    <string>{}</string>", xml_escape(dir));
    out.push_str("    <key>RunAtLoad</key>\n");
    out.push_str("    <true/>\n");
    out.push_str(&keep_alive);
    out.push_str("</dict>\n");
    out.push_str("</plist>\n");
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// systemd unit generator (Linux user unit) — PURE
// ─────────────────────────────────────────────────────────────────────────────

/// Shell-quote an argv element for a systemd `ExecStart=` line.
///
/// systemd's `ExecStart` uses an sh-like split; wrapping each element in
/// single quotes (and escaping embedded single quotes) keeps spaces and special
/// characters intact.
fn sh_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // Safe-char fast path avoids needless quoting for plain tokens.
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '@'))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Generate a systemd **user** unit (`.service`) for the given program/args
/// under a restart policy. Pure: returns the full unit text as a `String`, no
/// I/O.
///
/// - `Restart=` ← policy (`always` | `on-failure` | `no`).
/// - `ExecStart=` ← `program` + `args` (shell-quoted).
/// - `WorkingDirectory=` ← `dir`.
pub fn systemd_unit(
    description: &str,
    program: &str,
    args: &[String],
    dir: &str,
    policy: RestartPolicy,
) -> String {
    let mut exec = sh_quote(program);
    for a in args {
        exec.push(' ');
        exec.push_str(&sh_quote(a));
    }

    let restart = policy.systemd_restart();

    // `RestartSec` is benign and only meaningful when Restart != no; including
    // it unconditionally keeps the template simple and valid in all cases.
    format!(
        "[Unit]\n\
Description={description}\n\
\n\
[Service]\n\
Type=simple\n\
WorkingDirectory={dir}\n\
ExecStart={exec}\n\
Restart={restart}\n\
RestartSec=1\n\
\n\
[Install]\n\
WantedBy=default.target\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse + round-trip + fail-closed ──────────────────────────────────────

    #[test]
    fn parse_known_policies() {
        assert_eq!(RestartPolicy::parse("no").unwrap(), RestartPolicy::No);
        assert_eq!(
            RestartPolicy::parse("always").unwrap(),
            RestartPolicy::Always
        );
        assert_eq!(
            RestartPolicy::parse("unless-stopped").unwrap(),
            RestartPolicy::UnlessStopped
        );
        assert_eq!(
            RestartPolicy::parse("on-failure").unwrap(),
            RestartPolicy::OnFailure { max: 0 }
        );
        assert_eq!(
            RestartPolicy::parse("on-failure:5").unwrap(),
            RestartPolicy::OnFailure { max: 5 }
        );
    }

    #[test]
    fn parse_round_trips_via_as_str() {
        for s in [
            "no",
            "always",
            "unless-stopped",
            "on-failure",
            "on-failure:7",
        ] {
            let p = RestartPolicy::parse(s).unwrap();
            assert_eq!(p.as_str(), s, "round-trip failed for {s:?}");
            assert_eq!(RestartPolicy::parse(&p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn parse_fails_closed_on_garbage() {
        for bad in [
            "",
            "yes",
            "ALWAYS",
            "on-failure:",
            "on-failure:-1",
            "on-failure:abc",
            "on_failure",
            "always ",
            " no",
        ] {
            let r = RestartPolicy::parse(bad);
            assert!(r.is_err(), "expected error for {bad:?}, got {r:?}");
            assert!(matches!(r, Err(LightrError::InvalidRef(_))));
        }
    }

    // ── launchd plist mapping ─────────────────────────────────────────────────

    #[test]
    fn launchd_always_keepalive_true() {
        let p = launchd_plist(
            "com.hugr.lightr.web",
            "/usr/local/bin/lightr",
            &["run".to_string(), "--dir".to_string(), ".".to_string()],
            "/srv/app",
            RestartPolicy::Always,
        );
        assert!(p.contains("<key>Label</key>"));
        assert!(p.contains("<string>com.hugr.lightr.web</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(p.contains("<key>WorkingDirectory</key>\n    <string>/srv/app</string>"));
        assert!(p.contains("<string>/usr/local/bin/lightr</string>"));
        assert!(p.contains("<string>run</string>"));
        assert!(!p.contains("SuccessfulExit"));
    }

    #[test]
    fn launchd_on_failure_keepalive_successful_exit_false() {
        let p = launchd_plist(
            "x",
            "/bin/true",
            &[],
            "/tmp",
            RestartPolicy::OnFailure { max: 3 },
        );
        assert!(p.contains("<key>KeepAlive</key>\n    <dict>"));
        assert!(p.contains("<key>SuccessfulExit</key>\n        <false/>"));
    }

    #[test]
    fn launchd_no_omits_keepalive() {
        let p = launchd_plist("x", "/bin/true", &[], "/tmp", RestartPolicy::No);
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(!p.contains("KeepAlive"));
    }

    #[test]
    fn launchd_unless_stopped_keepalive_true() {
        let p = launchd_plist("x", "/bin/true", &[], "/tmp", RestartPolicy::UnlessStopped);
        assert!(p.contains("<key>KeepAlive</key>\n    <true/>"));
    }

    #[test]
    fn launchd_escapes_xml_metacharacters() {
        let p = launchd_plist(
            "a<b>&c",
            "/bin/echo",
            &["x & y".to_string(), "<tag>".to_string()],
            "/tmp",
            RestartPolicy::No,
        );
        assert!(p.contains("a&lt;b&gt;&amp;c"));
        assert!(p.contains("x &amp; y"));
        assert!(p.contains("&lt;tag&gt;"));
        // No raw, unescaped injected angle brackets from the payload.
        assert!(!p.contains("<tag>"));
    }

    #[test]
    fn launchd_is_well_formed_xml() {
        let p = launchd_plist(
            "x",
            "/bin/echo",
            &["hi".to_string()],
            "/tmp",
            RestartPolicy::Always,
        );
        assert!(p.starts_with("<?xml version=\"1.0\""));
        assert!(p.contains("<!DOCTYPE plist"));
        assert!(p.trim_end().ends_with("</plist>"));
        // Balanced top-level <dict>.
        assert_eq!(p.matches("<dict>").count(), p.matches("</dict>").count());
        assert_eq!(p.matches("<array>").count(), p.matches("</array>").count());
        assert_eq!(p.matches("<plist").count(), p.matches("</plist>").count());
    }

    // ── systemd unit mapping ──────────────────────────────────────────────────

    #[test]
    fn systemd_always_restart_always() {
        let u = systemd_unit(
            "lightr web",
            "/usr/local/bin/lightr",
            &["run".to_string(), "--dir".to_string(), ".".to_string()],
            "/srv/app",
            RestartPolicy::Always,
        );
        assert!(u.contains("[Unit]"));
        assert!(u.contains("[Service]"));
        assert!(u.contains("[Install]"));
        assert!(u.contains("Restart=always"));
        assert!(u.contains("WorkingDirectory=/srv/app"));
        assert!(u.contains("ExecStart=/usr/local/bin/lightr run --dir ."));
        assert!(u.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_on_failure_restart_on_failure() {
        let u = systemd_unit(
            "d",
            "/bin/true",
            &[],
            "/tmp",
            RestartPolicy::OnFailure { max: 0 },
        );
        assert!(u.contains("Restart=on-failure"));
    }

    #[test]
    fn systemd_no_restart_no() {
        let u = systemd_unit("d", "/bin/true", &[], "/tmp", RestartPolicy::No);
        assert!(u.contains("Restart=no"));
    }

    #[test]
    fn systemd_unless_stopped_restart_always() {
        let u = systemd_unit("d", "/bin/true", &[], "/tmp", RestartPolicy::UnlessStopped);
        assert!(u.contains("Restart=always"));
    }

    #[test]
    fn systemd_shell_quotes_args_with_spaces() {
        let u = systemd_unit(
            "d",
            "/bin/sh",
            &["-c".to_string(), "echo hi there".to_string()],
            "/tmp",
            RestartPolicy::No,
        );
        assert!(u.contains("ExecStart=/bin/sh -c 'echo hi there'"));
    }

    #[test]
    fn systemd_shell_quotes_embedded_single_quote() {
        let u = systemd_unit(
            "d",
            "/bin/sh",
            &["-c".to_string(), "it's fine".to_string()],
            "/tmp",
            RestartPolicy::No,
        );
        assert!(u.contains("'it'\\''s fine'"));
    }
}
