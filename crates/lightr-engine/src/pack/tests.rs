// `Write` (for the `write!` in write_hex8_is_zero_padded_uppercase) comes in
// via `super::*` — the module imports `std::io::Write` at its top.
use super::*;
use lightr_core::LightrError;

// ── A tiny newc parser, used ONLY to verify our writer round-trips ─────

struct ParsedEntry {
    name: String,
    mode: u32,
    data: Vec<u8>,
}

fn hex8(b: &[u8]) -> u32 {
    u32::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap()
}

/// Parse a newc cpio archive into its entries (stopping at TRAILER!!!).
fn parse_newc(buf: &[u8]) -> Vec<ParsedEntry> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    loop {
        assert!(pos + 110 <= buf.len(), "truncated header at {pos}");
        assert_eq!(&buf[pos..pos + 6], b"070701", "bad magic at {pos}");
        // Fields after magic, 8 hex chars each.
        let f = |i: usize| hex8(&buf[pos + 6 + i * 8..pos + 6 + i * 8 + 8]);
        let mode = f(1);
        let filesize = f(6) as usize;
        let namesize = f(11) as usize;

        let name_start = pos + 110;
        let name_end = name_start + namesize;
        // Name includes a trailing NUL.
        let name = std::str::from_utf8(&buf[name_start..name_end - 1])
            .unwrap()
            .to_string();

        // Data starts after name padded to 4 from the header start.
        let data_start = round4(110 + namesize) + pos;
        let data_end = data_start + filesize;
        let data = buf[data_start..data_end].to_vec();

        // Next header is at data_end padded to 4.
        pos = pos + (round4(data_start - pos + filesize));

        if name == "TRAILER!!!" {
            break;
        }
        entries.push(ParsedEntry { name, mode, data });
    }
    entries
}

/// Round `n` up to the next multiple of 4.
fn round4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

#[test]
fn build_initrd_first_entry_is_init_with_the_bytes() {
    let init_bytes = b"\x7fELF fake-init-binary contents";
    let archive = build_initrd_cpio(init_bytes);

    let entries = parse_newc(&archive);
    assert_eq!(entries.len(), 1, "exactly one real entry before TRAILER");
    assert_eq!(entries[0].name, "init", "first entry must be /init");
    assert_eq!(
        entries[0].data, init_bytes,
        "init entry must carry the exact init bytes"
    );
    assert_eq!(
        entries[0].mode & 0o7777,
        0o755,
        "init must be executable (0755)"
    );
    assert_eq!(
        entries[0].mode & 0o170_000,
        0o100_000,
        "must be a regular file"
    );
}

#[test]
fn build_initrd_is_4_byte_aligned_and_ends_with_trailer() {
    let archive = build_initrd_cpio(b"abc"); // odd length forces padding
    assert_eq!(archive.len() % 4, 0, "whole archive is 4-byte aligned");
    // The trailer name must be present.
    let s = String::from_utf8_lossy(&archive);
    assert!(s.contains("TRAILER!!!"), "archive must end with TRAILER!!!");
    // And it must still parse cleanly.
    let entries = parse_newc(&archive);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].data, b"abc");
}

#[test]
fn build_initrd_empty_init_roundtrips() {
    let archive = build_initrd_cpio(&[]);
    let entries = parse_newc(&archive);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "init");
    assert!(entries[0].data.is_empty());
}

// ── assemble_pack end to end ───────────────────────────────────────────

#[test]
fn assemble_pack_lays_out_kernel_and_valid_initrd() {
    let tmp = tempfile::tempdir().unwrap();
    let kernel = tmp.path().join("vmlinuz-fake");
    let init = tmp.path().join("lightr-init-fake");
    let out = tmp.path().join("pack-out");

    let kernel_bytes = b"FAKE-KERNEL-IMAGE-bytes";
    let init_bytes = b"\x7fELF fake init pid1";
    std::fs::write(&kernel, kernel_bytes).unwrap();
    std::fs::write(&init, init_bytes).unwrap();

    assemble_pack(&kernel, &init, &out, "aarch64", Some("6.12.0")).expect("assemble_pack ok");

    // kernel copied verbatim.
    let got_kernel = std::fs::read(out.join("kernel")).unwrap();
    assert_eq!(got_kernel, kernel_bytes, "kernel copied byte-for-byte");

    // initrd parses, first entry is /init carrying the init bytes.
    let initrd = std::fs::read(out.join("initrd")).unwrap();
    let entries = parse_newc(&initrd);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "init");
    assert_eq!(entries[0].data, init_bytes, "/init holds the init binary");

    // pack.json emitted with arch, kernel_version, and init digest.
    let manifest_bytes = std::fs::read(out.join("pack.json")).unwrap();
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest.arch, "aarch64");
    assert_eq!(manifest.kernel_version.as_deref(), Some("6.12.0"));
    assert_eq!(manifest.init_sha256, sha256_hex(init_bytes));
}

#[test]
fn assemble_pack_creates_out_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let kernel = tmp.path().join("k");
    let init = tmp.path().join("i");
    std::fs::write(&kernel, b"k").unwrap();
    std::fs::write(&init, b"i").unwrap();
    // Nested, non-existent out dir.
    let out = tmp.path().join("a").join("b").join("pack");
    assemble_pack(&kernel, &init, &out, "x86_64", None).expect("creates nested out_dir");
    assert!(out.join("kernel").exists());
    assert!(out.join("initrd").exists());
    assert!(out.join("pack.json").exists());
}

#[test]
fn assemble_pack_missing_init_is_err() {
    let tmp = tempfile::tempdir().unwrap();
    let kernel = tmp.path().join("k");
    std::fs::write(&kernel, b"k").unwrap();
    let missing_init = tmp.path().join("does-not-exist");
    let out = tmp.path().join("out");
    let err = assemble_pack(&kernel, &missing_init, &out, "aarch64", None).unwrap_err();
    assert!(
        matches!(err, LightrError::Io(_)),
        "missing init must surface as an Io error, got {err:?}"
    );
}

// Keep the `Write` import meaningful in the test module (mirrors writer).
#[test]
fn write_hex8_is_zero_padded_uppercase() {
    // 0o100755 = regular-file bit (0o100000 = 0x8000) | 0o755 (0x1ED).
    use std::io::Write;
    let mut v = Vec::new();
    let _ = write!(v, "{:08X}", 0o100_755u32);
    assert_eq!(&v, b"000081ED");
    let mut h = Vec::new();
    crate::pack::cpio::write_hex8(&mut h, 0o100_755);
    assert_eq!(h, v, "write_hex8 matches std formatting");
}

// ── verify_pack: structural validation (no boot) ───────────────────────

/// Assemble a structurally-valid pack from a FAKE kernel (some bytes) plus
/// a real-ish init binary (an ELF-magic stand-in), returning the temp dir
/// (kept alive by the caller) and the pack `out` path.
fn assemble_good_pack() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let kernel = tmp.path().join("vmlinuz-fake");
    let init = tmp.path().join("lightr-init");
    // A FAKE kernel — verify_pack validates STRUCTURE, never boots it.
    std::fs::write(&kernel, b"FAKE-KERNEL-IMAGE-not-bootable").unwrap();
    // A real-ish init binary: ELF magic + a little payload.
    std::fs::write(&init, b"\x7fELF\x02\x01\x01\x00 lightr-init pid1 stand-in").unwrap();
    let out = tmp.path().join("pack-out");
    assemble_pack(&kernel, &init, &out, "aarch64", Some("6.12.0")).expect("assemble ok");
    (tmp, out)
}

#[test]
fn verify_pack_accepts_a_well_formed_pack() {
    let (_tmp, out) = assemble_good_pack();
    let info = verify_pack(&out).expect("a well-formed pack must verify");
    assert!(info.kernel_present, "kernel must be reported present");
    assert!(info.init_executable, "/init must be reported executable");
    assert_eq!(info.arch, "aarch64", "arch read from pack.json");
    assert_eq!(
        info.kernel_bytes,
        b"FAKE-KERNEL-IMAGE-not-bootable".len() as u64,
        "kernel_bytes reflects the real file size"
    );
}

#[test]
fn verify_pack_accepts_when_pack_json_absent() {
    let (_tmp, out) = assemble_good_pack();
    // Remove the optional manifest — verify must still accept, arch unknown.
    std::fs::remove_file(out.join("pack.json")).unwrap();
    let info = verify_pack(&out).expect("pack.json is optional");
    assert_eq!(info.arch, "unknown", "arch falls back to 'unknown'");
    assert!(info.init_executable);
}

#[test]
fn verify_pack_rejects_missing_kernel() {
    let (_tmp, out) = assemble_good_pack();
    std::fs::remove_file(out.join("kernel")).unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("missing kernel")),
        "missing kernel must be a clear InvalidManifest, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_empty_kernel() {
    let (_tmp, out) = assemble_good_pack();
    std::fs::write(out.join("kernel"), b"").unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("empty")),
        "empty kernel must be rejected, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_missing_initrd() {
    let (_tmp, out) = assemble_good_pack();
    std::fs::remove_file(out.join("initrd")).unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("missing initrd")),
        "missing initrd must be rejected, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_initrd_not_cpio() {
    let (_tmp, out) = assemble_good_pack();
    // Overwrite the initrd with non-cpio bytes (bad magic, but long enough).
    std::fs::write(out.join("initrd"), vec![b'X'; 256]).unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("newc cpio")),
        "a non-cpio initrd must be rejected, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_initrd_too_short() {
    let (_tmp, out) = assemble_good_pack();
    std::fs::write(out.join("initrd"), b"070701").unwrap(); // magic only, < 110
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("too short")),
        "a truncated initrd must be rejected, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_init_not_executable() {
    let (_tmp, out) = assemble_good_pack();
    // Hand-build an initrd whose first entry is /init but with a NON-exec
    // regular-file mode (0644), so the structural exec check must reject.
    let mut initrd = Vec::new();
    write_newc_entry(&mut initrd, "init", 0o100_644, b"non-exec init");
    write_newc_trailer(&mut initrd);
    std::fs::write(out.join("initrd"), &initrd).unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("not executable")),
        "a non-executable /init must be rejected, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_first_entry_not_init() {
    let (_tmp, out) = assemble_good_pack();
    // First entry is some other file, not /init.
    let mut initrd = Vec::new();
    write_newc_entry(&mut initrd, "bin/sh", MODE_EXEC_FILE, b"not init");
    write_newc_trailer(&mut initrd);
    std::fs::write(out.join("initrd"), &initrd).unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("expected /init")),
        "first entry other than /init must be rejected, got {err:?}"
    );
}

#[test]
fn verify_pack_rejects_malformed_pack_json() {
    let (_tmp, out) = assemble_good_pack();
    // Valid kernel + initrd, but a corrupt pack.json.
    std::fs::write(out.join("pack.json"), b"{ this is not json").unwrap();
    let err = verify_pack(&out).unwrap_err();
    assert!(
        matches!(&err, LightrError::InvalidManifest(m) if m.contains("pack.json") && m.contains("malformed")),
        "a malformed pack.json must be rejected, got {err:?}"
    );
}

#[test]
fn sha256_hex_matches_known_vector() {
    // SHA-256("") is the well-known empty-input digest.
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
