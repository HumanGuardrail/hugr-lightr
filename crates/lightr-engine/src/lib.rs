//! lightr-engine — frozen contract: build-spec-r2.md §2 (bodies: WP R2-W2).

pub mod engine;
pub mod limits;
pub mod pack;

/// Re-export the guest-env PATH so the vz-memo key (lightr-cli handler) hashes
/// the EXACT value the engine injects into the guest command — one source of
/// truth (lightr_init::GUEST_PATH), so they can never drift and replay a HIT
/// produced under a different environment.
pub use lightr_init::GUEST_PATH;

// ── Flat re-exports (API-identical paths) ────────────────────────────────────

pub use engine::{
    engine_for, pack_status, probe, Engine, EngineCaps, EngineKind, ExecSpec, MountKind,
    NativeEngine, ResolvedMount,
};

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    // from_str roundtrip + reject
    #[test]
    fn from_str_roundtrip() {
        assert_eq!(EngineKind::from_str("native").unwrap(), EngineKind::Native);
        assert_eq!(EngineKind::from_str("ns").unwrap(), EngineKind::Ns);
        assert_eq!(EngineKind::from_str("vz").unwrap(), EngineKind::Vz);
        assert_eq!(EngineKind::from_str("wsl").unwrap(), EngineKind::Wsl);
    }

    // as_str is the exact inverse of from_str for every kind in all().
    #[test]
    fn as_str_inverts_from_str_for_all_kinds() {
        for &k in EngineKind::all() {
            assert_eq!(
                EngineKind::from_str(k.as_str()).unwrap(),
                k,
                "as_str/from_str roundtrip failed for {k:?}"
            );
        }
    }

    // all() lists every variant exactly once (guards future additions).
    #[test]
    fn all_lists_every_kind() {
        let all = EngineKind::all();
        assert!(all.contains(&EngineKind::Native));
        assert!(all.contains(&EngineKind::Ns));
        assert!(all.contains(&EngineKind::Vz));
        assert!(all.contains(&EngineKind::Wsl));
        assert_eq!(all.len(), 4, "exactly four engine kinds");
    }

    // platform_default picks this host's isolation engine; native always works.
    #[test]
    fn platform_default_matches_host() {
        let d = EngineKind::platform_default();
        #[cfg(target_os = "macos")]
        assert_eq!(d, EngineKind::Vz);
        #[cfg(target_os = "linux")]
        assert_eq!(d, EngineKind::Ns);
        #[cfg(target_os = "windows")]
        assert_eq!(d, EngineKind::Wsl);
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        assert_eq!(d, EngineKind::Native);
    }

    #[test]
    fn from_str_reject() {
        let err = EngineKind::from_str("bogus").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown engine"),
            "expected 'unknown engine' in: {msg}"
        );
    }

    // probe(Native) always available
    #[test]
    fn probe_native_available() {
        let caps = probe(EngineKind::Native);
        assert!(caps.available, "native must always be available");
        assert!(
            caps.detail.contains("native"),
            "detail should mention 'native': {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("no isolation"),
            "detail must be honest about no isolation: {}",
            caps.detail
        );
    }

    // probe(Ns) is false on macOS with "requires Linux"
    #[test]
    #[cfg(target_os = "macos")]
    fn probe_ns_unavailable_on_macos() {
        let caps = probe(EngineKind::Ns);
        assert!(!caps.available, "ns must be unavailable on macOS");
        assert!(
            caps.detail.contains("Linux"),
            "detail must mention Linux: {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("requires Linux"),
            "detail: {}",
            caps.detail
        );
    }

    // probe(Wsl) is false off-Windows with the host OS named (no overclaim).
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn probe_wsl_unavailable_off_windows() {
        let caps = probe(EngineKind::Wsl);
        assert!(!caps.available, "wsl must be unavailable off Windows");
        assert!(
            caps.detail.contains("Windows"),
            "detail must mention Windows: {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("WSL2"),
            "detail must mention WSL2: {}",
            caps.detail
        );
    }

    // engine_for(Wsl) off-Windows fails closed with a Windows reason.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn engine_for_wsl_off_windows_err_contains_windows() {
        match engine_for(EngineKind::Wsl) {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("Windows"), "error must mention Windows: {msg}");
            }
            Ok(_) => panic!("engine_for(Wsl) must fail off Windows"),
        }
    }

    // probe(Vz) is false (feature off) with actionable detail
    #[test]
    #[cfg(not(feature = "vz"))]
    fn probe_vz_unavailable_feature_off() {
        let caps = probe(EngineKind::Vz);
        assert!(!caps.available, "vz must be unavailable without feature");
        assert!(
            caps.detail.contains("'vz' build feature"),
            "detail must mention 'vz' build feature: {}",
            caps.detail
        );
        assert!(
            caps.detail.contains("install-pack"),
            "detail must be actionable (mention install-pack): {}",
            caps.detail
        );
    }

    // NativeEngine runs /bin/echo and returns 0 (inherit stdio)
    #[test]
    fn native_engine_echo_exit_0() {
        let engine = NativeEngine;
        let cwd = std::env::current_dir().unwrap();
        let command: Vec<String> = vec!["/bin/echo".to_string(), "lightr-engine-test".to_string()];
        let spec = ExecSpec {
            cwd: &cwd,
            command: &command,
            rootfs: None,
            limits: Default::default(),
            net: false,
            net_isolate: false,
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
            read_only: false,
            shm_size: None,
            cap_drop: &[],
            cap_add: &[],
            init: false,
        };
        let code = engine.run(&spec).expect("echo should not fail");
        assert_eq!(code, 0, "echo exits 0");
    }

    // NativeEngine maps exit code correctly (sh -c 'exit 5' => 5)
    #[test]
    fn native_engine_exit_code_mapping() {
        let engine = NativeEngine;
        let cwd = std::env::current_dir().unwrap();
        let command: Vec<String> = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 5".to_string(),
        ];
        let spec = ExecSpec {
            cwd: &cwd,
            command: &command,
            rootfs: None,
            limits: Default::default(),
            net: false,
            net_isolate: false,
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
            read_only: false,
            shm_size: None,
            cap_drop: &[],
            cap_add: &[],
            init: false,
        };
        let code = engine.run(&spec).expect("sh should not fail to launch");
        assert_eq!(code, 5, "exit code must be 5, got {code}");
    }

    // NativeEngine with Some(rootfs) => Err
    #[test]
    fn native_engine_rootfs_rejected() {
        let engine = NativeEngine;
        let cwd = std::env::current_dir().unwrap();
        let rootfs = std::path::PathBuf::from("/tmp/fake-rootfs");
        let command: Vec<String> = vec!["/bin/true".to_string()];
        let spec = ExecSpec {
            cwd: &cwd,
            command: &command,
            rootfs: Some(&rootfs),
            limits: Default::default(),
            net: false,
            net_isolate: false,
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
            read_only: false,
            shm_size: None,
            cap_drop: &[],
            cap_add: &[],
            init: false,
        };
        let err = engine.run(&spec).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no rootfs"),
            "expected 'no rootfs' in error: {msg}"
        );
    }

    // engine_for(Ns) on macOS => Err containing "Linux"
    #[test]
    #[cfg(target_os = "macos")]
    fn engine_for_ns_macos_err_contains_linux() {
        match engine_for(EngineKind::Ns) {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("Linux"), "error must mention Linux: {msg}");
            }
            Ok(_) => panic!("engine_for(Ns) must fail on macOS"),
        }
    }
}
