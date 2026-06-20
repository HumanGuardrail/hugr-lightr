//! Parser unit tests (WP-DF-01). Pure functions; no global state — parallel-safe.
use super::*;

fn parse(text: &str) -> Vec<BuildStep> {
    parse_dockerfile(text).unwrap()
}

// ---- back-compat: the original 7-instruction suite must still pass ---------

#[test]
fn original_seven_instructions() {
    let df = r#"
FROM scratch
RUN echo hello
COPY src/ /app/
ENV FOO=bar
WORKDIR /work
CMD ["sh","-c","start"]
LABEL version=1.0
"#;
    let steps = parse(df);
    assert_eq!(steps.len(), 7);
    assert_eq!(
        steps[0].instr,
        Instr::From {
            image_ref: "scratch".to_string(),
            platform: None,
            stage: None,
        }
    );
    assert_eq!(
        steps[1].instr,
        Instr::Run {
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hello".into()],
            form: CmdForm::Shell("echo hello".into()),
        }
    );
    if let Instr::Copy { src, dest, .. } = &steps[2].instr {
        assert_eq!(dest, "/app/");
        assert_eq!(src, &["src/"]);
    } else {
        panic!("expected Copy");
    }
    assert_eq!(
        steps[3].instr,
        Instr::Env {
            pairs: vec![("FOO".into(), "bar".into())]
        }
    );
    assert_eq!(
        steps[4].instr,
        Instr::Workdir {
            path: "/work".into()
        }
    );
    assert_eq!(
        steps[5].instr,
        Instr::Cmd {
            argv: vec!["sh".into(), "-c".into(), "start".into()],
            form: CmdForm::Exec(vec!["sh".into(), "-c".into(), "start".into()]),
        }
    );
    assert_eq!(
        steps[6].instr,
        Instr::Label {
            pairs: vec![("version".into(), "1.0".into())]
        }
    );
}

#[test]
fn case_insensitive_verb() {
    let steps = parse("from scratch\nrun echo hi\n");
    assert_eq!(steps.len(), 2);
    assert!(matches!(steps[0].instr, Instr::From { .. }));
    assert!(matches!(steps[1].instr, Instr::Run { .. }));
}

#[test]
fn comments_and_blanks_skipped() {
    let steps = parse("# header\n\nFROM scratch\n# comment\nRUN true\n");
    assert_eq!(steps.len(), 2);
}

#[test]
fn unknown_verb_is_error() {
    let err = parse_dockerfile("FROBNICATE foo\n").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unsupported instruction"), "got: {msg}");
    assert!(msg.contains("FROBNICATE"), "got: {msg}");
}

#[test]
fn maintainer_is_rejected() {
    let err = parse_dockerfile("MAINTAINER alice\n").unwrap_err();
    assert!(err.to_string().contains("MAINTAINER"));
}

// ---- exec vs shell form ----------------------------------------------------

#[test]
fn run_exec_form_json_array() {
    let steps = parse(r#"RUN ["/bin/echo","hi"]"#);
    assert_eq!(
        steps[0].instr,
        Instr::Run {
            argv: vec!["/bin/echo".into(), "hi".into()],
            form: CmdForm::Exec(vec!["/bin/echo".into(), "hi".into()]),
        }
    );
}

#[test]
fn run_shell_form_wraps_in_sh() {
    let steps = parse("RUN echo hi");
    assert_eq!(
        steps[0].instr,
        Instr::Run {
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            form: CmdForm::Shell("echo hi".into()),
        }
    );
}

#[test]
fn entrypoint_both_forms() {
    let exec = parse(r#"ENTRYPOINT ["/app","--serve"]"#);
    assert!(matches!(
        &exec[0].instr,
        Instr::Entrypoint { form: CmdForm::Exec(v), .. } if v == &["/app", "--serve"]
    ));
    let shell = parse("ENTRYPOINT /app --serve");
    assert!(matches!(
        &shell[0].instr,
        Instr::Entrypoint { form: CmdForm::Shell(s), .. } if s == "/app --serve"
    ));
}

#[test]
fn malformed_json_falls_back_to_shell() {
    // Not valid JSON array -> shell form (faithful to Docker's degradation).
    let steps = parse(r#"RUN [not, json]"#);
    assert!(matches!(
        &steps[0].instr,
        Instr::Run {
            form: CmdForm::Shell(_),
            ..
        }
    ));
}
