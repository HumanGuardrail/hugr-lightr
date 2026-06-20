//! Memo key assembly and memoized run entry points:
//! validate_mount_target, assemble_key, build_key, run_memoized,
//! run_memoized_with, predict.
//!
//! # R-KEY partition (parity-contract.md §0)
//!
//! The RUN-key domain partition the campaign enforces (env_explicit fold WIRED
//! by WP-RC-1 — `contribute_env_explicit`):
//!
//! - **IN the run key:** explicit env (`env_explicit`, folded `key=value\0` —
//!   WP-RC-1), image ENV, CAS-ref content, ro-bind fingerprint. (Build-only
//!   inputs — workdir/user/entrypoint + interp text — key in the BUILD domain.)
//! - **OUT of the run key (runtime):** caps, restart, health, ports, labels,
//!   network, tty, workdir/user/hostname at RUN time, and the discovery `env`
//!   channel (LEAD ARBITRATION env-split: `env` is UNKEYED; only `env_explicit`
//!   is keyed).
//! - **NON-memoizable (force-MISS, no AC write):** rw-bind, named, anon, tmpfs
//!   mounts.
//!
//! ## Per-domain v2 rule (LEAD ARBITRATION)
//!
//! The domain tag is bumped PER-KEY-DOMAIN, and ONLY when that key's input
//! format changes. The RUN key STAYS `lightr/run/v1` (env format unchanged by
//! the freeze-gate). The BUILD key bumps to `lightr/build/v2` at WP-DF-13 (when
//! interp text + workdir/user/entrypoint enter it) — see build/memo.rs. Each
//! bump is a documented one-time Action-Cache invalidation.

use lightr_core::{Digest, LightrError, Result, OUTPUT_CAP_BYTES};
use lightr_index::{scan, Index};
use lightr_store::Store;

use super::ac::{decode_ac_record, encode_ac_record};
use super::types::{RunOutcome, RunSpec};

// ---------------------------------------------------------------------------
// Mount target validation
// ---------------------------------------------------------------------------

/// WP-RC-1 (R-KEY): fold the user's explicit env (`env_explicit`) into the run
/// key — the ONLY env channel in the key (the discovery `env` stays UNKEYED;
/// `env_keys` is a separate var-NAME mechanism). Pairs are sorted so CLI order
/// never changes the key, but a different KEY/VALUE always does (no false hit).
/// A `\x03env_explicit\0` domain tag prefixes the block so it can't collide
/// with the `env_keys` folds above; an EMPTY slice writes nothing, so a run
/// with no `-e`/`--env-file` keys byte-identically to before (behavior-preserved).
fn contribute_env_explicit(hasher: &mut blake3::Hasher, env_explicit: &[(String, String)]) {
    if env_explicit.is_empty() {
        return;
    }
    let mut sorted = env_explicit.to_vec();
    sorted.sort();
    hasher.update(b"\x03env_explicit\0");
    for (k, v) in &sorted {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\0");
    }
}

pub(super) fn validate_mount_target(t: &str) -> Result<()> {
    use std::path::Path;
    let p = Path::new(t);
    // Must be relative
    if p.is_absolute() {
        return Err(LightrError::InvalidRef(format!(
            "mount target escapes cwd: {t}"
        )));
    }
    // Must not contain ".." components
    for component in p.components() {
        if component == std::path::Component::ParentDir {
            return Err(LightrError::InvalidRef(format!(
                "mount target escapes cwd: {t}"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Key assembly — exact order per contract:
//   update(b"lightr/run/v1\0")
//   inputs (spec.inputs; if empty use [spec.cwd]) in GIVEN order: canonicalize
//     vs cwd, scan, update(rel-path bytes + b"\0" + manifest.digest().0)
//   args: for each in spec.command: update(len.to_le_bytes() + arg bytes)
//   env_keys (sorted): present → update(key + b"=" + value + b"\0");
//     absent → update(key + b"\x01")
//   env_explicit (WP-RC-1): contribute_env_explicit (sorted, \x03-tagged block)
//   triple: update(OS + "-" + ARCH)
//   mounts (in order): validate target, update(ref_name + [0x02] + root digest)
//   key = finalize
// ---------------------------------------------------------------------------

/// Shared private key-assembly fn. `hydrate_mounts` controls whether to
/// actually hydrate into cwd (true for run_memoized, false for predict).
pub(super) fn assemble_key(spec: &RunSpec, store: &Store, hydrate_mounts: bool) -> Result<Digest> {
    use std::path::PathBuf;

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

    // WP-RC-1 (R-KEY): explicit env, folded after the discovery `env_keys` and
    // before the triple; empty ⇒ no-op (behavior-preserving).
    contribute_env_explicit(&mut hasher, &spec.env_explicit);

    // Target triple: OS-ARCH
    let triple = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    hasher.update(triple.as_bytes());

    // Mounts key contribution
    for mount in &spec.mounts {
        validate_mount_target(&mount.target)?;

        if hydrate_mounts {
            // Hydrate the mount into cwd/target
            let dest = spec.cwd.join(&mount.target);
            lightr_index::hydrate(&dest, store, &mount.ref_name)?;
        }

        // Key contribution: ref_name bytes + [0x02] + mount root digest
        let rec = store
            .ref_get(&mount.ref_name)?
            .ok_or_else(|| LightrError::RefNotFound(mount.ref_name.clone()))?;
        hasher.update(mount.ref_name.as_bytes());
        hasher.update(&[0x02u8]);
        hasher.update(&rec.root.0);
    }

    // F-309: secrets then configs contribute to the key (build-spec-parity.md §0).
    // A different secret/config ref ⇒ a different key (in-key inputs). Resolution
    // uses `store` (the ref's current root digest), exactly like mounts above.
    // Empty vecs leave the hasher untouched ⇒ existing keys unchanged.
    crate::secrets::contribute_to_key(&mut hasher, &spec.secrets, b"secret\0", store);
    crate::secrets::contribute_to_key(&mut hasher, &spec.configs, b"config\0", store);

    Ok(Digest(*hasher.finalize().as_bytes()))
}

// Keep the old build_key for backward-compat within existing tests. Used only
// on the fast path (no mounts AND no secrets/configs), so it needs no store;
// `run_memoized_with`/`predict` route any spec with secrets/configs through
// `assemble_key` (which resolves refs against the store).
pub(super) fn build_key(spec: &RunSpec) -> Result<Digest> {
    // No store needed for no-mounts case; but we must handle it.
    // For the unmounted path (used by existing tests), we short-circuit.
    use std::path::PathBuf;

    let mut hasher = blake3::Hasher::new();

    hasher.update(b"lightr/run/v1\0");

    let inputs: Vec<&PathBuf> = if spec.inputs.is_empty() {
        vec![&spec.cwd]
    } else {
        spec.inputs.iter().collect()
    };

    for input_path in inputs {
        let abs_path = if input_path.is_absolute() {
            input_path.clone()
        } else {
            spec.cwd.join(input_path)
        };
        let canonical = abs_path.canonicalize().map_err(LightrError::Io)?;
        let mut index = Index::load_for(&canonical)?;
        let report = scan(&canonical, &mut index)?;
        let rel_path_bytes = input_path.as_os_str().as_encoded_bytes();
        hasher.update(rel_path_bytes);
        hasher.update(b"\0");
        hasher.update(&report.manifest.digest().0);
    }

    for arg in &spec.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    let mut sorted_keys = spec.env_keys.clone();
    sorted_keys.sort();
    for key in &sorted_keys {
        if let Some(val) = std::env::var_os(key) {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_encoded_bytes());
            hasher.update(b"\0");
        } else {
            hasher.update(key.as_bytes());
            hasher.update(b"\x01");
        }
    }

    // WP-RC-1 (R-KEY): explicit env — must match `assemble_key` exactly so the
    // fast path and the store path agree. Empty ⇒ no-op (behavior-preserving).
    contribute_env_explicit(&mut hasher, &spec.env_explicit);

    let triple = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    hasher.update(triple.as_bytes());

    // No mount contribution (this fast path is only taken when mounts are empty).
    // No secrets/configs contribution either: this storeless fast path is only
    // reached when secrets AND configs are empty (a non-empty spec routes through
    // `assemble_key`, which has the store to resolve refs — F-309 §0). An empty
    // contribution is a no-op, so the key is identical to today's for the 16
    // existing (empty-vec) callers.

    Ok(Digest(*hasher.finalize().as_bytes()))
}

pub fn run_memoized(spec: &RunSpec, store: &Store) -> Result<RunOutcome> {
    run_memoized_with(spec, store, &lightr_core::ResourceLimits::default())
}

/// Run with explicit resource caps. `limits` are a **separate** exec parameter,
/// NOT part of the memo key (build-spec-parity.md §0): resource caps don't
/// change deterministic output, so an OOM-kill is an environmental failure, not
/// a cached result. The 16 callers of `run_memoized` keep unlimited defaults.
pub fn run_memoized_with(
    spec: &RunSpec,
    store: &Store,
    limits: &lightr_core::ResourceLimits,
) -> Result<RunOutcome> {
    // F-203: validate native limit enforceability BEFORE the AC lookup, so a
    // cache-HIT can't bypass the honest error (limits are excluded from the key).
    crate::limits::check_native_support(limits)?;

    // Fast path (storeless build_key) only when there are no store-backed
    // inputs at all — no mounts AND no secrets/configs. Any store-backed input
    // routes through assemble_key, which resolves refs against the store
    // (F-309 §0: secrets/configs are in-key, resolved like mounts).
    let needs_store_key =
        !spec.mounts.is_empty() || !spec.secrets.is_empty() || !spec.configs.is_empty();
    let key = if !needs_store_key {
        build_key(spec)?
    } else {
        // Validate mount targets before anything else (before hydration)
        for mount in &spec.mounts {
            validate_mount_target(&mount.target)?;
        }
        // assemble_key with hydrate_mounts=false first to check AC hit
        // then hydrate if miss
        assemble_key(spec, store, false)?
    };

    // --- Hit path ---
    if let Ok(Some(record_bytes)) = store.ac_get(&key) {
        if let Some((exit_code, stdout_d, stderr_d)) = decode_ac_record(&record_bytes) {
            let stdout_res = store.get_bytes(&stdout_d);
            let stderr_res = store.get_bytes(&stderr_d);
            if let (Ok(stdout), Ok(stderr)) = (stdout_res, stderr_res) {
                return Ok(RunOutcome {
                    key,
                    hit: true,
                    exit_code,
                    stdout,
                    stderr,
                });
            }
        }
    }

    // --- Miss path ---

    // Hydrate mounts now (only on miss)
    if !spec.mounts.is_empty() {
        for mount in &spec.mounts {
            let dest = spec.cwd.join(&mount.target);
            lightr_index::hydrate(&dest, store, &mount.ref_name)?;
        }
    }

    // F-309: hydrate secrets/configs into the run cwd (only on miss). Each ref
    // is materialized from the store at mode 0600 (secret) or 0644 (config),
    // content-verified against the sealed CAS before write.
    crate::secrets::hydrate(&spec.cwd, store, &spec.secrets, &spec.configs)?;

    if spec.command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let mut cmd = std::process::Command::new(&spec.command[0]);
    cmd.args(&spec.command[1..]).current_dir(&spec.cwd);
    // F-203: apply resource caps to the spawn. On Linux: RLIMIT_AS/RLIMIT_DATA
    // via pre_exec hook; cpu_millis unsupported on native (honest Err). No-op
    // when limits are unlimited.
    crate::limits::apply_native(&mut cmd, limits)?;
    let output = cmd.output().map_err(LightrError::Io)?;

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

    if exit_code == 0 && stdout.len() <= OUTPUT_CAP_BYTES && stderr.len() <= OUTPUT_CAP_BYTES {
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

/// Compute the memo key and whether the AC already has it — no execution.
pub fn predict(spec: &RunSpec, store: &Store) -> Result<(lightr_core::Digest, bool)> {
    // Validate mount targets (no hydration)
    for mount in &spec.mounts {
        validate_mount_target(&mount.target)?;
    }
    // Same fast-path rule as run_memoized_with: storeless build_key only when
    // there are no store-backed inputs (no mounts, secrets, or configs).
    let needs_store_key =
        !spec.mounts.is_empty() || !spec.secrets.is_empty() || !spec.configs.is_empty();
    let key = if !needs_store_key {
        build_key(spec)?
    } else {
        assemble_key(spec, store, false)?
    };
    let hit = match store.ac_get(&key) {
        Ok(Some(bytes)) => decode_ac_record(&bytes).is_some(),
        _ => false,
    };
    Ok((key, hit))
}
