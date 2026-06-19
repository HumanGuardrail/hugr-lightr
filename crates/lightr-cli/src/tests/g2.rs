use super::*;

// ── bench ─────────────────────────────────────────────────────────────

#[test]
fn bench_minimal() {
    let cli = parse(&["bench"]);
    match cli.cmd {
        Cmd::Bench { vs_docker, check } => {
            assert!(!vs_docker);
            assert!(!check);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn bench_all_flags() {
    let cli = parse(&["bench", "--vs-docker", "--check"]);
    match cli.cmd {
        Cmd::Bench { vs_docker, check } => {
            assert!(vs_docker);
            assert!(check);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn bench_json_flag() {
    let cli = parse(&["--json", "bench"]);
    assert!(cli.json);
}

// ── ps ────────────────────────────────────────────────────────────────

#[test]
fn ps_minimal() {
    parse(&["ps"]);
}

#[test]
fn ps_json() {
    let cli = parse(&["ps", "--json"]);
    match &cli.cmd {
        Cmd::Ps { json } => assert!(*json),
        _ => panic!("wrong cmd"),
    }
}

// ── logs ──────────────────────────────────────────────────────────────

#[test]
fn logs_minimal() {
    let cli = parse(&["logs", "abc123"]);
    match &cli.cmd {
        Cmd::Logs {
            id,
            stderr,
            both,
            follow,
        } => {
            assert_eq!(id, "abc123");
            assert!(!stderr);
            assert!(!both);
            assert!(!follow);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn logs_stderr_flag() {
    parse(&["logs", "id1", "--stderr"]);
}

#[test]
fn logs_both_flag() {
    parse(&["logs", "id1", "--both"]);
}

#[test]
fn logs_follow_flag() {
    parse(&["logs", "id1", "-f"]);
}

#[test]
fn logs_requires_id() {
    assert!(try_parse(&["logs"]).is_err());
}

// ── stop ──────────────────────────────────────────────────────────────

#[test]
fn stop_minimal() {
    parse(&["stop", "myid"]);
}

#[test]
fn stop_grace() {
    parse(&["stop", "myid", "--grace", "5"]);
}

#[test]
fn stop_default_grace() {
    let cli = parse(&["stop", "myid"]);
    match &cli.cmd {
        Cmd::Stop { grace, .. } => assert_eq!(*grace, 10),
        _ => panic!("wrong cmd"),
    }
}

// ── exec ──────────────────────────────────────────────────────────────

#[test]
fn exec_minimal() {
    parse(&["exec", "myid", "--", "echo", "hi"]);
}

#[test]
fn exec_requires_command() {
    assert!(try_parse(&["exec", "myid"]).is_err());
}

// ── gc ────────────────────────────────────────────────────────────────

#[test]
fn gc_minimal() {
    let cli = parse(&["gc"]);
    match &cli.cmd {
        Cmd::Gc {
            force,
            min_age,
            json,
        } => {
            assert!(!force);
            assert_eq!(*min_age, 3600);
            assert!(!json);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn gc_force() {
    parse(&["gc", "--force"]);
}

#[test]
fn gc_min_age() {
    parse(&["gc", "--min-age", "7200"]);
}

// ── undo ──────────────────────────────────────────────────────────────

#[test]
fn undo_name() {
    parse(&["undo", "--name", "myref"]);
}

#[test]
fn undo_requires_name() {
    assert!(try_parse(&["undo"]).is_err());
}

// ── diff ──────────────────────────────────────────────────────────────

#[test]
fn diff_minimal() {
    let cli = parse(&["diff", "--name", "myref"]);
    match &cli.cmd {
        Cmd::Diff { name, at, dir, .. } => {
            assert_eq!(name, "myref");
            assert_eq!(*at, 1);
            assert!(dir.is_none());
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn diff_at() {
    parse(&["diff", "--name", "r", "--at", "3"]);
}

#[test]
fn diff_dir() {
    parse(&["diff", "--name", "r", "--dir", "/tmp/x"]);
}

// ── bisect ────────────────────────────────────────────────────────────

#[test]
fn bisect_minimal() {
    parse(&["bisect", "--name", "r", "--", "sh", "-c", "true"]);
}

#[test]
fn bisect_requires_name_and_cmd() {
    assert!(try_parse(&["bisect"]).is_err());
}

// ── plan ──────────────────────────────────────────────────────────────

#[test]
fn plan_snapshot() {
    parse(&["plan", "snapshot", "--name", "r"]);
}

#[test]
fn plan_hydrate() {
    parse(&["plan", "hydrate", "/dest", "--name", "r"]);
}

#[test]
fn plan_run() {
    parse(&["plan", "run", "--", "echo"]);
}

// ── schema ────────────────────────────────────────────────────────────

#[test]
fn schema_no_verb_parses() {
    let cli = parse(&["schema"]);
    match &cli.cmd {
        Cmd::Schema { verb } => assert!(verb.is_none()),
        _ => panic!("expected Schema"),
    }
}

#[test]
fn schema_with_verb_parses() {
    let cli = parse(&["schema", "--verb", "run"]);
    match &cli.cmd {
        Cmd::Schema { verb } => assert_eq!(verb.as_deref(), Some("run")),
        _ => panic!("expected Schema"),
    }
}

#[test]
fn schema_unknown_verb_exits_2() {
    use crate::handlers::schema::run as schema_run;
    let code = schema_run(Some("notaverb"));
    assert_eq!(code, 2, "unknown verb must exit 2");
}

// ── mcp ───────────────────────────────────────────────────────────────

#[test]
fn mcp_parses() {
    parse(&["mcp"]);
}

// ── supervise (hidden) ─────────────────────────────────────────────────

#[test]
fn supervise_parses() {
    parse(&["__supervise", "/some/dir"]);
}

// ── global flags ──────────────────────────────────────────────────────

#[test]
fn global_json_before_subcommand() {
    let cli = parse(&["--json", "snapshot", "--name", "r"]);
    assert!(cli.json);
}

#[test]
fn global_explain_flag() {
    let cli = parse(&["--explain", "status", "--name", "r"]);
    assert!(cli.explain);
}

#[test]
fn events_global_flag() {
    let cli = parse(&["--events", "ps"]);
    assert!(cli.events);
}

#[test]
fn unknown_subcommand_fails() {
    assert!(try_parse(&["notaverb"]).is_err());
}

// ── EventEmitter unit tests ────────────────────────────────────────────

#[test]
fn emitter_start_contains_start_ev() {
    let mut buf = Vec::<u8>::new();
    crate::emit_event(&mut buf, "start", "snapshot", "");
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains(r#""ev":"start""#), "missing ev:start in: {s}");
    assert!(
        s.contains(r#""verb":"snapshot""#),
        "missing verb:snapshot in: {s}"
    );
    assert!(s.contains("\"ts\":"), "missing ts in: {s}");
}

#[test]
fn emitter_end_contains_ok_and_exit() {
    let mut buf = Vec::<u8>::new();
    crate::emit_event(&mut buf, "end", "run", r#","ok":true,"exit":0"#);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains(r#""ev":"end""#), "missing ev:end in: {s}");
    assert!(s.contains(r#""ok":true"#), "missing ok:true in: {s}");
    assert!(s.contains(r#""exit":0"#), "missing exit:0 in: {s}");
}
