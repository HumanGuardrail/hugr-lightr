//! Build execution: materialize_from_digest, BuildReport, build(), copy_dir_recursive,
//! step_reads_clock_or_net.
use lightr_core::{Digest, LightrError, Manifest, Result};
use lightr_store::Store;
use std::path::Path;

use super::memo::{load_meta, save_meta, step_key, TempDirGuard};
use super::parse::Instr;

/// Materialize a snapshot (identified by its manifest digest) into `dest`.
/// Clears `dest` first so we get a clean layer.
pub(crate) fn materialize_from_digest(
    dest: &Path,
    manifest_digest: &Digest,
    store: &Store,
) -> Result<()> {
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
                    }
                }
            }
        }
    }
    Ok(())
}

pub struct BuildReport {
    pub name: String,
    pub root: Digest,
    pub steps: u64,
    pub cached_steps: u64,
}
/// Execute a Dockerfile build.
///
/// - RUN steps use the **native engine** (`rootfs: None`). No filesystem
///   isolation -- RUN runs in the working tree directly.
/// - Memoization: each step has a content-derived key; AC hits replay the
///   cached layer without executing.
pub fn build(
    context_dir: &Path,
    dockerfile: &Path,
    name: &str,
    engine: lightr_engine::EngineKind,
    store: &Store,
) -> Result<BuildReport> {
    use super::parse::parse_dockerfile;

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
                materialize_from_digest(work_dir, &cached_root, store)?;
                prev_layer_root = Some(cached_root);
                cached_steps += 1;
                let meta = load_meta(work_dir);
                accumulated_env = meta.env.clone();
                if let Instr::Workdir { path } = &step.instr {
                    current_workdir = path.clone();
                }
                continue;
            }
        }

        match &step.instr {
            Instr::From { image_ref, .. } => {
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
                    lightr_index::hydrate(work_dir, store, image_ref)?;
                }
            }
            Instr::Run { argv, .. } => {
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
                    rootfs: None,
                    limits: Default::default(),
                    net: false,
                    net_fd: None,
                    net_mac: None,
                    mounts: &[],
                    env: &[],
                    workdir: None,
                    user: None,
                    hostname: None,
                    add_host: &[],
                    dns: &[],
                    mesh_ip: None,
                };
                let mut cmd_builder = std::process::Command::new(&argv[0]);
                if argv.len() > 1 {
                    cmd_builder.args(&argv[1..]);
                }
                for (k, v) in &accumulated_env {
                    cmd_builder.env(k, v);
                }
                let code = eng.run(&spec)?;
                if code != 0 {
                    return Err(LightrError::InvalidManifest(format!(
                        "RUN step exited with code {code}: {:?}",
                        argv
                    )));
                }
            }
            Instr::Copy { src, dest, .. } => {
                let dest_path = if dest.starts_with('/') {
                    work_dir.join(dest.trim_start_matches('/'))
                } else {
                    work_dir.join(dest)
                };
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
            Instr::Cmd { argv, .. } => {
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
            // WP-DF-01 parses these into the AST; execution is DF-02..15. Until
            // then they route to the SAME "unsupported instruction" error path
            // as before (fail-closed, behavior-preserving — these never built).
            other => {
                return Err(LightrError::InvalidManifest(format!(
                    "unsupported instruction: {}",
                    instr_verb(other)
                )));
            }
        }

        let snap = lightr_index::snapshot(work_dir, store, name)?;
        let new_root = snap.root;
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

/// Verb name for an `Instr`, used only to report not-yet-implemented
/// instructions through the existing "unsupported instruction" error path.
fn instr_verb(instr: &Instr) -> &'static str {
    match instr {
        Instr::From { .. } => "FROM",
        Instr::Run { .. } => "RUN",
        Instr::Cmd { .. } => "CMD",
        Instr::Entrypoint { .. } => "ENTRYPOINT",
        Instr::Label { .. } => "LABEL",
        Instr::Expose { .. } => "EXPOSE",
        Instr::Env { .. } => "ENV",
        Instr::Add { .. } => "ADD",
        Instr::Copy { .. } => "COPY",
        Instr::Volume { .. } => "VOLUME",
        Instr::User { .. } => "USER",
        Instr::Workdir { .. } => "WORKDIR",
        Instr::Arg { .. } => "ARG",
        Instr::Onbuild { .. } => "ONBUILD",
        Instr::Stopsignal { .. } => "STOPSIGNAL",
        Instr::Healthcheck { .. } => "HEALTHCHECK",
        Instr::Shell { .. } => "SHELL",
    }
}

pub(crate) fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
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

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
