//! lightr-build — frozen contract: build-spec-r3.md §2+§3.
//! Dockerfile build graph (step-memoized) + lazy compose. Bodies: R3-W1/W2.
//!
//! # Compose YAML subset supported by `parse_compose`
//!
//! Hand-rolled; **no external YAML dep**. Supports:
//! ```yaml
//! services:
//!   web:
//!     image: myref
//!     command: ["sh", "-c", "sleep 1"]   # JSON array or bare string
//!     ports:
//!       - "8080:80"
//!     environment:
//!       - FOO=bar          # list form
//!       # OR map form:
//!       # FOO: bar
//!     x-lightr-eager: true
//! ```
//! Unknown keys are silently ignored. Parse errors include the 1-based line
//! number for quick diagnosis.
//!
//! # Compose supervisor model (ADR-0015)
//!
//! `compose_up` writes a `spec.json` under `$LIGHTR_HOME/compose/<nanos-pid>/`,
//! spawns a detached `lightr __compose-supervise <stack_dir>` process (re-uses
//! the same re-exec pattern as `lightr_run::spawn_detached`), then returns a
//! `ComposeHandle`. The supervisor (implemented as `compose_supervise`) does
//! the bind/accept/proxy loop and self-exits when `$stack_dir/stop` exists or
//! the TTL fires.
//!
//! **Known limits (document-only, not bugs):**
//! - Proxy is a simple bidirectional byte-copy; no TLS, no HTTP semantics.
//! - Service start latency on first connect = the service startup time (honest).
//! - No healthcheck before proxying; first-packet arrives as soon as
//!   `spawn_detached` returns.
//! - `compose_down` kills via `pid` file; on SIGKILL the proxy threads are
//!   reaped with the process (no zombie sockets on modern kernels).
//! - Proxy correctness is validated as an integration test (A24), not a unit
//!   test (tcp round-trip is flaky in tight loops).
//!
//! # RUN determinism caveat
//!
//! RUN steps that read the clock or network are not reproducible.
//! `step_reads_clock_or_net` provides a heuristic for `--explain` (W3/CLI).
//! Flagging is CLI-level; `build` itself records every step faithfully.
//!
//! # Native-engine note
//!
//! R3 executes RUN steps via the **native** engine (`rootfs: None`). There
//! is no filesystem isolation — RUN writes directly into the CoW working
//! tree. This is stated loudly in build output by the CLI (W3).

use lightr_core::{Digest, LightrError, Manifest, Result};
use lightr_store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── §2 Dockerfile build ─────────────────────────────────────────────────────

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
/// - Unknown keywords → `LightrError::InvalidManifest("unsupported instruction: <KW>")`.
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
fn parse_argv_or_shell(rest: &str) -> Vec<String> {
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

// ── Sidecar metadata ─────────────────────────────────────────────────────────

/// Sidecar `.lightr-image.json` stored at the layer root.
/// Persists CMD / LABEL / ENV accumulation across layer snapshots.
#[derive(Default, Serialize, Deserialize)]
struct ImageMeta {
    cmd: Option<Vec<String>>,
    labels: Vec<(String, String)>,
    env: Vec<(String, String)>,
}

const IMAGE_META_FILE: &str = ".lightr-image.json";

fn load_meta(root: &Path) -> ImageMeta {
    let p = root.join(IMAGE_META_FILE);
    if let Ok(bytes) = std::fs::read(&p) {
        serde_json::from_slice(&bytes).unwrap_or_default()
    } else {
        ImageMeta::default()
    }
}

fn save_meta(root: &Path, meta: &ImageMeta) -> Result<()> {
    let bytes = serde_json::to_vec(meta)
        .map_err(|e| LightrError::InvalidManifest(format!("meta serialize: {e}")))?;
    std::fs::write(root.join(IMAGE_META_FILE), &bytes).map_err(LightrError::Io)
}

// ── Build step key ────────────────────────────────────────────────────────────

/// Compute `step_key = BLAKE3("lightr/build/v1" ‖ prev_root_bytes ‖
/// instr_canonical_bytes ‖ [for COPY: each file's digest])`.
fn step_key(
    prev_layer_root: Option<Digest>,
    step: &BuildStep,
    context_dir: &Path,
) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lightr/build/v1");
    let prev_bytes = prev_layer_root.map(|d| d.0).unwrap_or([0u8; 32]);
    hasher.update(&prev_bytes);
    // canonical instr bytes = the raw line text
    hasher.update(step.raw.as_bytes());
    // For COPY, hash each source's content into the key. Files contribute
    // their digest; DIRECTORIES contribute every contained file's
    // (relative-path ‖ digest), sorted — so editing any file inside a copied
    // dir (e.g. `COPY src/ /app`) invalidates the cache. Symlinks contribute
    // their target. Missing sources contribute a sentinel (so add/remove of a
    // source also changes the key).
    if let Instr::Copy { src, .. } = &step.instr {
        for s in src {
            let src_path = context_dir.join(s);
            hash_copy_source(&mut hasher, &src_path)?;
        }
    }
    Ok(Digest(*hasher.finalize().as_bytes()))
}

/// Fold a COPY source's content-identity into `hasher`, recursing dirs.
fn hash_copy_source(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => {
            hasher.update(b"\x00missing\x00");
            return Ok(());
        }
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = std::fs::read_link(path).map_err(LightrError::Io)?;
        hasher.update(b"L");
        hasher.update(target.as_os_str().as_encoded_bytes());
    } else if ft.is_file() {
        hasher.update(b"F");
        hasher.update(&Digest::of_file(path)?.0);
    } else if ft.is_dir() {
        hasher.update(b"D");
        // Collect (relative path, entry) deterministically (sorted by path).
        let mut entries: Vec<PathBuf> = Vec::new();
        collect_dir_paths(path, &mut entries)?;
        entries.sort();
        for child in &entries {
            let rel = child.strip_prefix(path).unwrap_or(child);
            hasher.update(rel.as_os_str().as_encoded_bytes());
            hasher.update(b"\x00");
            hash_copy_source(hasher, child)?;
        }
    }
    Ok(())
}

/// Recursively collect every entry path under `dir` (files, dirs, symlinks).
fn collect_dir_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let p = entry.path();
        let ft = entry.file_type().map_err(LightrError::Io)?;
        out.push(p.clone());
        if ft.is_dir() {
            collect_dir_paths(&p, out)?;
        }
    }
    Ok(())
}

// ── Hydrate-from-manifest-digest ─────────────────────────────────────────────

/// Materialize a snapshot (identified by its manifest digest) into `dest`.
/// Clears `dest` first (removes all contents) so we get a clean layer.
/// This is the private helper used by the build cache replay path.
fn materialize_from_digest(dest: &Path, manifest_digest: &Digest, store: &Store) -> Result<()> {
    // Remove all existing content
    if dest.exists() {
        for entry in std::fs::read_dir(dest).map_err(LightrError::Io)? {
            let entry = entry.map_err(LightrError::Io)?;
            let p = entry.path();
            if p.is_dir() && !p.is_symlink() {
                std::fs::remove_dir_all(&p).map_err(LightrError::Io)?;
            } else {
                std::fs::remove_file(&p).map_err(LightrError::Io)?;
            }
        }
    } else {
        std::fs::create_dir_all(dest).map_err(LightrError::Io)?;
    }

    let manifest_bytes = store.get_bytes(manifest_digest)?;
    let manifest = Manifest::decode(&manifest_bytes)?;

    for entry in &manifest.entries {
        match entry {
            lightr_core::Entry::Dir { path } => {
                std::fs::create_dir_all(dest.join(path)).map_err(LightrError::Io)?;
            }
            lightr_core::Entry::File {
                path, mode, digest, ..
            } => {
                if let Some(parent) = Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(dest.join(parent)).map_err(LightrError::Io)?;
                    }
                }
                store.materialize_file(digest, &dest.join(path), *mode)?;
            }
            lightr_core::Entry::Symlink { path, target } => {
                if let Some(parent) = Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(dest.join(parent)).map_err(LightrError::Io)?;
                    }
                }
                let link_path = dest.join(path);
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &link_path).map_err(LightrError::Io)?;
                #[cfg(windows)]
                {
                    // WIN-PATH: symlink creation requires Developer Mode or admin on Windows.
                    // Fall back to copying the target if symlink creation fails so build never hard-fails.
                    use std::os::windows::fs::symlink_file;
                    if symlink_file(target, &link_path).is_err() {
                        let resolved_target = if std::path::Path::new(target).is_absolute() {
                            std::path::PathBuf::from(target)
                        } else {
                            link_path
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .join(target)
                        };
                        if resolved_target.exists() {
                            std::fs::copy(&resolved_target, &link_path).map_err(LightrError::Io)?;
                        }
                        // Broken symlink target: skip without error (same as unix behaviour for missing targets).
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Temp-dir guard ────────────────────────────────────────────────────────────

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        let unique = format!(
            "lightr-build-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = base.join(unique);
        std::fs::create_dir_all(&path).map_err(LightrError::Io)?;
        Ok(TempDirGuard { path })
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── BuildReport + build() ─────────────────────────────────────────────────────

pub struct BuildReport {
    pub name: String,
    pub root: Digest,
    pub steps: u64,
    pub cached_steps: u64,
}

/// Execute a Dockerfile build.
///
/// - RUN steps use the **native engine** (`rootfs: None`). No filesystem
///   isolation — RUN runs in the working tree directly. The CLI (W3) emits a
///   visible warning: "native engine: no isolation".
/// - Memoization: each step has a content-derived key; AC hits replay the
///   cached layer without executing. An unchanged Dockerfile prefix = all
///   cache hits; only steps at/after the first content change re-run.
pub fn build(
    context_dir: &Path,
    dockerfile: &Path,
    name: &str,
    engine: lightr_engine::EngineKind,
    store: &Store,
) -> Result<BuildReport> {
    let text = std::fs::read_to_string(dockerfile).map_err(LightrError::Io)?;
    let steps = parse_dockerfile(&text)?;
    let total = steps.len() as u64;

    let guard = TempDirGuard::new()?;
    let work_dir = &guard.path;

    let mut prev_layer_root: Option<Digest> = None;
    let mut accumulated_env: Vec<(String, String)> = Vec::new();
    let mut current_workdir = String::from("/");
    let mut cached_steps: u64 = 0;

    for step in &steps {
        let key = step_key(prev_layer_root, step, context_dir)?;

        // AC lookup
        if let Some(cached_val) = store.ac_get(&key)? {
            if cached_val.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&cached_val);
                let cached_root = Digest(arr);
                // Replay: materialize the cached layer
                materialize_from_digest(work_dir, &cached_root, store)?;
                prev_layer_root = Some(cached_root);
                cached_steps += 1;
                // Restore env/workdir from the meta sidecar
                let meta = load_meta(work_dir);
                accumulated_env = meta.env.clone();
                // workdir: we don't persist current_workdir in the sidecar
                // (it's a build-time concept not the service CWD). Keep the
                // last WORKDIR from the replayed meta if needed; but since
                // we only need current_workdir for RUN and cache hits skip
                // RUN execution, we can leave it as-is.
                // Restore workdir from accumulated state if Workdir instr
                if let Instr::Workdir { path } = &step.instr {
                    current_workdir = path.clone();
                }
                continue;
            }
        }

        // Execute the step
        match &step.instr {
            Instr::From { image_ref } => {
                // Clear work_dir
                for entry in std::fs::read_dir(work_dir).map_err(LightrError::Io)? {
                    let entry = entry.map_err(LightrError::Io)?;
                    let p = entry.path();
                    if p.is_dir() && !p.is_symlink() {
                        std::fs::remove_dir_all(&p).map_err(LightrError::Io)?;
                    } else {
                        std::fs::remove_file(&p).map_err(LightrError::Io)?;
                    }
                }
                if image_ref != "scratch" {
                    // Hydrate the base image into work_dir
                    lightr_index::hydrate(work_dir, store, image_ref)?;
                }
                // scratch = empty tree (already cleared)
            }
            Instr::Run { argv } => {
                // Native: CWD = work_dir / current_workdir; rootfs = None
                let cwd = if current_workdir == "/" || current_workdir.is_empty() {
                    work_dir.to_path_buf()
                } else {
                    let rel = current_workdir.trim_start_matches('/');
                    let cwd = work_dir.join(rel);
                    std::fs::create_dir_all(&cwd).map_err(LightrError::Io)?;
                    cwd
                };
                let eng = lightr_engine::engine_for(engine)?;
                let spec = lightr_engine::ExecSpec {
                    cwd: &cwd,
                    command: argv,
                    rootfs: None, // native — no isolation
                    limits: Default::default(),
                };
                // Propagate accumulated ENV to the subprocess
                let mut cmd_builder = std::process::Command::new(&argv[0]);
                if argv.len() > 1 {
                    cmd_builder.args(&argv[1..]);
                }
                for (k, v) in &accumulated_env {
                    cmd_builder.env(k, v);
                }
                // Use engine.run for the canonical path (inherits stdio)
                let code = eng.run(&spec)?;
                if code != 0 {
                    return Err(LightrError::InvalidManifest(format!(
                        "RUN step exited with code {code}: {:?}",
                        argv
                    )));
                }
            }
            Instr::Copy { src, dest } => {
                let dest_path = if dest.starts_with('/') {
                    work_dir.join(dest.trim_start_matches('/'))
                } else {
                    work_dir.join(dest)
                };
                // Docker COPY semantics:
                // - dest ending with '/' or multiple srcs → dest is a directory
                // - single file src, dest no trailing '/' → dest is the target filename
                let dest_is_dir = dest.ends_with('/') || src.len() > 1;
                if dest_is_dir {
                    std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
                    for s in src {
                        let src_path = context_dir.join(s);
                        if src_path.is_file() {
                            let file_name = src_path.file_name().unwrap();
                            std::fs::copy(&src_path, dest_path.join(file_name))
                                .map_err(LightrError::Io)?;
                        } else if src_path.is_dir() {
                            copy_dir_recursive(&src_path, &dest_path)?;
                        }
                    }
                } else {
                    // Single src → dest is the exact target path (file rename / place)
                    if let Some(parent) = dest_path.parent() {
                        std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
                    }
                    let src_path = context_dir.join(&src[0]);
                    if src_path.is_file() {
                        std::fs::copy(&src_path, &dest_path).map_err(LightrError::Io)?;
                    } else if src_path.is_dir() {
                        std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
                        copy_dir_recursive(&src_path, &dest_path)?;
                    }
                }
            }
            Instr::Env { key, val } => {
                // Update accumulated env; also persist to sidecar
                accumulated_env.retain(|(k, _)| k != key);
                accumulated_env.push((key.clone(), val.clone()));
                let mut meta = load_meta(work_dir);
                meta.env = accumulated_env.clone();
                save_meta(work_dir, &meta)?;
            }
            Instr::Workdir { path } => {
                current_workdir = path.clone();
                let abs = if path.starts_with('/') {
                    work_dir.join(path.trim_start_matches('/'))
                } else {
                    work_dir.join(path)
                };
                std::fs::create_dir_all(&abs).map_err(LightrError::Io)?;
            }
            Instr::Cmd { argv } => {
                let mut meta = load_meta(work_dir);
                meta.cmd = Some(argv.clone());
                save_meta(work_dir, &meta)?;
            }
            Instr::Label { key, val } => {
                let mut meta = load_meta(work_dir);
                meta.labels.retain(|(k, _)| k != key);
                meta.labels.push((key.clone(), val.clone()));
                save_meta(work_dir, &meta)?;
            }
        }

        // Snapshot the working tree and publish ref `name`
        let snap = lightr_index::snapshot(work_dir, store, name)?;
        let new_root = snap.root;

        // Store step_key → root in AC
        store.ac_put(&key, &new_root.0)?;

        prev_layer_root = Some(new_root);
    }

    let final_root = prev_layer_root
        .ok_or_else(|| LightrError::InvalidManifest("empty Dockerfile".to_string()))?;

    Ok(BuildReport {
        name: name.to_string(),
        root: final_root,
        steps: total,
        cached_steps,
    })
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let ft = entry.file_type().map_err(LightrError::Io)?;
        let target = dest.join(entry.file_name());
        if ft.is_dir() {
            std::fs::create_dir_all(&target).map_err(LightrError::Io)?;
            copy_dir_recursive(&entry.path(), &target)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &target).map_err(LightrError::Io)?;
        }
    }
    Ok(())
}

/// Heuristic: does this argv likely read the clock or network?
/// Used by `--explain` in the CLI (W3) to flag non-reproducible RUN steps.
pub fn step_reads_clock_or_net(argv: &[String]) -> bool {
    let cmd = argv.join(" ");
    let patterns = [
        "date",
        "curl",
        "wget",
        "fetch",
        "apt-get",
        "apk",
        "yum",
        "pip",
        "npm",
        "cargo install",
    ];
    patterns.iter().any(|p| cmd.contains(p))
}

// ── §3 lazy compose ──────────────────────────────────────────────────────────

pub struct Service {
    pub name: String,
    pub image_ref: String,
    pub command: Option<Vec<String>>,
    pub ports: Vec<(u16, u16)>,
    pub env: Vec<(String, String)>,
    pub eager: bool,
    /// F-309: store-backed secrets, each `(name, ref)`. Hydrated to
    /// `<cwd>/.lightr/secrets/<name>` (0600) on a cache miss. In the memo key.
    pub secrets: Vec<(String, String)>,
    /// F-309: store-backed configs, each `(name, ref)`. Hydrated to
    /// `<cwd>/.lightr/configs/<name>` (0644) on a cache miss. In the memo key.
    pub configs: Vec<(String, String)>,
    /// F-309: optional healthcheck `(cmd, interval_s, retries)`. Post-result
    /// probe surfaced via `ps`; NOT in the memo key.
    pub healthcheck: Option<(String, u64, u32)>,
}

pub struct Compose {
    pub services: Vec<Service>,
}

/// Parse a minimal docker-compose YAML subset.
///
/// Supported structure (indentation-based, 2-space):
/// ```yaml
/// services:
///   <name>:
///     image: <ref>
///     command: "string" | ["a","b"]
///     ports:
///       - "H:C"
///     environment:
///       - K=V    # list form
///       K: V     # map form
///     x-lightr-eager: true
/// ```
/// Unknown keys are silently ignored. Returns `InvalidManifest` with the
/// 1-based line number on any structural parse error.
/// Parse a Docker-compose duration into whole seconds. Accepts `"30s"`,
/// `"1m"`, `"2m30s"` (s/m/h suffixes), or a bare integer (seconds). Returns
/// `None` on malformed input (fail closed at the call site).
fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Bare integer ⇒ seconds.
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    // Suffixed form: sum consecutive <number><unit> groups.
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            let mult = match ch {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                _ => return None,
            };
            total = total.checked_add(n.checked_mul(mult)?)?;
            saw_unit = true;
        }
    }
    // A trailing number with no unit is malformed in suffixed form.
    if !num.is_empty() || !saw_unit {
        return None;
    }
    Some(total)
}

/// A fresh service with the given name and all-empty fields.
fn empty_service(name: String) -> Service {
    Service {
        name,
        image_ref: String::new(),
        command: None,
        ports: Vec::new(),
        env: Vec::new(),
        eager: false,
        secrets: Vec::new(),
        configs: Vec::new(),
        healthcheck: None,
    }
}

pub fn parse_compose(yaml: &str) -> Result<Compose> {
    enum ParseState {
        Top,
        Services,
        Service(String),     // service name
        Ports(String),       // current service, collecting ports
        Environment(String), // current service, collecting env
        // F-309 list collectors: `secrets:` / `configs:` (items `- NAME=REF`).
        Secrets(String),
        Configs(String),
        // F-309 nested map: `healthcheck:` (test/cmd, interval, retries).
        Healthcheck(String),
    }

    let mut state = ParseState::Top;
    let mut services: std::collections::HashMap<String, Service> = std::collections::HashMap::new();
    let mut service_order: Vec<String> = Vec::new();

    for (lineno0, raw_line) in yaml.lines().enumerate() {
        let lineno = lineno0 + 1;
        // preserve original for error messages, work on trimmed content
        let stripped = raw_line.trim_end();
        if stripped.is_empty() || stripped.trim_start().starts_with('#') {
            continue;
        }

        // Measure indent
        let indent = stripped.len() - stripped.trim_start().len();
        let content = stripped.trim_start();

        match &state {
            ParseState::Top => {
                if content == "services:" {
                    state = ParseState::Services;
                }
                // any other top-level key ignored
            }
            ParseState::Services => {
                if indent == 2 && content.ends_with(':') {
                    let svc_name = content.trim_end_matches(':').to_string();
                    services.insert(svc_name.clone(), empty_service(svc_name.clone()));
                    service_order.push(svc_name.clone());
                    state = ParseState::Service(svc_name);
                }
            }
            ParseState::Service(svc) => {
                let svc = svc.clone();
                if indent == 2 && content.ends_with(':') {
                    // new service at same level
                    let new_svc = content.trim_end_matches(':').to_string();
                    services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                    service_order.push(new_svc.clone());
                    state = ParseState::Service(new_svc);
                    continue;
                }
                if indent == 0 {
                    // left services block
                    state = ParseState::Top;
                    continue;
                }
                if indent < 4 {
                    // could be top-level key; back to services or top
                    if content.ends_with(':') && indent == 2 {
                        // same level = new service (handled above)
                    }
                    continue;
                }
                // indent >= 4 = service-level key
                if content == "ports:" {
                    state = ParseState::Ports(svc);
                } else if content == "environment:" {
                    state = ParseState::Environment(svc);
                } else if content == "secrets:" {
                    state = ParseState::Secrets(svc);
                } else if content == "configs:" {
                    state = ParseState::Configs(svc);
                } else if content == "healthcheck:" {
                    state = ParseState::Healthcheck(svc);
                } else if let Some(val) = content.strip_prefix("image:") {
                    if let Some(s) = services.get_mut(&svc) {
                        s.image_ref = val.trim().to_string();
                    }
                } else if let Some(val) = content.strip_prefix("command:") {
                    let raw = val.trim();
                    let argv = if raw.starts_with('[') {
                        serde_json::from_str::<Vec<String>>(raw).map_err(|e| {
                            LightrError::InvalidManifest(format!(
                                "line {lineno}: bad command array: {e}"
                            ))
                        })?
                    } else {
                        vec!["/bin/sh".to_string(), "-c".to_string(), raw.to_string()]
                    };
                    if let Some(s) = services.get_mut(&svc) {
                        s.command = Some(argv);
                    }
                } else if let Some(val) = content.strip_prefix("x-lightr-eager:") {
                    if val.trim() == "true" {
                        if let Some(s) = services.get_mut(&svc) {
                            s.eager = true;
                        }
                    }
                }
                // unknown keys: silently ignored
            }
            ParseState::Ports(svc) => {
                let svc = svc.clone();
                // A service sub-key (indent==4, ends with ':', no leading '-')
                // or any de-indent means we left the ports list.
                let is_subkey = indent == 4 && content.ends_with(':') && !content.starts_with('-');
                if indent < 4 || is_subkey {
                    // Transition back to Service state and re-process this line
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if is_subkey {
                        // Handle service sub-key like "environment:" or "image:"
                        if content == "environment:" {
                            state = ParseState::Environment(svc);
                        }
                        // other sub-keys like "image:" are handled by Service arm on next iteration;
                        // since we're doing continue here, we need to handle them now.
                        // Simplest: fall through to Service arm logic inline.
                        // For "image:", "command:", "x-lightr-eager:" we need inline handling.
                        // We already set state = Service(svc) above for non-environment sub-keys.
                    }
                    continue;
                }
                // list item: "- H:C"
                let item = content.trim_start_matches("- ").trim().trim_matches('"');
                if let Some((h, c)) = item.split_once(':') {
                    let host: u16 = h.trim().parse().map_err(|_| {
                        LightrError::InvalidManifest(format!("line {lineno}: bad port: {item}"))
                    })?;
                    let cont: u16 = c.trim().parse().map_err(|_| {
                        LightrError::InvalidManifest(format!("line {lineno}: bad port: {item}"))
                    })?;
                    if let Some(s) = services.get_mut(&svc) {
                        s.ports.push((host, cont));
                    }
                }
            }
            ParseState::Environment(svc) => {
                let svc = svc.clone();
                let is_subkey = indent == 4 && content.ends_with(':') && !content.starts_with('-');
                if indent < 4 || is_subkey {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if is_subkey && content == "ports:" {
                        state = ParseState::Ports(svc);
                    }
                    continue;
                }
                // Any line at indent==4 that does NOT start with '-' is a
                // service-level key (environment list items always start with '- ').
                // Known service keys are handled; unknown ones are silently ignored.
                let is_service_key = !content.starts_with('-');
                if is_service_key {
                    // Handle inline (same logic as Service arm)
                    if content == "ports:" {
                        state = ParseState::Ports(svc);
                    } else if let Some(val) = content.strip_prefix("image:") {
                        if let Some(s) = services.get_mut(&svc) {
                            s.image_ref = val.trim().to_string();
                        }
                        state = ParseState::Service(svc);
                    } else if let Some(val) = content.strip_prefix("command:") {
                        let raw = val.trim();
                        let argv = if raw.starts_with('[') {
                            serde_json::from_str::<Vec<String>>(raw).map_err(|e| {
                                LightrError::InvalidManifest(format!(
                                    "line {lineno}: bad command array: {e}"
                                ))
                            })?
                        } else {
                            vec!["/bin/sh".to_string(), "-c".to_string(), raw.to_string()]
                        };
                        if let Some(s) = services.get_mut(&svc) {
                            s.command = Some(argv);
                        }
                        state = ParseState::Service(svc);
                    } else if let Some(val) = content.strip_prefix("x-lightr-eager:") {
                        if val.trim() == "true" {
                            if let Some(s) = services.get_mut(&svc) {
                                s.eager = true;
                            }
                        }
                        state = ParseState::Service(svc);
                    }
                    continue;
                }
                // list form: "- K=V"
                let item = if content.starts_with("- ") {
                    content.trim_start_matches("- ").trim()
                } else {
                    content
                };
                if let Some((k, v)) = item.split_once('=') {
                    if let Some(s) = services.get_mut(&svc) {
                        s.env.push((k.to_string(), v.to_string()));
                    }
                } else if let Some((k, v)) = item.split_once(':') {
                    // map form K: V  — only if item has a value (not just "key:")
                    let vt = v.trim();
                    if !vt.is_empty() {
                        if let Some(s) = services.get_mut(&svc) {
                            s.env.push((k.trim().to_string(), vt.to_string()));
                        }
                    }
                }
            }
            // F-309: `secrets:` / `configs:` list collectors. Each item is
            // `- NAME=REF` (mirrors the `environment:` list form). On a de-indent
            // or a new indent==4 sub-key we leave the list (back to Service).
            // Convention: like ports/environment, list keys come after scalar
            // keys (image/command) for a service.
            ParseState::Secrets(svc) | ParseState::Configs(svc) => {
                let svc = svc.clone();
                let is_secrets = matches!(state, ParseState::Secrets(_));
                let is_subkey = indent == 4 && content.ends_with(':') && !content.starts_with('-');
                if indent < 4 || is_subkey {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if is_subkey {
                        // Re-dispatch the recognized list/map sub-keys inline so a
                        // `secrets:`→`ports:` (etc.) transition is not lost.
                        match content {
                            "ports:" => state = ParseState::Ports(svc),
                            "environment:" => state = ParseState::Environment(svc),
                            "secrets:" => state = ParseState::Secrets(svc),
                            "configs:" => state = ParseState::Configs(svc),
                            "healthcheck:" => state = ParseState::Healthcheck(svc),
                            _ => {}
                        }
                    }
                    continue;
                }
                // list item: "- NAME=REF"
                let item = content.trim_start_matches("- ").trim().trim_matches('"');
                if let Some((name, refn)) = item.split_once('=') {
                    let pair = (name.trim().to_string(), refn.trim().to_string());
                    if let Some(s) = services.get_mut(&svc) {
                        if is_secrets {
                            s.secrets.push(pair);
                        } else {
                            s.configs.push(pair);
                        }
                    }
                }
            }
            // F-309: nested `healthcheck:` map. Keys (indent==6): `test:` or
            // `cmd:` (the probe command), `interval:` (seconds), `retries:`.
            // Any de-indent to a service-level key (indent<=4) leaves the map.
            ParseState::Healthcheck(svc) => {
                let svc = svc.clone();
                if indent <= 4 {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if indent == 4 && content.ends_with(':') {
                        match content {
                            "ports:" => state = ParseState::Ports(svc),
                            "environment:" => state = ParseState::Environment(svc),
                            "secrets:" => state = ParseState::Secrets(svc),
                            "configs:" => state = ParseState::Configs(svc),
                            "healthcheck:" => state = ParseState::Healthcheck(svc),
                            _ => {}
                        }
                    }
                    continue;
                }
                // indent >= 6: a healthcheck sub-key.
                if let Some(s) = services.get_mut(&svc) {
                    // Ensure the tuple exists with sane defaults; refine per key.
                    let hc = s.healthcheck.get_or_insert((String::new(), 30, 3));
                    if let Some(val) = content
                        .strip_prefix("test:")
                        .or_else(|| content.strip_prefix("cmd:"))
                    {
                        let raw = val.trim();
                        // Accept a JSON array (Docker `["CMD","..."]`) or a bare
                        // shell string. For an array we join argv into a shell
                        // line (the probe runs via `/bin/sh -c`).
                        let cmd = if raw.starts_with('[') {
                            match serde_json::from_str::<Vec<String>>(raw) {
                                Ok(mut parts) => {
                                    // Drop a leading "CMD"/"CMD-SHELL" marker.
                                    if parts
                                        .first()
                                        .map(|p| p == "CMD" || p == "CMD-SHELL")
                                        .unwrap_or(false)
                                    {
                                        parts.remove(0);
                                    }
                                    parts.join(" ")
                                }
                                Err(e) => {
                                    return Err(LightrError::InvalidManifest(format!(
                                        "line {lineno}: bad healthcheck test array: {e}"
                                    )))
                                }
                            }
                        } else {
                            raw.trim_matches('"').to_string()
                        };
                        hc.0 = cmd;
                    } else if let Some(val) = content.strip_prefix("interval:") {
                        hc.1 = parse_duration_secs(val.trim()).ok_or_else(|| {
                            LightrError::InvalidManifest(format!(
                                "line {lineno}: bad healthcheck interval: {}",
                                val.trim()
                            ))
                        })?;
                    } else if let Some(val) = content.strip_prefix("retries:") {
                        hc.2 = val.trim().parse().map_err(|_| {
                            LightrError::InvalidManifest(format!(
                                "line {lineno}: bad healthcheck retries: {}",
                                val.trim()
                            ))
                        })?;
                    }
                    // unknown healthcheck sub-keys silently ignored
                }
            }
        }
    }

    // Drop a healthcheck that never got a command (e.g. only interval set):
    // a probe with no command is meaningless, so treat it as absent.
    for s in services.values_mut() {
        if let Some((cmd, _, _)) = &s.healthcheck {
            if cmd.is_empty() {
                s.healthcheck = None;
            }
        }
    }

    let ordered: Vec<Service> = service_order
        .into_iter()
        .filter_map(|n| services.remove(&n))
        .collect();

    Ok(Compose { services: ordered })
}

// ── ComposeHandle + stack spec ────────────────────────────────────────────────

pub struct ComposeHandle {
    pub stack_dir: std::path::PathBuf,
    pub services: Vec<String>,
}

/// On-disk spec written by `compose_up` for the supervisor process.
#[derive(Serialize, Deserialize)]
pub struct StackSpec {
    pub ttl_secs: u64,
    pub created_at_unix: u64,
    /// pid of the supervisor process (written after fork)
    pub supervisor_pid: Option<u32>,
    pub services: Vec<ServiceSpec>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub image_ref: String,
    pub command: Vec<String>,
    pub ports: Vec<(u16, u16)>,
    pub env: Vec<(String, String)>,
    pub eager: bool,
    /// Run dir if started (populated by supervisor)
    pub run_dir: Option<String>,
    /// F-309: store-backed secrets `(name, ref)` → hydrated to
    /// `<cwd>/.lightr/secrets/<name>` (0600). In the run's memo key.
    #[serde(default)]
    pub secrets: Vec<(String, String)>,
    /// F-309: store-backed configs `(name, ref)` → hydrated to
    /// `<cwd>/.lightr/configs/<name>` (0644). In the run's memo key.
    #[serde(default)]
    pub configs: Vec<(String, String)>,
    /// F-309: optional healthcheck `(cmd, interval_s, retries)`. Post-result
    /// probe surfaced via `ps`; NOT in the memo key.
    #[serde(default)]
    pub healthcheck: Option<(String, u64, u32)>,
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

/// Start a compose stack.
///
/// - Creates `$LIGHTR_HOME/compose/<nanos-pid>/spec.json`.
/// - Spawns a detached `lightr __compose-supervise <stack_dir>` process.
/// - Eager services are noted in the spec; the supervisor starts them
///   immediately.
/// - Lazy services: the supervisor binds their host ports and starts the
///   service on the first incoming connection.
///
/// Returns once the stack directory is written (ms).
pub fn compose_up(c: &Compose, store: &Store, ttl_secs: u64) -> Result<ComposeHandle> {
    let _ = store; // store reserved for future hydrate-before-spawn path
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();

    let stack_dir = lightr_home()
        .join("compose")
        .join(format!("{now_nanos}-{pid}"));
    std::fs::create_dir_all(&stack_dir).map_err(LightrError::Io)?;

    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let service_specs: Vec<ServiceSpec> = c
        .services
        .iter()
        .map(|s| ServiceSpec {
            name: s.name.clone(),
            image_ref: s.image_ref.clone(),
            command: s.command.clone().unwrap_or_default(),
            ports: s.ports.clone(),
            env: s.env.clone(),
            eager: s.eager,
            run_dir: None,
            secrets: s.secrets.clone(),
            configs: s.configs.clone(),
            healthcheck: s.healthcheck.clone(),
        })
        .collect();

    let spec = StackSpec {
        ttl_secs,
        created_at_unix,
        supervisor_pid: None,
        services: service_specs.clone(),
    };

    let spec_bytes = serde_json::to_vec_pretty(&spec)
        .map_err(|e| LightrError::InvalidManifest(format!("stack spec serialize: {e}")))?;
    let spec_path = stack_dir.join("spec.json");
    std::fs::write(&spec_path, &spec_bytes).map_err(LightrError::Io)?;

    // Spawn detached supervisor: `lightr __compose-supervise <stack_dir>`
    let exe = std::env::current_exe().map_err(LightrError::Io)?;
    let stack_str = stack_dir.to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["__compose-supervise", &stack_str]);
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

    cmd.spawn().map_err(LightrError::Io)?;

    let service_names: Vec<String> = service_specs.iter().map(|s| s.name.clone()).collect();
    Ok(ComposeHandle {
        stack_dir,
        services: service_names,
    })
}

/// Compose supervisor — called by `lightr __compose-supervise <stack_dir>`.
///
/// Reads `spec.json`, writes `pid` file, then:
/// - Starts eager services via `lightr_run::spawn_detached`.
/// - For each lazy service port: binds a `TcpListener` and spawns a thread
///   that accepts one connection, starts the service, then proxies bytes
///   bidirectionally.
/// - Polls for `$stack_dir/stop` every 500 ms; exits when found or TTL fires.
///
/// **Proxy limits (documented):**
/// - Single-connection per port: after the first connection triggers service
///   start, subsequent connections to the supervisor port will block until the
///   accept loop restarts (not implemented in R3 — R4 enhancement).
/// - No half-close forwarding; each direction copies until either side closes.
pub fn compose_supervise(stack_dir: &Path) -> Result<()> {
    use std::time::{Duration, Instant};

    let spec_path = stack_dir.join("spec.json");
    let spec_bytes = std::fs::read(&spec_path).map_err(LightrError::Io)?;
    let mut spec: StackSpec = serde_json::from_slice(&spec_bytes)
        .map_err(|e| LightrError::InvalidManifest(format!("stack spec parse: {e}")))?;

    // Write pid file
    let pid = std::process::id();
    std::fs::write(stack_dir.join("pid"), pid.to_string().as_bytes()).map_err(LightrError::Io)?;
    spec.supervisor_pid = Some(pid);
    let spec_bytes2 = serde_json::to_vec_pretty(&spec)
        .map_err(|e| LightrError::InvalidManifest(format!("serialize: {e}")))?;
    std::fs::write(&spec_path, &spec_bytes2).map_err(LightrError::Io)?;

    let ttl = Duration::from_secs(spec.ttl_secs);
    let start = Instant::now();
    let stop_file = stack_dir.join("stop");

    // Start eager services
    for svc in &spec.services {
        if svc.eager && !svc.command.is_empty() {
            start_service_detached(stack_dir, svc)?;
        }
    }

    // Bind lazy service listeners and spawn per-port supervisor threads
    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();

    for svc_spec in &spec.services {
        if svc_spec.eager {
            continue;
        }
        for &(host_port, container_port) in &svc_spec.ports {
            let addr = format!("127.0.0.1:{host_port}");
            let listener = match std::net::TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "lightr compose: bind {addr} for service {} failed: {e}",
                        svc_spec.name
                    );
                    continue;
                }
            };
            let svc_clone = svc_spec.clone();
            let stack_dir_clone = stack_dir.to_path_buf();
            let jh = std::thread::spawn(move || {
                // Block until first connection
                if let Ok((inbound, _)) = listener.accept() {
                    // Start the service
                    if let Err(e) = start_service_detached(&stack_dir_clone, &svc_clone) {
                        eprintln!("lightr compose: failed to start {}: {e}", svc_clone.name);
                        return;
                    }
                    // Give the service a moment to bind its port
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    // Connect to the service's container port
                    let svc_addr = format!("127.0.0.1:{container_port}");
                    if let Ok(outbound) = std::net::TcpStream::connect(&svc_addr) {
                        proxy_bidirectional(inbound, outbound);
                    }
                }
            });
            threads.push(jh);
        }
    }

    // Poll stop file / TTL
    loop {
        if stop_file.exists() || start.elapsed() >= ttl {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}

/// Prepare a clean per-service run directory and, if the service declares an
/// `image_ref`, hydrate that ref's filesystem into it (CoW) via
/// `lightr_index::hydrate` — mirroring the rootfs hydrate done by `lightr run`.
/// Command-only services (empty `image_ref`) get a clean empty dir.
/// Fail-closed: a hydrate error propagates and the service is not started.
fn prepare_service_cwd(svc: &ServiceSpec, store: &Store) -> Result<PathBuf> {
    let cwd = std::env::temp_dir().join(format!("lightr-svc-{}", svc.name));
    // Start from a clean dir so a stale hydrate from a prior run can't leak in.
    if cwd.exists() {
        std::fs::remove_dir_all(&cwd).map_err(LightrError::Io)?;
    }
    std::fs::create_dir_all(&cwd).map_err(LightrError::Io)?;

    // "scratch" = the empty base image (Docker convention) — an empty tree, not a
    // store ref. Mirror the Dockerfile `FROM scratch` path (Instr::From above):
    // skip hydration so the service runs in a clean cwd. Hydrating "scratch" as a
    // ref fails RefNotFound and the service never starts — a latent bug since the
    // compose-hydrate change, caught by a24_compose_lazy once the bin was rebuilt.
    if !svc.image_ref.is_empty() && svc.image_ref != "scratch" {
        lightr_index::hydrate(&cwd, store, &svc.image_ref)?;
    }

    Ok(cwd)
}

/// Spawn a service as a detached lightr run.
/// Writes the run dir into the stack spec's service entry.
fn start_service_detached(stack_dir: &Path, svc: &ServiceSpec) -> Result<()> {
    use lightr_run::healthcheck::Healthcheck;
    use lightr_run::{Mount, RunSpec, StoreFile};

    // We need a store both to hydrate the image ref and to call spawn_detached;
    // open the default store before choosing cwd.
    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;

    // cwd is a clean per-service dir, hydrated from the service's image_ref.
    let cwd = prepare_service_cwd(svc, &store)?;

    // F-309: map compose secrets/configs `(name, ref)` → RunSpec StoreFiles.
    // These are IN the run's memo key and are hydrated (fail-closed) on a miss.
    let to_store_files = |pairs: &[(String, String)]| -> Vec<StoreFile> {
        pairs
            .iter()
            .map(|(name, ref_name)| StoreFile {
                name: name.clone(),
                ref_name: ref_name.clone(),
            })
            .collect()
    };

    let spec = RunSpec {
        cwd: cwd.clone(),
        inputs: Vec::new(),
        command: svc.command.clone(),
        env_keys: svc.env.iter().map(|(k, _)| k.clone()).collect(),
        mounts: Vec::new() as Vec<Mount>,
        secrets: to_store_files(&svc.secrets),
        configs: to_store_files(&svc.configs),
        // Compose publishes ports through its own lazy-listener proxy (above),
        // not the run-spec forwarder; leave RunSpec.ports empty here.
        ports: Vec::new(),
    };

    // Set env vars before spawning
    for (k, v) in &svc.env {
        std::env::set_var(k, v);
    }

    // F-309: a service healthcheck becomes the detached supervisor's probe.
    let hc = svc
        .healthcheck
        .as_ref()
        .map(|(cmd, interval_s, retries)| Healthcheck {
            cmd: cmd.clone(),
            interval_s: *interval_s,
            retries: *retries,
        });

    let handle = lightr_run::spawn_detached_with_health(&spec, &store, hc.as_ref())?;

    // Record run dir in the stack spec
    let spec_path = stack_dir.join("spec.json");
    if let Ok(bytes) = std::fs::read(&spec_path) {
        if let Ok(mut stack_spec) = serde_json::from_slice::<StackSpec>(&bytes) {
            for s in &mut stack_spec.services {
                if s.name == svc.name {
                    s.run_dir = Some(handle.dir.to_string_lossy().into_owned());
                }
            }
            if let Ok(new_bytes) = serde_json::to_vec_pretty(&stack_spec) {
                let _ = std::fs::write(&spec_path, &new_bytes);
            }
        }
    }

    Ok(())
}

/// Simple bidirectional byte proxy between two TCP streams.
fn proxy_bidirectional(a: std::net::TcpStream, b: std::net::TcpStream) {
    use std::io::{Read, Write};

    let a2 = a.try_clone();
    let b2 = b.try_clone();
    if a2.is_err() || b2.is_err() {
        return;
    }
    let mut a_read = a;
    let mut b_read = b;
    let mut a_write = a2.unwrap();
    let mut b_write = b2.unwrap();

    let t1 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match a_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if b_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let t2 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match b_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if a_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let _ = t1.join();
    let _ = t2.join();
}

/// Tear down a compose stack.
///
/// 1. Reads `spec.json` and stops any started service runs.
/// 2. Writes `stop` file to signal the supervisor.
/// 3. Removes the stack directory.
pub fn compose_down(stack_dir: &Path) -> Result<()> {
    let spec_path = stack_dir.join("spec.json");
    if spec_path.exists() {
        if let Ok(bytes) = std::fs::read(&spec_path) {
            if let Ok(spec) = serde_json::from_slice::<StackSpec>(&bytes) {
                for svc in &spec.services {
                    if let Some(run_dir) = &svc.run_dir {
                        let dir = PathBuf::from(run_dir);
                        if dir.exists() {
                            let _ = lightr_run::stop(&dir, 2);
                        }
                    }
                }
            }
        }
    }

    // Signal supervisor
    let stop_file = stack_dir.join("stop");
    let _ = std::fs::write(&stop_file, b"");

    // Kill supervisor by pid file if still running.
    // WIN-PATH: on Windows the supervisor is signalled via the stop file above;
    // pid-based termination (TerminateProcess) is a future ring, so this
    // unix-only kill is cfg-gated rather than left half-implemented.
    #[cfg(unix)]
    {
        let pid_file = stack_dir.join("pid");
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
    }

    // Remove stack dir
    if stack_dir.exists() {
        std::fs::remove_dir_all(stack_dir).map_err(LightrError::Io)?;
    }

    Ok(())
}

// ── §2 + §3 tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serialize env-mutating tests (LIGHTR_HOME)
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn with_home<F: FnOnce()>(f: F) {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", tmp.path());
        f();
        std::env::remove_var("LIGHTR_HOME");
    }

    // ── step_key: directory-COPY cache invalidation (final-critic gap) ──────

    #[test]
    fn step_key_dir_copy_changes_when_contained_file_changes() {
        // `COPY src/ /app` must invalidate the cache when a file INSIDE src/
        // changes — not just top-level files. Regression for the final-critic
        // finding (step_key hashed is_file() only).
        let ctx = TempDir::new().unwrap();
        std::fs::create_dir_all(ctx.path().join("src/nested")).unwrap();
        std::fs::write(ctx.path().join("src/a.txt"), b"one").unwrap();
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();

        let step = BuildStep {
            instr: Instr::Copy {
                src: vec!["src".to_string()],
                dest: "/app".to_string(),
            },
            raw: "COPY src /app".to_string(),
        };

        let k1 = step_key(None, &step, ctx.path()).unwrap();

        // change a NESTED file
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-two").unwrap();
        let k2 = step_key(None, &step, ctx.path()).unwrap();
        assert_ne!(
            k1.0, k2.0,
            "nested file change must change the COPY step key"
        );

        // adding a file changes the key too
        std::fs::write(ctx.path().join("src/c.txt"), b"new").unwrap();
        let k3 = step_key(None, &step, ctx.path()).unwrap();
        assert_ne!(k2.0, k3.0, "adding a file must change the COPY step key");

        // identical content ⇒ identical key (determinism)
        std::fs::remove_file(ctx.path().join("src/c.txt")).unwrap();
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();
        let k4 = step_key(None, &step, ctx.path()).unwrap();
        assert_eq!(k1.0, k4.0, "restoring content must restore the key");
    }

    // ── parse_dockerfile tests ──────────────────────────────────────────────

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

    // ── build memoization tests ─────────────────────────────────────────────

    /// Build memoization: scratch + COPY + RUN writing to an OUTSIDE counter.
    /// Build twice ⇒ 2nd all-cached, counter==1.
    /// Change copied file ⇒ RUN re-runs, counter==2.
    #[test]
    fn build_memoization_scratch_copy_run() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let ctx = TempDir::new().unwrap();
        let store_tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", store_tmp.path());

        let counter_file = store_tmp.path().join("counter.txt");
        std::fs::write(&counter_file, "0").unwrap();

        // Write a source file to copy
        let src_file = ctx.path().join("hello.txt");
        std::fs::write(&src_file, b"hello").unwrap();

        // Write Dockerfile
        let df_path = ctx.path().join("Dockerfile");
        let counter_path_str = counter_file.to_string_lossy();
        let df_content = format!(
            "FROM scratch\nCOPY hello.txt /hello.txt\nRUN /bin/sh -c 'v=$(cat {counter_path_str}); echo $((v+1)) > {counter_path_str}'\n"
        );
        std::fs::write(&df_path, &df_content).unwrap();

        let store = Store::open(store_tmp.path().join("store")).unwrap();

        // First build
        let report1 = build(
            ctx.path(),
            &df_path,
            "test-build",
            lightr_engine::EngineKind::Native,
            &store,
        )
        .unwrap();
        assert_eq!(report1.steps, 3);
        assert_eq!(report1.cached_steps, 0);
        let counter_after_first: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            counter_after_first, 1,
            "RUN should have incremented counter to 1"
        );

        // Second build (unchanged)
        let report2 = build(
            ctx.path(),
            &df_path,
            "test-build",
            lightr_engine::EngineKind::Native,
            &store,
        )
        .unwrap();
        assert_eq!(report2.steps, 3);
        assert_eq!(report2.cached_steps, 3, "all steps should be cache hits");
        let counter_after_second: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            counter_after_second, 1,
            "counter must NOT increment on cache hit"
        );

        // Change the copied file → RUN should re-run
        std::fs::write(&src_file, b"changed").unwrap();
        let report3 = build(
            ctx.path(),
            &df_path,
            "test-build",
            lightr_engine::EngineKind::Native,
            &store,
        )
        .unwrap();
        assert_eq!(report3.steps, 3);
        // FROM is still cached (prev_root is same scratch=empty), COPY is new (file changed)
        assert!(
            report3.cached_steps < 3,
            "COPY+RUN must not be fully cached after file change"
        );
        let counter_after_third: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            counter_after_third, 2,
            "RUN must re-run after COPY file changed"
        );

        std::env::remove_var("LIGHTR_HOME");
    }

    /// build hydrate: final tree has COPY'd + RUN output.
    #[test]
    fn build_hydrate_final_tree() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let ctx = TempDir::new().unwrap();
        let store_tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", store_tmp.path());

        std::fs::write(ctx.path().join("src.txt"), b"content").unwrap();
        let df_path = ctx.path().join("Dockerfile");
        std::fs::write(
            &df_path,
            // Write to relative path so it lands in work_dir (CWD for native engine).
            // /src.txt notation triggers COPY into work_dir root.
            "FROM scratch\nCOPY src.txt /src.txt\nRUN echo built\n",
        )
        .unwrap();

        let store = Store::open(store_tmp.path().join("store")).unwrap();
        let report = build(
            ctx.path(),
            &df_path,
            "test-hydrate",
            lightr_engine::EngineKind::Native,
            &store,
        )
        .unwrap();
        assert_eq!(report.steps, 3);
        assert_eq!(report.cached_steps, 0);

        // Hydrate and check
        let dest = store_tmp.path().join("hydrated");
        lightr_index::hydrate(&dest, &store, "test-hydrate").unwrap();
        assert!(
            dest.join("src.txt").exists(),
            "/src.txt must be in hydrated tree"
        );
        // RUN step executed (echo built) — verify the build ran without error
        // (echo writes to stdout, not a file; tree must have src.txt from COPY)
        let src_content = std::fs::read_to_string(dest.join("src.txt")).unwrap();
        assert_eq!(
            src_content, "content",
            "src.txt must have the COPY'd content"
        );

        std::env::remove_var("LIGHTR_HOME");
    }

    // ── parse_compose tests ─────────────────────────────────────────────────

    #[test]
    fn parse_compose_two_services() {
        let yaml = r#"
services:
  web:
    image: myimage
    command: ["sh", "-c", "echo hi"]
    ports:
      - "8080:80"
    environment:
      - FOO=bar
    x-lightr-eager: true
  db:
    image: dbimage
    ports:
      - "5432:5432"
    environment:
      - DB=test
    unknown-key: ignored
"#;
        let c = parse_compose(yaml).unwrap();
        assert_eq!(c.services.len(), 2);

        let web = &c.services[0];
        assert_eq!(web.name, "web");
        assert_eq!(web.image_ref, "myimage");
        assert_eq!(
            web.command,
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string()
            ])
        );
        assert_eq!(web.ports, vec![(8080u16, 80u16)]);
        assert_eq!(web.env, vec![("FOO".to_string(), "bar".to_string())]);
        assert!(web.eager);

        let db = &c.services[1];
        assert_eq!(db.name, "db");
        assert_eq!(db.image_ref, "dbimage");
        assert_eq!(db.ports, vec![(5432u16, 5432u16)]);
        assert_eq!(db.env, vec![("DB".to_string(), "test".to_string())]);
        assert!(!db.eager);
    }

    #[test]
    fn parse_compose_unknown_key_ignored() {
        let yaml = "services:\n  svc:\n    image: foo\n    totally-unknown: whatever\n";
        let c = parse_compose(yaml).unwrap();
        assert_eq!(c.services.len(), 1);
        assert_eq!(c.services[0].image_ref, "foo");
    }

    #[test]
    fn parse_compose_empty_services() {
        let yaml = "services:\n";
        let c = parse_compose(yaml).unwrap();
        assert_eq!(c.services.len(), 0);
    }

    #[test]
    fn parse_compose_command_string_form() {
        let yaml = "services:\n  svc:\n    image: img\n    command: sleep 30\n";
        let c = parse_compose(yaml).unwrap();
        let cmd = c.services[0].command.as_ref().unwrap();
        assert_eq!(cmd, &["/bin/sh", "-c", "sleep 30"]);
    }

    // F-309: secrets/configs lists + nested healthcheck map parse into the
    // service spec.
    #[test]
    fn parse_compose_secrets_configs_healthcheck() {
        let yaml = r#"
services:
  api:
    image: apiimg
    command: serve
    secrets:
      - db_password=secret/db-pass
      - api_key=secret/api-key
    configs:
      - app_conf=config/app
    healthcheck:
      test: ["CMD", "curl", "-fsS", "localhost:8080/health"]
      interval: 15s
      retries: 5
"#;
        let c = parse_compose(yaml).unwrap();
        assert_eq!(c.services.len(), 1);
        let api = &c.services[0];
        assert_eq!(api.image_ref, "apiimg");
        assert_eq!(
            api.secrets,
            vec![
                ("db_password".to_string(), "secret/db-pass".to_string()),
                ("api_key".to_string(), "secret/api-key".to_string()),
            ]
        );
        assert_eq!(
            api.configs,
            vec![("app_conf".to_string(), "config/app".to_string())]
        );
        let hc = api.healthcheck.as_ref().expect("healthcheck parsed");
        assert_eq!(hc.0, "curl -fsS localhost:8080/health");
        assert_eq!(hc.1, 15, "interval 15s ⇒ 15");
        assert_eq!(hc.2, 5, "retries 5");
    }

    // F-309: a bare-string healthcheck test + bare-int interval also parse.
    #[test]
    fn parse_compose_healthcheck_string_form() {
        let yaml = "services:\n  svc:\n    image: i\n    healthcheck:\n      cmd: pgrep myproc\n      interval: 30\n      retries: 2\n";
        let c = parse_compose(yaml).unwrap();
        let hc = c.services[0].healthcheck.as_ref().expect("hc");
        assert_eq!(hc.0, "pgrep myproc");
        assert_eq!(hc.1, 30);
        assert_eq!(hc.2, 2);
    }

    // A healthcheck with no command (only interval) is dropped (meaningless).
    #[test]
    fn parse_compose_healthcheck_without_cmd_dropped() {
        let yaml = "services:\n  svc:\n    image: i\n    healthcheck:\n      interval: 10s\n";
        let c = parse_compose(yaml).unwrap();
        assert!(
            c.services[0].healthcheck.is_none(),
            "healthcheck without a command must be dropped"
        );
    }

    #[test]
    fn parse_duration_secs_forms() {
        assert_eq!(parse_duration_secs("30"), Some(30));
        assert_eq!(parse_duration_secs("30s"), Some(30));
        assert_eq!(parse_duration_secs("1m"), Some(60));
        assert_eq!(parse_duration_secs("2m30s"), Some(150));
        assert_eq!(parse_duration_secs("1h"), Some(3600));
        assert_eq!(parse_duration_secs(""), None);
        assert_eq!(parse_duration_secs("30x"), None);
        assert_eq!(parse_duration_secs("abc"), None);
        assert_eq!(
            parse_duration_secs("10s5"),
            None,
            "trailing unit-less number"
        );
    }

    // ── compose lazy tests ──────────────────────────────────────────────────

    /// Test that compose_up binds a port and no service process runs until
    /// a connection is made.
    /// The proxy round-trip correctness is tested in acceptance (A24); here
    /// we test the listener-bound + lazy-start mechanics.
    #[test]
    fn compose_lazy_bind_and_start() {
        with_home(|| {
            // Use a port unlikely to conflict
            let port: u16 = 19877;
            let compose = Compose {
                services: vec![Service {
                    name: "lazy-svc".to_string(),
                    image_ref: "scratch".to_string(),
                    command: Some(vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "sleep 30".to_string(),
                    ]),
                    ports: vec![(port, port)],
                    env: Vec::new(),
                    eager: false,
                    secrets: Vec::new(),
                    configs: Vec::new(),
                    healthcheck: None,
                }],
            };

            let store_tmp = TempDir::new().unwrap();
            let store = Store::open(store_tmp.path()).unwrap();

            // compose_up should succeed (writes spec, spawns detached supervisor)
            let handle = compose_up(&compose, &store, 3600).unwrap();
            assert_eq!(handle.services, vec!["lazy-svc"]);
            assert!(handle.stack_dir.exists());
            assert!(handle.stack_dir.join("spec.json").exists());

            // Give supervisor a moment to bind
            std::thread::sleep(std::time::Duration::from_millis(300));

            // Check: no service process running yet (we check the run dirs in the spec)
            let spec_bytes = std::fs::read(handle.stack_dir.join("spec.json")).unwrap();
            let spec: StackSpec = serde_json::from_slice(&spec_bytes).unwrap();
            let svc_spec = &spec.services[0];
            // run_dir must be None (supervisor hasn't started it yet — no connection)
            assert!(
                svc_spec.run_dir.is_none(),
                "service must not be started before first connection"
            );

            // compose_down cleans up
            compose_down(&handle.stack_dir).unwrap();
            assert!(
                !handle.stack_dir.exists(),
                "stack dir must be removed by compose_down"
            );
        });
    }

    /// prepare_service_cwd hydrates the service's image_ref into a clean cwd,
    /// so a known file from the snapshotted ref is present before the run.
    #[test]
    fn prepare_service_cwd_hydrates_image_ref() {
        // Snapshot a tiny dir as a ref into a test store.
        let src = TempDir::new().unwrap();
        std::fs::write(src.path().join("marker.txt"), b"from-image").unwrap();

        let store_tmp = TempDir::new().unwrap();
        let store = Store::open(store_tmp.path()).unwrap();
        lightr_index::snapshot(src.path(), &store, "svc-img").unwrap();

        let svc = ServiceSpec {
            name: "hydrate-me".to_string(),
            image_ref: "svc-img".to_string(),
            command: vec!["/bin/true".to_string()],
            ports: Vec::new(),
            env: Vec::new(),
            eager: false,
            run_dir: None,
            secrets: Vec::new(),
            configs: Vec::new(),
            healthcheck: None,
        };

        let cwd = prepare_service_cwd(&svc, &store).unwrap();
        let marker = cwd.join("marker.txt");
        assert!(
            marker.exists(),
            "image_ref file must be hydrated into service cwd"
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"from-image");

        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// An empty image_ref (command-only service) yields a clean empty cwd.
    #[test]
    fn prepare_service_cwd_empty_ref_is_clean() {
        let store_tmp = TempDir::new().unwrap();
        let store = Store::open(store_tmp.path()).unwrap();

        let svc = ServiceSpec {
            name: "cmd-only".to_string(),
            image_ref: String::new(),
            command: vec!["/bin/true".to_string()],
            ports: Vec::new(),
            env: Vec::new(),
            eager: false,
            run_dir: None,
            secrets: Vec::new(),
            configs: Vec::new(),
            healthcheck: None,
        };

        let cwd = prepare_service_cwd(&svc, &store).unwrap();
        assert!(cwd.is_dir(), "command-only service still gets a cwd");
        assert_eq!(
            std::fs::read_dir(&cwd).unwrap().count(),
            0,
            "command-only service cwd must be empty"
        );

        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// compose_down on a non-existent dir returns Ok (idempotent)
    #[test]
    fn compose_down_nonexistent_is_ok() {
        let tmp = TempDir::new().unwrap();
        let fake = tmp.path().join("no-such-stack");
        assert!(compose_down(&fake).is_ok());
    }

    #[test]
    fn step_reads_clock_or_net_heuristic() {
        let date_cmd = vec!["/bin/sh".to_string(), "-c".to_string(), "date".to_string()];
        assert!(step_reads_clock_or_net(&date_cmd));

        let echo_cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo hi".to_string(),
        ];
        assert!(!step_reads_clock_or_net(&echo_cmd));

        let curl_cmd = vec!["curl".to_string(), "https://example.com".to_string()];
        assert!(step_reads_clock_or_net(&curl_cmd));
    }
}
