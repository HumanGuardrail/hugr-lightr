//! `lightr engine` handlers — build-spec-r2 §4.
//!
//! Sub-verbs:
//!   engine ls [--json]
//!   engine install-pack <dir>

use lightr_engine::{probe, EngineKind};
use serde::Serialize;

use crate::lightr_home;

// ── engine ls ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct EngineEntry {
    kind: String,
    available: bool,
    detail: String,
}

pub fn ls(json: bool) -> i32 {
    let kinds = [EngineKind::Native, EngineKind::Ns, EngineKind::Vz];
    let entries: Vec<EngineEntry> = kinds
        .iter()
        .map(|&kind| {
            let caps = probe(kind);
            let kind_str = match kind {
                EngineKind::Native => "native",
                EngineKind::Ns => "ns",
                EngineKind::Vz => "vz",
            };
            EngineEntry {
                kind: kind_str.to_string(),
                available: caps.available,
                detail: caps.detail,
            }
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&entries).expect("serialize engine ls")
        );
    } else {
        for e in &entries {
            let avail_str = if e.available {
                "available"
            } else {
                "unavailable"
            };
            println!("{:<10}{:<14}{}", e.kind, avail_str, e.detail);
        }
    }

    0
}

// ── engine install-pack ───────────────────────────────────────────────────────

pub fn install_pack(dir: &str) -> i32 {
    let src = std::path::Path::new(dir);
    let kernel_src = src.join("kernel");
    let initrd_src = src.join("initrd");

    // Validate both required files are present
    let has_kernel = kernel_src.exists();
    let has_initrd = initrd_src.exists();

    if !has_kernel {
        eprintln!("lightr: install-pack: missing file 'kernel' in {dir}");
        return 1;
    }
    if !has_initrd {
        eprintln!("lightr: install-pack: missing file 'initrd' in {dir}");
        return 1;
    }

    // Destination: $LIGHTR_HOME/packs/linux/
    let dest_dir = lightr_home().join("packs").join("linux");
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        eprintln!(
            "lightr: install-pack: cannot create {}: {e}",
            dest_dir.display()
        );
        return 1;
    }

    let kernel_dst = dest_dir.join("kernel");
    let initrd_dst = dest_dir.join("initrd");

    if let Err(e) = std::fs::copy(&kernel_src, &kernel_dst) {
        eprintln!("lightr: install-pack: copy kernel failed: {e}");
        return 1;
    }
    if let Err(e) = std::fs::copy(&initrd_src, &initrd_dst) {
        eprintln!("lightr: install-pack: copy initrd failed: {e}");
        return 1;
    }

    println!("installed linux pack → {}", dest_dir.display());
    0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn ls_human_returns_0() {
        assert_eq!(ls(false), 0);
    }

    #[test]
    fn ls_json_returns_0() {
        assert_eq!(ls(true), 0);
    }

    #[test]
    fn install_pack_missing_kernel() {
        let tmp = TempDir::new().unwrap();
        // Only write initrd, not kernel
        fs::write(tmp.path().join("initrd"), b"initrd-data").unwrap();
        let code = install_pack(tmp.path().to_str().unwrap());
        assert_eq!(code, 1, "missing kernel must exit 1");
    }

    #[test]
    fn install_pack_missing_initrd() {
        let tmp = TempDir::new().unwrap();
        // Only write kernel, not initrd
        fs::write(tmp.path().join("kernel"), b"kernel-data").unwrap();
        let code = install_pack(tmp.path().to_str().unwrap());
        assert_eq!(code, 1, "missing initrd must exit 1");
    }

    #[test]
    fn install_pack_missing_both() {
        let tmp = TempDir::new().unwrap();
        let code = install_pack(tmp.path().to_str().unwrap());
        assert_eq!(code, 1);
    }

    #[test]
    fn install_pack_succeeds_with_both_files() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("kernel"), b"kernel-data").unwrap();
        fs::write(src_dir.join("initrd"), b"initrd-data").unwrap();

        // Point LIGHTR_HOME to a temp dir so we don't pollute the real one
        let home_tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", home_tmp.path());

        let code = install_pack(src_dir.to_str().unwrap());

        std::env::remove_var("LIGHTR_HOME");

        assert_eq!(code, 0, "install_pack should succeed");
        // Verify files landed in the right place
        let pack_dir = home_tmp.path().join("packs").join("linux");
        assert!(pack_dir.join("kernel").exists(), "kernel must be installed");
        assert!(pack_dir.join("initrd").exists(), "initrd must be installed");
        assert_eq!(fs::read(pack_dir.join("kernel")).unwrap(), b"kernel-data");
        assert_eq!(fs::read(pack_dir.join("initrd")).unwrap(), b"initrd-data");
    }
}
