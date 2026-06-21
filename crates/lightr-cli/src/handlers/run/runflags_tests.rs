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
    // WP-NET3: no networking flags ⇒ all-default ⇒ byte-identical no-op.
    assert!(f.network.is_none());
    assert!(f.network_alias.is_empty());
    assert!(f.add_host.is_empty());
    assert!(f.dns.is_empty());
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
fn network_flag_threads_through_resolve() {
    // WP-NET3: `--network` is now WIRED — `resolve` carries it through (the
    // engine/rootfs guardrail lives in the handler, not here).
    let f = RawRunFlags {
        network: Some("mynet".to_string()),
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(f.network.as_deref(), Some("mynet"));
}

#[test]
fn network_alias_and_dns_thread_through_resolve() {
    // WP-NET3: `--network-alias` + `--dns` are carried through verbatim.
    let f = RawRunFlags {
        network_alias: vec!["a".to_string(), "b".to_string()],
        dns: vec!["1.1.1.1".to_string()],
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(f.network_alias, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(f.dns, vec!["1.1.1.1".to_string()]);
}

#[test]
fn add_host_well_formed_threads_through() {
    // WP-NET3: a well-formed `HOST:IP` `--add-host` is carried as a raw string.
    let f = RawRunFlags {
        add_host: vec!["h:1.2.3.4".to_string()],
        ..raw()
    }
    .resolve()
    .unwrap();
    assert_eq!(f.add_host, vec!["h:1.2.3.4".to_string()]);
}

#[test]
fn add_host_malformed_is_honest_error() {
    // WP-NET3: `resolve` value-validates `--add-host` shape (svz would silently
    // drop a malformed entry — NET3 surfaces the parse error as exit 2).
    for bad in ["nocolon", ":1.2.3.4", "host:", ""] {
        let err = RawRunFlags {
            add_host: vec![bad.to_string()],
            ..raw()
        }
        .resolve()
        .unwrap_err();
        assert_eq!(err, 2, "malformed --add-host {bad:?} must exit 2");
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
