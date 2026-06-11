//! `lightr run` handler — build-spec v2 §7 + build-spec-r1 §4.
//!
//! Exit = child's exit code.
//!
//! Stderr memo marker BEFORE streaming outputs:
//!   `lightr: memo HIT key=<hex16>` or `lightr: memo MISS key=<hex16>`
//!
//! Streaming: write stdout bytes to stdout, stderr bytes to stderr (raw, lossless).
//!
//! --json: raw child streams still flow; a JSON object `{"key","hit","exit_code"}`
//!         goes to a final line on STDERR prefixed `lightr-json: ` (machine readable
//!         without corrupting child stdout). exit = outcome.exit_code.
//!
//! --explain: extra stderr lines prefixed `lightr: explain `
//!   for run: the key composition counts (inputs n, argv n, env n, os-arch).
//!
//! --detach: spawn a detached run; print id=<handle.id>; exit 0.
//! --mount REF:TARGET: mount a ref into the run's cwd at TARGET (relative).

use std::io::Write;

use lightr_core::validate_ref_name;
use lightr_run::{run_memoized, spawn_detached, Mount, RunSpec};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_internal;

#[derive(Serialize)]
struct RunJson {
    key: String,
    hit: bool,
    exit_code: i32,
}

/// Parse a raw "ref:target" mount string into (ref_name, target).
/// Returns Err(exit_code) on validation failure (already printed to stderr).
fn parse_mount(raw: &str) -> Result<Mount, i32> {
    // Split on FIRST ':' only
    let colon = raw.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid --mount value (missing ':'): {raw}");
        2i32
    })?;
    let ref_name = &raw[..colon];
    let target = &raw[colon + 1..];

    // Validate ref name
    if let Err(e) = validate_ref_name(ref_name) {
        eprintln!("lightr: invalid mount ref name: {e}");
        return Err(2);
    }

    // Validate target is relative (not absolute)
    if target.starts_with('/') {
        eprintln!("lightr: mount target must be relative, got: {target}");
        return Err(2);
    }

    Ok(Mount {
        ref_name: ref_name.to_string(),
        target: target.to_string(),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    dir: &str,
    inputs: &[String],
    env_keys: &[String],
    command: &[String],
    json: bool,
    explain: bool,
    detach: bool,
    mounts_raw: &[String],
) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_internal(&e),
    };

    // Parse mounts
    let mut mounts: Vec<Mount> = Vec::new();
    for raw in mounts_raw {
        match parse_mount(raw) {
            Ok(m) => mounts.push(m),
            Err(code) => return code,
        }
    }

    let cwd = std::path::PathBuf::from(dir);

    let input_paths: Vec<std::path::PathBuf> = if inputs.is_empty() {
        vec![cwd.clone()]
    } else {
        inputs.iter().map(std::path::PathBuf::from).collect()
    };

    let spec = RunSpec {
        cwd,
        inputs: input_paths,
        command: command.to_vec(),
        env_keys: env_keys.to_vec(),
        mounts,
    };

    // Detach path: spawn detached and print the run id
    if detach {
        match spawn_detached(&spec, &store) {
            Ok(handle) => {
                println!("id={}", handle.id);
                return 0;
            }
            Err(e) => return die_internal(&e),
        }
    }

    if explain {
        let os_arch = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        eprintln!(
            "lightr: explain run: inputs={} argv={} env={} os-arch={}",
            spec.inputs.len(),
            spec.command.len(),
            spec.env_keys.len(),
            os_arch
        );
    }

    let outcome = match run_memoized(&spec, &store) {
        Ok(o) => o,
        Err(e) => return die_internal(&e),
    };

    let hex = outcome.key.to_hex();
    let short = &hex[..16];
    let hit_str = if outcome.hit { "HIT" } else { "MISS" };
    eprintln!("lightr: memo {hit_str} key={short}");

    // Stream stdout then stderr raw (lossless).
    {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(&outcome.stdout).ok();
    }
    {
        let stderr = std::io::stderr();
        let mut err = stderr.lock();
        err.write_all(&outcome.stderr).ok();
    }

    if json {
        let obj = RunJson {
            key: hex.clone(),
            hit: outcome.hit,
            exit_code: outcome.exit_code,
        };
        eprintln!(
            "lightr-json: {}",
            serde_json::to_string(&obj).expect("serialize run")
        );
    }

    outcome.exit_code
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::parse_mount;

    #[test]
    fn mount_parse_splits_on_first_colon() {
        let m = parse_mount("myref:some/target").expect("should parse");
        assert_eq!(m.ref_name, "myref");
        assert_eq!(m.target, "some/target");
    }

    #[test]
    fn mount_parse_splits_on_first_colon_extra_colons() {
        // "ref:sub:extra" → ref_name="ref", target="sub:extra" (split on FIRST colon)
        let m = parse_mount("ref:sub:extra").expect("should parse");
        assert_eq!(m.ref_name, "ref");
        assert_eq!(m.target, "sub:extra");
    }

    #[test]
    fn mount_rejects_absolute_target() {
        let result = parse_mount("ref:/abs/path");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), 2);
    }

    #[test]
    fn mount_rejects_invalid_ref_name() {
        // Uppercase ref name is invalid
        let result = parse_mount("INVALID:target");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), 2);
    }

    #[test]
    fn mount_rejects_missing_colon() {
        let result = parse_mount("nocoton");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), 2);
    }

    #[test]
    fn mount_accepts_relative_target() {
        let m = parse_mount("valid-ref:sub/dir").expect("should parse");
        assert_eq!(m.ref_name, "valid-ref");
        assert_eq!(m.target, "sub/dir");
    }
}
