//! AC record format "LRR1":
//!   4B magic b"LRR1"
//!   4B i32  exit_code  (LE)
//!  32B      stdout digest
//!  32B      stderr digest
//! Total: 72 bytes

use lightr_core::Digest;

pub(super) const AC_MAGIC: &[u8; 4] = b"LRR1";
pub(super) const AC_RECORD_LEN: usize = 4 + 4 + 32 + 32; // 72

pub(super) fn encode_ac_record(exit_code: i32, stdout_d: &Digest, stderr_d: &Digest) -> Vec<u8> {
    let mut buf = Vec::with_capacity(AC_RECORD_LEN);
    buf.extend_from_slice(AC_MAGIC);
    buf.extend_from_slice(&exit_code.to_le_bytes());
    buf.extend_from_slice(&stdout_d.0);
    buf.extend_from_slice(&stderr_d.0);
    buf
}

pub(super) fn decode_ac_record(bytes: &[u8]) -> Option<(i32, Digest, Digest)> {
    if bytes.len() != AC_RECORD_LEN {
        return None;
    }
    if &bytes[..4] != AC_MAGIC {
        return None;
    }
    let exit_code = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let mut stdout_raw = [0u8; 32];
    let mut stderr_raw = [0u8; 32];
    stdout_raw.copy_from_slice(&bytes[8..40]);
    stderr_raw.copy_from_slice(&bytes[40..72]);
    Some((exit_code, Digest(stdout_raw), Digest(stderr_raw)))
}
