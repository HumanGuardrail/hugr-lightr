//! lightr-run — frozen contract: build-spec v2 §6.
//! Memo key, native exec, replay. Bodies are WP-4.

use lightr_core::{Digest, LightrError, Result, OUTPUT_CAP_BYTES};
use lightr_index::{scan, Index};
use lightr_store::Store;
use std::path::PathBuf;

pub struct RunSpec {
    pub cwd: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
}

pub struct RunOutcome {
    pub key: Digest,
    pub hit: bool,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

// ---------------------------------------------------------------------------
// AC record format "LRR1":
//   4B magic b"LRR1"
//   4B i32  exit_code  (LE)
//  32B      stdout digest
//  32B      stderr digest
// Total: 72 bytes
// ---------------------------------------------------------------------------

const AC_MAGIC: &[u8; 4] = b"LRR1";
const AC_RECORD_LEN: usize = 4 + 4 + 32 + 32; // 72

fn encode_ac_record(exit_code: i32, stdout_d: &Digest, stderr_d: &Digest) -> Vec<u8> {
    let mut buf = Vec::with_capacity(AC_RECORD_LEN);
    buf.extend_from_slice(AC_MAGIC);
    buf.extend_from_slice(&exit_code.to_le_bytes());
    buf.extend_from_slice(&stdout_d.0);
    buf.extend_from_slice(&stderr_d.0);
    buf
}

fn decode_ac_record(bytes: &[u8]) -> Option<(i32, Digest, Digest)> {
    if bytes.len() != AC_RECORD_LEN {
        return None;
    }
    if &bytes[..4] != AC_MAGIC {
        return None;
    }
    let exit_code = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let mut stdout_raw = [0u8; 32];
    let mut stderr_raw = [0u8; 32];
    stdout_raw.copy_from_slice(&bytes[8..40]);
    stderr_raw.copy_from_slice(&bytes[40..72]);
    Some((exit_code, Digest(stdout_raw), Digest(stderr_raw)))
}

// ---------------------------------------------------------------------------
// Key assembly — exact order per contract:
//   update(b"lightr/run/v1\0")
//   for each input (spec.inputs; if empty use [spec.cwd]) in GIVEN order:
//       canonicalize against cwd
//       scan(path, &mut Index::load_for(path)?)
//       update(rel-path-as-given bytes + b"\0" + manifest.digest().0)
//   for each arg in spec.command:
//       update((arg.len() as u64).to_le_bytes() + arg bytes)
//   env: collect spec.env_keys sorted, for each present in std::env:
//       update(key + b"=" + value + b"\0")
//       absent keys: update(key + b"\x01")
//   update(std::env::consts::OS + "-" + std::env::consts::ARCH)
//   key = finalize
// ---------------------------------------------------------------------------

fn build_key(spec: &RunSpec) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();

    // Domain separator
    hasher.update(b"lightr/run/v1\0");

    // Input manifests
    let inputs: Vec<&PathBuf> = if spec.inputs.is_empty() {
        vec![&spec.cwd]
    } else {
        spec.inputs.iter().collect()
    };

    for input_path in inputs {
        // Canonicalize against cwd
        let abs_path = if input_path.is_absolute() {
            input_path.clone()
        } else {
            spec.cwd.join(input_path)
        };
        let canonical = abs_path.canonicalize().map_err(LightrError::Io)?;

        // Scan to get the manifest
        let mut index = Index::load_for(&canonical)?;
        let report = scan(&canonical, &mut index)?;

        // Use rel-path-as-given bytes
        let rel_path_bytes = input_path.as_os_str().as_encoded_bytes();
        hasher.update(rel_path_bytes);
        hasher.update(b"\0");
        hasher.update(&report.manifest.digest().0);
    }

    // Command args
    for arg in &spec.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    // Env keys — sorted
    let mut sorted_keys = spec.env_keys.clone();
    sorted_keys.sort();
    for key in &sorted_keys {
        if let Some(val) = std::env::var_os(key) {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_encoded_bytes());
            hasher.update(b"\0");
        } else {
            // Absent key: contribute key + \x01
            hasher.update(key.as_bytes());
            hasher.update(b"\x01");
        }
    }

    // Target triple: OS-ARCH
    let triple = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    hasher.update(triple.as_bytes());

    Ok(Digest(*hasher.finalize().as_bytes()))
}

pub fn run_memoized(spec: &RunSpec, store: &Store) -> Result<RunOutcome> {
    let key = build_key(spec)?;

    // --- Hit path ---
    if let Ok(Some(record_bytes)) = store.ac_get(&key) {
        if let Some((exit_code, stdout_d, stderr_d)) = decode_ac_record(&record_bytes) {
            // Fetch stored outputs; any NotFound or Integrity => treat as miss
            let stdout_res = store.get_bytes(&stdout_d);
            let stderr_res = store.get_bytes(&stderr_d);
            match (stdout_res, stderr_res) {
                (Ok(stdout), Ok(stderr)) => {
                    return Ok(RunOutcome {
                        key,
                        hit: true,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                _ => {
                    // Fall through to miss path
                }
            }
        }
        // Corrupt record or fetch failure: treat as miss, fall through
    }

    // --- Miss path ---
    if spec.command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let output = std::process::Command::new(&spec.command[0])
        .args(&spec.command[1..])
        .current_dir(&spec.cwd)
        // Full parent env passthrough (env_keys selects key material only)
        .output()
        .map_err(LightrError::Io)?;

    let exit_code = {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            output
                .status
                .code()
                .unwrap_or_else(|| 128 + output.status.signal().unwrap_or(0))
        }
        #[cfg(not(unix))]
        {
            output.status.code().unwrap_or(1)
        }
    };

    let stdout = output.stdout;
    let stderr = output.stderr;

    // Memoize ONLY if exit_code == 0 AND both outputs <= OUTPUT_CAP_BYTES
    if exit_code == 0 && stdout.len() <= OUTPUT_CAP_BYTES && stderr.len() <= OUTPUT_CAP_BYTES {
        // Store output objects and AC record
        let stdout_d = store.put_bytes(&stdout)?;
        let stderr_d = store.put_bytes(&stderr)?;
        let record = encode_ac_record(exit_code, &stdout_d, &stderr_d);
        store.ac_put(&key, &record)?;
    }

    Ok(RunOutcome {
        key,
        hit: false,
        exit_code,
        stdout,
        stderr,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lightr_store::Store;
    use std::fs;
    use std::io::Write;

    fn make_store(dir: &std::path::Path) -> Store {
        Store::open(dir.join("store")).expect("store open")
    }

    fn make_spec(cwd: &std::path::Path, command: Vec<&str>) -> RunSpec {
        RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: command.into_iter().map(|s| s.to_string()).collect(),
            env_keys: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // key_stability: same spec twice => same key via two scans
    // -----------------------------------------------------------------------
    #[test]
    fn key_stability() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // Create a file so the scan has something to digest
        fs::write(cwd.join("file.txt"), b"hello").unwrap();

        let spec = make_spec(cwd, vec!["/bin/echo", "hello"]);
        let k1 = build_key(&spec).expect("key1");
        let k2 = build_key(&spec).expect("key2");
        assert_eq!(k1.0, k2.0, "same spec must produce same key");
    }

    // -----------------------------------------------------------------------
    // key_changes_when_input_file_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_input_file_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("data.txt"), b"version1").unwrap();

        let spec = make_spec(cwd, vec!["/bin/echo", "x"]);
        let k1 = build_key(&spec).expect("k1");

        fs::write(cwd.join("data.txt"), b"version2").unwrap();
        let k2 = build_key(&spec).expect("k2");

        assert_ne!(
            k1.0, k2.0,
            "key must change when input file content changes"
        );
    }

    // -----------------------------------------------------------------------
    // key_changes_when_arg_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_arg_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("f.txt"), b"data").unwrap();

        let spec1 = make_spec(cwd, vec!["/bin/echo", "argA"]);
        let spec2 = make_spec(cwd, vec!["/bin/echo", "argB"]);

        let k1 = build_key(&spec1).expect("k1");
        let k2 = build_key(&spec2).expect("k2");
        assert_ne!(k1.0, k2.0, "key must change when args change");
    }

    // -----------------------------------------------------------------------
    // key_changes_when_selected_env_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_selected_env_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("f.txt"), b"data").unwrap();

        // Env var present
        std::env::set_var("LIGHTR_TEST_VAR_KCW", "valueA");
        let spec1 = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "x".to_string()],
            env_keys: vec!["LIGHTR_TEST_VAR_KCW".to_string()],
        };
        let k1 = build_key(&spec1).expect("k1");

        std::env::set_var("LIGHTR_TEST_VAR_KCW", "valueB");
        let k2 = build_key(&spec1).expect("k2");

        std::env::remove_var("LIGHTR_TEST_VAR_KCW");
        assert_ne!(
            k1.0, k2.0,
            "key must change when selected env value changes"
        );
    }

    // -----------------------------------------------------------------------
    // miss_then_hit: run twice; side-effect file written once; 2nd run is HIT
    // -----------------------------------------------------------------------
    #[test]
    fn miss_then_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let store = make_store(cwd);

        // Side-effect file outside inputs
        let side_effect = tmp.path().join("side_effect.txt");

        // Command: append one line to side_effect
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo hit >> {}", side_effect.display()),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
        };

        // First run: miss
        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit, "first run must be miss");
        assert_eq!(out1.exit_code, 0);

        // Side-effect written once
        let contents1 = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count1 = contents1.lines().count();
        assert_eq!(line_count1, 1, "side effect written once after first run");

        // Second run: hit — command should NOT execute again
        let out2 = run_memoized(&spec, &store).expect("run2");
        assert!(out2.hit, "second run must be hit");
        assert_eq!(out2.exit_code, 0);
        assert_eq!(out1.stdout, out2.stdout, "replayed stdout must match");

        // Side-effect still only 1 line (command did not re-execute)
        let contents2 = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count2 = contents2.lines().count();
        assert_eq!(line_count2, 1, "side effect must not be re-written on hit");
    }

    // -----------------------------------------------------------------------
    // exit_nonzero_never_memoized: exit-7 cmd twice, both MISS, side-effect written twice
    // -----------------------------------------------------------------------
    #[test]
    fn exit_nonzero_never_memoized() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let store = make_store(cwd);

        let side_effect = tmp.path().join("side_effect_fail.txt");

        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo fail >> {}; exit 7", side_effect.display()),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
        };

        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit, "first run must be miss");
        assert_eq!(out1.exit_code, 7, "exit code must be 7");

        let out2 = run_memoized(&spec, &store).expect("run2");
        assert!(!out2.hit, "second run must also be miss (not memoized)");
        assert_eq!(out2.exit_code, 7, "exit code must still be 7");

        // Side-effect written twice (command executed both times)
        let contents = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count = contents.lines().count();
        assert_eq!(line_count, 2, "side effect must be written twice");
    }

    // -----------------------------------------------------------------------
    // output_cap_not_memoized: >5MiB stdout not memoized
    // -----------------------------------------------------------------------
    #[test]
    fn output_cap_not_memoized() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let store = make_store(cwd);

        let side_effect = tmp.path().join("side_effect_cap.txt");

        // Generate >5MiB of stdout output (5 * 1024 * 1024 + 1 bytes)
        // We use a shell one-liner: dd a large file and cat it
        let large_file = tmp.path().join("large.bin");
        {
            // Write 5MiB + 1 byte file
            let mut f = fs::File::create(&large_file).unwrap();
            let buf = vec![b'x'; OUTPUT_CAP_BYTES + 1];
            f.write_all(&buf).unwrap();
        }

        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "cat {} && echo side >> {}",
                large_file.display(),
                side_effect.display()
            ),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
        };

        // First run: miss (output too large)
        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit, "first run must be miss");
        assert_eq!(out1.exit_code, 0);
        assert!(
            out1.stdout.len() > OUTPUT_CAP_BYTES,
            "stdout must exceed cap"
        );

        // Second run: also miss (output was not memoized)
        let out2 = run_memoized(&spec, &store).expect("run2");
        assert!(
            !out2.hit,
            "second run must also be miss (output cap exceeded)"
        );

        // Side-effect written twice (command executed both times)
        let contents = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count = contents.lines().count();
        assert_eq!(
            line_count, 2,
            "side effect must be written twice when output cap exceeded"
        );
    }

    // -----------------------------------------------------------------------
    // corrupt_ac_record_treated_as_miss: flip 1 byte in AC record => miss not error
    // -----------------------------------------------------------------------
    #[test]
    fn corrupt_ac_record_treated_as_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let store = make_store(cwd);

        let side_effect = tmp.path().join("side_effect_corrupt.txt");

        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo ok >> {}", side_effect.display()),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
        };

        // First run: miss, gets memoized
        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit);
        assert_eq!(out1.exit_code, 0);

        // Build the key to corrupt the AC record directly
        let key = build_key(&spec).expect("key");

        // Read the AC record, corrupt it, write it back
        let record = store.ac_get(&key).expect("ac_get").expect("record present");
        let mut corrupt = record.clone();
        // Flip a byte in the magic to corrupt it
        corrupt[0] ^= 0xFF;
        store.ac_put(&key, &corrupt).expect("ac_put");

        // Third run: corrupt record => miss (not error)
        let out3 = run_memoized(&spec, &store).expect("run3 must not error");
        assert!(!out3.hit, "corrupt AC record must be treated as miss");
        assert_eq!(out3.exit_code, 0);

        // Side-effect written twice (run1 + run3, run2 was just setup)
        let contents = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count = contents.lines().count();
        assert_eq!(line_count, 2, "command executed on miss and after corrupt");
    }
}
