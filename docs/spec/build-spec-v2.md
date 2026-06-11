# HuGR Lightr — Build Spec v2 (R0: the warp core)

- **Status:** FROZEN under the owner overnight mandate 2026-06-11
  (`../decisions-log.md`); governed by ADRs 0009/0010/0012 + 0001/0004/
  0006/0007. Self-contained: **zero clw / zero network code in R0** (the
  wire arrives with ADR-0011 crates in R2+).
- Any deviation an implementing agent wants = BLOCKED + ask the lead.
  Agents transcribe; they do not decide.

## 1. R0 scope

`lightr snapshot | hydrate | status | run | bench | --version`, local-only,
native engine, macOS arm64 primary (Linux x86_64 if free). Out of R0:
views/mounts (R2, ADR-0013), vz/VMs (R2, ADR-0014), compose (R3), build
(R3), OCI (R2), wire (R4), `--events`, `plan`, MCP (R1+).

## 2. Workspace

```
Cargo.toml                 # workspace; resolver=2; edition 2021; publish=false; license UNLICENSED
rust-toolchain.toml        # 1.96.0 (ADR-0006; proxy verified at scaffold)
crates/lightr-core/        # WP-1: Digest, Manifest(+binary codec), RefRecord, errors
crates/lightr-store/       # WP-2: object plane, CoW ladder, refs, AC
crates/lightr-index/       # WP-3: stat-index + walk + snapshot/status/hydrate ops
crates/lightr-run/         # WP-4: memo key, native exec, replay
crates/lightr-cli/         # WP-5: bin `lightr` (verbs, --json, --explain, bench)
crates/lightr-acceptance/  # WP-6: A1–A8 end-to-end vs the built binary
```

Dependency rule (one-way): core ← store ← index ← run ← cli;
acceptance → binary only. **No other external crates than:** `blake3`
(+rayon feature), `rayon`, `ignore`, `libc`, `memmap2`, `clap` (derive,
cli only), `serde`+`serde_json` (cli only, `--json` output only),
`tempfile`+`assert_cmd` (dev-deps). Workspace-pinned versions.

## 3. FROZEN — `lightr-core`

```rust
pub const OUTPUT_CAP_BYTES: usize = 5 * 1024 * 1024;     // run stdout/stderr memo cap
pub const MANIFEST_MAGIC: &[u8; 4] = b"LMF1";

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);
impl Digest {
    pub fn of_bytes(data: &[u8]) -> Self;                 // BLAKE3
    pub fn of_file(path: &Path) -> Result<Self>;          // mmap + rayon for large
    pub fn to_hex(&self) -> String;
    pub fn from_hex(s: &str) -> Result<Self>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {                                          // file-LEVEL (ADR-0009)
    File { path: String, mode: u32, size: u64, digest: Digest },
    Symlink { path: String, target: String },
    Dir { path: String },                                 // empty dirs only
}
impl Entry { pub fn path(&self) -> &str; }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest { pub version: u32, pub total_size: u64, pub entries: Vec<Entry> }
impl Manifest {
    pub fn encode(&self) -> Vec<u8>;                      // LMF1: LE, path-sorted, no JSON
    pub fn decode(bytes: &[u8]) -> Result<Self>;
    pub fn digest(&self) -> Digest;                       // of encode() bytes
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefRecord {
    pub name: String, pub root: Digest, pub parent: Option<Digest>,
    pub created_at_unix: u64, pub tool_version: String,
}
impl RefRecord { pub fn encode(&self) -> Vec<u8>; pub fn decode(b: &[u8]) -> Result<Self>; }

pub fn validate_ref_name(name: &str) -> Result<()>;       // ADR-0004 grammar
pub fn ref_key(name: &str) -> Digest;                     // BLAKE3("lightr/ref/v1/" || name)

#[derive(Debug, thiserror::Error)]  // NO — thiserror not in dep list; hand-impl Display/Error
pub enum LightrError {
    NotFound(Digest), RefNotFound(String),
    Integrity { expected: Digest, actual: Digest },
    TooLarge { size: u64, cap: u64 }, InvalidRef(String),
    InvalidManifest(String), Io(std::io::Error),
}
pub type Result<T> = std::result::Result<T, LightrError>;
```
(`thiserror` correction: hand-implement `Display`/`Error` — dep list §2 is
the law. The enum shape above is frozen.)

LMF1 codec (frozen format): `LMF1` magic · u32 version=1 · u64 total_size ·
u32 entry_count · entries path-sorted, each: u8 kind (0=File,1=Symlink,
2=Dir) · u32 mode · u64 size · 32B digest (zeroed for non-File) · u16
path_len · path UTF-8 · (Symlink: u16 target_len · target). LE throughout.

## 4. FROZEN — `lightr-store`

Layout: `$LIGHTR_HOME/store/{objects,refs,ac}/<2hex>/<rest-hex>`;
`LIGHTR_HOME` default `~/.lightr` (env override; acceptance depends on it).

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CowRung { Clone, Reflink, CopyRange, Copy }      // probed at open (ADR-0009)

pub struct Store { /* root, rung */ }
impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self>;   // creates shards; probes rung
    pub fn default_root() -> PathBuf;                          // $LIGHTR_HOME/store
    pub fn rung(&self) -> CowRung;
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Digest>;  // atomic temp+rename; idempotent
    pub fn ingest_file(&self, path: &Path) -> Result<Digest>; // hash, then CoW INTO store when rung allows
    pub fn get_bytes(&self, d: &Digest) -> Result<Vec<u8>>;   // rehash; mismatch=Integrity, evidence kept
    pub fn exists(&self, d: &Digest) -> bool;
    pub fn materialize_file(&self, d: &Digest, dest: &Path, mode: u32) -> Result<()>; // CoW out + chmod
    pub fn ref_get(&self, name: &str) -> Result<Option<RefRecord>>;
    pub fn ref_put(&self, rec: &RefRecord) -> Result<()>;     // last-write-wins, atomic
    pub fn ac_get(&self, key: &Digest) -> Result<Option<Vec<u8>>>;
    pub fn ac_put(&self, key: &Digest, value: &[u8]) -> Result<()>;
}
```

Behavior law: objects chmod 0o444 after write · `materialize_file` rungs:
clonefile(dest)→FICLONE→copy_file_range→std copy, then set mode ·
corruption NEVER silently deleted · `ingest_file` reflink-into-store keeps
ingestion O(metadata) on Clone/Reflink rungs (falls back to copy).

## 5. FROZEN — `lightr-index` (ops layer)

Index file: `$LIGHTR_HOME/index/<blake3(root-abs-path)-hex>` — binary,
path-sorted: u8 kind · u32 mode · u64 size · u64 mtime_ns · u64 ino · 32B
digest · u16 path_len · path. Racily-clean rule per ADR-0010.

```rust
pub struct Index { /* entries, loaded mmap or owned */ }
impl Index {
    pub fn load_for(root: &Path) -> Result<Self>;          // empty if absent
    pub fn save_for(&self, root: &Path) -> Result<()>;     // atomic
}

pub struct WalkReport { pub manifest: Manifest, pub rehashed: u64, pub from_index: u64 }
pub fn scan(root: &Path, index: &mut Index) -> Result<WalkReport>;
//  parallel ignore-aware walk; stat-match → index digest; else rehash (rayon) + index update

pub struct SnapshotReport { pub root: Digest, pub files: u64, pub bytes_total: u64, pub objects_new: u64 }
pub fn snapshot(root: &Path, store: &Store, name: &str) -> Result<SnapshotReport>;
//  scan → ingest missing objects → put manifest → ref_put (parent = previous ref root)

pub struct HydrateReport { pub root: Digest, pub files: u64, pub bytes_total: u64, pub rung: CowRung }
pub fn hydrate(dest: &Path, store: &Store, name: &str) -> Result<HydrateReport>;
//  ref → manifest → mkdirs + parallel materialize_file + symlinks; dest must be empty or absent

pub struct StatusReport { pub clean: bool, pub added: Vec<String>, pub removed: Vec<String>, pub changed: Vec<String> }
pub fn status(root: &Path, store: &Store, name: &str) -> Result<StatusReport>;
//  scan vs ref manifest diff (path-sorted merge)
```

## 6. FROZEN — `lightr-run`

```rust
pub struct RunSpec {
    pub cwd: PathBuf, pub inputs: Vec<PathBuf>,            // empty ⇒ [cwd]
    pub command: Vec<String>, pub env_keys: Vec<String>,   // captured into key + child env
}
pub struct RunOutcome { pub key: Digest, pub hit: bool, pub exit_code: i32,
                        pub stdout: Vec<u8>, pub stderr: Vec<u8> }
pub fn run_memoized(spec: &RunSpec, store: &Store) -> Result<RunOutcome>;
```

Law: key = BLAKE3("lightr/run/v1" ‖ per-input manifest digests (via
`scan`, index-fast) ‖ argv ‖ sorted env KV ‖ target triple) · AC record
(binary: exit_code, stdout digest, stderr digest) + output objects ·
memoize **only** exit==0 AND both outputs ≤ `OUTPUT_CAP_BYTES` · replay
streams stored bytes, no exec · child env = parent env (passthrough) —
`env_keys` selects what enters the KEY, not what the child sees.

## 7. FROZEN — CLI (`lightr-cli`, bin `lightr`)

| Verb | Form | Exit |
|---|---|---|
| `snapshot` | `lightr snapshot [--dir .] --name <ref>` | 0 ok · 2 usage/invalid-ref · 1 error |
| `hydrate` | `lightr hydrate <dest> --name <ref>` | 0 · 2 ref-not-found/usage · 1 |
| `status` | `lightr status [--dir .] --name <ref>` | 0 clean · 1 dirty · 2 not-found/usage |
| `run` | `lightr run [--dir .] [--input <p>]… [--env <K>]… [--name-context <ref>] -- <cmd>…` | child's code · 2 usage |
| `bench` | `lightr bench [--vs-docker] [--json]` | 0 · 1 budget-fail (CI mode `--check`) |
| `--version` | prints `lightr <semver> (<rung-less>)` | 0 |

Global law: `--json` on every verb (stable keys; reports above serialize
1:1; serde only here) · `--explain` adds structured detail (memo key
composition, CoW rung, counts) to stderr · human output stays terse ·
stderr memo marker: `lightr: memo HIT key=<16hex>` / `MISS` · no config
files read in R0 · grammar errors exit 2 with one-line fix hint ·
`--help` states: "native execution — reproducibility, not a sandbox".

## 8. FROZEN — Acceptance (A1–A8, `lightr-acceptance`)

Every test: `LIGHTR_HOME` → per-test tempdir; never touches `~`. Fixture
tree generator shared: nested dirs, exec-bit file, symlink, empty dir,
~200 files incl. one ≥8 MiB.

- **A1 roundtrip** — snapshot → hydrate to fresh dir → recursive identical
  (bytes, modes, symlink targets, empty dirs).
- **A2 memo hit** — run twice (side-effect file outside inputs): 1 line in
  side-effect, 2nd run `HIT`, stdouts identical, exit 0.
- **A3 failure not memoized** — exit-7 cmd twice: 2 side-effect lines,
  both `MISS`, exit 7 both.
- **A4 no daemon** — `pgrep -x lightr` empty after all ops; `$LIGHTR_HOME`
  contains only plain files/dirs.
- **A5 status** — clean→0; touch a file→1 + names it; unknown ref→2.
- **A6 offline-structural** — A1+A2 under `HTTP_PROXY=http://127.0.0.1:9`
  + no network deps linked (compile-time fact, asserted in docs).
- **A7 integrity fail-closed** — flip 1 byte in an object: hydrate exits 1,
  stderr contains `integrity`, corrupt file still present.
- **A8 agent surface** — `--json` on all 4 verbs parses; keys stable;
  `run --json` carries `{key, hit, exit_code}`.

## 9. Budgets (release build; bench `--check` in CI)

B1 `--version` <5 ms · B2 memo-hit `run` ≤10 ms · B4 replay ≤10 ms ·
B6 status 10k warm-index ≤500 ms · B7 binary ≤10 MB · B8 zero processes
between invocations. Margins: ×3 on debug/CI-noise; medians-of-5 after 1
warmup.

**S4-calibrated (2026-06-11, `spikes/RESULTS.md` — this Intel dev box):**
per-file metadata ops cost ~2 ms here, so O(files) materialization budgets
bind to machine class: **B3′** hydrate 10k warm ≤5 s (parallel CoW) ·
**B5′** snapshot 10k ≤2.5 s cold-hash / ≤500 ms warm-index. The
whitepaper's ~ms materialization targets bind to **views (ADR-0013, R2)**
and Apple-Silicon hardware — S4 is the empirical justification for the
views layer — and stay unclaimed until the bench measures them there.

## 10. Wave partition (conflict map: CONFLICT-FREE)

| WP | Crate (owner-glob) | Model | Depends on |
|---|---|---|---|
| W0 scaffold | workspace + all skeletons w/ frozen sigs + `todo!()` | **lead** | — |
| WP-1 | `crates/lightr-core/**` | sonnet | §3 |
| WP-2 | `crates/lightr-store/**` | sonnet | §4 (+S4 numbers) |
| WP-3 | `crates/lightr-index/**` | sonnet | §5 |
| WP-4 | `crates/lightr-run/**` | sonnet | §6 |
| WP-5 | `crates/lightr-cli/**` | sonnet | §7 |
| WP-6 | `crates/lightr-acceptance/**` | sonnet | §8 |
| critic | suite coverage vs §8/MVP-DoD (read-only) | opus | post-WP-6 |

Lead-owned, agents must NOT touch: workspace `Cargo.toml`, `Cargo.lock`,
`rust-toolchain.toml`, `docs/**`, `README.md`, `CLAUDE.md`. Worktrees
under `.worktrees/` (gitignored), branch `wp/<id>`, merge order WP-1 →
2 → 3 → 4 → 5 → 6. Gates per WP: `cargo fmt --check` + `clippy -p <crate>
--all-targets -- -D warnings` + `cargo test -p <crate>`. Integration:
workspace fmt/clippy/test + A1–A8 green + post-flight sweep (no writes
outside owner-globs).
