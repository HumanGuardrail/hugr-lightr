//! Unit tests for the `rootfs` file writers, split out of `rootfs.rs` to keep
//! that module under the 400-LOC godfile invariant. `append_rootfs_file` is
//! `pub(super)` in `rootfs`, i.e. visible throughout `ns_impl` and its
//! descendants, so this sibling test module reaches it via `super::rootfs`.

use super::rootfs::append_rootfs_file;

#[test]
fn append_rootfs_file_preserves_existing_then_appends() {
    let tmp = std::env::temp_dir().join(format!("lightr-hosts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("etc")).unwrap();
    // Seed an image-style /etc/hosts.
    std::fs::write(tmp.join("etc/hosts"), b"127.0.0.1\tlocalhost\n").unwrap();
    // --add-host adds one line.
    append_rootfs_file(&tmp, "etc/hosts", b"10.9.8.7\tmyhost.local\n").unwrap();
    let got = std::fs::read_to_string(tmp.join("etc/hosts")).unwrap();
    assert!(
        got.contains("127.0.0.1\tlocalhost"),
        "existing line preserved"
    );
    assert!(got.contains("10.9.8.7\tmyhost.local"), "new line appended");
    // order: existing first, appended after (no truncation).
    assert!(
        got.find("localhost").unwrap() < got.find("myhost.local").unwrap(),
        "append, not prepend/truncate"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn append_rootfs_file_creates_when_missing() {
    let tmp = std::env::temp_dir().join(format!("lightr-hosts2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    // No pre-existing /etc/hosts (alpine-style): the helper creates it + parent.
    append_rootfs_file(&tmp, "etc/hosts", b"10.9.8.7\tmyhost.local\n").unwrap();
    let got = std::fs::read_to_string(tmp.join("etc/hosts")).unwrap();
    assert_eq!(got, "10.9.8.7\tmyhost.local\n");
    let _ = std::fs::remove_dir_all(&tmp);
}
