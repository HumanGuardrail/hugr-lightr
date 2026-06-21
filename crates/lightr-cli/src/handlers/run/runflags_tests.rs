//! WP-RUNFLAGS — unit tests for `RawRunFlags::resolve` (pure parsing/lowering,
//! no I/O, parallel-safe).

use super::*;

fn raw() -> RawRunFlags {
    RawRunFlags::default()
}

#[test]
fn default_resolves_to_all_default_noop() {
    let f = raw().resolve().unwrap();
    assert!(f.volumes.is_empty());
    assert!(f.tmpfs.is_empty());
    assert!(f.name.is_none());
    assert!(!f.rm);
    assert!(f.entrypoint.is_none());
}

#[test]
fn host_bind_parses_source_and_target() {
    let f = RawRunFlags {
        volume: vec!["/host/dir:data".to_string()],
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(f.volumes.len(), 1);
    assert_eq!(f.volumes[0].source, "/host/dir");
    assert_eq!(f.volumes[0].target, "data");
    assert!(!f.volumes[0].readonly);
}

#[test]
fn host_bind_ro_sets_readonly() {
    let f = RawRunFlags {
        volume: vec!["/host/dir:data:ro".to_string()],
        ..raw()
    }
    .resolve()
    .unwrap();
    assert!(f.volumes[0].readonly, "the :ro option must set readonly");
}

#[test]
fn named_volume_is_phase2_error() {
    // A bare name (no path separator) parses as a NamedVolume → honest exit 2.
    let err = RawRunFlags {
        volume: vec!["myvol:data".to_string()],
        ..raw()
    }
    .resolve()
    .unwrap_err();
    assert_eq!(err, 2, "named volumes are Phase 2 → exit 2");
}

#[test]
fn entrypoint_lowers_to_single_token() {
    let f = RawRunFlags {
        entrypoint: Some("/bin/myinit".to_string()),
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(
        f.entrypoint.as_deref(),
        Some(&["/bin/myinit".to_string()][..])
    );
}

#[test]
fn empty_entrypoint_is_error() {
    let err = RawRunFlags {
        entrypoint: Some("   ".to_string()),
        ..raw()
    }
    .resolve()
    .unwrap_err();
    assert_eq!(err, 2, "an empty --entrypoint must exit 2");
}

#[test]
fn network_flag_is_honest_phase2_error() {
    let err = RawRunFlags {
        network: Some("mynet".to_string()),
        ..raw()
    }
    .resolve()
    .unwrap_err();
    assert_eq!(err, 2, "--network is honest Phase 2 → exit 2");
}

#[test]
fn network_alias_add_host_dns_are_phase2_errors() {
    for f in [
        RawRunFlags {
            network_alias: vec!["a".to_string()],
            ..raw()
        },
        RawRunFlags {
            add_host: vec!["h:1.2.3.4".to_string()],
            ..raw()
        },
        RawRunFlags {
            dns: vec!["1.1.1.1".to_string()],
            ..raw()
        },
    ] {
        assert_eq!(f.resolve().unwrap_err(), 2);
    }
}

#[test]
fn name_and_rm_pass_through() {
    let f = RawRunFlags {
        name: Some("web".to_string()),
        rm: true,
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(f.name.as_deref(), Some("web"));
    assert!(f.rm);
}

#[test]
fn tmpfs_passes_through() {
    let f = RawRunFlags {
        tmpfs: vec!["scratch".to_string()],
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(f.tmpfs, vec!["scratch".to_string()]);
}
