//! lightr-run — frozen contract: build-spec v2 §6 + build-spec-r1 §2.
//! Memo key, native exec, replay, supervisor, ps, logs, stop, exec_in.

use lightr_core::{Digest, LightrError, Result, OUTPUT_CAP_BYTES};
use lightr_engine::EngineKind;
use lightr_index::{scan, Index};
use lightr_store::Store;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Per-feature seam modules (build-spec-parity.md §1). A0 wires the call sites
// to honest stubs in these files; A1/A3 fill the bodies.
pub mod healthcheck;
pub mod limits;
// F-308 (build-spec-parity.md §3): PURE OS-supervisor unit-file templates +
// RestartPolicy. No I/O lives here; the install/uninstall/list flow is in
// lightr-cli::handlers::supervise. We ship NO daemon — we generate a unit and
// tell the user the opt-in command.
pub mod portforward;
pub mod restart;
pub mod secrets;

// F-304 Phase-2 (ADR-0018): daemonless userspace L2 switch for vz container
// networking (container↔container, name-DNS, udp). CONTRACT STUB — the C-wave
// (C1 network / C2 switch / C3 dhcp / C4 dns / C5 runtime) fills the bodies.
// unix-only (RawFd + datagram sockets); windows networking is a future ring.
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod vswitch;

/// A published-port mapping: `host` (on 127.0.0.1) → `container` (on 127.0.0.1
/// where the run's server listens). TCP only in v1 (Networking Phase 1).
///
/// Ports are a **runtime** parameter, NOT a memo-key input — exactly like
/// resource limits, and exactly like Docker, which does not key on `-p`. They
/// never enter `build_key`/`assemble_key` (see the `ports_excluded_from_key`
/// test).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortMap {
    pub host: u16,
    pub container: u16,
}

pub struct RunSpec {
    pub cwd: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
    // R1: mounts hydrated CoW into <cwd>/<target> pre-key/pre-exec
    // (build-spec-r1 §2); part of the memo key in order.
    pub mounts: Vec<Mount>,
    // F-309 (build-spec-parity.md §0/§A0.2): store-backed inputs. IN the memo
    // key (a different secret/config ⇒ a different run). Hydrated on miss to
    // <cwd>/.lightr/secrets/<name> (0600) / <cwd>/.lightr/configs/<name> (0644).
    pub secrets: Vec<StoreFile>,
    pub configs: Vec<StoreFile>,
    // Networking Phase 1: published host→container TCP ports. RUNTIME ONLY —
    // never part of the memo key (like resource limits; like Docker `-p`). The
    // detached supervisor publishes each entry by forwarding 127.0.0.1:host →
    // 127.0.0.1:container for the run's lifetime.
    pub ports: Vec<PortMap>,
}

pub struct Mount {
    pub ref_name: String,
    pub target: String,
}

/// A store-backed file injected into a run. `ref_name` resolves via lightr_index.
pub struct StoreFile {
    pub name: String,
    pub ref_name: String,
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
// Mount target validation
// ---------------------------------------------------------------------------

fn validate_mount_target(t: &str) -> Result<()> {
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
//   mounts: for each mount in order:
//       validate target (reject "..")
//       update(ref_name bytes + [0x02] + mount-ref's CURRENT root digest bytes)
//   key = finalize
// ---------------------------------------------------------------------------

/// Shared private key-assembly fn. `hydrate_mounts` controls whether to
/// actually hydrate into cwd (true for run_memoized, false for predict).
fn assemble_key(spec: &RunSpec, store: &Store, hydrate_mounts: bool) -> Result<Digest> {
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
fn build_key(spec: &RunSpec) -> Result<Digest> {
    // No store needed for no-mounts case; but we must handle it.
    // For the unmounted path (used by existing tests), we short-circuit.
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

    // F-309: hydrate secrets/configs (only on miss). A0 stub is Ok(()); WP-A3 fills it.
    crate::secrets::hydrate(&spec.cwd, store, &spec.secrets, &spec.configs)?;

    if spec.command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let mut cmd = std::process::Command::new(&spec.command[0]);
    cmd.args(&spec.command[1..]).current_dir(&spec.cwd);
    // F-203: apply resource caps to the spawn. A0 stub is Ok(()); WP-A1 fills it.
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

// ---------------------------------------------------------------------------
// vz-memo — memoize container runs (build-spec-prod.md, the product's moat).
//
// A `vz` container run (`lightr run --engine vz --rootfs <ref> -- <cmd>`) is
// memoized EXACTLY like the native path: the 1st run boots the VM + captures
// {exit, stdout, stderr}; an identical 2nd run is a HIT that replays them from
// the Action Cache with NO VM boot. The hit/miss flow mirrors
// `run_memoized_with` byte-for-byte — the only difference is that the "run" is a
// caller-supplied closure (boot the VM, read the guest's capture files) instead
// of a native `Command`. Caching law is identical: store ONLY when `exit == 0`
// AND both streams are within `OUTPUT_CAP_BYTES`; replay is byte-exact.
//
// The memo key is a SEPARATE, domain-separated key (`b"lightr-vz-memo-v1"`) — a
// vz run keys on (command, rootfs image digest, env), NOT on a cwd scan, so it
// never collides with a native `run/v1` key.
// ---------------------------------------------------------------------------

/// Inputs that identify a memoizable `vz` container run. A different command,
/// rootfs image, or env ⇒ a different run ⇒ a different key.
///
/// `rootfs_digest` is the resolved content digest of the rootfs image (the
/// ref's current root), so two refs pointing at the same content share a memo
/// entry and a ref re-pointed at new content misses — exactly like a mount's
/// key contribution in `assemble_key`.
pub struct VzMemoKey {
    pub command: Vec<String>,
    pub rootfs_digest: lightr_core::Digest,
    pub env: Vec<(String, String)>,
}

/// Compute the memo key for a `vz` container run. blake3 over a
/// DOMAIN-SEPARATED, length-prefixed encoding so the layout is unambiguous and
/// no field boundary can be forged by concatenation:
///   update(b"lightr-vz-memo-v1")            // fixed domain tag
///   update(rootfs_digest.0)                 // 32B image content digest
///   for arg in command:  len(u64 LE) + arg  // length-prefixed, ordered
///   for (k, v) in env:   len(k=v, u64 LE) + "k=v"  // length-prefixed, ordered
///   update(OS) update("-") update(ARCH)     // target triple
///
/// Determinism is essential: identical inputs ⇒ identical key (see
/// `vz_memo_key_is_deterministic`); any field change ⇒ a different key (see
/// `vz_memo_key_is_sensitive_to_every_field`).
pub fn vz_memo_key(k: &VzMemoKey) -> lightr_core::Digest {
    let mut hasher = blake3::Hasher::new();

    // Fixed domain tag — separates this key space from `run/v1` etc.
    hasher.update(b"lightr-vz-memo-v1");

    // Rootfs image content digest (32 bytes, fixed width — no length prefix
    // needed; it is always exactly 32 bytes).
    hasher.update(&k.rootfs_digest.0);

    // Command args — length-prefixed, in order.
    for arg in &k.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    // Env — length-prefixed `key=value`, in order. The encoded `k=v` is
    // length-prefixed as a whole so an env split (e.g. `A`,`B=C` vs `A=B`,`C`)
    // can never collide.
    for (key, val) in &k.env {
        let mut kv = Vec::with_capacity(key.len() + 1 + val.len());
        kv.extend_from_slice(key.as_bytes());
        kv.push(b'=');
        kv.extend_from_slice(val.as_bytes());
        let len = kv.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(&kv);
    }

    // Target triple: os/arch (a cached Linux-guest result is host-arch specific).
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(b"-");
    hasher.update(std::env::consts::ARCH.as_bytes());

    Digest(*hasher.finalize().as_bytes())
}

/// Memoize a `vz` container run. Mirrors `run_memoized_with`'s hit/miss flow
/// EXACTLY, with the "run" supplied as a closure returning
/// `(exit_code, stdout, stderr)`:
///
/// * HIT  — `ac_get(key)` → `decode_ac_record` → `get_bytes` both streams ⇒
///   `RunOutcome { hit: true, .. }`. The closure is NEVER invoked (no VM boot).
/// * MISS — invoke `run()`, then store ONLY when `exit == 0` AND both streams
///   are `<= OUTPUT_CAP_BYTES` (`put_bytes` × 2 + `encode_ac_record` +
///   `ac_put`) ⇒ `RunOutcome { hit: false, .. }`.
///
/// Caching law is identical to the native memo: a non-zero exit or an oversized
/// stream is environmental/unbounded and is never cached, so it always re-runs.
pub fn run_vz_memoized(
    k: &VzMemoKey,
    store: &Store,
    run: impl FnOnce() -> Result<(i32, Vec<u8>, Vec<u8>)>,
) -> Result<RunOutcome> {
    let key = vz_memo_key(k);

    // --- Hit path (mirror of run_memoized_with) ---
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

    // --- Miss path: run the closure (boots the VM) and capture the result ---
    let (exit_code, stdout, stderr) = run()?;

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

// ---------------------------------------------------------------------------
// R1 — run control types and helpers
// ---------------------------------------------------------------------------

pub struct RunHandle {
    pub id: String,
    pub dir: std::path::PathBuf,
}

pub struct RunInfo {
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub command: Vec<String>,
    pub created_at_unix: u64,
    /// F-309: last healthcheck verdict from `<run_dir>/health`, if a
    /// healthcheck was configured for this run. `None` ⇒ no healthcheck (the
    /// common case). NOT part of the memo key.
    pub health: Option<crate::healthcheck::Health>,
    /// WP-PS-ENRICH: the engine that ran this detached job ("native" or "vz").
    /// Sourced from `SpecOnDisk::engine`; defaults to "native" for old run dirs
    /// whose spec.json pre-dates the engine field (back-compat via serde default).
    pub engine: String,
    /// WP-PS-ENRICH: published host→container TCP port mappings. Empty for
    /// runs with no `-p` flags. Sourced from `SpecOnDisk::ports`.
    pub ports: Vec<(u16, u16)>,
    /// WP-PS-ENRICH: the rootfs ref the vz engine booted, if any. `None` for
    /// native runs. Sourced from `SpecOnDisk::rootfs_ref`.
    pub rootfs_ref: Option<String>,
}

pub enum LogStream {
    Stdout,
    Stderr,
    Both,
}

// ---------------------------------------------------------------------------
// SpecOnDisk — private serde mirror for spec.json
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct MountOnDisk {
    ref_name: String,
    target: String,
}

#[derive(Serialize, Deserialize)]
struct SpecOnDisk {
    cwd: String,
    command: Vec<String>,
    env_keys: Vec<String>,
    mounts: Vec<MountOnDisk>,
    detached: bool,
    created_at_unix: u64,
    // Networking Phase 1: published (host, container) TCP ports the supervisor
    // forwards. `#[serde(default)]` keeps JSON back-compat: spec.json files
    // written before this field existed (no `ports`) still parse to an empty
    // Vec, so an old detached run never breaks on read.
    #[serde(default)]
    ports: Vec<(u16, u16)>,
    // WP-NET2: the engine that runs this detached job. `#[serde(default)]` →
    // "native" for spec.json files written before this field existed, so an old
    // detached run keeps the native supervisor branch. The vz branch (a Linux
    // container in a microVM, with host→guest port forwarding) is selected by
    // engine == "vz" AND a present `rootfs_ref`.
    #[serde(default = "default_engine")]
    engine: String,
    // WP-NET2: the rootfs ref the vz branch hydrates + boots. None for native
    // runs (serde default). Present ⇒ a vz container run.
    #[serde(default)]
    rootfs_ref: Option<String>,
    /// WP-DISC: explicit env vars set on the detached child (compose service
    /// discovery: <PEER>_HOST/<PEER>_PORT). serde-defaulted = back-compat. NOT a
    /// memo-key input (runtime addressing, like ports) — and detached runs aren't
    /// memoized anyway.
    #[serde(default)]
    env: Vec<(String, String)>,
}

/// Serde default for [`SpecOnDisk::engine`] — the native supervisor branch, so a
/// pre-WP-NET2 spec.json (no `engine` field) keeps its original behaviour.
fn default_engine() -> String {
    "native".to_string()
}

fn read_spec_on_disk(dir: &std::path::Path) -> Result<SpecOnDisk> {
    let bytes = std::fs::read(dir.join("spec.json")).map_err(LightrError::Io)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| LightrError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

fn lightr_home() -> PathBuf {
    if let Ok(h) = std::env::var("LIGHTR_HOME") {
        PathBuf::from(h)
    } else {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        home.join(".lightr")
    }
}

fn run_dir_for_id(id: &str) -> PathBuf {
    lightr_home().join("run").join(id)
}

fn new_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{nanos}-{pid}")
}

fn write_spec_json(dir: &std::path::Path, spec: &SpecOnDisk) -> Result<()> {
    let bytes = serde_json::to_vec(spec).map_err(|e| LightrError::Io(std::io::Error::other(e)))?;
    std::fs::write(dir.join("spec.json"), &bytes).map_err(LightrError::Io)
}

fn read_pid_file(dir: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("pid"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

fn read_status_file(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("status"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn parse_exit_code_from_status(status: &str) -> Option<i32> {
    status
        .strip_prefix("exited ")
        .and_then(|s| s.parse::<i32>().ok())
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

// WIN-PATH: liveness via OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) +
// GetExitCodeProcess; alive iff the process is still STILL_ACTIVE (259).
// Runtime-validatable only on a real Windows box.
#[cfg(windows)]
fn pid_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() {
            // Could not open: either gone or access-denied. Treat as not alive
            // (the supervisor owns the pid it spawned, so denial implies dead).
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        // STILL_ACTIVE is i32 259; GetExitCodeProcess writes a u32.
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

// ---------------------------------------------------------------------------
// Process termination — transport for SIGTERM/SIGKILL semantics.
// ---------------------------------------------------------------------------

// WIN-PATH: Windows has no signal model. SIGKILL maps to a forced
// TerminateProcess; SIGTERM is best-effort (no graceful-term equivalent — we
// force-terminate so `stop` makes progress). Graceful-term semantics differ
// from unix and are only validatable on a real Windows box.
// Returns true if a terminate was attempted successfully.
#[cfg(windows)]
fn win_terminate(pid: i32, exit_code: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, exit_code);
        CloseHandle(handle);
        ok != 0
    }
}

// ---------------------------------------------------------------------------
// Control transport path.
// unix: a `.sock` unix-domain-socket path inside the run dir.
// windows: a named pipe whose name is derived deterministically from the run
//          id (the run dir's file name), so client and server agree without
//          any extra shared state. A presence sentinel file in the run dir
//          mirrors `.sock`'s "does the endpoint exist?" check.
// JSON wire protocol is identical on both transports.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn ctl_sock_path(dir: &std::path::Path) -> PathBuf {
    dir.join("ctl.sock")
}

// WIN-PATH: named-pipe address `\\.\pipe\lightr-<id>`. The id is the run dir's
// file name — the same identity the unix `.sock` lives under — so a client
// computes the identical pipe name from the same `dir`. Runtime-validatable
// only on a real Windows box.
#[cfg(windows)]
fn ctl_pipe_name(dir: &std::path::Path) -> String {
    let id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "default".to_string());
    format!(r"\\.\pipe\lightr-{id}")
}

// Windows sentinel mirroring `ctl.sock`'s existence semantics. The named pipe
// itself is not a filesystem object pollable via `Path::exists`, so the
// supervisor touches this file once the pipe server is listening and removes
// it on exit. `ps`/`stop` test this exactly like the unix `.sock` path.
#[cfg(windows)]
fn ctl_sock_path(dir: &std::path::Path) -> PathBuf {
    dir.join("ctl.pipe.live")
}

#[cfg(unix)]
fn send_ctl_op(dir: &std::path::Path, op: &str) -> Option<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock = ctl_sock_path(dir);
    if !sock.exists() {
        return None;
    }
    let mut stream = UnixStream::connect(&sock).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    stream.write_all(op.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

// WIN-PATH: named-pipe client. Opens `\\.\pipe\lightr-<id>` with CreateFileW
// (the pipe server is a BLOCKING PIPE_TYPE_BYTE / PIPE_WAIT pipe — see
// `supervise`), wraps the handle in a std File, and exchanges the SAME
// newline-delimited JSON request/response as the unix transport. The wire
// protocol is byte-identical; only the transport differs.
// Runtime-validatable only on a real Windows box.
#[cfg(windows)]
fn send_ctl_op(dir: &std::path::Path, op: &str) -> Option<serde_json::Value> {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};

    // Mirror the unix `sock.exists()` guard: if the supervisor's live sentinel
    // is absent, there is no endpoint to talk to.
    let sentinel = ctl_sock_path(dir);
    if !sentinel.exists() {
        return None;
    }

    let name = ctl_pipe_name(dir);
    // Build a NUL-terminated wide string for CreateFileW.
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    // dwShareMode=0, no security attrs, no extra flags (FILE_FLAGS_AND_ATTRIBUTES
    // is a u32 alias in windows-sys 0.59 — pass 0), no template handle.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    // SAFETY: handle is a valid, owned pipe handle; File takes ownership and
    // closes it on drop.
    let file = unsafe { File::from_raw_handle(handle as *mut _) };
    // We need two independent halves (write the request, then buffered-read the
    // reply). try_clone duplicates the underlying handle.
    let mut writer = file.try_clone().ok()?;
    writer.write_all(op.as_bytes()).ok()?;
    writer.write_all(b"\n").ok()?;
    writer.flush().ok()?;

    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

// ---------------------------------------------------------------------------
// spawn_detached
// ---------------------------------------------------------------------------

pub fn spawn_detached(spec: &RunSpec, store: &Store) -> Result<RunHandle> {
    spawn_detached_engine(spec, store, None, EngineKind::Native, None, &[])
}

/// `spawn_detached` plus an optional healthcheck (F-309). When `hc` is
/// `Some`, it is persisted into the run dir (`healthcheck.json`) and the
/// detached supervisor probes it on its interval, writing `Healthy`/`Unhealthy`
/// to `<run_dir>/health` so `ps` can surface liveness. The healthcheck is a
/// post-result probe and is **not** part of the memo key (build-spec-parity.md
/// §0); it never affects caching or the run's output.
///
/// `spawn_detached` delegates here with `None`, so its 2 existing callers (the
/// CLI run handler and compose's `start_service_detached`) keep their behaviour
/// unchanged.
pub fn spawn_detached_with_health(
    spec: &RunSpec,
    store: &Store,
    hc: Option<&crate::healthcheck::Healthcheck>,
) -> Result<RunHandle> {
    spawn_detached_engine(spec, store, hc, EngineKind::Native, None, &[])
}

/// `spawn_detached_with_health` plus the engine + rootfs ref (WP-NET2). The
/// `native` path (`engine = Native`, `rootfs_ref = None`) is the existing
/// supervisor: it spawns the command as a host process. The `vz` path
/// (`engine = Vz` + a `rootfs_ref`) boots a Linux container in a microVM inside
/// the supervisor and forwards each published port to the guest's DHCP IP — the
/// `-p`-for-a-Linux-image case. The engine + rootfs ref are persisted to
/// spec.json (serde-defaulted, so old native runs read back unchanged) and are
/// NOT memo-key inputs (a detached run is never memoized).
///
/// WP-DISC: `env` is an explicit set of `(key, value)` pairs applied to the
/// detached NATIVE child (compose service discovery: `<PEER>_HOST`/`<PEER>_PORT`
/// plus the service's own env). It is persisted to spec.json (serde-defaulted)
/// and is NOT a memo-key input — runtime addressing, like ports, and detached
/// runs aren't memoized anyway. The vz branch ignores it.
pub fn spawn_detached_engine(
    spec: &RunSpec,
    _store: &Store,
    hc: Option<&crate::healthcheck::Healthcheck>,
    engine: EngineKind,
    rootfs_ref: Option<&str>,
    env: &[(String, String)],
) -> Result<RunHandle> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let id = new_run_id();
    let dir = run_dir_for_id(&id);
    std::fs::create_dir_all(&dir).map_err(LightrError::Io)?;

    // Persist the healthcheck (if any) BEFORE forking the supervisor, so the
    // supervisor finds it on startup. Not in the memo key (§0).
    if let Some(hc) = hc {
        crate::healthcheck::save_for(&dir, hc)?;
    }

    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let spec_on_disk = SpecOnDisk {
        cwd: spec.cwd.to_string_lossy().into_owned(),
        command: spec.command.clone(),
        env_keys: spec.env_keys.clone(),
        mounts: spec
            .mounts
            .iter()
            .map(|m| MountOnDisk {
                ref_name: m.ref_name.clone(),
                target: m.target.clone(),
            })
            .collect(),
        detached: true,
        created_at_unix,
        ports: spec.ports.iter().map(|p| (p.host, p.container)).collect(),
        engine: engine.as_str().to_string(),
        rootfs_ref: rootfs_ref.map(|s| s.to_string()),
        env: env.to_vec(),
    };
    write_spec_json(&dir, &spec_on_disk)?;

    let exe = std::env::current_exe().map_err(LightrError::Io)?;
    let dir_str = dir.to_string_lossy().into_owned();

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["__supervise", &dir_str]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    // WIN-PATH: Windows has no `setsid`/process-session model. The closest
    // correctness analog is detaching the supervisor from the parent's console
    // and giving it its own process group so a Ctrl-C to the launcher does not
    // tear down the detached supervisor. Full process-tree containment via job
    // objects is a future ring. Validatable only on a real Windows box.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        };
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    cmd.spawn().map_err(LightrError::Io)?;

    Ok(RunHandle { id, dir })
}

// ---------------------------------------------------------------------------
// supervise_vz (WP-NET2) — detached vz container with host→guest port forwarding
// ---------------------------------------------------------------------------

/// Supervise a `vz` container run: boot a Linux microVM in THIS process and
/// forward each published port (`127.0.0.1:host` → `guest_ip:container`) to the
/// guest's DHCP IP. This is the `-p`-for-a-Linux-image case.
///
/// Lifecycle:
/// 1. Hydrate the rootfs ref CoW into `<run_dir>/rootfs` (lives for the VM, gc'd
///    with the run dir).
/// 2. Boot the VM on a worker thread — `engine.run(net=true)` blocks until the VM
///    stops. `net=true` makes the engine attach the NAT NIC (`ip=dhcp`) and the
///    guest publish its IP to `IP_FILE`.
/// 3. Read the guest IP from `IP_FILE` (or bail if the VM exits first).
/// 4. Write pid (our own) + status, start a forwarder per published port.
/// 5. Serve `ctl.sock` (status/signal) + poll the VM. `signal` writes the guest
///    `EXIT_FILE` with the `128+sig` code; the shim polls it and force-stops the
///    VM (no new shim code), the worker returns, and we exit cleanly.
///
/// Stop semantics: `stop` sends `signal` via `ctl.sock` (→ force-stop, clean
/// status), with the usual pid-SIGKILL fallback — and since the VM runs IN this
/// process, killing the supervisor tears the VM down too.
#[cfg(unix)]
fn supervise_vz(dir: &std::path::Path, spec: &SpecOnDisk, store: &Store) -> Result<i32> {
    use lightr_engine::{engine_for, ExecSpec};
    use lightr_init::{EXIT_FILE, IP_FILE};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let rootfs_ref = spec
        .rootfs_ref
        .clone()
        .ok_or_else(|| LightrError::InvalidRef("vz supervise: missing rootfs_ref".to_string()))?;
    let cwd = PathBuf::from(&spec.cwd);

    // 1. Hydrate the rootfs ref into <run_dir>/rootfs (persists for the VM's life;
    //    cleaned with the run dir, unlike the memo path's throwaway temp dir).
    let rootfs_dir = dir.join("rootfs");
    std::fs::create_dir_all(&rootfs_dir).map_err(LightrError::Io)?;
    lightr_index::hydrate(&rootfs_dir, store, &rootfs_ref)?;

    // The guest's durable EXIT_FILE + IP_FILE on the share. Writing EXIT_FILE
    // force-stops the VM (the shim polls it); IP_FILE is where the guest publishes
    // its DHCP IP. Both paths agree with the engine/guest by construction (rootfs
    // dir + the lightr_init const), so they can never drift.
    let exit_file = rootfs_dir.join(EXIT_FILE.trim_start_matches('/'));
    let ip_file = rootfs_dir.join(IP_FILE.trim_start_matches('/'));

    // 2. Boot the VM on a worker thread (engine.run blocks until the VM stops).
    //    Safety of env mutation inside engine.run: VzEngine::run sets LIGHTR_VZ_NET
    //    / LIGHTR_VZ_EXITFILE ONCE at the very start of run() — before the VM boot
    //    FFI call — at which point the only other live thread is this main thread,
    //    polling IP_FILE via std::fs (no getenv). The forwarder + ctl threads do
    //    not exist yet (they start in step 4, ~1–2s later after the IP appears), so
    //    no thread reads the environment concurrently with those set_var calls.
    let vm_done = Arc::new(AtomicBool::new(false));
    let vm_code = Arc::new(Mutex::new(255i32));
    let command = spec.command.clone();
    {
        let vm_done = Arc::clone(&vm_done);
        let vm_code = Arc::clone(&vm_code);
        let rootfs_dir = rootfs_dir.clone();
        let cwd = cwd.clone();
        std::thread::spawn(move || {
            let code = match engine_for(EngineKind::Vz) {
                Ok(engine) => {
                    let spec = ExecSpec {
                        cwd: &cwd,
                        command: &command,
                        rootfs: Some(&rootfs_dir),
                        limits: lightr_core::ResourceLimits::default(),
                        net: true,
                    };
                    engine.run(&spec).unwrap_or(255)
                }
                Err(_) => 255, // vz unavailable (non-macOS / no pack) → honest non-zero
            };
            *vm_code.lock().expect("vm_code mutex") = code;
            vm_done.store(true, Ordering::SeqCst);
        });
    }

    // 3. Wait for the guest IP (boot + kernel DHCP, ~1–2s) OR an early VM exit
    //    (boot failure / instant command exit). Generous deadline for a cold boot.
    let ip_deadline = Instant::now() + Duration::from_secs(60);
    let guest_ip: Option<String> = loop {
        if let Ok(s) = std::fs::read_to_string(&ip_file) {
            let ip = s.trim().to_string();
            if !ip.is_empty() {
                break Some(ip);
            }
        }
        if vm_done.load(Ordering::SeqCst) {
            break None; // VM stopped before publishing an IP
        }
        if Instant::now() >= ip_deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    // No IP ⇒ the run is not networkable: record the VM's (final) exit code and
    // stop. Force-stop best-effort in case the VM is up but networking failed.
    let Some(guest_ip) = guest_ip else {
        for _ in 0..20 {
            if vm_done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::write(&exit_file, "143");
        let code = *vm_code.lock().expect("vm_code mutex");
        let _ = std::fs::write(dir.join("status"), format!("exited {code}"));
        return Ok(code);
    };

    // 4. Live: write our pid (stop()'s SIGKILL fallback kills us → the in-process
    //    VM dies with us) + status, then forward each published port to the guest.
    std::fs::write(dir.join("pid"), format!("{}", std::process::id())).map_err(LightrError::Io)?;
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;

    // 5. A forwarder per published port → the guest IP. A bind failure is logged
    //    and skipped (a port clash on one publish must not down the whole run),
    //    exactly like the native path. Held until the loop exits, then dropped.
    let mut forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    for &(host_port, container_port) in &spec.ports {
        match crate::portforward::start_to(host_port, &guest_ip, container_port) {
            Ok(fwd) => forwarders.push(fwd),
            Err(e) => {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("stderr.log"))
                {
                    let _ = writeln!(
                        f,
                        "lightr: publish 127.0.0.1:{host_port} -> {guest_ip}:{container_port} failed: {e}"
                    );
                }
            }
        }
    }

    // 6. ctl.sock loop: serve status/signal + poll the VM (mirrors the unix native
    //    loop). `signal` writes EXIT_FILE (force-stop); the shim stops the VM, the
    //    worker returns, vm_done flips, and we break with the real exit code.
    let sock_path = ctl_sock_path(dir);
    let listener = UnixListener::bind(&sock_path).map_err(LightrError::Io)?;
    listener.set_nonblocking(true).map_err(LightrError::Io)?;

    let exit_code = loop {
        if vm_done.load(Ordering::SeqCst) {
            break *vm_code.lock().expect("vm_code mutex");
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(1))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(1))).ok();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let line = line.trim();
                    if let Ok(req) = serde_json::from_str::<serde_json::Value>(line) {
                        let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                        let reply: serde_json::Value = match op {
                            "status" => serde_json::json!({"status": "running"}),
                            "signal" => {
                                // Force-stop: write the guest EXIT_FILE with the
                                // 128+signal code; the shim polls it and stops the
                                // VM. Default sig 15 (SIGTERM ⇒ 143). The VM is
                                // in-process, so this is how the supervisor relays
                                // a "stop" into the guest's force-teardown.
                                let sig = req.get("sig").and_then(|v| v.as_i64()).unwrap_or(15);
                                let code = 128 + sig as i32;
                                let _ = std::fs::write(&exit_file, format!("{code}"));
                                serde_json::json!({"ok": true})
                            }
                            _ => serde_json::json!({"error": "unknown op"}),
                        };
                        let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
                        reply_bytes.push(b'\n');
                        let mut w = &stream;
                        let _ = w.write_all(&reply_bytes);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    std::fs::write(dir.join("status"), format!("exited {exit_code}")).map_err(LightrError::Io)?;
    let _ = std::fs::remove_file(&sock_path);
    drop(forwarders); // close listeners + per-connection threads
    Ok(exit_code)
}

/// Non-unix stub: the `vz` engine is macOS-only (unix). On a non-unix host a vz
/// run never reaches here (the CLI won't route it), but the symbol must exist for
/// `supervise`'s unconditional call to compile. Fails closed.
#[cfg(not(unix))]
fn supervise_vz(_dir: &std::path::Path, _spec: &SpecOnDisk, _store: &Store) -> Result<i32> {
    Err(LightrError::InvalidRef(
        "vz supervise requires a unix host (macOS)".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// supervise
// ---------------------------------------------------------------------------

pub fn supervise(dir: &std::path::Path) -> Result<i32> {
    let spec = read_spec_on_disk(dir)?;
    let cwd = PathBuf::from(&spec.cwd);

    // Hydrate mounts (same law as run_memoized)
    // We need a store for hydration — open from LIGHTR_HOME
    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;

    // WP-NET2: a vz container run (engine "vz" + a rootfs ref) boots a Linux
    // microVM in this supervisor process and forwards each published port to the
    // guest's DHCP IP, instead of spawning a host child. Everything below is the
    // unchanged native path. Selected by the engine field written at spawn time.
    if spec.engine == "vz" && spec.rootfs_ref.is_some() {
        return supervise_vz(dir, &spec, &store);
    }

    for m in &spec.mounts {
        validate_mount_target(&m.target)?;
        let dest = cwd.join(&m.target);
        lightr_index::hydrate(&dest, &store, &m.ref_name)?;
    }

    // Open log files
    let stdout_log = std::fs::File::create(dir.join("stdout.log")).map_err(LightrError::Io)?;
    let stderr_log = std::fs::File::create(dir.join("stderr.log")).map_err(LightrError::Io)?;

    // Spawn child
    let mut child = std::process::Command::new(&spec.command[0])
        .args(&spec.command[1..])
        .current_dir(&cwd)
        // WP-DISC: explicit per-child env (compose service discovery
        // <PEER>_HOST/<PEER>_PORT + the service's own env), plumbed through
        // spec.json instead of the racy process-global set_var. Empty for a
        // plain `lightr run -d` (byte-identical to before).
        .envs(spec.env.iter().cloned())
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log))
        .spawn()
        .map_err(LightrError::Io)?;

    let child_pid = child.id() as i32;

    // Write pid file
    std::fs::write(dir.join("pid"), format!("{child_pid}")).map_err(LightrError::Io)?;

    // Write status = running
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;

    // Networking Phase 1: publish each declared port by forwarding
    // 127.0.0.1:host → 127.0.0.1:container (where the child's server listens).
    // A bind failure is logged to stderr.log and skipped — it never kills the
    // run (a port clash on one publish must not take the whole service down).
    // The handles are held for the supervisor loop's lifetime; when the
    // supervisor exits (child gone / stop), they drop, the listeners close, and
    // the accept-loop + per-connection threads end. `_forwarders` is bound (not
    // `let _ =`) precisely so it is NOT dropped early.
    let mut _forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    if !spec.ports.is_empty() {
        for &(host_port, container_port) in &spec.ports {
            match crate::portforward::start(host_port, container_port) {
                Ok(fwd) => _forwarders.push(fwd),
                Err(e) => {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("stderr.log"))
                    {
                        let _ = writeln!(
                            f,
                            "lightr: publish 127.0.0.1:{host_port} -> 127.0.0.1:{container_port} failed: {e}"
                        );
                    }
                }
            }
        }
    }

    // F-309: load an optional healthcheck persisted by spawn_detached_with_health.
    // The probe runs on the supervisor's poll loop at `interval_s`; its verdict
    // is written to `<run_dir>/health` for `ps`. Never part of the memo key (§0).
    let health_cfg = crate::healthcheck::load_for(dir)?;

    // The control transport is cfg-split below; the JSON wire protocol
    // (newline-delimited `{"op":...}` request → `{...}` reply) is identical.

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        // Bind ctl.sock
        let sock_path = ctl_sock_path(dir);
        let listener = UnixListener::bind(&sock_path).map_err(LightrError::Io)?;
        listener.set_nonblocking(true).map_err(LightrError::Io)?;

        // Healthcheck cwd is the run's cwd; first probe runs immediately so `ps`
        // surfaces a verdict without waiting a full interval.
        let health_cwd = cwd.clone();
        let mut next_probe = Instant::now();

        // Main loop: serve ctl.sock + poll child + (if configured) probe health
        let exit_code = loop {
            // Healthcheck probe round (interval-gated). A failing probe flips
            // <run_dir>/health to "unhealthy"; never aborts the loop.
            if let Some(ref hc) = health_cfg {
                if Instant::now() >= next_probe {
                    let verdict = crate::healthcheck::probe(hc, &health_cwd);
                    crate::healthcheck::write_state(dir, verdict);
                    next_probe = Instant::now() + Duration::from_secs(hc.interval_s.max(1));
                }
            }

            // Poll child
            if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
                use std::os::unix::process::ExitStatusExt;
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                break code;
            }

            // Accept ctl connections (non-blocking)
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(1))).ok();
                    stream.set_write_timeout(Some(Duration::from_secs(1))).ok();
                    let mut reader = BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() {
                        let line = line.trim();
                        if let Ok(req) = serde_json::from_str::<serde_json::Value>(line) {
                            let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                            let reply: serde_json::Value = match op {
                                "status" => serde_json::json!({"status": "running"}),
                                "signal" => {
                                    if let Some(sig) = req.get("sig").and_then(|v| v.as_i64()) {
                                        unsafe {
                                            libc::kill(child_pid, sig as libc::c_int);
                                        }
                                        serde_json::json!({"ok": true})
                                    } else {
                                        serde_json::json!({"ok": false})
                                    }
                                }
                                _ => serde_json::json!({"error": "unknown op"}),
                            };
                            let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
                            reply_bytes.push(b'\n');
                            let mut w = &stream;
                            let _ = w.write_all(&reply_bytes);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {}
            }

            std::thread::sleep(Duration::from_millis(100));
        };

        // Write final status
        std::fs::write(dir.join("status"), format!("exited {exit_code}"))
            .map_err(LightrError::Io)?;

        // Remove ctl.sock
        let _ = std::fs::remove_file(&sock_path);

        Ok(exit_code)
    }

    // WIN-PATH: named-pipe control server. A dedicated thread runs a BLOCKING
    // pipe accept loop (CreateNamedPipeW + ConnectNamedPipe, PIPE_TYPE_BYTE |
    // PIPE_WAIT) — one instance per connection, the Windows analog of the unix
    // thread-per-connection model — while the main thread polls the child, the
    // same as the unix path. The JSON wire protocol is identical. The pipe
    // runtime is validatable only on a real Windows box.
    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        // Presence sentinel mirroring unix `ctl.sock` existence semantics: write
        // it once the pipe server is listening, remove it on exit. `ps`/`stop`
        // poll this file exactly like the unix `.sock`.
        let sentinel = ctl_sock_path(dir);
        let pipe_name = ctl_pipe_name(dir);

        let done = Arc::new(AtomicBool::new(false));
        let done_srv = Arc::clone(&done);
        // `server_exited` lets shutdown retry the nudge until the server thread
        // actually leaves its loop. A single best-effort nudge can MISS — if the
        // server is between its `done` check and CreateNamedPipeW there is no
        // instance to connect to, and the next ConnectNamedPipe would then block
        // forever and hang join(). The server sets this the instant its loop ends.
        let server_exited = Arc::new(AtomicBool::new(false));
        let server_exited_srv = Arc::clone(&server_exited);
        let pipe_name_srv = pipe_name.clone();

        // Pipe-server thread: blocking accept loop. Handles the SAME ops as the
        // unix listener; `signal` maps to a Windows TerminateProcess (no signal
        // model on Windows), with the exit code following the unix
        // 128+sig convention so callers observe 143 (SIGTERM) / 137 (SIGKILL).
        let server = std::thread::spawn(move || {
            win_pipe_server_loop(&pipe_name_srv, child_pid, &done_srv);
            server_exited_srv.store(true, Ordering::SeqCst);
        });

        // Now that the server thread is up and will create the first pipe
        // instance, publish the sentinel so clients/`ps` see the endpoint.
        std::fs::write(&sentinel, b"live").map_err(LightrError::Io)?;

        // F-309: same interval-gated health probe as the unix path. WIN-PATH:
        // run_once uses `cmd /C`; runtime-validatable only on a real Windows box.
        let health_cwd = cwd.clone();
        let mut next_probe = std::time::Instant::now();

        // Main loop: poll child (identical cadence to the unix path).
        let exit_code = loop {
            if let Some(ref hc) = health_cfg {
                if std::time::Instant::now() >= next_probe {
                    let verdict = crate::healthcheck::probe(hc, &health_cwd);
                    crate::healthcheck::write_state(dir, verdict);
                    next_probe =
                        std::time::Instant::now() + Duration::from_secs(hc.interval_s.max(1));
                }
            }
            if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
                // No signal() on Windows; ExitStatus::code is authoritative.
                break status.code().unwrap_or(1);
            }
            std::thread::sleep(Duration::from_millis(100));
        };

        // Tell the server thread to stop, then keep nudging until it actually
        // leaves its loop. Retrying closes the race where a single nudge lands
        // before the server has created a pipe instance — after which the next
        // ConnectNamedPipe would block forever and join() would hang.
        done.store(true, Ordering::SeqCst);
        while !server_exited.load(Ordering::SeqCst) {
            win_pipe_nudge(&pipe_name);
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = server.join();

        // Write final status
        std::fs::write(dir.join("status"), format!("exited {exit_code}"))
            .map_err(LightrError::Io)?;

        // Remove the presence sentinel (the named pipe itself is freed when its
        // handles close in the server thread).
        let _ = std::fs::remove_file(&sentinel);

        Ok(exit_code)
    }
}

// WIN-PATH: blocking named-pipe accept loop for the control server. Each
// iteration creates one pipe instance, waits for a single client
// (ConnectNamedPipe), reads one newline-delimited JSON request, writes one
// JSON reply, then tears the instance down — the Windows analog of the unix
// accept-then-thread-per-connection model. Loops until `done` is set; the
// supervisor unblocks the final ConnectNamedPipe via `win_pipe_nudge`.
// Validatable only on a real Windows box.
#[cfg(windows)]
fn win_pipe_server_loop(pipe_name: &str, child_pid: i32, done: &std::sync::atomic::AtomicBool) {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::FromRawHandle;
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();

    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }

        // Create one blocking pipe instance.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // Could not create the instance; back off briefly and retry unless
            // we are shutting down.
            if done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        // Block until a client connects (or the nudge connection arrives).
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        // ConnectNamedPipe returns 0 on failure; ERROR_PIPE_CONNECTED also means
        // a client is already present. Either way, if shutting down we bail.
        let _ = connected;

        if done.load(Ordering::SeqCst) {
            unsafe {
                DisconnectNamedPipe(handle);
                CloseHandle(handle);
            }
            break;
        }

        // Serve exactly one request/response on this instance using the SAME
        // newline-delimited JSON protocol as the unix transport.
        // SAFETY: handle is a valid owned pipe handle; File owns and closes it.
        let file = unsafe { File::from_raw_handle(handle as *mut _) };
        if let Ok(write_half) = file.try_clone() {
            let mut writer = write_half;
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let line = line.trim();
                if let Ok(req) = serde_json::from_str::<serde_json::Value>(line) {
                    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                    let reply: serde_json::Value = match op {
                        "status" => serde_json::json!({"status": "running"}),
                        "signal" => {
                            if let Some(sig) = req.get("sig").and_then(|v| v.as_i64()) {
                                // Map unix signal → forced TerminateProcess.
                                // Exit code follows the unix 128+sig convention.
                                let code = (128 + sig) as u32;
                                let ok = win_terminate(child_pid, code);
                                serde_json::json!({"ok": ok})
                            } else {
                                serde_json::json!({"ok": false})
                            }
                        }
                        _ => serde_json::json!({"error": "unknown op"}),
                    };
                    let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
                    reply_bytes.push(b'\n');
                    let _ = writer.write_all(&reply_bytes);
                    let _ = writer.flush();
                }
            }
            // `reader` (and the underlying handle) and `writer` drop here,
            // flushing and closing the instance — disconnecting the client.
        }
    }
}

// WIN-PATH: unblock a pending ConnectNamedPipe by opening the pipe once and
// immediately dropping the connection. Used by the supervisor on child exit so
// the blocking server thread can observe `done` and terminate. Best-effort —
// failure is harmless (the next loop check still exits). Validatable only on a
// real Windows box.
#[cfg(windows)]
fn win_pipe_nudge(pipe_name: &str) {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};

    let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        // Own and immediately drop → closes the handle, completing the
        // server's ConnectNamedPipe so it can re-check `done`.
        let _f = unsafe { File::from_raw_handle(handle as *mut _) };
    }
}

// ---------------------------------------------------------------------------
// ps
// ---------------------------------------------------------------------------

pub fn ps(store_home: &std::path::Path) -> Result<Vec<RunInfo>> {
    let run_dir = store_home.join("run");

    if !run_dir.exists() {
        return Ok(vec![]);
    }

    let mut infos: Vec<RunInfo> = Vec::new();

    let entries = std::fs::read_dir(&run_dir).map_err(LightrError::Io)?;
    for entry in entries {
        let entry = entry.map_err(LightrError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Read spec.json
        let spec = match read_spec_on_disk(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Determine running state
        let sock = ctl_sock_path(&path);
        let running = if sock.exists() {
            // Also check pid alive
            if let Some(pid) = read_pid_file(&path) {
                #[cfg(any(unix, windows))]
                {
                    pid_alive(pid)
                }
                #[cfg(not(any(unix, windows)))]
                {
                    true
                }
            } else {
                false
            }
        } else {
            false
        };

        let exit_code = read_status_file(&path)
            .as_deref()
            .and_then(parse_exit_code_from_status);

        let health = crate::healthcheck::read_state(&path);

        infos.push(RunInfo {
            id,
            running,
            exit_code,
            command: spec.command,
            created_at_unix: spec.created_at_unix,
            health,
            engine: spec.engine,
            ports: spec.ports,
            rootfs_ref: spec.rootfs_ref,
        });
    }

    // Sort by id descending (newest first — id starts with unix_nanos)
    infos.sort_by(|a, b| b.id.cmp(&a.id));

    Ok(infos)
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

pub fn logs(dir: &std::path::Path, stream: LogStream, follow: bool) -> Result<()> {
    use std::io::Write;

    fn print_file(path: &std::path::Path, offset: &mut u64) -> Result<bool> {
        let data = std::fs::read(path).map_err(LightrError::Io)?;
        let start = *offset as usize;
        if start < data.len() {
            std::io::stdout()
                .write_all(&data[start..])
                .map_err(LightrError::Io)?;
            *offset = data.len() as u64;
            return Ok(true);
        }
        Ok(false)
    }

    let stdout_path = dir.join("stdout.log");
    let stderr_path = dir.join("stderr.log");

    if !follow {
        match stream {
            LogStream::Stdout => {
                let _ = print_file(&stdout_path, &mut 0u64);
            }
            LogStream::Stderr => {
                let _ = print_file(&stderr_path, &mut 0u64);
            }
            LogStream::Both => {
                let _ = print_file(&stdout_path, &mut 0u64);
                let _ = print_file(&stderr_path, &mut 0u64);
            }
        }
        return Ok(());
    }

    // Follow mode
    let mut stdout_off = 0u64;
    let mut stderr_off = 0u64;

    loop {
        let mut had_new = false;
        match stream {
            LogStream::Stdout => {
                if stdout_path.exists() {
                    had_new |= print_file(&stdout_path, &mut stdout_off)?;
                }
            }
            LogStream::Stderr => {
                if stderr_path.exists() {
                    had_new |= print_file(&stderr_path, &mut stderr_off)?;
                }
            }
            LogStream::Both => {
                if stdout_path.exists() {
                    had_new |= print_file(&stdout_path, &mut stdout_off)?;
                }
                if stderr_path.exists() {
                    had_new |= print_file(&stderr_path, &mut stderr_off)?;
                }
            }
        }
        let _ = had_new;

        // Check if exited and no new bytes
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            // Drain any remaining
            let mut drained = false;
            match stream {
                LogStream::Stdout => {
                    if stdout_path.exists() {
                        drained |= print_file(&stdout_path, &mut stdout_off)?;
                    }
                }
                LogStream::Stderr => {
                    if stderr_path.exists() {
                        drained |= print_file(&stderr_path, &mut stderr_off)?;
                    }
                }
                LogStream::Both => {
                    if stdout_path.exists() {
                        drained |= print_file(&stdout_path, &mut stdout_off)?;
                    }
                    if stderr_path.exists() {
                        drained |= print_file(&stderr_path, &mut stderr_off)?;
                    }
                }
            }
            if !drained {
                break;
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// stop
// ---------------------------------------------------------------------------

pub fn stop(dir: &std::path::Path, grace_secs: u64) -> Result<i32> {
    use std::time::{Duration, Instant};

    let sock = ctl_sock_path(dir);

    if sock.exists() {
        // Try sending SIGTERM via ctl.sock
        send_ctl_op(dir, r#"{"op":"signal","sig":15}"#);
    } else if let Some(pid) = read_pid_file(dir) {
        // Direct kill
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        // WIN-PATH: no SIGTERM equivalent — best-effort forced terminate with
        // the unix 128+SIGTERM(15)=143 exit code. Graceful-term semantics
        // differ from unix; validatable only on a real Windows box.
        #[cfg(windows)]
        {
            win_terminate(pid, 143);
        }
    }

    // Poll for grace_secs
    let deadline = Instant::now() + Duration::from_secs(grace_secs);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        // Check if already exited
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            if let Some(code) = parse_exit_code_from_status(&status) {
                return Ok(code);
            }
        }
        // Check pid alive (pid_alive is implemented on unix and windows)
        if let Some(pid) = read_pid_file(dir) {
            if !pid_alive(pid) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Check again after grace
    let status = read_status_file(dir).unwrap_or_default();
    if status.starts_with("exited") {
        if let Some(code) = parse_exit_code_from_status(&status) {
            return Ok(code);
        }
    }

    // Still alive — SIGKILL
    if let Some(pid) = read_pid_file(dir) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        // WIN-PATH: SIGKILL → forced TerminateProcess with the unix
        // 128+SIGKILL(9)=137 exit code. Validatable only on a real Windows box.
        #[cfg(windows)]
        {
            win_terminate(pid, 137);
        }
    }

    // Wait a bit for status file update
    let kill_deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            if let Some(code) = parse_exit_code_from_status(&status) {
                return Ok(code);
            }
        }
        if std::time::Instant::now() >= kill_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(137)
}

// ---------------------------------------------------------------------------
// exec_in
// ---------------------------------------------------------------------------

pub fn exec_in(dir: &std::path::Path, command: &[String]) -> Result<i32> {
    let spec = read_spec_on_disk(dir)?;
    let cwd = PathBuf::from(&spec.cwd);

    if command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(LightrError::Io)?;

    let status = child.wait().map_err(LightrError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code().unwrap_or(1))
    }
}

// ---------------------------------------------------------------------------
// R4 additions — frozen contract: build-spec-r4.md §1 (bodies: R4-W1)
// ---------------------------------------------------------------------------

/// Deep-memo (opt-in nitro, ADR-0016): process-tree memoization via a
/// spawn-shim. Degrades HONESTLY to whole-run memo when the shim can't
/// attach (SIP/static binaries) — never silently claims the capability.
pub struct DeepMemoConfig {
    pub enabled: bool,
}

/// Probe whether the deep-memo spawn-shim mechanism is available on this host.
///
/// Returns `(available, reason)`.
///
/// R4 scope: no prebuilt dylib ships yet, so this always returns `(false,
/// reason)`.  A future WP that ships the dylib flips this to `(true, "")` by
/// (a) checking `$LIGHTR_HOME/shims/lightr_shim.dylib` exists and
/// (b) confirming DYLD injection is allowed for the target interpreter.
/// The caller (CLI W2) is responsible for surfacing `reason` to the user
/// when `available` is false and `--deep-memo` was requested.
///
/// Note: DYLD_INSERT_LIBRARIES injection is blocked for SIP-protected
/// system interpreters (e.g. `/bin/sh`); that check belongs here once
/// a real shim path is probed.
pub fn deep_memo_available() -> (bool, String) {
    // Probe: does the shim dylib exist at $LIGHTR_HOME/shims/lightr_shim.dylib?
    let shim_path = lightr_home().join("shims").join("lightr_shim.dylib");
    if !shim_path.exists() {
        return (
            false,
            format!(
                "deep-memo unavailable (no shim installed at {}) \
                 \u{2014} falling back to whole-run memo",
                shim_path.display()
            ),
        );
    }
    // Shim exists: future WP validates DYLD injection is permitted and
    // loads the dylib. For now, treat presence as insufficient (not yet
    // integrated) and return unavailable.
    (
        false,
        "deep-memo unavailable (shim present but not yet integrated) \
         \u{2014} falling back to whole-run memo"
            .to_string(),
    )
}

/// run_memoized with optional deep-memo (build-spec-r4 §1, ADR-0016).
///
/// Behaviour:
/// - `cfg.enabled == false`: exactly `run_memoized(spec, store)` — no change.
/// - `cfg.enabled == true`: calls `deep_memo_available()`; since R4 ships no
///   prebuilt shim, this always returns `(false, reason)`, so the function
///   falls back to `run_memoized`.  The CLI (W2) surfaces the reason string
///   to the user via `deep_memo_available()`.  **No sub-process memoization
///   is faked; the fallback is to whole-run memo, honestly.**
///
/// The `RunOutcome` returned is identical to `run_memoized` in all cases.
pub fn run_memoized_deep(
    spec: &RunSpec,
    store: &Store,
    cfg: &DeepMemoConfig,
) -> Result<RunOutcome> {
    if !cfg.enabled {
        return run_memoized(spec, store);
    }

    // Probe the shim mechanism.  On this host deep-memo is not yet available
    // (R4 ships no dylib); the CLI is responsible for printing the reason.
    let (_available, _reason) = deep_memo_available();
    // _available is always false in R4; _reason is consumed by the CLI layer.
    // Honest fallback: whole-run memoization, correctness preserved.
    run_memoized(spec, store)
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

    // LIGHTR_HOME is process-global (index dir): serialize tests and isolate
    // each one in a tempdir home so ~ is never touched.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("LIGHTR_HOME", home.path());
        (home, guard)
    }

    fn make_store(dir: &std::path::Path) -> Store {
        Store::open(dir.join("store")).expect("store open")
    }

    fn make_spec(cwd: &std::path::Path, command: Vec<&str>) -> RunSpec {
        RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: command.into_iter().map(|s| s.to_string()).collect(),
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // WP-NET2: SpecOnDisk gains engine + rootfs_ref — serde back-compat + roundtrip
    // -----------------------------------------------------------------------

    /// A pre-WP-NET2 spec.json (no `engine`/`rootfs_ref`) must still parse, with
    /// `engine == "native"` (the supervisor's native branch) and no rootfs ref —
    /// so an old detached native run is read back byte-for-byte in behaviour.
    #[test]
    fn spec_on_disk_legacy_json_defaults_to_native() {
        let legacy = r#"{
            "cwd": "/w", "command": ["sleep","1"], "env_keys": [],
            "mounts": [], "detached": true, "created_at_unix": 1
        }"#;
        let spec: SpecOnDisk = serde_json::from_str(legacy).expect("legacy spec parses");
        assert_eq!(spec.engine, "native", "missing engine ⇒ native branch");
        assert!(spec.rootfs_ref.is_none(), "missing rootfs_ref ⇒ None");
        assert!(
            spec.ports.is_empty(),
            "missing ports ⇒ empty (existing default)"
        );
    }

    /// A vz container spec roundtrips through write/read with engine + rootfs_ref
    /// preserved — what the supervisor reads to select the vz branch.
    #[test]
    fn spec_on_disk_vz_roundtrip_preserves_engine_and_rootfs() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SpecOnDisk {
            cwd: "/w".to_string(),
            command: vec!["sh".to_string()],
            env_keys: vec![],
            mounts: vec![],
            detached: true,
            created_at_unix: 1,
            ports: vec![(18080, 80)],
            engine: "vz".to_string(),
            rootfs_ref: Some("alpine".to_string()),
            env: vec![],
        };
        write_spec_json(dir.path(), &spec).expect("write");
        let back = read_spec_on_disk(dir.path()).expect("read");
        assert_eq!(back.engine, "vz");
        assert_eq!(back.rootfs_ref.as_deref(), Some("alpine"));
        assert_eq!(back.ports, vec![(18080, 80)]);
    }

    // -----------------------------------------------------------------------
    // key_stability: same spec twice => same key via two scans
    // -----------------------------------------------------------------------
    #[test]
    fn key_stability() {
        let (_home, _env_guard) = isolated_home();
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
    // MEMO-KEY LAW: ports are RUNTIME, not a key input (like resource limits;
    // like Docker, which does not key on -p). Two specs differing ONLY in
    // `ports` MUST produce the same memo key.
    // -----------------------------------------------------------------------
    #[test]
    fn ports_excluded_from_key() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("f.txt"), b"data").unwrap();

        let mut spec_no_ports = make_spec(cwd, vec!["/bin/echo", "x"]);
        spec_no_ports.ports = vec![];

        let mut spec_with_ports = make_spec(cwd, vec!["/bin/echo", "x"]);
        spec_with_ports.ports = vec![
            PortMap {
                host: 8080,
                container: 80,
            },
            PortMap {
                host: 9090,
                container: 90,
            },
        ];

        let k1 = build_key(&spec_no_ports).expect("k1");
        let k2 = build_key(&spec_with_ports).expect("k2");
        assert_eq!(
            k1.0, k2.0,
            "ports must NOT affect the memo key (runtime-only, like -p in Docker)"
        );
    }

    // -----------------------------------------------------------------------
    // key_changes_when_input_file_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_input_file_changes() {
        let (_home, _env_guard) = isolated_home();
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
        let (_home, _env_guard) = isolated_home();
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
        let (_home, _env_guard) = isolated_home();
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
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
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
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

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
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
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
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

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
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
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
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

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
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
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
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

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
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
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

    // -----------------------------------------------------------------------
    // R1 tests
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Helper: create a run dir + spec.json and launch supervise() in a thread.
    // Returns (home_path, run_dir, thread_handle).
    // Unit tests cannot use spawn_detached (requires real `lightr` binary via
    // current_exe) so we call supervise() directly in a thread instead.
    // -----------------------------------------------------------------------
    fn start_supervised(
        home_path: &std::path::Path,
        cwd: &std::path::Path,
        command: Vec<String>,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<i32>) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let id = format!("{nanos}-test");
        let run_dir = home_path.join("run").join(&id);
        fs::create_dir_all(&run_dir).unwrap();

        let spec_on_disk = SpecOnDisk {
            cwd: cwd.to_string_lossy().into_owned(),
            command,
            env_keys: vec![],
            mounts: vec![],
            detached: false,
            created_at_unix: nanos / 1_000_000_000,
            ports: vec![],
            engine: "native".to_string(),
            rootfs_ref: None,
            env: vec![],
        };
        write_spec_json(&run_dir, &spec_on_disk).unwrap();

        let run_dir_clone = run_dir.clone();
        let t = std::thread::spawn(move || supervise(&run_dir_clone).unwrap_or(-1));
        (run_dir, t)
    }

    // -----------------------------------------------------------------------
    // detach_lifecycle: supervisor sleep 5 → ps shows running → stop → ps exited
    // (uses supervise() directly in a thread — spawn_detached needs real binary)
    // -----------------------------------------------------------------------
    #[test]
    fn detach_lifecycle() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();

        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        let (run_dir, _supervisor_thread) =
            start_supervised(&home_path, cwd, vec!["sleep".to_string(), "10".to_string()]);

        // Give supervisor time to write pid+status+ctl.sock
        let startup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if ctl_sock_path(&run_dir).exists()
                && read_status_file(&run_dir)
                    .map(|s| s == "running")
                    .unwrap_or(false)
            {
                break;
            }
            if std::time::Instant::now() >= startup_deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // ps should show it running
        let infos = ps(&home_path).expect("ps");
        let id = run_dir.file_name().unwrap().to_string_lossy().into_owned();
        let found = infos.iter().find(|i| i.id == id);
        assert!(found.is_some(), "run not found in ps output");
        let info = found.unwrap();
        assert!(info.running, "run should be running");

        // stop it (grace=2s)
        let exit_code = stop(&run_dir, 2).expect("stop");
        // exit code after SIGTERM/SIGKILL: 143, 137, or 0 (if supervisor exited first)
        assert!(
            exit_code == 143 || exit_code == 137 || exit_code == 0,
            "unexpected exit code: {exit_code}"
        );

        // ps should now show not running
        let infos2 = ps(&home_path).expect("ps2");
        let found2 = infos2.iter().find(|i| i.id == id);
        // Either not found (dir removed) or found as not-running
        if let Some(info2) = found2 {
            assert!(!info2.running, "run should not be running after stop");
        }
    }

    // -----------------------------------------------------------------------
    // logs_non_follow: write known content via supervisor, check log files
    // (uses supervise() directly in a thread — spawn_detached needs real binary)
    // -----------------------------------------------------------------------
    #[test]
    fn logs_non_follow() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();

        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        let (run_dir, supervisor_thread) = start_supervised(
            &home_path,
            cwd,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo STDOUT_CONTENT; echo STDERR_CONTENT >&2".to_string(),
            ],
        );

        // Wait for supervisor to finish (process is short-lived)
        let _ = supervisor_thread.join();

        // Check stdout.log content
        let stdout_content = fs::read_to_string(run_dir.join("stdout.log")).unwrap_or_default();
        assert!(
            stdout_content.contains("STDOUT_CONTENT"),
            "stdout.log missing STDOUT_CONTENT: {stdout_content:?}"
        );

        // Check stderr.log content
        let stderr_content = fs::read_to_string(run_dir.join("stderr.log")).unwrap_or_default();
        assert!(
            stderr_content.contains("STDERR_CONTENT"),
            "stderr.log missing STDERR_CONTENT: {stderr_content:?}"
        );
    }

    // -----------------------------------------------------------------------
    // exec_in_cwd: exec_in should run in the spec's cwd
    // -----------------------------------------------------------------------
    #[test]
    fn exec_in_cwd_correctness() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();

        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().to_path_buf();
        let store = make_store(&home_path);

        let spec = RunSpec {
            cwd: cwd.clone(),
            inputs: vec![],
            command: vec!["sleep".to_string(), "30".to_string()],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        };

        let handle = spawn_detached(&spec, &store).expect("spawn_detached");
        let run_dir = handle.dir.clone();

        // Give supervisor time to write spec.json
        std::thread::sleep(std::time::Duration::from_millis(300));

        // exec_in with pwd — should print the run's cwd
        // We capture by using a temp file
        let out_file = tmp.path().join("pwd_output.txt");
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("pwd > {}", out_file.display()),
        ];

        let exit_code = exec_in(&run_dir, &cmd).expect("exec_in");
        assert_eq!(exit_code, 0, "exec_in should exit 0");

        let output = fs::read_to_string(&out_file).unwrap_or_default();
        let canonical_cwd = cwd.canonicalize().unwrap();
        assert!(
            output.trim() == canonical_cwd.to_string_lossy().as_ref(),
            "exec_in cwd mismatch: got {output:?}, expected {:?}",
            canonical_cwd
        );

        // Clean up: stop the sleeper
        let _ = stop(&run_dir, 1);
    }

    // -----------------------------------------------------------------------
    // mount_escape_rejected: mount target with ".." rejected
    // -----------------------------------------------------------------------
    #[test]
    fn mount_escape_rejected() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        let err = validate_mount_target("../escape");
        assert!(err.is_err(), "mount target with '..' must be rejected");

        let err2 = validate_mount_target("a/../../escape");
        assert!(
            err2.is_err(),
            "mount target escaping via a/../../ must be rejected"
        );

        // Valid relative target
        assert!(validate_mount_target("subdir").is_ok());
        assert!(validate_mount_target("a/b/c").is_ok());

        // Absolute path rejected
        assert!(validate_mount_target("/abs").is_err());

        let _ = cwd; // suppress unused
    }

    // -----------------------------------------------------------------------
    // mounts_run: run with mount of snapshotted ref → file present in cwd/target
    //             + key changes when mount ref repointed
    // -----------------------------------------------------------------------
    #[test]
    fn mounts_run_and_key_change() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();

        // Create a source dir with a file to snapshot
        let src_v1 = tmp.path().join("src_v1");
        fs::create_dir(&src_v1).unwrap();
        fs::write(src_v1.join("hello.txt"), b"hello from v1").unwrap();

        // Snapshot src_v1 as ref "testmount"
        lightr_index::snapshot(&src_v1, &store, "testmount").expect("snapshot v1");

        // Create cwd (separate from input)
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        // Run with mount: ref=testmount, target=mounted
        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat mounted/hello.txt".to_string(),
            ],
            env_keys: vec![],
            mounts: vec![Mount {
                ref_name: "testmount".to_string(),
                target: "mounted".to_string(),
            }],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        };

        let out1 = run_memoized(&spec, &store).expect("run1 with mount");
        assert_eq!(out1.exit_code, 0, "mounted run should exit 0");
        assert!(
            out1.stdout.starts_with(b"hello from v1"),
            "stdout should contain file content"
        );

        // Verify file was hydrated
        // (After run, the mounted dir should be present — it was hydrated for the miss)
        // Key from first run
        let key1 = out1.key;

        // Now create v2 with different content, re-snapshot to "testmount"
        let src_v2 = tmp.path().join("src_v2");
        fs::create_dir(&src_v2).unwrap();
        fs::write(src_v2.join("hello.txt"), b"hello from v2").unwrap();
        lightr_index::snapshot(&src_v2, &store, "testmount").expect("snapshot v2");

        // Remove the mounted dir so hydrate can succeed again
        let mounted_dir = work.join("mounted");
        if mounted_dir.exists() {
            fs::remove_dir_all(&mounted_dir).unwrap();
        }

        // Re-run: key must change because mount ref's root digest changed
        let out2 = run_memoized(&spec, &store).expect("run2 with mount v2");
        assert_eq!(out2.exit_code, 0);
        assert!(
            out2.stdout.starts_with(b"hello from v2"),
            "stdout should contain v2 content"
        );
        assert_ne!(
            key1, out2.key,
            "key must change when mount ref is repointed"
        );
    }

    // -----------------------------------------------------------------------
    // predict: miss → run → predict hit
    // -----------------------------------------------------------------------
    #[test]
    fn predict_miss_run_hit() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "predict-test".to_string()],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        };

        // predict before run: should be miss
        let (key1, hit1) = predict(&spec, &store).expect("predict1");
        assert!(!hit1, "predict before run must be miss");

        // Run it
        let out = run_memoized(&spec, &store).expect("run");
        assert!(!out.hit, "first run must be miss");
        assert_eq!(out.key, key1, "predict key must match run key");

        // predict after run: should be hit
        let (key2, hit2) = predict(&spec, &store).expect("predict2");
        assert_eq!(key1, key2, "key must be stable");
        assert!(hit2, "predict after run must be hit");
    }

    // -----------------------------------------------------------------------
    // R4 tests — build-spec-r4.md §1
    // -----------------------------------------------------------------------

    // deep_memo_disabled_equals_run_memoized:
    // run_memoized_deep(cfg.enabled=false) must produce same key and hit
    // behaviour as run_memoized — miss on first call, hit on second.
    #[test]
    fn deep_memo_disabled_equals_run_memoized() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let side_effect = tmp.path().join("dm_disabled_side.txt");
        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!("echo deep >> {}", side_effect.display()),
            ],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        };
        let cfg = DeepMemoConfig { enabled: false };

        // First call: miss (same as run_memoized miss)
        let out1 = run_memoized_deep(&spec, &store, &cfg).expect("deep miss");
        assert!(!out1.hit, "disabled deep-memo first call must be miss");
        assert_eq!(out1.exit_code, 0);

        // Second call: hit (run_memoized would also hit)
        let out2 = run_memoized_deep(&spec, &store, &cfg).expect("deep hit");
        assert!(out2.hit, "disabled deep-memo second call must be hit");
        assert_eq!(out2.key, out1.key, "key must be stable across calls");

        // Verify same key as plain run_memoized would produce
        let out_plain = run_memoized(&spec, &store).expect("plain hit");
        assert!(out_plain.hit, "plain run_memoized should also hit");
        assert_eq!(
            out1.key, out_plain.key,
            "deep disabled key must match plain key"
        );

        // Side-effect written once (command did not re-execute on hit)
        let line_count = std::fs::read_to_string(&side_effect)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(line_count, 1, "side effect must be written exactly once");
    }

    // deep_memo_available_returns_false_with_shim_reason:
    // On this host (no shim installed), deep_memo_available() must return
    // (false, reason) where reason is non-empty and mentions "shim" or "unavailable".
    #[test]
    fn deep_memo_available_returns_false_with_shim_reason() {
        let (_home, _env_guard) = isolated_home();
        let (available, reason) = deep_memo_available();
        assert!(
            !available,
            "deep_memo_available must return false on R4 host"
        );
        assert!(!reason.is_empty(), "reason must be non-empty");
        let reason_lower = reason.to_lowercase();
        assert!(
            reason_lower.contains("shim") || reason_lower.contains("unavailable"),
            "reason must mention 'shim' or 'unavailable', got: {reason:?}"
        );
    }

    // deep_memo_enabled_fallback_correctness:
    // run_memoized_deep(cfg.enabled=true) on this host falls back to
    // whole-run memo: miss then hit; deep_memo_available() confirms (false, reason).
    #[test]
    fn deep_memo_enabled_fallback_correctness() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let side_effect = tmp.path().join("dm_enabled_side.txt");
        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!("echo enabled >> {}", side_effect.display()),
            ],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        };
        let cfg_on = DeepMemoConfig { enabled: true };

        // Confirm probe says unavailable before we call the function
        let (available, reason) = deep_memo_available();
        assert!(!available);
        assert!(!reason.is_empty());

        // First call with enabled=true: should return Ok, fall back to miss
        let out1 = run_memoized_deep(&spec, &store, &cfg_on).expect("enabled call 1 must not err");
        assert!(
            !out1.hit,
            "first enabled call must be miss (fallback to whole-run memo)"
        );
        assert_eq!(out1.exit_code, 0);

        // Second call with enabled=true: should hit (whole-run memo populated)
        let out2 = run_memoized_deep(&spec, &store, &cfg_on).expect("enabled call 2 must not err");
        assert!(
            out2.hit,
            "second enabled call must be hit (fallback memoized)"
        );
        assert_eq!(out2.key, out1.key, "keys must be stable");

        // Side-effect written once (no double-exec)
        let line_count = std::fs::read_to_string(&side_effect)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(line_count, 1, "side effect must be written exactly once");
    }

    // -----------------------------------------------------------------------
    // F-309 — secrets / configs (build-spec-parity.md §0/§4)
    // -----------------------------------------------------------------------

    /// Snapshot a dir holding one file `<file_name>` with `bytes` as ref `name`.
    /// Returns the ref's root digest.
    fn snapshot_file_ref(
        store: &Store,
        name: &str,
        file_name: &str,
        bytes: &[u8],
    ) -> lightr_core::Digest {
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join(file_name), bytes).unwrap();
        let rep = lightr_index::snapshot(src.path(), store, name).expect("snapshot ref");
        rep.root
    }

    // Changing a secret REF must change the memo key (cache miss). Two specs
    // differing only in a secret ref must produce different keys (§0).
    #[test]
    fn secret_ref_changes_memo_key() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        // Two distinct refs (different content ⇒ different root digest).
        snapshot_file_ref(&store, "sec-a", "token", b"AAAA");
        snapshot_file_ref(&store, "sec-b", "token", b"BBBB");

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        let mk = |ref_name: &str| RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "x".to_string()],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![StoreFile {
                name: "token".to_string(),
                ref_name: ref_name.to_string(),
            }],
            configs: vec![],
            ports: vec![],
        };

        let spec_a = mk("sec-a");
        let spec_b = mk("sec-b");

        // predict computes the key without executing (routes through assemble_key
        // because secrets is non-empty).
        let (key_a, _) = predict(&spec_a, &store).expect("predict a");
        let (key_b, _) = predict(&spec_b, &store).expect("predict b");
        assert_ne!(
            key_a, key_b,
            "a different secret ref must produce a different memo key"
        );

        // And the same secret ref is stable.
        let (key_a2, _) = predict(&spec_a, &store).expect("predict a2");
        assert_eq!(key_a, key_a2, "same secret ref ⇒ stable key");
    }

    // A config ref likewise contributes to the key, in its own domain (so a
    // secret and a config with the SAME name+ref do not collide).
    #[test]
    fn config_ref_changes_memo_key_and_domain_separated() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        snapshot_file_ref(&store, "cfg-ref", "data", b"hello");

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        let base = || RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "x".to_string()],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![],
            configs: vec![],
            ports: vec![],
        };

        let mut as_secret = base();
        as_secret.secrets = vec![StoreFile {
            name: "f".to_string(),
            ref_name: "cfg-ref".to_string(),
        }];
        let mut as_config = base();
        as_config.configs = vec![StoreFile {
            name: "f".to_string(),
            ref_name: "cfg-ref".to_string(),
        }];

        let (key_secret, _) = predict(&as_secret, &store).expect("predict secret");
        let (key_config, _) = predict(&as_config, &store).expect("predict config");
        let (key_none, _) = predict(&base(), &store).expect("predict none");

        assert_ne!(key_secret, key_none, "a secret must change the key");
        assert_ne!(key_config, key_none, "a config must change the key");
        assert_ne!(
            key_secret, key_config,
            "secret vs config domains must be separated (same name+ref must not collide)"
        );
    }

    // Empty secrets/configs ⇒ key is byte-identical to a spec with no F-309
    // fields, i.e. the storeless fast path. Guards the 16 existing callers.
    #[test]
    fn empty_secrets_configs_leave_key_unchanged() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("f.txt"), b"data").unwrap();

        let spec = make_spec(&work, vec!["/bin/echo", "x"]);
        // build_key is the storeless fast path (no mounts/secrets/configs).
        let fast = build_key(&spec).expect("fast key");
        // predict routes through the same fast path when there are no
        // store-backed inputs; it must agree byte-for-byte.
        let (predicted, _) = predict(&spec, &store).expect("predict");
        assert_eq!(
            fast, predicted,
            "empty secrets/configs ⇒ fast path key == predict key"
        );
    }

    // Secret hydrated to <cwd>/.lightr/secrets/<name> at 0600; config at
    // <cwd>/.lightr/configs/<name> at 0644 (unix). The ref is a snapshot tree,
    // so <name> is a dir holding the snapshot's file at the requested mode.
    #[test]
    fn secret_config_hydrate_path_and_mode() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        snapshot_file_ref(&store, "my-secret", "token.txt", b"s3cr3t");
        snapshot_file_ref(&store, "my-config", "app.conf", b"k=v");

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        secrets::hydrate(
            &work,
            &store,
            &[StoreFile {
                name: "sec".to_string(),
                ref_name: "my-secret".to_string(),
            }],
            &[StoreFile {
                name: "cfg".to_string(),
                ref_name: "my-config".to_string(),
            }],
        )
        .expect("hydrate ok");

        let secret_file = work.join(".lightr/secrets/sec/token.txt");
        let config_file = work.join(".lightr/configs/cfg/app.conf");
        assert!(secret_file.exists(), "secret file must be materialized");
        assert!(config_file.exists(), "config file must be materialized");
        assert_eq!(fs::read(&secret_file).unwrap(), b"s3cr3t");
        assert_eq!(fs::read(&config_file).unwrap(), b"k=v");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let smode = fs::metadata(&secret_file).unwrap().permissions().mode() & 0o777;
            let cmode = fs::metadata(&config_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(smode, 0o600, "secret file must be 0600, got {smode:o}");
            assert_eq!(cmode, 0o644, "config file must be 0644, got {cmode:o}");
        }
    }

    // A missing secret ref must fail CLOSED (Err), no run proceeds.
    #[test]
    fn missing_secret_ref_fails_closed() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        let err = secrets::hydrate(
            &work,
            &store,
            &[StoreFile {
                name: "sec".to_string(),
                ref_name: "no-such-ref".to_string(),
            }],
            &[],
        );
        assert!(err.is_err(), "missing secret ref must fail closed");

        // And via run_memoized_with: a missing secret aborts the whole run.
        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "x".to_string()],
            env_keys: vec![],
            mounts: vec![],
            secrets: vec![StoreFile {
                name: "sec".to_string(),
                ref_name: "no-such-ref".to_string(),
            }],
            configs: vec![],
            ports: vec![],
        };
        let run_err = run_memoized_with(&spec, &store, &lightr_core::ResourceLimits::default());
        assert!(run_err.is_err(), "run with a missing secret must Err");
    }

    // End-to-end: a detached supervisor with a FAILING healthcheck writes
    // "unhealthy" to <run_dir>/health and ps surfaces it. Uses supervise()
    // directly (spawn_detached needs the real binary).
    #[test]
    fn supervisor_health_flips_unhealthy() {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();

        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        // Build a run dir with a long-lived child + a persisted FAILING probe.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let id = format!("{nanos}-health");
        let run_dir = home_path.join("run").join(&id);
        fs::create_dir_all(&run_dir).unwrap();

        let spec_on_disk = SpecOnDisk {
            cwd: cwd.to_string_lossy().into_owned(),
            command: vec!["sleep".to_string(), "10".to_string()],
            env_keys: vec![],
            mounts: vec![],
            detached: false,
            created_at_unix: nanos / 1_000_000_000,
            ports: vec![],
            engine: "native".to_string(),
            rootfs_ref: None,
            env: vec![],
        };
        write_spec_json(&run_dir, &spec_on_disk).unwrap();
        healthcheck::save_for(
            &run_dir,
            &healthcheck::Healthcheck {
                cmd: "exit 1".to_string(), // always fails ⇒ Unhealthy
                interval_s: 1,
                retries: 0,
            },
        )
        .unwrap();

        let run_dir_clone = run_dir.clone();
        let t = std::thread::spawn(move || supervise(&run_dir_clone).unwrap_or(-1));

        // Wait for the supervisor to write the first health verdict.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut health = None;
        while Instant::now() < deadline {
            if let Some(h) = healthcheck::read_state(&run_dir) {
                health = Some(h);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            health,
            Some(healthcheck::Health::Unhealthy),
            "a failing healthcheck must flip the run to Unhealthy"
        );

        // ps surfaces the same verdict while the run is alive.
        let infos = ps(&home_path).expect("ps");
        let info = infos.iter().find(|i| i.id == id).expect("run in ps");
        assert_eq!(info.health, Some(healthcheck::Health::Unhealthy));

        // Clean up the sleeper + supervisor.
        let _ = stop(&run_dir, 2);
        let _ = t.join();
    }

    // -----------------------------------------------------------------------
    // ps_enrich_fields: ps() surfaces engine, ports, and rootfs_ref from
    // SpecOnDisk (WP-PS-ENRICH). Verifies defaults (native / empty / None)
    // and explicit values without spinning up a real supervisor.
    // -----------------------------------------------------------------------
    #[test]
    fn ps_enrich_fields() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();

        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        // --- Case A: native run, no ports, no rootfs_ref ---
        let id_a = format!("{nanos}-enrich-a");
        let run_dir_a = home_path.join("run").join(&id_a);
        fs::create_dir_all(&run_dir_a).unwrap();
        let spec_a = SpecOnDisk {
            cwd: cwd.to_string_lossy().into_owned(),
            command: vec!["true".to_string()],
            env_keys: vec![],
            mounts: vec![],
            detached: true,
            created_at_unix: nanos / 1_000_000_000,
            ports: vec![],
            engine: "native".to_string(),
            rootfs_ref: None,
            env: vec![],
        };
        write_spec_json(&run_dir_a, &spec_a).unwrap();
        // Write exited status so ps picks it up without a real supervisor.
        fs::write(run_dir_a.join("status"), "exited 0").unwrap();

        // --- Case B: vz run, one port pair, with rootfs_ref ---
        let id_b = format!("{nanos}-enrich-b");
        let run_dir_b = home_path.join("run").join(&id_b);
        fs::create_dir_all(&run_dir_b).unwrap();
        let spec_b = SpecOnDisk {
            cwd: cwd.to_string_lossy().into_owned(),
            command: vec!["/bin/nginx".to_string()],
            env_keys: vec![],
            mounts: vec![],
            detached: true,
            created_at_unix: nanos / 1_000_000_000,
            ports: vec![(8080, 80)],
            engine: "vz".to_string(),
            rootfs_ref: Some("my-rootfs".to_string()),
            env: vec![],
        };
        write_spec_json(&run_dir_b, &spec_b).unwrap();
        fs::write(run_dir_b.join("status"), "exited 0").unwrap();

        let infos = ps(&home_path).expect("ps");

        let info_a = infos.iter().find(|i| i.id == id_a).expect("run A in ps");
        assert_eq!(info_a.engine, "native", "case A: engine must be native");
        assert!(info_a.ports.is_empty(), "case A: ports must be empty");
        assert_eq!(info_a.rootfs_ref, None, "case A: rootfs_ref must be None");

        let info_b = infos.iter().find(|i| i.id == id_b).expect("run B in ps");
        assert_eq!(info_b.engine, "vz", "case B: engine must be vz");
        assert_eq!(
            info_b.ports,
            vec![(8080u16, 80u16)],
            "case B: ports must match"
        );
        assert_eq!(
            info_b.rootfs_ref,
            Some("my-rootfs".to_string()),
            "case B: rootfs_ref must match"
        );
    }

    // =======================================================================
    // vz-memo — key determinism/sensitivity + HIT/MISS flow (the product moat)
    // =======================================================================

    fn vz_key(command: Vec<&str>, rootfs: [u8; 32], env: Vec<(&str, &str)>) -> VzMemoKey {
        VzMemoKey {
            command: command.into_iter().map(|s| s.to_string()).collect(),
            rootfs_digest: lightr_core::Digest(rootfs),
            env: env
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    // -----------------------------------------------------------------------
    // vz_memo_key_is_deterministic: identical inputs ⇒ identical key.
    // -----------------------------------------------------------------------
    #[test]
    fn vz_memo_key_is_deterministic() {
        let k1 = vz_key(
            vec!["/bin/echo", "hi"],
            [7u8; 32],
            vec![("PATH", "/usr/bin")],
        );
        let k2 = vz_key(
            vec!["/bin/echo", "hi"],
            [7u8; 32],
            vec![("PATH", "/usr/bin")],
        );
        assert_eq!(
            vz_memo_key(&k1).0,
            vz_memo_key(&k2).0,
            "same inputs must produce the same vz memo key"
        );
    }

    // -----------------------------------------------------------------------
    // vz_memo_key_is_sensitive_to_every_field: any field change ⇒ a new key.
    // Covers command, rootfs_digest, and env (the three key inputs), plus
    // env-split ambiguity (the length-prefix must defeat it).
    // -----------------------------------------------------------------------
    #[test]
    fn vz_memo_key_is_sensitive_to_every_field() {
        let base = vz_key(
            vec!["/bin/echo", "hi"],
            [7u8; 32],
            vec![("PATH", "/usr/bin")],
        );
        let base_key = vz_memo_key(&base).0;

        // (a) command arg change
        let diff_cmd = vz_key(
            vec!["/bin/echo", "bye"],
            [7u8; 32],
            vec![("PATH", "/usr/bin")],
        );
        assert_ne!(
            base_key,
            vz_memo_key(&diff_cmd).0,
            "a command change must change the key"
        );

        // (b) command arity change (one arg vs two — length-prefix defeats
        //     "echo"+"hi" colliding with "echohi").
        let diff_arity = vz_key(vec!["/bin/echohi"], [7u8; 32], vec![("PATH", "/usr/bin")]);
        assert_ne!(
            base_key,
            vz_memo_key(&diff_arity).0,
            "argument boundaries must matter (length-prefixed)"
        );

        // (c) rootfs digest change (a different image ⇒ a different run)
        let diff_rootfs = vz_key(
            vec!["/bin/echo", "hi"],
            [8u8; 32],
            vec![("PATH", "/usr/bin")],
        );
        assert_ne!(
            base_key,
            vz_memo_key(&diff_rootfs).0,
            "a rootfs image change must change the key"
        );

        // (d) env value change
        let diff_env_val = vz_key(vec!["/bin/echo", "hi"], [7u8; 32], vec![("PATH", "/bin")]);
        assert_ne!(
            base_key,
            vz_memo_key(&diff_env_val).0,
            "an env value change must change the key"
        );

        // (e) env split ambiguity: ["A=B", "C"] vs ["A", "B=C"] must differ.
        let split1 = vz_key(vec!["/bin/x"], [7u8; 32], vec![("A", "B"), ("CKEY", "V")]);
        let split2 = vz_key(vec!["/bin/x"], [7u8; 32], vec![("A", "BCKEY"), ("", "V")]);
        assert_ne!(
            vz_memo_key(&split1).0,
            vz_memo_key(&split2).0,
            "env entries must be unambiguously delimited (length-prefixed k=v)"
        );
    }

    // -----------------------------------------------------------------------
    // run_vz_memoized_miss_runs_closure_and_stores: first call MISSes, invokes
    // the closure, and (exit==0, bounded) caches the result.
    // -----------------------------------------------------------------------
    #[test]
    fn run_vz_memoized_miss_runs_closure_and_stores() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());

        let key = vz_key(vec!["/bin/echo", "hi"], [1u8; 32], vec![("PATH", "/bin")]);

        let mut calls = 0u32;
        let out = run_vz_memoized(&key, &store, || {
            calls += 1;
            Ok((0, b"out-bytes".to_vec(), b"err-bytes".to_vec()))
        })
        .expect("miss run");

        assert_eq!(calls, 1, "closure must be invoked exactly once on a miss");
        assert!(!out.hit, "first run must be a miss");
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout, b"out-bytes");
        assert_eq!(out.stderr, b"err-bytes");

        // The result must now be in the Action Cache (exit==0 + bounded).
        let rec = store
            .ac_get(&vz_memo_key(&key))
            .expect("ac_get")
            .expect("record present after a cacheable miss");
        assert!(
            decode_ac_record(&rec).is_some(),
            "the stored AC record must decode"
        );
    }

    // -----------------------------------------------------------------------
    // run_vz_memoized_hit_replays_without_closure: after a cacheable first
    // call, the second call is a HIT that replays {exit, stdout, stderr} from
    // the CAS and NEVER invokes the closure (proven with a counter) — NO VM
    // boot. This is the "work ceases to exist" thesis.
    // -----------------------------------------------------------------------
    #[test]
    fn run_vz_memoized_hit_replays_without_closure() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());

        let key = vz_key(vec!["/bin/echo", "hi"], [2u8; 32], vec![("PATH", "/bin")]);

        // First call seeds the AC (exit==0, bounded).
        let mut first_calls = 0u32;
        let out1 = run_vz_memoized(&key, &store, || {
            first_calls += 1;
            Ok((0, b"replay-out".to_vec(), b"replay-err".to_vec()))
        })
        .expect("seed run");
        assert_eq!(first_calls, 1);
        assert!(!out1.hit);

        // Second identical call MUST be a hit and MUST NOT invoke the closure.
        let mut second_calls = 0u32;
        let out2 = run_vz_memoized(&key, &store, || {
            second_calls += 1;
            // If this ever runs, return a DIFFERENT result so a regression is
            // loud (a real boot would also differ from the cached replay).
            Ok((123, b"SHOULD-NOT-RUN".to_vec(), b"SHOULD-NOT-RUN".to_vec()))
        })
        .expect("hit run");

        assert_eq!(
            second_calls, 0,
            "the closure must NOT run on a hit (no VM boot)"
        );
        assert!(out2.hit, "second run must be a hit");
        assert_eq!(out2.exit_code, 0, "replayed exit code");
        assert_eq!(out2.stdout, b"replay-out", "stdout replayed byte-exact");
        assert_eq!(out2.stderr, b"replay-err", "stderr replayed byte-exact");
        assert_eq!(out1.key.0, out2.key.0, "same key across the two calls");
    }

    // -----------------------------------------------------------------------
    // run_vz_memoized_nonzero_exit_not_cached: a non-zero exit is never cached
    // (mirrors the native exit_nonzero_never_memoized law) ⇒ it re-runs.
    // -----------------------------------------------------------------------
    #[test]
    fn run_vz_memoized_nonzero_exit_not_cached() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());

        let key = vz_key(vec!["/bin/false"], [3u8; 32], vec![("PATH", "/bin")]);

        let mut calls = 0u32;
        let run = |calls: &mut u32| {
            *calls += 1;
            run_vz_memoized(&key, &store, || Ok((7, b"o".to_vec(), b"e".to_vec())))
        };

        let out1 = run(&mut calls).expect("run1");
        assert!(!out1.hit, "first run is a miss");
        assert_eq!(out1.exit_code, 7);

        let out2 = run(&mut calls).expect("run2");
        assert!(
            !out2.hit,
            "a non-zero exit must NOT be cached ⇒ the second run is also a miss"
        );
        assert_eq!(out2.exit_code, 7);

        // Nothing was ever written to the AC for this key.
        assert!(
            store.ac_get(&vz_memo_key(&key)).expect("ac_get").is_none(),
            "a non-zero exit must leave the AC empty"
        );
    }

    // -----------------------------------------------------------------------
    // run_vz_memoized_oversized_output_not_cached: a stdout over OUTPUT_CAP
    // bytes is never cached (mirrors the native output_cap_not_memoized law).
    // -----------------------------------------------------------------------
    #[test]
    fn run_vz_memoized_oversized_output_not_cached() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());

        let key = vz_key(vec!["/bin/yes"], [4u8; 32], vec![("PATH", "/bin")]);
        let big = vec![b'x'; OUTPUT_CAP_BYTES + 1];

        let out1 = run_vz_memoized(&key, &store, {
            let big = big.clone();
            || Ok((0, big, Vec::new()))
        })
        .expect("run1");
        assert!(!out1.hit);
        assert_eq!(out1.exit_code, 0);

        // Over-cap ⇒ not cached ⇒ the next call is still a miss.
        assert!(
            store.ac_get(&vz_memo_key(&key)).expect("ac_get").is_none(),
            "an over-cap stdout must not be cached"
        );
    }
}
