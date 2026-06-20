//! Tests for the WP-VOL-1 mount-grammar parsers (parity-contract.md §0
//! R-MOUNT). Pure parsing — no I/O, no global state, fully parallel-safe.
#![cfg(test)]

use crate::run::mount::{parse_mount_long, parse_tmpfs, parse_v, MountKind};

// ── parse_v ───────────────────────────────────────────────────────────────

#[test]
fn v_named_volume() {
    let m = parse_v("data:/var/lib/data").unwrap();
    assert_eq!(m.kind, MountKind::NamedVolume);
    assert_eq!(m.source.as_deref(), Some("data"));
    assert_eq!(m.target, "/var/lib/data");
    assert!(!m.readonly);
    assert!(m.opts.is_empty());
}

#[test]
fn v_named_volume_dotted_name() {
    // `my.vol-1` has no `/` and no leading `.`/`~` → a valid volume name.
    let m = parse_v("my.vol-1:/dst").unwrap();
    assert_eq!(m.kind, MountKind::NamedVolume);
    assert_eq!(m.source.as_deref(), Some("my.vol-1"));
}

#[test]
fn v_host_bind_absolute() {
    let m = parse_v("/home/u/src:/app").unwrap();
    assert_eq!(m.kind, MountKind::HostBind);
    assert_eq!(m.source.as_deref(), Some("/home/u/src"));
    assert_eq!(m.target, "/app");
}

#[test]
fn v_host_bind_relative_dot() {
    let m = parse_v("./rel:/app").unwrap();
    assert_eq!(m.kind, MountKind::HostBind);
    assert_eq!(m.source.as_deref(), Some("./rel"));
}

#[test]
fn v_host_bind_home_tilde() {
    let m = parse_v("~/work:/app").unwrap();
    assert_eq!(m.kind, MountKind::HostBind);
    assert_eq!(m.source.as_deref(), Some("~/work"));
}

#[test]
fn v_anon_volume() {
    let m = parse_v("/data").unwrap();
    assert_eq!(m.kind, MountKind::AnonVolume);
    assert_eq!(m.source, None);
    assert_eq!(m.target, "/data");
}

#[test]
fn v_readonly_opt() {
    let m = parse_v("/host:/app:ro").unwrap();
    assert_eq!(m.kind, MountKind::HostBind);
    assert!(m.readonly);
    assert!(m.opts.is_empty());
}

#[test]
fn v_rw_opt_explicit() {
    let m = parse_v("/host:/app:rw").unwrap();
    assert!(!m.readonly);
}

#[test]
fn v_opts_passthrough_and_ro() {
    let m = parse_v("/host:/app:ro,z,cached").unwrap();
    assert!(m.readonly);
    assert_eq!(m.opts, vec!["z".to_string(), "cached".to_string()]);
}

#[test]
fn v_empty_value_errors() {
    assert!(parse_v("").is_err());
}

#[test]
fn v_empty_target_errors() {
    assert!(parse_v("data:").is_err());
}

#[test]
fn v_too_many_colons_errors() {
    assert!(parse_v("a:b:c:d").is_err());
}

#[test]
fn v_invalid_volume_name_errors() {
    // `bad name` (space) is not a path and not a valid volume name.
    assert!(parse_v("bad name:/dst").is_err());
}

// ── parse_v: CAS-ref source (WP-VOL-2) ──────────────────────────────────────

#[test]
fn v_cas_ref() {
    // `@ref:/dst` → the imageless 4th kind; `@` stripped from the source.
    let m = parse_v("@myref:/data").unwrap();
    assert_eq!(m.kind, MountKind::CasRef);
    assert_eq!(m.source.as_deref(), Some("myref"));
    assert_eq!(m.target, "/data");
    assert!(!m.readonly);
}

#[test]
fn v_cas_ref_with_opts() {
    let m = parse_v("@myref:/data:ro,z").unwrap();
    assert_eq!(m.kind, MountKind::CasRef);
    assert_eq!(m.source.as_deref(), Some("myref"));
    assert!(m.readonly);
    assert_eq!(m.opts, vec!["z".to_string()]);
}

#[test]
fn v_cas_ref_empty_errors() {
    // A bare `@` (empty ref) is fail-closed.
    assert!(parse_v("@:/data").is_err());
}

#[test]
fn v_cas_ref_invalid_name_errors() {
    // `@bad name` reuses the name validator → rejected.
    assert!(parse_v("@bad name:/data").is_err());
}

#[test]
fn v_cas_ref_beats_name() {
    // Precedence: `@` wins even when the rest would be a valid volume name
    // (no slash, no leading `.`/`~`). `@` > path > name.
    let m = parse_v("@plainname:/data").unwrap();
    assert_eq!(m.kind, MountKind::CasRef);
    assert_eq!(m.source.as_deref(), Some("plainname"));
}

// ── parse_mount_long ────────────────────────────────────────────────────────

#[test]
fn mount_bind() {
    let m = parse_mount_long("type=bind,source=/host,target=/ctr").unwrap();
    assert_eq!(m.kind, MountKind::HostBind);
    assert_eq!(m.source.as_deref(), Some("/host"));
    assert_eq!(m.target, "/ctr");
    assert!(!m.readonly);
}

#[test]
fn mount_volume_named() {
    let m = parse_mount_long("type=volume,source=vol,target=/ctr").unwrap();
    assert_eq!(m.kind, MountKind::NamedVolume);
    assert_eq!(m.source.as_deref(), Some("vol"));
}

#[test]
fn mount_volume_default_type_with_source() {
    // No `type=` + a source → defaults to a named volume.
    let m = parse_mount_long("source=vol,target=/ctr").unwrap();
    assert_eq!(m.kind, MountKind::NamedVolume);
}

#[test]
fn mount_volume_default_type_no_source_is_anon() {
    let m = parse_mount_long("target=/ctr").unwrap();
    assert_eq!(m.kind, MountKind::AnonVolume);
    assert_eq!(m.source, None);
}

#[test]
fn mount_tmpfs() {
    let m = parse_mount_long("type=tmpfs,target=/tmp,tmpfs-size=64m").unwrap();
    assert_eq!(m.kind, MountKind::Tmpfs);
    assert_eq!(m.target, "/tmp");
    assert_eq!(m.opts, vec!["tmpfs-size=64m".to_string()]);
}

#[test]
fn mount_src_and_dst_aliases() {
    let m = parse_mount_long("type=bind,src=/h,dst=/c").unwrap();
    assert_eq!(m.source.as_deref(), Some("/h"));
    assert_eq!(m.target, "/c");
}

#[test]
fn mount_destination_alias() {
    let m = parse_mount_long("type=bind,src=/h,destination=/c").unwrap();
    assert_eq!(m.target, "/c");
}

#[test]
fn mount_readonly_bare() {
    let m = parse_mount_long("type=bind,src=/h,dst=/c,readonly").unwrap();
    assert!(m.readonly);
}

#[test]
fn mount_ro_eq_true() {
    let m = parse_mount_long("type=bind,src=/h,dst=/c,ro=true").unwrap();
    assert!(m.readonly);
}

#[test]
fn mount_readonly_false() {
    let m = parse_mount_long("type=bind,src=/h,dst=/c,readonly=false").unwrap();
    assert!(!m.readonly);
}

#[test]
fn mount_unknown_key_passthrough() {
    let m = parse_mount_long("type=volume,target=/c,volume-driver=local").unwrap();
    assert_eq!(m.opts, vec!["volume-driver=local".to_string()]);
}

#[test]
fn mount_missing_target_errors() {
    assert!(parse_mount_long("type=bind,source=/h").is_err());
}

#[test]
fn mount_empty_target_errors() {
    assert!(parse_mount_long("type=bind,target=").is_err());
}

#[test]
fn mount_unknown_type_errors() {
    assert!(parse_mount_long("type=bogus,target=/c").is_err());
}

#[test]
fn mount_bare_type_errors() {
    assert!(parse_mount_long("type,target=/c").is_err());
}

#[test]
fn mount_readonly_non_bool_errors() {
    assert!(parse_mount_long("type=bind,target=/c,readonly=maybe").is_err());
}

#[test]
fn mount_empty_value_errors() {
    assert!(parse_mount_long("").is_err());
}

#[test]
fn mount_cas_ref_source() {
    // `source=@ref` → CasRef (imageless 4th kind); `@` stripped.
    let m = parse_mount_long("source=@myref,target=/data").unwrap();
    assert_eq!(m.kind, MountKind::CasRef);
    assert_eq!(m.source.as_deref(), Some("myref"));
    assert_eq!(m.target, "/data");
}

#[test]
fn mount_cas_ref_beats_type() {
    // A `@`-source wins over `type=` — the imageless ref is the intent.
    let m = parse_mount_long("type=volume,source=@myref,target=/data").unwrap();
    assert_eq!(m.kind, MountKind::CasRef);
    assert_eq!(m.source.as_deref(), Some("myref"));
}

#[test]
fn mount_cas_ref_empty_errors() {
    assert!(parse_mount_long("source=@,target=/data").is_err());
}

// ── parse_tmpfs ─────────────────────────────────────────────────────────────

#[test]
fn tmpfs_bare_target() {
    let m = parse_tmpfs("/run").unwrap();
    assert_eq!(m.kind, MountKind::Tmpfs);
    assert_eq!(m.source, None);
    assert_eq!(m.target, "/run");
    assert!(m.opts.is_empty());
    assert!(!m.readonly);
}

#[test]
fn tmpfs_with_size() {
    let m = parse_tmpfs("/run:size=64m").unwrap();
    assert_eq!(m.target, "/run");
    assert_eq!(m.opts, vec!["size=64m".to_string()]);
}

#[test]
fn tmpfs_size_and_mode() {
    let m = parse_tmpfs("/run:size=64m,mode=1777").unwrap();
    assert_eq!(
        m.opts,
        vec!["size=64m".to_string(), "mode=1777".to_string()]
    );
}

#[test]
fn tmpfs_empty_value_errors() {
    assert!(parse_tmpfs("").is_err());
}

#[test]
fn tmpfs_empty_target_errors() {
    assert!(parse_tmpfs(":size=64m").is_err());
}
