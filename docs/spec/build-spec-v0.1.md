# HuGR Cell — Build Spec v0.1 (freeze candidate)

- **Status:** Draft — becomes FROZEN when ADRs 0001–0007 are Accepted; a
  contract hash is then recorded here and any change requires owner sign-off.
- **Baseline facts:** clw consumed at `corelink-workspaces @ f8f5edf` (clean
  tree, verified 2026-06-11). All clw signatures below were extracted
  verbatim from that baseline.
- **Governing docs:** `docs/MVP-v0.1.md` (scope/DoD) · ADRs 0001–0008 ·
  whitepaper §9 principles.

## 1. Scope (one sprint)

`cell snapshot | hydrate | status | run` — local-only, native execution,
macOS arm64 (Linux x86_64 if free). Out: microVMs, namespaces, OCI import,
remote/auth/teams, any corelink-server change. (Full list: MVP doc.)

## 2. Workspace (ADR-0001, ADR-0002, ADR-0006)

```
Cargo.toml                # workspace: resolver 2, edition 2021, publish=false, license UNLICENSED
rust-toolchain.toml       # channel 1.96.0 (scaffold-time proxy verification per ADR-0006)
crates/cell-store/        # WP-1
crates/cell-cli/          # WP-2
crates/cell-acceptance/   # WP-3
```

Path-deps (read-only sibling):
`clw-types`, `clw-cache`, `clw-snapshot`, `clw-hydrate`, `clw-run`,
`clw-manifest` = `{ path = "../corelink-workspaces/crates/<name>" }`.

External deps (workspace-pinned): `tokio` (rt-multi-thread, macros),
`clap` (derive), `async-trait` (trait impls), `anyhow` (cli error surface),
`tempfile` + `assert_cmd` (dev/acceptance). No others without spec change.

## 3. FROZEN — `cell-store` public API (código-âncora)

```rust
// crates/cell-store/src/lib.rs — public surface, verbatim target
use std::path::PathBuf;
use clw_types::{AcTransport, CasTransport, Digest, Result};

/// Local CAS+AC store: the Stage-1 source of truth. Fail-closed (ADR-0003).
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    /// Open (or create) a store rooted at `root`.
    /// Creates `<root>/cas/<00..ff>/` and `<root>/ac/<00..ff>/` shard dirs.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self>;

    /// Resolution order: explicit arg → $CELL_STORE_DIR → ~/.cell/store
    pub fn default_root() -> PathBuf;
}

#[async_trait::async_trait]
impl CasTransport for LocalStore {
    async fn exists(&self, digest: &Digest) -> Result<bool>;
    /// Missing → ClwError::NotFound(d). Corrupt (rehash mismatch) →
    /// ClwError::Integrity { expected, actual } — file is NOT deleted.
    async fn get(&self, digest: &Digest) -> Result<Vec<u8>>;
    /// len > CAS_BLOB_CAP_BYTES → ClwError::TooLarge. Idempotent.
    /// Atomic: temp file + rename within the shard dir.
    async fn put(&self, digest: &Digest, bytes: Vec<u8>) -> Result<()>;
}

#[async_trait::async_trait]
impl AcTransport for LocalStore {
    /// Absent → Ok(None).
    async fn get(&self, key: &Digest) -> Result<Option<Vec<u8>>>;
    /// Last-write-wins overwrite. Atomic (temp + rename).
    async fn put(&self, key: &Digest, value: Vec<u8>) -> Result<()>;
}
```

Layout on disk: `<root>/cas/<first-2-hex>/<full-64-hex>` and
`<root>/ac/<first-2-hex>/<full-64-hex>` (file content = raw bytes).
The clw trait contracts this implements (extracted verbatim from
`clw-types @ f8f5edf`):

```rust
#[async_trait::async_trait]
pub trait CasTransport: Send + Sync {
    async fn exists(&self, digest: &Digest) -> Result<bool>;
    async fn get(&self, digest: &Digest) -> Result<Vec<u8>>;
    /// `bytes.len()` MUST be ≤ `CAS_BLOB_CAP_BYTES` (else `ClwError::TooLarge`).
    async fn put(&self, digest: &Digest, bytes: Vec<u8>) -> Result<()>;
}
#[async_trait::async_trait]
pub trait AcTransport: Send + Sync {
    async fn get(&self, key: &Digest) -> Result<Option<Vec<u8>>>;
    async fn put(&self, key: &Digest, value: Vec<u8>) -> Result<()>;
}
// pub const CAS_BLOB_CAP_BYTES: usize = 5 * 1024 * 1024;
```

## 4. FROZEN — CLI surface (`cell-cli`, bin `cell`)

Pipelines consumed as-is (signatures verbatim from baseline):
`clw_snapshot::snapshot<C>(root, client, cache, opts) -> SnapshotReport`,
`clw_snapshot::build_manifest_local(root) -> Manifest`,
`clw_hydrate::hydrate<C>(dest, client, cache, opts) -> HydrateReport`,
`clw_run::run_memoized<C>(cwd, client, opts) -> RunOutcome`,
`clw_manifest::diff(old, new) -> ManifestDiff`,
all with `C: CasTransport + AcTransport` = `LocalStore`.
L1 cache: `clw_cache::LocalCache` at `$CELL_CACHE_DIR` | `~/.cell/cache`.

| Verb | Form | Behavior | Exit |
|---|---|---|---|
| `snapshot` | `cell snapshot [--dir <path=.>] --name <ref>` | snapshot dir → store; print `root=<hex> files=<n> bytes=<n> chunks_uploaded=<n>` | 0 ok · 2 usage/invalid-ref · 1 error |
| `hydrate` | `cell hydrate <dest> --name <ref>` | materialize ref into `<dest>`; print `root=<hex> files=<n> bytes_total=<n> from_cache=<n>` | 0 ok · 2 ref-not-found/usage · 1 error |
| `status` | `cell status [--dir <path=.>] --name <ref>` | `build_manifest_local` vs ref manifest via `diff`; print added/removed/changed | 0 clean · 1 dirty · 2 ref-not-found/usage |
| `run` | `cell run [--input <path>]... [--env <KEY>]... --name? -- <cmd> [args...]` | `run_memoized`; stream stored/captured stdout/stderr; marker line to **stderr**: `cell: memo HIT key=<hex>` or `cell: memo MISS key=<hex>` | child's exit code · 2 usage |

Global rules (frozen):
- Ref grammar `^(@[a-z0-9-]+/)?[a-z0-9._-]{1,64}$` (ADR-0004); violation →
  stderr error + exit 2.
- Env contract: `CELL_STORE_DIR`, `CELL_CACHE_DIR` (tests depend on these).
- No network code paths exist in v0.1 (ADR-0007). No `--engine` flag
  (ADR-0005). `cell --help` states: "native execution — reproducibility,
  not a sandbox".
- Exit-code classes: 0 success/clean · 1 dirty-or-runtime-error · 2
  usage/not-found · (`run` is the exception: child's code passes through).
- `run` memo law is clw's, not ours: memoize **only** exit 0 AND
  stdout/stderr each ≤ 5 MiB (quoted from `clw-run @ f8f5edf`).

## 5. FROZEN — Acceptance suite A1–A7 (`cell-acceptance`)

End-to-end via `assert_cmd` against the compiled `cell` binary; every test
sets `CELL_STORE_DIR`/`CELL_CACHE_DIR` to per-test tempdirs; no test touches
`~`. Each item maps to the MVP DoD.

- **A1 roundtrip** — fixture tree (nested dirs, exec-bit file, symlink,
  empty dir, ~100 files incl. one >4 MiB multi-chunk) → `snapshot` →
  `hydrate` to fresh dir → recursive compare: bytes, modes, symlink targets
  identical.
- **A2 memo hit** — `run --input <inputs> -- sh -c "<append to side-effect
  file OUTSIDE inputs; echo out>"` twice: side-effect file has **1** line;
  2nd invocation stderr has `memo HIT`; stdout identical both times; exit 0.
- **A3 failure never memoized** — same shape, command exits 7: two
  invocations → side-effect **2** lines, both exit 7, both `memo MISS`.
- **A4 no daemon** — after A1–A3, zero `cell` processes alive
  (`pgrep -x cell` empty); store root contains only plain files/dirs.
- **A5 status** — post-snapshot `status` exits 0; modify one file → exits 1
  and names it `~ <path>`; unknown ref → exits 2.
- **A6 offline-structural** — A1+A2 pass with `CLW_ENDPOINT`/`CLW_TOKEN`
  pointing at a black-hole (`http://127.0.0.1:9`) — proving those env vars
  are dead code to `cell`.
- **A7 integrity fail-closed** — flip one byte in one CAS blob → `hydrate`
  into a fresh dir exits 1 with `integrity` in stderr; the corrupt file
  still exists afterward (no silent deletion).

## 6. Gates

- **Per-WP (scoped):** `cargo fmt --check` (touched crate) · `cargo clippy
  -p <crate> --all-targets -- -D warnings` · `cargo test -p <crate>`.
- **Integration (wave close):** fmt --check workspace · clippy workspace
  `-D warnings` · `cargo test --workspace` (A1–A7 green) · `git status`
  clean · no file outside the WP's owner-globs changed (post-flight sweep).
- Rigor compact applies: a red gate is fixed at the root; waivers are
  human-only and logged verbatim.

## 7. Wave partition (conflict map)

| WP | Owner-globs (disjoint) | Depends on |
|---|---|---|
| W0 scaffold (lead) | `Cargo.toml`, `rust-toolchain.toml`, crate skeletons, this spec's stubs | ADRs accepted |
| W1 store | `crates/cell-store/**` | §3 frozen |
| W2 cli | `crates/cell-cli/**` | §3 (via stubs) + §4 frozen |
| W3 acceptance | `crates/cell-acceptance/**` | §4 + §5 frozen |

Shared/lead-owned (agents must NOT touch): workspace `Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`, `docs/**`, `README.md`, `CLAUDE.md`.
Pairwise verdict: **CONFLICT-FREE** (three disjoint crates).
Merge order: W1 → W2 → W3; suite authored red-first, goes green at
integration; an independent cold critic checks A1–A7 coverage against the
MVP DoD before the wave closes.

## 8. Freeze record

- Contract hash: _pending ADR acceptance_
- Frozen by: _owner decision pending_
