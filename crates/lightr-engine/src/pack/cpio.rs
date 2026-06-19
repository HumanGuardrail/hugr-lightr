//! newc cpio writer — the low-level byte machinery for building initramfs archives.
//!
//! This module contains the byte-level cpio writer used by [`super::build_initrd_cpio`].
//! All functions here are internal to the pack crate; the public surface is
//! `build_initrd_cpio` re-exported from `pack/mod.rs`.

use std::io::Write;

use super::{MODE_EXEC_FILE, NEWC_MAGIC};

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
pub fn write_newc_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
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
pub fn write_newc_trailer(out: &mut Vec<u8>) {
    write_newc_entry(out, "TRAILER!!!", 0, &[]);
}

/// Write the fixed 110-byte newc header: 6-byte magic + 13 eight-hex fields.
///
/// Field order (newc): ino, mode, uid, gid, nlink, mtime, filesize, devmajor,
/// devminor, rdevmajor, rdevminor, namesize, check.
pub fn write_newc_header(out: &mut Vec<u8>, mode: u32, filesize: u32, namesize: u32) {
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
pub fn write_hex8(out: &mut Vec<u8>, value: u32) {
    // `write!` to a Vec<u8> never fails.
    let _ = write!(out, "{value:08X}");
}

/// Pad `out` with NUL bytes so its length is a multiple of 4, given that
/// `data_len` bytes of content were just appended at a 4-aligned offset.
pub fn pad4(out: &mut Vec<u8>, data_len: usize) {
    let rem = data_len % 4;
    if rem != 0 {
        for _ in 0..(4 - rem) {
            out.push(0);
        }
    }
}

/// Pad `out` to a 4-byte boundary measured from `absolute_offset` (used after
/// the name, whose alignment is reckoned from the start of the header).
pub fn pad4_from(out: &mut Vec<u8>, absolute_offset: usize) {
    let rem = absolute_offset % 4;
    if rem != 0 {
        for _ in 0..(4 - rem) {
            out.push(0);
        }
    }
}
