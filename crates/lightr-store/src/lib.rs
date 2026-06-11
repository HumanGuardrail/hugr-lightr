//! lightr-store — frozen contract: build-spec v2 §4 (ADR-0009).
//! Object plane + refs + AC + CoW ladder. Bodies are WP-2.

use lightr_core::{Digest, LightrError, RefRecord, Result};
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::fs::{self, File, Permissions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CowRung {
    Clone,
    Reflink,
    CopyRange,
    Copy,
}

pub struct Store {
    root: PathBuf,
    rung: CowRung,
}

// ─────────────────────────────── helpers ────────────────────────────────────

/// Returns the two-char shard prefix and 62-char remainder from a Digest hex.
fn shard_parts(hex: &str) -> (&str, &str) {
    (&hex[..2], &hex[2..])
}

/// Object path: <root>/objects/<2hex>/<62hex>
fn object_path(root: &Path, d: &Digest) -> PathBuf {
    let hex = d.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("objects").join(pre).join(rest)
}

/// Ref path: <root>/refs/<2hex>/<62hex of ref_key digest>
fn ref_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("refs").join(pre).join(rest)
}

/// AC path: <root>/ac/<2hex>/<62hex>
fn ac_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("ac").join(pre).join(rest)
}

/// A cheap nonce for temp file names: PID + digest-hex-prefix + nanos.
fn temp_suffix(hint: &str) -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{pid}-{hint}-{nanos}")
}

/// Atomic write: write `data` to a temp file in `parent`, then rename to `dest`.
fn atomic_write(parent: &Path, dest: &Path, data: &[u8]) -> Result<()> {
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".tmp-{}", temp_suffix("w")));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
    }
    fs::rename(&tmp, dest)?;
    Ok(())
}

/// chmod a path to the given mode bits (unix only).
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))?;
    Ok(())
}

// ─────────────────────── CoW ladder (platform-gated) ────────────────────────

/// Probe: find the best CoW rung available under `root`.
/// Creates <root>/.probe-src, tries the ladder toward <root>/.probe-dst, cleans up both.
fn probe_rung(root: &Path) -> CowRung {
    let src = root.join(".probe-src");
    let dst = root.join(".probe-dst");

    // Write a tiny probe source file.
    if File::create(&src)
        .and_then(|mut f| f.write_all(b"probe"))
        .is_err()
    {
        return CowRung::Copy;
    }

    let rung = try_ladder_probe(&src, &dst);

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);

    rung
}

fn try_ladder_probe(src: &Path, dst: &Path) -> CowRung {
    #[cfg(target_os = "macos")]
    {
        let _ = fs::remove_file(dst);
        if cow_clone(src, dst).is_ok() {
            return CowRung::Clone;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = fs::remove_file(dst);
        if cow_reflink(src, dst).is_ok() {
            return CowRung::Reflink;
        }
        let _ = fs::remove_file(dst);
        if cow_copy_range(src, dst).is_ok() {
            return CowRung::CopyRange;
        }
    }

    CowRung::Copy
}

/// macOS: libc::clonefile(src, dst, 0)
#[cfg(target_os = "macos")]
fn cow_clone(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;

    let src_c = CString::new(src.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dst_c = CString::new(dst.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let ret = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Linux: ioctl FICLONE (0x40049409)
#[cfg(target_os = "linux")]
fn cow_reflink(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let src_file = File::open(src)?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    const FICLONE: libc::c_ulong = 0x40049409;
    let ret = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Linux: copy_file_range loop
#[cfg(target_os = "linux")]
fn cow_copy_range(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let src_file = File::open(src)?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    let src_len = src_file.metadata()?.len();
    let mut off_in: libc::loff_t = 0;
    let mut off_out: libc::loff_t = 0;
    let mut remaining = src_len as usize;

    while remaining > 0 {
        let n = unsafe {
            libc::copy_file_range(
                src_file.as_raw_fd(),
                &mut off_in as *mut libc::loff_t,
                dst_file.as_raw_fd(),
                &mut off_out as *mut libc::loff_t,
                remaining,
                0,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            break;
        }
        remaining -= n as usize;
    }
    Ok(())
}

/// Apply the CoW ladder to copy src→dst at the given rung; falls back to
/// std::fs::copy on failure (silent-correct, not silent-hidden — counted
/// at the call sites but not exposed in the API per R0 law).
fn cow_copy_file(src: &Path, dst: &Path, rung: CowRung) -> Result<()> {
    // Ensure parent exists.
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }

    match try_cow_at_rung(src, dst, rung) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Fall through to std::fs::copy (silently-correct per spec).
            let _ = fs::remove_file(dst); // remove partial if any
            fs::copy(src, dst)?;
            Ok(())
        }
    }
}

fn try_cow_at_rung(src: &Path, dst: &Path, rung: CowRung) -> std::io::Result<()> {
    match rung {
        #[cfg(target_os = "macos")]
        CowRung::Clone => cow_clone(src, dst),
        #[cfg(target_os = "linux")]
        CowRung::Reflink => cow_reflink(src, dst),
        #[cfg(target_os = "linux")]
        CowRung::CopyRange => cow_copy_range(src, dst),
        // CowRung::Copy (always available) or any rung on the wrong platform:
        _ => {
            fs::copy(src, dst)?;
            Ok(())
        }
    }
}

// ─────────────────────────────── Store impl ─────────────────────────────────

impl Store {
    /// Open (or create) a store at `root`.
    /// Creates objects/, refs/, ac/ top dirs lazily (shards created on write).
    /// Probes CoW rung.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root: PathBuf = root.into();
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("refs"))?;
        fs::create_dir_all(root.join("ac"))?;

        let rung = probe_rung(&root);

        Ok(Store { root, rung })
    }

    /// Default root: $LIGHTR_HOME/store  (LIGHTR_HOME defaults to ~/.lightr).
    pub fn default_root() -> PathBuf {
        if let Some(home) = std::env::var_os("LIGHTR_HOME") {
            PathBuf::from(home).join("store")
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".lightr").join("store")
        }
    }

    /// Return the probed CoW rung.
    pub fn rung(&self) -> CowRung {
        self.rung
    }

    /// Content-address `bytes` and store them.  Idempotent: if the object
    /// already exists the digest is returned immediately without any write.
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let d = Digest::of_bytes(bytes);
        let path = object_path(&self.root, &d);

        if path.exists() {
            return Ok(d);
        }

        let hex = d.to_hex();
        let (pre, _) = shard_parts(&hex);
        let shard = self.root.join("objects").join(pre);
        fs::create_dir_all(&shard)?;

        let tmp_name = format!(".tmp-{}", temp_suffix(&hex[..8]));
        let tmp = shard.join(tmp_name);
        {
            let mut f = File::create(&tmp)?;
            f.write_all(bytes)?;
            f.flush()?;
        }
        fs::rename(&tmp, &path)?;
        set_mode(&path, 0o444)?;

        Ok(d)
    }

    /// Hash `path` and CoW-clone it into the store.  Idempotent.
    pub fn ingest_file(&self, path: &Path) -> Result<Digest> {
        let d = Digest::of_file(path)?;
        let dest = object_path(&self.root, &d);

        if dest.exists() {
            return Ok(d);
        }

        let hex = d.to_hex();
        let (pre, _) = shard_parts(&hex);
        let shard = self.root.join("objects").join(pre);
        fs::create_dir_all(&shard)?;

        let tmp_name = format!(".tmp-{}", temp_suffix(&hex[..8]));
        let tmp = shard.join(tmp_name);

        // Try CoW into a temp, then rename+chmod.
        // On failure fall through to fs::copy.
        let used_cow = match try_cow_at_rung(path, &tmp, self.rung) {
            Ok(()) => true,
            Err(_) => {
                let _ = fs::remove_file(&tmp);
                fs::copy(path, &tmp)?;
                false
            }
        };
        let _ = used_cow; // counted but not surfaced in API

        fs::rename(&tmp, &dest)?;
        set_mode(&dest, 0o444)?;

        Ok(d)
    }

    /// Read and verify `d`.  Missing → NotFound.  Hash mismatch → Integrity
    /// (evidence file kept, never deleted).
    pub fn get_bytes(&self, d: &Digest) -> Result<Vec<u8>> {
        let path = object_path(&self.root, d);
        if !path.exists() {
            return Err(LightrError::NotFound(*d));
        }
        let bytes = fs::read(&path)?;
        let actual = Digest::of_bytes(&bytes);
        if actual != *d {
            return Err(LightrError::Integrity {
                expected: *d,
                actual,
            });
        }
        Ok(bytes)
    }

    /// Returns true iff the object file exists (no rehash).
    pub fn exists(&self, d: &Digest) -> bool {
        object_path(&self.root, d).exists()
    }

    /// CoW the object identified by `d` to `dest`, then set its mode to
    /// `mode`.  Missing object → NotFound.  Parent dirs created if absent.
    pub fn materialize_file(&self, d: &Digest, dest: &Path, mode: u32) -> Result<()> {
        let src = object_path(&self.root, d);
        if !src.exists() {
            return Err(LightrError::NotFound(*d));
        }

        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }

        // Remove any stale dest so clonefile can succeed (it fails if dst exists).
        let _ = fs::remove_file(dest);

        cow_copy_file(&src, dest, self.rung)?;

        // Always apply the manifest mode (clonefile carries 0o444 from the store).
        set_mode(dest, mode)?;

        Ok(())
    }

    /// Read a ref.  `name` is validated; absent → Ok(None).
    pub fn ref_get(&self, name: &str) -> Result<Option<RefRecord>> {
        lightr_core::validate_ref_name(name)?;
        let key = lightr_core::ref_key(name);
        let path = ref_path(&self.root, &key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let rec = RefRecord::decode(&bytes)?;
        Ok(Some(rec))
    }

    /// Write a ref atomically (last-write-wins).
    pub fn ref_put(&self, rec: &RefRecord) -> Result<()> {
        lightr_core::validate_ref_name(&rec.name)?;
        let key = lightr_core::ref_key(&rec.name);
        let path = ref_path(&self.root, &key);

        let hex = key.to_hex();
        let (pre, _) = shard_parts(&hex);
        let shard = self.root.join("refs").join(pre);

        let data = rec.encode();
        atomic_write(&shard, &path, &data)?;
        Ok(())
    }

    /// Read an AC entry.  Absent → Ok(None).
    pub fn ac_get(&self, key: &Digest) -> Result<Option<Vec<u8>>> {
        let path = ac_path(&self.root, key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        Ok(Some(bytes))
    }

    /// Write an AC entry atomically (overwrite via temp+rename).
    pub fn ac_put(&self, key: &Digest, value: &[u8]) -> Result<()> {
        let path = ac_path(&self.root, key);
        let hex = key.to_hex();
        let (pre, _) = shard_parts(&hex);
        let shard = self.root.join("ac").join(pre);
        atomic_write(&shard, &path, value)?;
        Ok(())
    }
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Open a fresh store in a temp dir.
    fn tmp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    // ── object plane ────────────────────────────────────────────────────────

    #[test]
    fn roundtrip_put_get() {
        let (_dir, store) = tmp_store();
        let data = b"hello, lightr!";
        let d = store.put_bytes(data).unwrap();
        let got = store.get_bytes(&d).unwrap();
        assert_eq!(&got[..], data);
    }

    #[test]
    fn idempotent_double_put() {
        let (_dir, store) = tmp_store();
        let data = b"idempotent data";
        let d1 = store.put_bytes(data).unwrap();
        let d2 = store.put_bytes(data).unwrap();
        assert_eq!(d1, d2);
        // Verify get still works.
        assert_eq!(store.get_bytes(&d1).unwrap(), data);
    }

    #[test]
    fn integrity_corruption() {
        let (_dir, store) = tmp_store();
        let data = b"tamper me";
        let d = store.put_bytes(data).unwrap();

        // Locate the object file, relax permissions, flip a byte, restore.
        let obj_path = object_path(&store.root, &d);
        fs::set_permissions(&obj_path, Permissions::from_mode(0o644)).unwrap();
        let mut bytes = fs::read(&obj_path).unwrap();
        bytes[0] ^= 0xFF;
        fs::write(&obj_path, &bytes).unwrap();
        fs::set_permissions(&obj_path, Permissions::from_mode(0o444)).unwrap();

        let err = store.get_bytes(&d).unwrap_err();
        match err {
            LightrError::Integrity { expected, actual } => {
                assert_eq!(expected, d);
                assert_ne!(actual, d);
            }
            other => panic!("expected Integrity, got {:?}", other),
        }

        // Evidence file must still be present (never deleted).
        assert!(
            obj_path.exists(),
            "evidence file was deleted — violates spec"
        );
    }

    #[test]
    fn notfound() {
        let (_dir, store) = tmp_store();
        // Construct a digest that was never stored.
        let d = Digest::of_bytes(b"never stored");
        let err = store.get_bytes(&d).unwrap_err();
        assert!(matches!(err, LightrError::NotFound(_)));
    }

    // ── materialize ─────────────────────────────────────────────────────────

    #[test]
    fn materialize_preserves_bytes_and_mode() {
        let (dir, store) = tmp_store();
        let data = b"file content for materialize";
        let d = store.put_bytes(data).unwrap();

        let dest = dir.path().join("out").join("materialized.txt");
        store.materialize_file(&d, &dest, 0o755).unwrap();

        let got = fs::read(&dest).unwrap();
        assert_eq!(&got[..], data);

        let meta = fs::metadata(&dest).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "mode mismatch: got {mode:o}");
    }

    #[test]
    fn materialize_notfound() {
        let (dir, store) = tmp_store();
        let d = Digest::of_bytes(b"not in store");
        let dest = dir.path().join("x");
        let err = store.materialize_file(&d, &dest, 0o644).unwrap_err();
        assert!(matches!(err, LightrError::NotFound(_)));
    }

    // ── rung ─────────────────────────────────────────────────────────────────

    #[test]
    fn rung_returns_probed_value() {
        let (_dir, store) = tmp_store();
        // Just assert it's a valid CowRung variant — the value is machine-dependent.
        let r = store.rung();
        let valid = matches!(
            r,
            CowRung::Clone | CowRung::Reflink | CowRung::CopyRange | CowRung::Copy
        );
        assert!(valid);
    }

    // ── refs ─────────────────────────────────────────────────────────────────

    fn make_ref_record(name: &str) -> RefRecord {
        RefRecord {
            name: name.to_string(),
            root: Digest::of_bytes(name.as_bytes()),
            parent: None,
            created_at_unix: 1_700_000_000,
            tool_version: "0.1.0".to_string(),
        }
    }

    #[test]
    fn ref_roundtrip() {
        let (_dir, store) = tmp_store();
        let rec = make_ref_record("main");

        store.ref_put(&rec).unwrap();
        let got = store.ref_get("main").unwrap();
        assert!(got.is_some());
        let got = got.unwrap();
        assert_eq!(got.name, rec.name);
        assert_eq!(got.root, rec.root);
        assert_eq!(got.created_at_unix, rec.created_at_unix);
    }

    #[test]
    fn ref_last_write_wins() {
        let (_dir, store) = tmp_store();
        let rec1 = make_ref_record("dev");
        let mut rec2 = make_ref_record("dev");
        rec2.root = Digest::of_bytes(b"second root");

        store.ref_put(&rec1).unwrap();
        store.ref_put(&rec2).unwrap();

        let got = store.ref_get("dev").unwrap().unwrap();
        assert_eq!(got.root, rec2.root, "last-write-wins violated");
    }

    #[test]
    fn ref_absent_returns_none() {
        let (_dir, store) = tmp_store();
        let got = store.ref_get("nonexistent").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn ref_invalid_name_rejected() {
        let (_dir, store) = tmp_store();
        let rec = RefRecord {
            name: "INVALID NAME WITH SPACES".to_string(),
            root: Digest::of_bytes(b"x"),
            parent: None,
            created_at_unix: 0,
            tool_version: "0.1.0".to_string(),
        };
        let put_err = store.ref_put(&rec).unwrap_err();
        assert!(matches!(put_err, LightrError::InvalidRef(_)));
        let get_err = store.ref_get("INVALID NAME WITH SPACES").unwrap_err();
        assert!(matches!(get_err, LightrError::InvalidRef(_)));
    }

    // ── ac ───────────────────────────────────────────────────────────────────

    #[test]
    fn ac_roundtrip() {
        let (_dir, store) = tmp_store();
        let key = Digest::of_bytes(b"ac-key");
        let val = b"ac-value-bytes";

        store.ac_put(&key, val).unwrap();
        let got = store.ac_get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(val.as_slice()));
    }

    #[test]
    fn ac_overwrite() {
        let (_dir, store) = tmp_store();
        let key = Digest::of_bytes(b"ac-key-2");

        store.ac_put(&key, b"first").unwrap();
        store.ac_put(&key, b"second").unwrap();
        let got = store.ac_get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(b"second".as_slice()));
    }

    #[test]
    fn ac_absent_returns_none() {
        let (_dir, store) = tmp_store();
        let key = Digest::of_bytes(b"never-put");
        assert!(store.ac_get(&key).unwrap().is_none());
    }

    // ── default_root ─────────────────────────────────────────────────────────

    // Env vars are process-global; these two tests mutate LIGHTR_HOME and
    // race under the parallel test runner — serialize them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn default_root_honors_lightr_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let orig = std::env::var_os("LIGHTR_HOME");
        std::env::set_var("LIGHTR_HOME", "/tmp/custom-lightr-home");
        let root = Store::default_root();
        // Restore before any assert so we don't leave env dirty on failure.
        match orig {
            Some(v) => std::env::set_var("LIGHTR_HOME", v),
            None => std::env::remove_var("LIGHTR_HOME"),
        }
        assert_eq!(root, PathBuf::from("/tmp/custom-lightr-home/store"));
    }

    #[test]
    fn default_root_fallback_to_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let orig_lightr = std::env::var_os("LIGHTR_HOME");
        std::env::remove_var("LIGHTR_HOME");
        let root = Store::default_root();
        match orig_lightr {
            Some(v) => std::env::set_var("LIGHTR_HOME", v),
            None => std::env::remove_var("LIGHTR_HOME"),
        }
        // Must end with .lightr/store
        assert!(
            root.ends_with(".lightr/store"),
            "expected path ending in .lightr/store, got {:?}",
            root
        );
    }

    // ── ingest_file ──────────────────────────────────────────────────────────

    #[test]
    fn ingest_file_roundtrip() {
        let (dir, store) = tmp_store();
        let src = dir.path().join("input.txt");
        let data = b"ingest this file";
        fs::write(&src, data).unwrap();

        let d = store.ingest_file(&src).unwrap();

        // Object must be readable and correct.
        let got = store.get_bytes(&d).unwrap();
        assert_eq!(&got[..], data);
    }

    #[test]
    fn ingest_file_idempotent() {
        let (dir, store) = tmp_store();
        let src = dir.path().join("idem.txt");
        fs::write(&src, b"idempotent ingest").unwrap();

        let d1 = store.ingest_file(&src).unwrap();
        let d2 = store.ingest_file(&src).unwrap();
        assert_eq!(d1, d2);
    }
}
