//! Linux pack assembly (build-spec-prod §WP-B-vsock).
//!
//! A "pack" is what the vz engine boots: a Linux `kernel` plus an `initrd`
//! whose `/init` is the `lightr-init` PID1 binary (see `crates/lightr-init`).
//! [`assemble_pack`] lays both out in an output directory so that
//! `lightr engine install-pack <out_dir>` has something real to install.
//!
//! The initrd is a freshly built **newc cpio** archive (the format the Linux
//! kernel unpacks as an initramfs) carrying the init binary as `/init`. The
//! cpio writer here is hand-rolled — roughly 80 lines, no new dependencies —
//! and is fully tested by parsing the archive back (see the tests at the foot
//! of this module).
//!
//! ## What is real vs. documented
//!
//! The cpio assembly is REAL and tested. Sourcing the actual Linux **kernel**
//! image is out of scope for this WP and stays a documented step:
//!
//! PACK: kernel from Apple's Containerization kernel (the vmlinuz shipped with
//! `apple/containerization`), or any bzImage/vmlinuz with virtiofs + AF_VSOCK
//! built in. `assemble_pack` copies whatever kernel file the caller hands it;
//! choosing/fetching that file is the install pipeline's job, not this one's.

use lightr_core::{LightrError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use std::io::Write;
use std::path::Path;

/// newc cpio magic ("new ASCII" format, the kernel initramfs format).
const NEWC_MAGIC: &[u8; 6] = b"070701";

/// Mode for a regular file, executable (`0100755`), as the cpio mode field.
const MODE_EXEC_FILE: u32 = 0o100_755;

/// `S_IFMT` mask isolating the file-type bits of a cpio/POSIX mode.
const S_IFMT: u32 = 0o170_000;
/// `S_IFREG` — regular-file type bits.
const S_IFREG: u32 = 0o100_000;
/// Any-execute permission bits (owner|group|other).
const ANY_EXEC_BITS: u32 = 0o111;

/// Structured view of a pack's manifest, emitted as `pack.json` alongside the
/// `kernel` + `initrd` so a consumer (e.g. `lightr engine install-pack`) and
/// the build recipe can both report what was produced.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackManifest {
    /// Guest architecture the pack targets (e.g. `aarch64`, `x86_64`).
    pub arch: String,
    /// Kernel version string when known (e.g. `6.12.0`); `None` if the build
    /// recipe could not determine it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_version: Option<String>,
    /// Lowercase-hex SHA-256 of the init binary embedded as `/init`.
    pub init_sha256: String,
}

/// Result of [`verify_pack`]: a pack's structural facts, gathered without
/// booting anything. Returned only when the pack is structurally valid; a
/// malformed pack yields a [`LightrError::InvalidManifest`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackInfo {
    /// Architecture from `pack.json` if present, else `"unknown"`.
    pub arch: String,
    /// A non-empty `kernel` file exists (always true on success).
    pub kernel_present: bool,
    /// The initrd's first entry is `/init` with an executable mode.
    pub init_executable: bool,
    /// Size of the `kernel` file in bytes.
    pub kernel_bytes: u64,
}

/// Assemble a linux pack into `out_dir`:
///   - `out_dir/kernel` — a copy of `kernel`.
///   - `out_dir/initrd` — a newc cpio archive whose first entry is `/init`,
///     holding the bytes of `init_bin`.
///   - `out_dir/pack.json` — a [`PackManifest`] (arch, optional kernel
///     version, init SHA-256).
///
/// `out_dir` is created if absent. Both inputs must exist and be readable.
pub fn assemble_pack(
    kernel: &Path,
    init_bin: &Path,
    out_dir: &Path,
    arch: &str,
    kernel_version: Option<&str>,
) -> Result<()> {
    std::fs::create_dir_all(out_dir).map_err(LightrError::Io)?;

    // PACK: kernel from Apple's Containerization kernel (see module doc). Here
    // we copy whatever kernel file the caller selected — sourcing it is the
    // install pipeline's concern, not assembly's.
    let kernel_dst = out_dir.join("kernel");
    std::fs::copy(kernel, &kernel_dst).map_err(LightrError::Io)?;

    // Build the initrd: a newc cpio with the init binary as /init.
    let init_bytes = std::fs::read(init_bin).map_err(LightrError::Io)?;
    let initrd = build_initrd_cpio(&init_bytes);
    let initrd_dst = out_dir.join("initrd");
    std::fs::write(&initrd_dst, &initrd).map_err(LightrError::Io)?;

    // Emit pack.json: arch + optional kernel version + init digest. The digest
    // is over the init binary's bytes (the `/init` payload), so a consumer can
    // confirm the initrd carries the init it expects.
    let manifest = PackManifest {
        arch: arch.to_string(),
        kernel_version: kernel_version.map(str::to_string),
        init_sha256: sha256_hex(&init_bytes),
    };
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| LightrError::InvalidManifest(format!("pack.json serialize: {e}")))?;
    std::fs::write(out_dir.join("pack.json"), json.as_bytes()).map_err(LightrError::Io)?;

    Ok(())
}

/// Validate a pack directory's STRUCTURE (no boot). Checks, in order:
///   1. `kernel` is present and non-empty.
///   2. `initrd` is present and is a valid newc cpio whose FIRST entry is
///      `/init` (the path the kernel execs) with an executable, regular-file
///      mode.
///   3. `pack.json`, if present, parses as a [`PackManifest`].
///
/// On success returns a [`PackInfo`]. Any structural problem surfaces as a
/// [`LightrError::InvalidManifest`] naming what is wrong.
pub fn verify_pack(dir: &Path) -> Result<PackInfo> {
    // ── 1. kernel present + non-empty ──────────────────────────────────────
    let kernel = dir.join("kernel");
    let kernel_meta = std::fs::metadata(&kernel).map_err(|_| {
        LightrError::InvalidManifest(format!("missing kernel file at {}", kernel.display()))
    })?;
    let kernel_bytes = kernel_meta.len();
    if kernel_bytes == 0 {
        return Err(LightrError::InvalidManifest(format!(
            "kernel file is empty at {}",
            kernel.display()
        )));
    }

    // ── 2. initrd present + valid cpio + first entry is executable /init ────
    let initrd_path = dir.join("initrd");
    let initrd = std::fs::read(&initrd_path).map_err(|_| {
        LightrError::InvalidManifest(format!("missing initrd file at {}", initrd_path.display()))
    })?;
    let first = parse_first_newc_entry(&initrd)?;
    // The kernel execs `/init`; cpio names are root-relative, so the on-disk
    // entry name is "init" (no leading slash). Accept either spelling.
    if first.name != "init" && first.name != "/init" {
        return Err(LightrError::InvalidManifest(format!(
            "initrd first entry is {:?}, expected /init",
            first.name
        )));
    }
    if first.mode & S_IFMT != S_IFREG {
        return Err(LightrError::InvalidManifest(format!(
            "initrd /init is not a regular file (mode {:#o})",
            first.mode
        )));
    }
    let init_executable = first.mode & ANY_EXEC_BITS != 0;
    if !init_executable {
        return Err(LightrError::InvalidManifest(format!(
            "initrd /init is not executable (mode {:#o})",
            first.mode
        )));
    }

    // ── 3. pack.json (if present) parses ───────────────────────────────────
    let manifest_path = dir.join("pack.json");
    let arch = if manifest_path.exists() {
        let bytes = std::fs::read(&manifest_path).map_err(LightrError::Io)?;
        let manifest: PackManifest = serde_json::from_slice(&bytes).map_err(|e| {
            LightrError::InvalidManifest(format!(
                "pack.json at {} is malformed: {e}",
                manifest_path.display()
            ))
        })?;
        manifest.arch
    } else {
        "unknown".to_string()
    };

    Ok(PackInfo {
        arch,
        kernel_present: true,
        init_executable,
        kernel_bytes,
    })
}

/// Lowercase-hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    // 32 bytes → 64 hex chars; build directly (the module's `Write` import is
    // `io::Write`, for the cpio Vec writer — using a nibble table here keeps
    // both `Write` traits out of the way and is allocation-light).
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Minimal facts about a parsed newc entry — what [`verify_pack`] needs.
struct FirstEntry {
    name: String,
    mode: u32,
}

/// Parse the FIRST entry of a newc cpio archive (header + name), validating
/// the format enough to trust the entry. Returns a clear
/// [`LightrError::InvalidManifest`] when the bytes are not a well-formed newc
/// header. Does not read the file payload — only the header + name field.
fn parse_first_newc_entry(buf: &[u8]) -> Result<FirstEntry> {
    // newc header is a fixed 110 bytes: 6-byte magic + 13 eight-hex fields.
    if buf.len() < 110 {
        return Err(LightrError::InvalidManifest(
            "initrd is too short to be a cpio archive (< 110 bytes)".to_string(),
        ));
    }
    if &buf[0..6] != NEWC_MAGIC {
        return Err(LightrError::InvalidManifest(format!(
            "initrd is not a newc cpio archive (bad magic: {:?})",
            String::from_utf8_lossy(&buf[0..6])
        )));
    }
    // Field i (0-based, after the 6-byte magic) is 8 ASCII hex chars.
    let field = |i: usize| -> Result<u32> {
        let start = 6 + i * 8;
        let raw = &buf[start..start + 8];
        let s = std::str::from_utf8(raw).map_err(|_| {
            LightrError::InvalidManifest("non-ASCII byte in cpio header field".to_string())
        })?;
        u32::from_str_radix(s, 16)
            .map_err(|_| LightrError::InvalidManifest(format!("non-hex cpio header field: {s:?}")))
    };
    let mode = field(1)?; // field order: ino(0), mode(1), ...
    let namesize = field(11)? as usize; // ..., namesize(11), check(12)
    if namesize == 0 {
        return Err(LightrError::InvalidManifest(
            "cpio entry has zero-length name".to_string(),
        ));
    }
    let name_start: usize = 110;
    let name_end = name_start
        .checked_add(namesize)
        .filter(|&e| e <= buf.len())
        .ok_or_else(|| {
            LightrError::InvalidManifest("cpio name field runs past end of archive".to_string())
        })?;
    // namesize includes the trailing NUL; strip it.
    let name = std::str::from_utf8(&buf[name_start..name_end - 1])
        .map_err(|_| LightrError::InvalidManifest("non-UTF8 cpio entry name".to_string()))?
        .to_string();

    Ok(FirstEntry { name, mode })
}

/// Build a minimal newc cpio initramfs containing exactly one entry — `init`
/// (i.e. `/init`, the path the kernel execs) — followed by the format's
/// `TRAILER!!!` end marker.
pub fn build_initrd_cpio(init_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // The kernel execs `/init`; the cpio entry name is the leading-slash-less
    // "init" (newc names are relative to the unpacked root).
    write_newc_entry(&mut out, "init", MODE_EXEC_FILE, init_bytes);
    write_newc_trailer(&mut out);
    out
}

/// Write one newc cpio entry: a 110-byte ASCII header, the NUL-terminated
/// name padded to a 4-byte boundary, then the file data padded likewise.
fn write_newc_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
    // namesize counts the trailing NUL.
    let name_bytes = name.as_bytes();
    let namesize = name_bytes.len() as u32 + 1;

    write_newc_header(out, mode, data.len() as u32, namesize);

    // Name + NUL, padded so the *data* starts on a 4-byte boundary. The pad is
    // measured from the start of the header (110) + namesize.
    out.extend_from_slice(name_bytes);
    out.push(0);
    pad4_from(out, 110 + namesize as usize);

    // Data, padded to a 4-byte boundary.
    out.extend_from_slice(data);
    pad4(out, data.len());
}

/// Write the newc `TRAILER!!!` entry that terminates the archive (mode 0,
/// empty data, name = "TRAILER!!!"). It is just a normal entry with no data.
fn write_newc_trailer(out: &mut Vec<u8>) {
    write_newc_entry(out, "TRAILER!!!", 0, &[]);
}

/// Write the fixed 110-byte newc header: 6-byte magic + 13 eight-hex fields.
///
/// Field order (newc): ino, mode, uid, gid, nlink, mtime, filesize, devmajor,
/// devminor, rdevmajor, rdevminor, namesize, check.
fn write_newc_header(out: &mut Vec<u8>, mode: u32, filesize: u32, namesize: u32) {
    out.extend_from_slice(NEWC_MAGIC);
    let fields = [
        0,        // ino   — 0 is fine for an initramfs
        mode,     // mode
        0,        // uid   — root
        0,        // gid   — root
        1,        // nlink — 1 for a regular file / trailer
        0,        // mtime — deterministic build: 0
        filesize, // filesize
        0,        // devmajor
        0,        // devminor
        0,        // rdevmajor
        0,        // rdevminor
        namesize, // namesize (includes trailing NUL)
        0,        // check — unused for newc (the "crc" variant is 070702)
    ];
    for f in fields {
        write_hex8(out, f);
    }
}

/// Append `value` as exactly 8 uppercase ASCII hex digits, zero-padded.
fn write_hex8(out: &mut Vec<u8>, value: u32) {
    // `write!` to a Vec<u8> never fails.
    let _ = write!(out, "{value:08X}");
}

/// Pad `out` with NUL bytes so its length is a multiple of 4, given that
/// `data_len` bytes of content were just appended at a 4-aligned offset.
fn pad4(out: &mut Vec<u8>, data_len: usize) {
    let rem = data_len % 4;
    if rem != 0 {
        for _ in 0..(4 - rem) {
            out.push(0);
        }
    }
}

/// Pad `out` to a 4-byte boundary measured from `absolute_offset` (used after
/// the name, whose alignment is reckoned from the start of the header).
fn pad4_from(out: &mut Vec<u8>, absolute_offset: usize) {
    let rem = absolute_offset % 4;
    if rem != 0 {
        for _ in 0..(4 - rem) {
            out.push(0);
        }
    }
}

#[cfg(test)]
mod tests {
    // `Write` (for the `write!` in write_hex8_is_zero_padded_uppercase) comes in
    // via `super::*` — the module imports `std::io::Write` at its top.
    use super::*;

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
        let mut v = Vec::new();
        let _ = write!(v, "{:08X}", 0o100_755u32);
        assert_eq!(&v, b"000081ED");
        let mut h = Vec::new();
        super::write_hex8(&mut h, 0o100_755);
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
}
