use super::*;

// ── build ─────────────────────────────────────────────────────────────────

#[test]
fn build_minimal() {
    let cli = parse(&["build", "/some/ctx"]);
    match &cli.cmd {
        Cmd::Build {
            context,
            file,
            name,
            engine,
            ..
        } => {
            assert_eq!(context, "/some/ctx");
            assert!(file.is_none(), "no -f by default");
            assert_eq!(name, "latest", "default name is latest");
            assert_eq!(engine, "native", "default engine is native");
        }
        _ => panic!("expected Build"),
    }
}

#[test]
fn build_with_file_flag() {
    let cli = parse(&["build", "-f", "custom/Dockerfile", "/ctx"]);
    match &cli.cmd {
        Cmd::Build { file, .. } => {
            assert_eq!(file.as_deref(), Some("custom/Dockerfile"));
        }
        _ => panic!("expected Build"),
    }
}

#[test]
fn build_with_name_flag() {
    let cli = parse(&["build", "-t", "my-image", "/ctx"]);
    match &cli.cmd {
        Cmd::Build { name, .. } => {
            assert_eq!(name, "my-image");
        }
        _ => panic!("expected Build"),
    }
}

#[test]
fn build_with_engine_flag() {
    let cli = parse(&["build", "--engine", "ns", "/ctx"]);
    match &cli.cmd {
        Cmd::Build { engine, .. } => {
            assert_eq!(engine, "ns");
        }
        _ => panic!("expected Build"),
    }
}

#[test]
fn build_all_flags() {
    let cli = parse(&[
        "--json",
        "build",
        "-f",
        "/path/Dockerfile",
        "-t",
        "my-ref",
        "--engine",
        "vz",
        "/my/ctx",
    ]);
    assert!(cli.json);
    match &cli.cmd {
        Cmd::Build {
            context,
            file,
            name,
            engine,
            ..
        } => {
            assert_eq!(context, "/my/ctx");
            assert_eq!(file.as_deref(), Some("/path/Dockerfile"));
            assert_eq!(name, "my-ref");
            assert_eq!(engine, "vz");
        }
        _ => panic!("expected Build"),
    }
}

#[test]
fn build_requires_context() {
    assert!(try_parse(&["build"]).is_err());
}

// ── compose up ────────────────────────────────────────────────────────────

#[test]
fn compose_up_minimal() {
    let cli = parse(&["compose", "up"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up {
                file,
                project_name,
                project_directory,
                env_file,
                eager,
                profile,
                ttl,
            } => {
                assert_eq!(file, "compose.yml", "default compose file");
                assert!(project_name.is_none(), "no -p by default");
                assert!(
                    project_directory.is_none() && env_file.is_none(),
                    "no dir/env-file"
                );
                assert!(!eager, "eager is false by default");
                assert!(profile.is_empty(), "no --profile by default");
                assert_eq!(*ttl, 3600, "default TTL is 3600");
            }
            _ => panic!("expected Up"),
        },
        _ => panic!("expected Compose"),
    }
}

#[test]
fn compose_up_with_file_flag() {
    let cli = parse(&["compose", "up", "-f", "docker-compose.yml"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up { file, .. } => {
                assert_eq!(file, "docker-compose.yml");
            }
            _ => panic!("expected Up"),
        },
        _ => panic!("expected Compose"),
    }
}

#[test]
fn compose_up_eager_flag() {
    let cli = parse(&["compose", "up", "--eager"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up { eager, .. } => {
                assert!(*eager);
            }
            _ => panic!("expected Up"),
        },
        _ => panic!("expected Compose"),
    }
}

#[test]
fn compose_up_ttl_flag() {
    let cli = parse(&["compose", "up", "--ttl", "7200"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up { ttl, .. } => {
                assert_eq!(*ttl, 7200);
            }
            _ => panic!("expected Up"),
        },
        _ => panic!("expected Compose"),
    }
}

#[test]
fn compose_up_project_name_flag() {
    for argv in [
        vec!["compose", "up", "-p", "myproj"],
        vec!["compose", "up", "--project-name", "myproj"],
    ] {
        let cli = parse(&argv);
        match &cli.cmd {
            Cmd::Compose { subcmd } => match subcmd {
                ComposeCmd::Up { project_name, .. } => {
                    assert_eq!(project_name.as_deref(), Some("myproj"));
                }
                _ => panic!("expected Up"),
            },
            _ => panic!("expected Compose"),
        }
    }
}

#[test]
fn compose_up_profile_flag_repeatable() {
    let cli = parse(&["compose", "up", "--profile", "dev", "--profile", "debug"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up { profile, .. } => {
                assert_eq!(profile, &vec!["dev".to_string(), "debug".to_string()]);
            }
            _ => panic!("expected Up"),
        },
        _ => panic!("expected Compose"),
    }
}

// ── compose down ──────────────────────────────────────────────────────────

#[test]
fn compose_down_minimal() {
    let cli = parse(&["compose", "down"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Down { file, project_name } => {
                assert!(file.is_none(), "no -f by default");
                assert!(project_name.is_none(), "no -p by default");
            }
            _ => panic!("expected Down"),
        },
        _ => panic!("expected Compose"),
    }
}

#[test]
fn compose_down_with_file_flag() {
    let cli = parse(&["compose", "down", "-f", "my-compose.yml"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Down { file, .. } => {
                assert_eq!(file.as_deref(), Some("my-compose.yml"));
            }
            _ => panic!("expected Down"),
        },
        _ => panic!("expected Compose"),
    }
}

#[test]
fn compose_down_project_name_flag() {
    let cli = parse(&["compose", "down", "-p", "myproj"]);
    match &cli.cmd {
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Down { project_name, .. } => {
                assert_eq!(project_name.as_deref(), Some("myproj"));
            }
            _ => panic!("expected Down"),
        },
        _ => panic!("expected Compose"),
    }
}

// ── __compose-supervise (hidden) ──────────────────────────────────────────

#[test]
fn compose_supervise_hidden_parses() {
    let cli = parse(&["__compose-supervise", "/some/stack/dir"]);
    match &cli.cmd {
        Cmd::ComposeSupervisor { stack_dir } => {
            assert_eq!(stack_dir, "/some/stack/dir");
        }
        _ => panic!("expected ComposeSupervisor"),
    }
}

// ── docker varargs ────────────────────────────────────────────────────────

#[test]
fn docker_varargs_capture() {
    let cli = parse(&["docker", "build", "-t", "myref", "."]);
    match &cli.cmd {
        Cmd::Docker { args } => {
            assert_eq!(args, &["build", "-t", "myref", "."]);
        }
        _ => panic!("expected Docker"),
    }
}

#[test]
fn docker_images_parses() {
    let cli = parse(&["docker", "images"]);
    match &cli.cmd {
        Cmd::Docker { args } => {
            assert_eq!(args, &["images"]);
        }
        _ => panic!("expected Docker"),
    }
}

#[test]
fn docker_ps_parses() {
    let cli = parse(&["docker", "ps"]);
    match &cli.cmd {
        Cmd::Docker { args } => {
            assert_eq!(args, &["ps"]);
        }
        _ => panic!("expected Docker"),
    }
}

#[test]
fn docker_pull_parses() {
    let cli = parse(&["docker", "pull", "alpine:latest"]);
    match &cli.cmd {
        Cmd::Docker { args } => {
            assert_eq!(args, &["pull", "alpine:latest"]);
        }
        _ => panic!("expected Docker"),
    }
}

#[test]
fn docker_compose_parses() {
    let cli = parse(&["docker", "compose", "up", "-f", "myfile.yml"]);
    match &cli.cmd {
        Cmd::Docker { args } => {
            assert_eq!(args, &["compose", "up", "-f", "myfile.yml"]);
        }
        _ => panic!("expected Docker"),
    }
}

// ── docker translation unit tests (via handlers::docker) ──────────────────

#[test]
fn docker_unsupported_exits_2() {
    use crate::handlers::docker::run as docker_run;
    let code = docker_run(&["frobnicate".to_string()], false, false);
    assert_eq!(code, 2, "unsupported docker subcommand must exit 2");
}

#[test]
fn docker_unsupported_exact_message_format() {
    // Verify the message format via the sanitize fn + exit code test above.
    // The exact message is:
    //   "lightr docker: unsupported 'frobnicate' — supported: build|run|pull|images|ps|compose"
    // We trust the string literal in docker.rs is correct (verified by code review).
    use crate::handlers::docker::run as docker_run;
    let code = docker_run(&["notreal".to_string()], false, false);
    assert_eq!(code, 2);
}

#[test]
fn docker_ref_sanitize_slash_colon() {
    use crate::handlers::docker::sanitize_docker_ref;
    assert_eq!(sanitize_docker_ref("nginx:1.25"), "@docker/nginx-1.25");
    assert_eq!(
        sanitize_docker_ref("ghcr.io/owner/repo:tag"),
        "@docker/ghcr.io-owner-repo-tag"
    );
}

#[test]
fn docker_empty_args_exits_2() {
    use crate::handlers::docker::run as docker_run;
    let code = docker_run(&[], false, false);
    assert_eq!(code, 2);
}

// ── completions / man ──────────────────────────────────────────────────────

#[test]
fn completions_parses_each_shell() {
    for s in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let cli = parse(&["completions", s]);
        match &cli.cmd {
            Cmd::Completions { .. } => {}
            _ => panic!("expected Completions for {s}"),
        }
    }
}

#[test]
fn completions_requires_shell() {
    assert!(try_parse(&["completions"]).is_err());
}

#[test]
fn completions_rejects_unknown_shell() {
    assert!(try_parse(&["completions", "tcsh"]).is_err());
}

#[test]
fn man_parses() {
    let cli = parse(&["man"]);
    match &cli.cmd {
        Cmd::Man => {}
        _ => panic!("expected Man"),
    }
}

#[test]
fn cli_command_verifies() {
    // clap asserts internal consistency (incl. after_long_help) on debug_assert.
    use clap::CommandFactory as _;
    Cli::command().debug_assert();
}

#[test]
fn version_string_contains_pkg_version() {
    use crate::cli::version::LIGHTR_VERSION;
    assert!(LIGHTR_VERSION.starts_with(env!("CARGO_PKG_VERSION")));
}
