//! Length-prefixed binary codec for `ImageManifestRecord` (R-IMGREC) — split
//! out of imgmeta.rs for the 400-LOC godfile cap. Wire layout: see imgmeta.rs.

use super::{ImageDescriptor, ImageManifestRecord};
use lightr_core::{Digest, LightrError, Result};

const IMG_MANIFEST_CODEC_VERSION: u32 = 1;

pub(super) fn encode_manifest_record(rec: &ImageManifestRecord) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&IMG_MANIFEST_CODEC_VERSION.to_le_bytes());
    out.extend_from_slice(&(rec.manifest_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&rec.manifest_bytes);
    out.extend_from_slice(&(rec.platform.len() as u32).to_le_bytes());
    out.extend_from_slice(rec.platform.as_bytes());
    out.extend_from_slice(&(rec.descriptors.len() as u32).to_le_bytes());
    for d in &rec.descriptors {
        out.extend_from_slice(&(d.media_type.len() as u32).to_le_bytes());
        out.extend_from_slice(d.media_type.as_bytes());
        out.extend_from_slice(&d.digest.0);
        out.extend_from_slice(&d.size.to_le_bytes());
    }
    out
}

/// A bounds-checked cursor reader for the length-prefixed codec.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| {
                LightrError::InvalidManifest("image manifest record truncated".into())
            })?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_le_bytes(a))
    }
    fn digest(&mut self) -> Result<Digest> {
        let s = self.take(32)?;
        let mut a = [0u8; 32];
        a.copy_from_slice(s);
        Ok(Digest(a))
    }
    fn string(&mut self, n: usize) -> Result<String> {
        let s = self.take(n)?;
        String::from_utf8(s.to_vec())
            .map_err(|_| LightrError::InvalidManifest("non-UTF8 in image manifest record".into()))
    }
}

pub(super) fn decode_manifest_record(bytes: &[u8]) -> Result<ImageManifestRecord> {
    let mut r = Reader::new(bytes);
    let version = r.u32()?;
    if version != IMG_MANIFEST_CODEC_VERSION {
        return Err(LightrError::InvalidManifest(format!(
            "unknown image manifest record version: {version}"
        )));
    }
    let mlen = r.u64()? as usize;
    let manifest_bytes = r.take(mlen)?.to_vec();
    let plen = r.u32()? as usize;
    let platform = r.string(plen)?;
    let n = r.u32()? as usize;
    let mut descriptors = Vec::with_capacity(n);
    for _ in 0..n {
        let mtlen = r.u32()? as usize;
        let media_type = r.string(mtlen)?;
        let digest = r.digest()?;
        let size = r.u64()?;
        descriptors.push(ImageDescriptor {
            media_type,
            digest,
            size,
        });
    }
    Ok(ImageManifestRecord {
        manifest_bytes,
        descriptors,
        platform,
    })
}
