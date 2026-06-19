//! Dockerfile instruction parsing.
use lightr_core::{LightrError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instr {
    From { image_ref: String },
    Run { argv: Vec<String> },
    Copy { src: Vec<String>, dest: String },
    Env { key: String, val: String },
    Workdir { path: String },
    Cmd { argv: Vec<String> },
    Label { key: String, val: String },
}

#[derive(Clone, Debug)]
pub struct BuildStep {
    pub instr: Instr,
    pub raw: String,
}

/// Parse a Dockerfile text into a list of `BuildStep`s.
///
/// Rules:
/// - Lines ending with `\` are joined with the next line (continuation).
/// - Lines starting with `#` (after leading whitespace) are comments, skipped.
/// - Blank logical lines are skipped.
/// - Keyword is case-insensitive; content after the keyword is the payload.
/// - Unknown keywords -> `LightrError::InvalidManifest("unsupported instruction: <KW>")`.
pub fn parse_dockerfile(text: &str) -> Result<Vec<BuildStep>> {
    // Phase 1: join continuation lines
    let mut logical_lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for raw_line in text.lines() {
        if raw_line.ends_with('\\') {
            current.push_str(raw_line.trim_end_matches('\\'));
            current.push(' ');
        } else {
            current.push_str(raw_line);
            logical_lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        logical_lines.push(current);
    }

    let mut steps = Vec::new();
    for line in logical_lines {
        let trimmed = line.trim();
        // skip comments and blanks
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // keyword = first token
        let (kw, rest) = trimmed
            .split_once(|c: char| c.is_ascii_whitespace())
            .map(|(k, r)| (k, r.trim()))
            .unwrap_or((trimmed, ""));

        let instr = match kw.to_uppercase().as_str() {
            "FROM" => Instr::From {
                image_ref: rest.to_string(),
            },
            "RUN" => {
                let argv = parse_argv_or_shell(rest);
                Instr::Run { argv }
            }
            "COPY" => {
                let tokens: Vec<String> =
                    rest.split_ascii_whitespace().map(str::to_string).collect();
                if tokens.len() < 2 {
                    return Err(LightrError::InvalidManifest(
                        "COPY requires at least src dest".to_string(),
                    ));
                }
                let dest = tokens.last().unwrap().clone();
                let src = tokens[..tokens.len() - 1].to_vec();
                Instr::Copy { src, dest }
            }
            "ENV" => {
                // ENV k=v  OR  ENV k v
                if let Some((k, v)) = rest.split_once('=') {
                    Instr::Env {
                        key: k.trim().to_string(),
                        val: v.trim().to_string(),
                    }
                } else {
                    let (k, v) = rest
                        .split_once(|c: char| c.is_ascii_whitespace())
                        .map(|(a, b)| (a.trim(), b.trim()))
                        .unwrap_or((rest, ""));
                    Instr::Env {
                        key: k.to_string(),
                        val: v.to_string(),
                    }
                }
            }
            "WORKDIR" => Instr::Workdir {
                path: rest.to_string(),
            },
            "CMD" => {
                let argv = parse_argv_or_shell(rest);
                Instr::Cmd { argv }
            }
            "LABEL" => {
                // LABEL k=v  OR  LABEL k v
                if let Some((k, v)) = rest.split_once('=') {
                    Instr::Label {
                        key: k.trim().to_string(),
                        val: v.trim().to_string(),
                    }
                } else {
                    let (k, v) = rest
                        .split_once(|c: char| c.is_ascii_whitespace())
                        .map(|(a, b)| (a.trim(), b.trim()))
                        .unwrap_or((rest, ""));
                    Instr::Label {
                        key: k.to_string(),
                        val: v.to_string(),
                    }
                }
            }
            other => {
                return Err(LightrError::InvalidManifest(format!(
                    "unsupported instruction: {other}"
                )));
            }
        };
        steps.push(BuildStep {
            instr,
            raw: trimmed.to_string(),
        });
    }
    Ok(steps)
}

/// Parse exec-form JSON array `["a","b"]` or fall back to shell form
/// `["/bin/sh", "-c", rest]`.
pub(crate) fn parse_argv_or_shell(rest: &str) -> Vec<String> {
    let t = rest.trim();
    if t.starts_with('[') {
        // Try JSON parse
        if let Ok(v) = serde_json::from_str::<Vec<String>>(t) {
            return v;
        }
    }
    // shell form
    vec!["/bin/sh".to_string(), "-c".to_string(), t.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dockerfile_all_instructions() {
        let df = r#"
FROM scratch
RUN echo hello
COPY src/ /app/
ENV FOO=bar
WORKDIR /work
CMD ["sh","-c","start"]
LABEL version=1.0
"#;
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(steps.len(), 7);
        assert_eq!(
            steps[0].instr,
            Instr::From {
                image_ref: "scratch".to_string()
            }
        );
        assert_eq!(
            steps[1].instr,
            Instr::Run {
                argv: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "echo hello".to_string()
                ]
            }
        );
        if let Instr::Copy { src, dest } = &steps[2].instr {
            assert_eq!(dest, "/app/");
            assert_eq!(src, &["src/"]);
        } else {
            panic!("expected Copy")
        }
        assert_eq!(
            steps[3].instr,
            Instr::Env {
                key: "FOO".to_string(),
                val: "bar".to_string()
            }
        );
        assert_eq!(
            steps[4].instr,
            Instr::Workdir {
                path: "/work".to_string()
            }
        );
        assert_eq!(
            steps[5].instr,
            Instr::Cmd {
                argv: vec!["sh".to_string(), "-c".to_string(), "start".to_string()]
            }
        );
        assert_eq!(
            steps[6].instr,
            Instr::Label {
                key: "version".to_string(),
                val: "1.0".to_string()
            }
        );
    }

    #[test]
    fn parse_dockerfile_continuation_line() {
        let df = "RUN echo \\\n  hello world\n";
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(steps.len(), 1);
        if let Instr::Run { argv } = &steps[0].instr {
            assert!(argv.last().unwrap().contains("hello world"));
        } else {
            panic!("expected Run")
        }
    }

    #[test]
    fn parse_dockerfile_comments_and_blanks() {
        let df = "# header\n\nFROM scratch\n# comment\nRUN true\n";
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn parse_dockerfile_exec_form_run() {
        let df = r#"RUN ["/bin/sh","-c","hello"]"#;
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(
            steps[0].instr,
            Instr::Run {
                argv: vec!["/bin/sh".to_string(), "-c".to_string(), "hello".to_string()]
            }
        );
    }

    #[test]
    fn parse_dockerfile_shell_form_run() {
        let df = "RUN echo hi";
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(
            steps[0].instr,
            Instr::Run {
                argv: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "echo hi".to_string()
                ]
            }
        );
    }

    #[test]
    fn parse_dockerfile_unknown_keyword_err() {
        let df = "FROBNICATE foo\n";
        let err = parse_dockerfile(df).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported instruction"), "got: {msg}");
        assert!(msg.contains("FROBNICATE"), "got: {msg}");
    }

    #[test]
    fn parse_dockerfile_case_insensitive() {
        let df = "from scratch\nrun echo hi\n";
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0].instr, Instr::From { .. }));
        assert!(matches!(steps[1].instr, Instr::Run { .. }));
    }

    #[test]
    fn parse_dockerfile_env_kv_form() {
        let df = "ENV KEY value with spaces\n";
        let steps = parse_dockerfile(df).unwrap();
        assert_eq!(
            steps[0].instr,
            Instr::Env {
                key: "KEY".to_string(),
                val: "value with spaces".to_string()
            }
        );
    }

    #[test]
    fn parse_dockerfile_label_kv_form() {
        let df = "LABEL org.opencontainers.image.version=1.2.3\n";
        let steps = parse_dockerfile(df).unwrap();
        if let Instr::Label { key, val } = &steps[0].instr {
            assert_eq!(key, "org.opencontainers.image.version");
            assert_eq!(val, "1.2.3");
        } else {
            panic!()
        }
    }
}
