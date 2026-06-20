//! Parser unit tests, part 2 (WP-DF-01): per-instruction flags, ONBUILD,
//! HEALTHCHECK, SHELL, continuations/escape, directives. Parallel-safe.
use super::*;

fn parse(text: &str) -> Vec<BuildStep> {
    parse_dockerfile(text).unwrap()
}

// ---- FROM flags + stage ----------------------------------------------------

#[test]
fn from_with_platform_and_stage() {
    let steps = parse("FROM --platform=linux/amd64 alpine:3.19 AS builder");
    assert_eq!(
        steps[0].instr,
        Instr::From {
            image_ref: "alpine:3.19".into(),
            platform: Some("linux/amd64".into()),
            stage: Some("builder".into()),
        }
    );
}

#[test]
fn from_plain() {
    let steps = parse("FROM ubuntu");
    assert_eq!(
        steps[0].instr,
        Instr::From {
            image_ref: "ubuntu".into(),
            platform: None,
            stage: None,
        }
    );
}

#[test]
fn from_missing_image_errors() {
    assert!(parse_dockerfile("FROM --platform=linux/arm64").is_err());
}

#[test]
fn from_trailing_garbage_errors() {
    assert!(parse_dockerfile("FROM img extra").is_err());
}

// ---- COPY / ADD flags ------------------------------------------------------

#[test]
fn copy_with_from_chown_chmod() {
    let steps = parse("COPY --from=builder --chown=1000:1000 --chmod=755 /src /dst");
    assert_eq!(
        steps[0].instr,
        Instr::Copy {
            src: vec!["/src".into()],
            dest: "/dst".into(),
            from: Some("builder".into()),
            chown: Some("1000:1000".into()),
            chmod: Some("755".into()),
        }
    );
}

#[test]
fn copy_multiple_sources() {
    let steps = parse("COPY a b c /dest/");
    if let Instr::Copy {
        src, dest, from, ..
    } = &steps[0].instr
    {
        assert_eq!(src, &["a", "b", "c"]);
        assert_eq!(dest, "/dest/");
        assert!(from.is_none());
    } else {
        panic!("expected Copy");
    }
}

#[test]
fn add_with_chown() {
    let steps = parse("ADD --chown=root:root file.tar /opt");
    assert_eq!(
        steps[0].instr,
        Instr::Add {
            src: vec!["file.tar".into()],
            dest: "/opt".into(),
            chown: Some("root:root".into()),
            chmod: None,
        }
    );
}

#[test]
fn copy_requires_src_dest() {
    assert!(parse_dockerfile("COPY onlyone").is_err());
}

// ---- ENV / LABEL / ARG -----------------------------------------------------

#[test]
fn env_kv_and_space_forms() {
    assert_eq!(
        parse("ENV A=1")[0].instr,
        Instr::Env {
            key: "A".into(),
            val: "1".into()
        }
    );
    assert_eq!(
        parse("ENV KEY value with spaces")[0].instr,
        Instr::Env {
            key: "KEY".into(),
            val: "value with spaces".into()
        }
    );
}

#[test]
fn label_dotted_key() {
    if let Instr::Label { key, val } =
        &parse("LABEL org.opencontainers.image.version=1.2.3")[0].instr
    {
        assert_eq!(key, "org.opencontainers.image.version");
        assert_eq!(val, "1.2.3");
    } else {
        panic!("expected Label");
    }
}

#[test]
fn arg_with_and_without_default() {
    assert_eq!(
        parse("ARG VERSION=1.0")[0].instr,
        Instr::Arg {
            name: "VERSION".into(),
            default: Some("1.0".into())
        }
    );
    assert_eq!(
        parse("ARG TOKEN")[0].instr,
        Instr::Arg {
            name: "TOKEN".into(),
            default: None
        }
    );
}

// ---- EXPOSE / VOLUME / USER / STOPSIGNAL -----------------------------------

#[test]
fn expose_multiple_with_proto() {
    assert_eq!(
        parse("EXPOSE 80 443/tcp 53/udp")[0].instr,
        Instr::Expose {
            ports: vec!["80".into(), "443/tcp".into(), "53/udp".into()]
        }
    );
}

#[test]
fn volume_shell_and_json_forms() {
    assert_eq!(
        parse("VOLUME /data /var/log")[0].instr,
        Instr::Volume {
            paths: vec!["/data".into(), "/var/log".into()]
        }
    );
    assert_eq!(
        parse(r#"VOLUME ["/data","/cache"]"#)[0].instr,
        Instr::Volume {
            paths: vec!["/data".into(), "/cache".into()]
        }
    );
}

#[test]
fn user_and_stopsignal() {
    assert_eq!(
        parse("USER app:app")[0].instr,
        Instr::User {
            user: "app:app".into()
        }
    );
    assert_eq!(
        parse("STOPSIGNAL SIGTERM")[0].instr,
        Instr::Stopsignal {
            signal: "SIGTERM".into()
        }
    );
}

#[test]
fn empty_required_arg_errors() {
    assert!(parse_dockerfile("WORKDIR").is_err());
    assert!(parse_dockerfile("USER").is_err());
}

// ---- SHELL -----------------------------------------------------------------

#[test]
fn shell_json_required() {
    assert_eq!(
        parse(r#"SHELL ["pwsh","-Command"]"#)[0].instr,
        Instr::Shell {
            shell: vec!["pwsh".into(), "-Command".into()]
        }
    );
    assert!(parse_dockerfile("SHELL /bin/bash -c").is_err());
    assert!(parse_dockerfile("SHELL []").is_err());
}

// ---- ONBUILD ---------------------------------------------------------------

#[test]
fn onbuild_wraps_inner() {
    let steps = parse("ONBUILD RUN make");
    if let Instr::Onbuild { instr } = &steps[0].instr {
        assert!(matches!(**instr, Instr::Run { .. }));
    } else {
        panic!("expected Onbuild");
    }
}

#[test]
fn onbuild_chain_and_from_rejected() {
    assert!(parse_dockerfile("ONBUILD ONBUILD RUN x").is_err());
    assert!(parse_dockerfile("ONBUILD FROM scratch").is_err());
}

// ---- HEALTHCHECK -----------------------------------------------------------

#[test]
fn healthcheck_none() {
    assert_eq!(
        parse("HEALTHCHECK NONE")[0].instr,
        Instr::Healthcheck {
            check: Healthcheck::None
        }
    );
}

#[test]
fn healthcheck_cmd_with_opts() {
    let steps =
        parse("HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD curl -f http://localhost/");
    if let Instr::Healthcheck {
        check: Healthcheck::Cmd { opts, cmd },
    } = &steps[0].instr
    {
        assert_eq!(opts.interval.as_deref(), Some("30s"));
        assert_eq!(opts.timeout.as_deref(), Some("5s"));
        assert_eq!(opts.retries.as_deref(), Some("3"));
        assert!(opts.start_period.is_none());
        assert!(matches!(cmd, CmdForm::Shell(s) if s == "curl -f http://localhost/"));
    } else {
        panic!("expected Healthcheck::Cmd");
    }
}

#[test]
fn healthcheck_unknown_flag_errors() {
    assert!(parse_dockerfile("HEALTHCHECK --bogus=1 CMD true").is_err());
}

#[test]
fn healthcheck_requires_cmd_or_none() {
    assert!(parse_dockerfile("HEALTHCHECK something").is_err());
}

// ---- continuations / escape / directives -----------------------------------

#[test]
fn continuation_default_escape() {
    let steps = parse("RUN echo \\\n  hello world\n");
    assert_eq!(steps.len(), 1);
    if let Instr::Run { argv, .. } = &steps[0].instr {
        assert!(argv.last().unwrap().contains("hello world"));
    } else {
        panic!("expected Run");
    }
}

#[test]
fn multiline_json_array_continuation() {
    let steps = parse("CMD [\"sh\", \\\n  \"-c\", \\\n  \"echo hi\"]\n");
    assert_eq!(
        steps[0].instr,
        Instr::Cmd {
            argv: vec!["sh".into(), "-c".into(), "echo hi".into()],
            form: CmdForm::Exec(vec!["sh".into(), "-c".into(), "echo hi".into()]),
        }
    );
}

#[test]
fn comment_inside_continuation_dropped() {
    // A comment line within a continuation is dropped (Docker behavior).
    let steps = parse("RUN echo a \\\n# this is a comment\n  && echo b\n");
    if let Instr::Run {
        form: CmdForm::Shell(s),
        ..
    } = &steps[0].instr
    {
        assert!(s.contains("echo a"), "got: {s}");
        assert!(s.contains("echo b"), "got: {s}");
        assert!(!s.contains("comment"), "comment must be dropped: {s}");
    } else {
        panic!("expected Run shell form");
    }
}

#[test]
fn escape_directive_backtick() {
    let df = "# escape=`\nRUN echo a `\n  b\n";
    let (dirs, steps) = parse_dockerfile_full(df).unwrap();
    assert_eq!(dirs.escape, Some('`'));
    if let Instr::Run {
        form: CmdForm::Shell(s),
        ..
    } = &steps[0].instr
    {
        assert!(s.contains("a") && s.contains("b"), "got: {s}");
    } else {
        panic!("expected Run");
    }
}

#[test]
fn syntax_directive_captured() {
    let df = "# syntax=docker/dockerfile:1\nFROM scratch\n";
    let (dirs, steps) = parse_dockerfile_full(df).unwrap();
    assert_eq!(dirs.syntax.as_deref(), Some("docker/dockerfile:1"));
    assert_eq!(steps.len(), 1);
}

#[test]
fn directive_block_ends_at_first_instruction() {
    // An `escape` directive AFTER an instruction is just a comment, ignored.
    let df = "FROM scratch\n# escape=`\n";
    let (dirs, _) = parse_dockerfile_full(df).unwrap();
    assert_eq!(dirs.escape, None);
}

#[test]
fn all_eighteen_recognized() {
    // One of each known instruction parses without error (taproot completeness).
    let df = r#"# syntax=docker/dockerfile:1
FROM scratch AS base
ARG V=1
RUN true
CMD ["true"]
ENTRYPOINT ["/bin/true"]
LABEL a=b
EXPOSE 8080
ENV E=v
ADD x /y
COPY p /q
VOLUME /data
USER nobody
WORKDIR /w
STOPSIGNAL SIGINT
HEALTHCHECK NONE
SHELL ["/bin/sh","-c"]
ONBUILD RUN echo deferred
"#;
    let (dirs, steps) = parse_dockerfile_full(df).unwrap();
    assert_eq!(dirs.syntax.as_deref(), Some("docker/dockerfile:1"));
    assert_eq!(steps.len(), 17, "17 instructions (directive is not a step)");
}
