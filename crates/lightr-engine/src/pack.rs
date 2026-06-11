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
use std::io::Write;
use std::path::Path;

/// newc cpio magic ("new ASCII" format, the kernel initramfs format).
const NEWC_MAGIC: &[u8; 6] = b"070701";

/// Mode for a regular file, executable (`0100755`), as the cpio mode field.
const MODE_EXEC_FILE: u32 = 0o100_755;

/// Assemble a linux pack into `out_dir`:
///   - `out_dir/kernel` — a copy of `kernel`.
///   - `out_dir/initrd` — a newc cpio archive whose first entry is `/init`,
///     holding the bytes of `init_bin`.
///
/// `out_dir` is created if absent. Both inputs must exist and be readable.
pub fn assemble_pack(kernel: &Path, init_bin: &Path, out_dir: &Path) -> Result<()> {
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

    Ok(())
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

        assemble_pack(&kernel, &init, &out).expect("assemble_pack ok");

        // kernel copied verbatim.
        let got_kernel = std::fs::read(out.join("kernel")).unwrap();
        assert_eq!(got_kernel, kernel_bytes, "kernel copied byte-for-byte");

        // initrd parses, first entry is /init carrying the init bytes.
        let initrd = std::fs::read(out.join("initrd")).unwrap();
        let entries = parse_newc(&initrd);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "init");
        assert_eq!(entries[0].data, init_bytes, "/init holds the init binary");
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
        assemble_pack(&kernel, &init, &out).expect("creates nested out_dir");
        assert!(out.join("kernel").exists());
        assert!(out.join("initrd").exists());
    }

    #[test]
    fn assemble_pack_missing_init_is_err() {
        let tmp = tempfile::tempdir().unwrap();
        let kernel = tmp.path().join("k");
        std::fs::write(&kernel, b"k").unwrap();
        let missing_init = tmp.path().join("does-not-exist");
        let out = tmp.path().join("out");
        let err = assemble_pack(&kernel, &missing_init, &out).unwrap_err();
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
}
