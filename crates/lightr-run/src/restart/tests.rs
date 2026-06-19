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
