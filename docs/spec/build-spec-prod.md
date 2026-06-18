# Build Spec — Production Hardening + VM Foundation (parallel phase)

- **Status:** FROZEN (owner "os dois em paralelo" mandate, 2026-06-12).
  Additive; all R0–R4 surfaces unchanged. Two tracks, disjoint by crate so
  they fan out conflict-free.
- **Track A** (harden the local product toward shippable) +
  **Track B** (VM foundation that progresses on Intel before ARM/S5).
- Baseline facts verified 2026-06-12: pull uses `net_agent()` (10 s/60 s
  timeouts, ureq v2), picks `linux/amd64` hardcoded, `parse_image_ref →
  (registry, repo, tag)`; store `atomic_write` flushes but does NOT fsync;
  `gc` mark is point-in-time with no lock; `supervise` reaps via `try_wait`
  loop while alive.

## WP-A-pull — `lightr-oci` pull hardening (BLOCKER→MAJOR closer)

Frozen behavior (extend `pull`, keep `import_layout` + sha256 verify intact):
1. **Private-registry auth:** read `~/.docker/config.json` (or
   `$DOCKER_CONFIG/config.json`); for the target registry use its
   `auths.<reg>.auth` (base64 user:pass) → Basic on the token endpoint, or
   bearer flow as today for anonymous. Env override `LIGHTR_REGISTRY_AUTH`
   (base64 user:pass) wins. No creds ever logged.
2. **Retry + backoff:** on 429 and 5xx, retry up to 4 times with
   exponential backoff (200 ms → ~2 s); honor `Retry-After` when present.
   4xx (except 429) ⇒ no retry.
3. **Stream blobs to disk:** layer/config blobs download to temp files via
   a bounded copy (`std::io::copy` from the response reader), never fully
   into a `Vec` — sha256 computed streaming over the same reader. (Kills the
   OOM risk.)
4. **HTTP status → typed errors:** 401/403 ⇒ `LightrError::Auth`-class msg
   (add a variant OR reuse InvalidRef with "auth" — pre-decide: add
   `LightrError::Registry { status: u16, msg: String }` to lightr-core,
   mapped to exit 1 by the CLI). 404 ⇒ clear "image/blob not found". Network
   ⇒ Io. Each distinct, never collapsed.
5. **Multi-arch:** pick `linux/<host-arch>` (`std::env::consts::ARCH` →
   amd64/arm64), fall back to amd64, then any linux entry; error names what
   was available if none match.
Tests: config.json parse (basic+bearer), retry on injected 503 (mock via a
local one-shot TCP server or a seam), streaming import of a large fixture
without RAM blowup (assert it works on a ≥64 MiB layer), status mapping,
arch selection. Network lane stays behind `LIGHTR_NET_TESTS`.

## WP-A-dur — durability (`lightr-store` + `lightr-index`)

1. **fsync:** `atomic_write` (+ the inline writers at store.rs:313,
   index.rs:228) must `f.sync_all()` before rename AND fsync the parent
   directory after rename (open dir, `File::sync_all`). A helper
   `fsync_dir(path)`. This makes object/ref/AC/index writes crash-durable.
2. **gc lock:** `gc` takes an EXCLUSIVE advisory lock on
   `<store>/.gc.lock` (flock via libc) for the whole mark+sweep; `put_bytes`,
   `ingest_file`, `ref_put`, `ac_put` take a SHARED lock for their write.
   So gc can't sweep an object a concurrent writer is publishing. Lock helper
   in lightr-store; gc (in lightr-index) calls a `store.gc_guard()` →
   exclusive guard. Document the ordering.
3. Keep all existing behavior/tests green; add: a fsync-path test (write,
   reopen, content intact), a gc-vs-writer test (spawn a thread putting
   objects while gc runs — no live object swept).

## WP-A-ci — `.github/workflows/ci.yml`

GitHub-hosted runner (rustup works normally there — the local-toolchain
workaround is a founder-Mac quirk, NOT needed in CI). Jobs:
- `gate`: checkout, `rustup` honors `rust-toolchain.toml` (1.96.0),
  `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace`.
- `bench`: `cargo build --release` + `./target/release/lightr bench --check`
  (perf-regression gate; allowed to be a separate job, may be `continue-on-
  error: false` only on a perf-labeled runner — for now run it, mark
  machine-class in a comment).
No secrets, no network tests (LIGHTR_NET_TESTS unset). This is the missing
"gates green before merge" mechanization CLAUDE.md mandates.

## WP-B-init — NEW crate `lightr-init` (guest PID1)

A standalone static binary = the Linux guest's PID 1 (whitepaper §5, the
"~1 MB Rust PID1"). **Compiles + unit-tests on the host (Intel/macOS); only
RUNS inside a microVM on ARM later — so all OS-specific actions sit behind
testable seams.**
Frozen surface:
```rust
// crates/lightr-init/src/lib.rs
/// What PID1 must do, as data — read from the guest's mounted spec.
pub struct InitSpec { pub command: Vec<String>, pub cwd: String,
                      pub env: Vec<(String,String)> }
impl InitSpec { pub fn from_json(b: &[u8]) -> Result<Self, String>;
                pub fn to_json(&self) -> Vec<u8>; }

/// Where PID1 reports the guest process exit code (the fix for the fake
/// exitCode=0). Seam so tests use a Vec; the real impl writes the code to
/// `EXIT_FILE` on the rootfs virtiofs share (macOS has no host AF_VSOCK).
pub trait ExitSink { fn report(&mut self, code: i32) -> std::io::Result<()>; }

/// The init lifecycle, parameterized over the OS actions (mount, spawn) so
/// it's unit-testable on any host. Returns the guest process exit code.
pub fn run_init<M: GuestOps>(spec: &InitSpec, ops: &mut M, sink: &mut dyn ExitSink)
    -> std::io::Result<i32>;
pub trait GuestOps { /* mount_virtiofs(tag,dest), spawn_wait(cmd,cwd,env)->i32 */ }
```
The `bin/init.rs` wires the REAL Linux impl (mount syscalls + a file sink that
writes the exit code to `EXIT_FILE` on the rootfs share; the host reads it
back) behind `#[cfg(target_os="linux")]`; the lib + a `FakeOps`/`VecSink` make the
lifecycle fully testable on Intel/macOS now. Tests: spec json roundtrip,
run_init drives mount→spawn→report in order, exit code propagates, sink
receives it. **No fake success: the code path that reports the exit is real
and tested; only the syscalls are seamed.**

## WP-A-honesty — README + whitepaper outward tense (LEAD)

The internal ledgers are honest; the outward top-of-README (`brew install` +
a fabricated "hydrated 1.2 GB" transcript) and whitepaper §1 ("linux
resumed") claim what doesn't run. Lead rewrites the README opening + a
"Status: what runs today vs the target" box that matches `parity-audit.md`,
and adds a tense disclaimer to whitepaper §1's scene. No product claim beyond
the ledger. (Lead-owned; not a fleet WP.)

## Wave plan (P1 parallel, then P2)

| WP | Owner-glob | Model | Wave |
|---|---|---|---|
| A-pull | `crates/lightr-oci/**` (+ `LightrError::Registry` in lightr-core) | sonnet | P1 |
| A-dur | `crates/lightr-store/**` + gc-guard call in `crates/lightr-index/**` | sonnet | P1 |
| A-ci | `.github/**` | sonnet | P1 |
| B-init | `crates/lightr-init/**` (new) | opus | P1 |
| A-honesty | `README.md`, `docs/whitepaper/**` | lead | P1 |
| B-vsock+pack | `crates/lightr-engine/**` (vsock sink + pack pipeline) | opus | P2 |
| A-dist | `packaging/**` (brew formula, install.sh) — publish gated by ADR-0008 | sonnet | P2 |

Conflict note: A-pull needs a new `LightrError::Registry` variant in
lightr-core — the LEAD adds that variant in the scaffold (shared file),
so A-pull only touches lightr-oci. A-dur's gc call site in lightr-index is a
one-line change the LEAD pre-wires in scaffold too, keeping A-dur inside
lightr-store. Gates/laws per build-spec v2 §6/§10.
