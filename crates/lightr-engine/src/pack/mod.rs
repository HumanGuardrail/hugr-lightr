//! Linux pack assembly (build-spec-prod §WP-B).
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
//! PACK: kernel from `scripts/build-kernel-x86.sh` (Linux bzImage with
//! virtio-pci/console/fs built in), or any bzImage/vmlinuz carrying virtiofs.
//! The guest reports its exit code through a **file channel** — PID1 writes the
//! code to `EXIT_FILE` on the rootfs virtiofs share and the host reads it back
//! (macOS has no host AF_VSOCK, so the old vsock receiver was removed as dead
//! code). `assemble_pack` copies whatever kernel file the caller hands it;
//! choosing/fetching that file is the install pipeline's job, not this one's.

pub mod cpio;

use lightr_core::{LightrError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use std::path::Path;

pub use cpio::{build_initrd_cpio, write_newc_entry, write_newc_trailer};

/// newc cpio magic ("new ASCII" format, the kernel initramfs format).
pub(crate) const NEWC_MAGIC: &[u8; 6] = b"070701";

/// Mode for a regular file, executable (`0100755`), as the cpio mode field.
pub(crate) const MODE_EXEC_FILE: u32 = 0o100_755;

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
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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
pub(crate) struct FirstEntry {
    pub(crate) name: String,
    pub(crate) mode: u32,
}

/// Parse the FIRST entry of a newc cpio archive (header + name), validating
/// the format enough to trust the entry. Returns a clear
/// [`LightrError::InvalidManifest`] when the bytes are not a well-formed newc
/// header. Does not read the file payload — only the header + name field.
pub(crate) fn parse_first_newc_entry(buf: &[u8]) -> Result<FirstEntry> {
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

#[cfg(test)]
mod tests;
