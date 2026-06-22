//! `lightr build` handler — build-spec-r3 §5.
//!
//! Form: `lightr build [-f Dockerfile] [-t <ref>] [--engine native|ns|vz] <context>`
//!
//! Exit codes:
//!   0  — build succeeded
//!   1  — build error (die_lightr)
//!   2  — bad ref / bad engine / usage error
//!
//! Human output: `name=<ref> root=<hex16> steps=<n> cached=<n>`
//! --json mirrors BuildReport: `{"name","root","steps","cached_steps"}`
//! --explain: per-step note to stderr including non-reproducible RUN flag.

use lightr_build::{build_target, parse_dockerfile, step_reads_clock_or_net, BuildReport, Instr};
use lightr_core::validate_ref_name;
use lightr_engine::EngineKind;
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

// ── JSON mirror of BuildReport ────────────────────────────────────────────────

#[derive(Serialize)]
struct BuildJson {
    name: String,
    root: String,
    steps: u64,
    cached_steps: u64,
}

fn print_report(report: &BuildReport, json: bool) {
    if json {
        let out = BuildJson {
            name: report.name.clone(),
            root: report.root.to_hex(),
            steps: report.steps,
            cached_steps: report.cached_steps,
        };
        println!(
            "{}",
            serde_json::to_string(&out).expect("serialize build report")
        );
    } else {
        let hex = report.root.to_hex();
        let short = &hex[..16];
        println!(
            "name={} root={} steps={} cached={}",
            report.name, short, report.steps, report.cached_steps
        );
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn run(
    context: &str,
    dockerfile: Option<&str>,
    name: &str,
    engine_str: &str,
    build_arg: &[String],
    json: bool,
    explain: bool,
    target: Option<&str>,
) -> i32 {
    // Validate ref name — exit 2 on invalid
    if let Err(e) = validate_ref_name(name) {
        return die_lightr(&e);
    }

    // Parse engine kind — bad value ⇒ exit 2
    let engine_kind = match engine_str.parse::<EngineKind>() {
        Ok(k) => k,
        Err(e) => return die_lightr(&e),
    };

    // Parse `--build-arg NAME=VALUE` into (name, value) pairs. A bare `NAME`
    // (no `=`) means "pass this from the environment" in Docker; we resolve it
    // from the process env (NAME=$NAME), and drop it if unset — Docker also
    // drops a bare --build-arg whose env var is unset.
    let mut build_args: Vec<(String, String)> = Vec::new();
    for spec in build_arg {
        if let Some((k, v)) = spec.split_once('=') {
            build_args.push((k.to_string(), v.to_string()));
        } else if let Ok(v) = std::env::var(spec) {
            build_args.push((spec.to_string(), v));
        }
    }

    let context_path = std::path::Path::new(context);

    // Resolve dockerfile: -f <path> or <context>/Dockerfile
    let df_buf;
    let dockerfile_path = if let Some(f) = dockerfile {
        std::path::Path::new(f)
    } else {
        df_buf = context_path.join("Dockerfile");
        df_buf.as_path()
    };

    // --explain: parse dockerfile and emit per-step notes before building
    if explain {
        eprintln!(
            "lightr: build --explain: engine={engine_str} context={context} dockerfile={}",
            dockerfile_path.display()
        );
        eprintln!(
            "lightr: build --explain: native engine — no filesystem isolation (stated loudly)"
        );
        // Parse to get steps for annotation
        if let Ok(text) = std::fs::read_to_string(dockerfile_path) {
            if let Ok(steps) = parse_dockerfile(&text) {
                for (i, step) in steps.iter().enumerate() {
                    if let Instr::Run { argv, .. } = &step.instr {
                        if step_reads_clock_or_net(argv) {
                            eprintln!(
                                "lightr: build --explain: step {}: RUN reads clock/net — not reproducible: {:?}",
                                i + 1,
                                argv
                            );
                        } else {
                            eprintln!(
                                "lightr: build --explain: step {}: RUN (reproducible)",
                                i + 1
                            );
                        }
                    } else {
                        eprintln!(
                            "lightr: build --explain: step {}: {}",
                            i + 1,
                            step.raw.split_ascii_whitespace().next().unwrap_or("?")
                        );
                    }
                }
            }
        }
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // Emit "native engine" warning
    if engine_kind == EngineKind::Native {
        eprintln!("lightr: build: native engine — no filesystem isolation");
    }

    let report = match build_target(
        context_path,
        dockerfile_path,
        name,
        engine_kind,
        &store,
        &build_args,
        target,
    ) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    print_report(&report, json);
    0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Bad ref name ⇒ exit 2
    #[test]
    fn build_bad_ref_exits_2() {
        let code = super::run(
            "/some/ctx",
            None,
            "INVALID-REF",
            "native",
            &[],
            false,
            false,
            None,
        );
        assert_eq!(code, 2, "uppercase ref must exit 2");
    }

    /// Bad engine string ⇒ exit 2
    #[test]
    fn build_bad_engine_exits_2() {
        // validate_ref_name must pass first — use a valid name
        let code = super::run(
            "/some/ctx",
            None,
            "my-ref",
            "bogus-engine",
            &[],
            false,
            false,
            None,
        );
        assert_eq!(code, 2, "bad engine must exit 2");
    }

    /// Empty ref name ⇒ exit 2
    #[test]
    fn build_empty_ref_exits_2() {
        let code = super::run("/some/ctx", None, "", "native", &[], false, false, None);
        assert_eq!(code, 2, "empty ref must exit 2");
    }
}
