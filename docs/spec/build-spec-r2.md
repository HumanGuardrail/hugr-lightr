# HuGR Lightr — Build Spec R2 (the Linux tier: OCI + engines)

- **Status:** FROZEN (owner R1→R4 mandate). Additive; R0/R1 surfaces
  unchanged. Platform law (decisions-log 2026-06-12): this dev box is
  Intel x86_64 — vz save/restore and Apple's arm64 kernel need Apple
  Silicon; everything here is **capability-probed with loud, tested
  error messages**, never silent skips.
- Features: F-301 (oci import/pull), F-201/204/205 partial (engine trait,
  ns, vz boot-path), F-601 partial (linux pack as lazy ref).

## 1. New crates & deps

```
crates/lightr-engine/   # Engine trait + native/ns/vz implementations
crates/lightr-oci/      # OCI bridge: registry pull + layout import (sync)
```
- `lightr-engine`: deps lightr-core/store/index + libc. Feature `vz`
  (default OFF): builds the Swift shim via build.rs (swiftc staticlib) —
  default build stays pure Rust (microwave clause).
- `lightr-oci`: deps lightr-core/store/index + `ureq` (rustls), `flate2`,
  `tar`, serde+serde_json. Bridge crate: sync, no tokio (ADR-0011 allows
  network here only).
- Workspace dep additions pinned in root Cargo.toml (lead-owned).

## 2. FROZEN — `lightr-engine`

```rust
pub enum EngineKind { Native, Ns, Vz }
impl std::str::FromStr for EngineKind { /* "native"|"ns"|"vz" */ }

pub struct EngineCaps { pub available: bool, pub detail: String }
/// Probe WITHOUT side effects: Native always available; Ns ⇒ linux +
/// userns clone test; Vz ⇒ macos + feature "vz" compiled + linux pack
/// present ($LIGHTR_HOME/packs/linux/{kernel,initrd} or LIGHTR_LINUX_PACK
/// dir override).
pub fn probe(kind: EngineKind) -> EngineCaps;

pub struct ExecSpec<'a> {
    pub cwd: &'a std::path::Path,
    pub command: &'a [String],
    pub rootfs: Option<&'a std::path::Path>, // ns/vz: CoW-materialized tree
}
pub trait Engine {
    /// Spawn + wait; stdout/stderr inherit. Exit law: code or 128+signal.
    fn run(&self, spec: &ExecSpec) -> lightr_core::Result<i32>;
}
pub fn engine_for(kind: EngineKind) -> lightr_core::Result<Box<dyn Engine>>;
/// Unavailable kind ⇒ Err(InvalidRef(format!("engine {kind}: {detail}")))
/// — the CLI maps it to exit 2 with the probe's actionable detail.
```
- `NativeEngine`: process spawn (parity with lightr-run's executor).
- `NsEngine` (cfg(target_os="linux")): clone3/unshare user+mount+pid,
  pivot_root into `rootfs` (CoW tree — NO overlayfs), exec. On macOS the
  type exists but probe says unavailable ("ns engine requires Linux").
- `VzEngine` (cfg(target_os="macos"), feature `vz`): Swift shim
  (`shim/vz.swift`, ~300 lines: VZVirtualMachineConfiguration, virtio-fs
  share of the rootfs + store, console to log files, boot kernel+initrd,
  run PID1 cmdline, report exit). Boot-path only in R2 (resume is AS-only;
  ADR-0014). Without the feature or pack: probe.detail explains exactly
  what to install.
- Linux pack: refs `@lightr/pack-linux-x86_64` materialized to
  `$LIGHTR_HOME/packs/linux/` by `lightr engine install-pack <dir|ref>`
  (R2 ships the local-dir form; registry form arrives with Stage 2).

## 3. FROZEN — `lightr-oci`

```rust
pub struct ImportReport { pub name: String, pub root: lightr_core::Digest,
                          pub layers: u64, pub files: u64 }
/// Import an OCI **layout directory or tar** (skopeo/`docker save`-style):
/// parse index.json → manifest → apply layers in order (tar.gz/tar,
/// whiteouts honored) into a temp tree → snapshot as `name` (parent chain
/// per repeated imports). Pure-local, no network.
pub fn import_layout(path: &std::path::Path, store: &Store, name: &str)
    -> lightr_core::Result<ImportReport>;

/// Pull from a registry (OCI distribution v2; anonymous + token auth
/// dance for docker.io), then import. Network — bridge-only.
pub fn pull(image: &str, store: &Store, name: &str)
    -> lightr_core::Result<ImportReport>;
```
Layer application law: gz or plain tar autodetected; entry types file/dir/
symlink/hardlink(→copy); `.wh.` whiteouts delete; `.wh..wh..opq` clears
dir; modes preserved; ownership ignored (rootless). Digest of each blob
verified against the manifest before applying (fail-closed).

## 4. FROZEN — CLI additions

| Verb | Form | Exit |
|---|---|---|
| `run --engine` | `lightr run [--engine native\|ns\|vz] [--rootfs <ref>] …` | engine unavailable ⇒ 2 + probe detail; else child's code |
| `engine ls` | `lightr engine ls [--json]` | 0; lists kinds + caps (available/detail) |
| `engine install-pack` | `lightr engine install-pack <dir>` | 0; validates kernel+initrd present; copies into packs/ |
| `oci import` | `lightr oci import <layout-dir\|tar> --name <ref> [--json]` | 0 · 2 bad layout/name · 1 error |
| `oci pull` | `lightr oci pull <image> --name <ref> [--json]` | 0 · 1 network/registry error · 2 usage |

`--rootfs <ref>`: hydrates the ref CoW into a temp tree handed to the
engine as rootfs (ns/vz); with native engine ⇒ exit 2 "native engine has
no rootfs" (honesty: native is not a container).

## 5. FROZEN — Acceptance A17–A21

- **A17 oci import roundtrip (offline)** — the test BUILDS a tiny OCI
  layout fixture in-test (index.json + manifest + config + 2 layers as
  tar.gz made with the `tar` crate: layer1 adds /bin/sh-stub + /etc/x;
  layer2 whiteouts /etc/x + adds /app/hello) → `oci import` → `hydrate` →
  tree equals the EXPECTED post-whiteout tree (modes incl. 0755).
- **A18 import idempotent+lineage** — import same layout twice to same
  name: same root digest; ref_log len 2.
- **A19 engine probes honest** — `engine ls --json`: native.available
  true; on macOS ns.available false with "requires Linux" detail;
  vz.available false (feature off) with actionable detail. `run --engine
  ns -- true` exits 2 and stderr contains the detail; same for vz.
- **A20 rootfs guard** — `run --engine native --rootfs @x -- true` exits 2
  ("no rootfs").
- **A21 pull network-gated, loud** — WITHOUT `LIGHTR_NET_TESTS=1`: the
  test asserts `oci pull alpine --name @t/a` fails fast with a clear
  network-class error (exit 1) — proving no silent hang; WITH the env var
  set (manual/CI-net lane) it performs a real pull of
  `registry-1.docker.io/library/alpine:latest` and hydrate lists `/bin/`.
  The gated branch is explicit in-code with a loud eprintln of which lane
  ran (documented in spec — not a hidden skip).

## 6. Wave partition

| WP | Owner | Model | Scope |
|---|---|---|---|
| R2-W0 scaffold | lead | — | crates + workspace deps + stubs + clap skeleton |
| R2-W1 | `crates/lightr-oci/**` | sonnet | §3 + unit tests w/ in-test fixtures |
| R2-W2 | `crates/lightr-engine/**` | sonnet | §2 (native+ns real; vz shim compiled behind feature, boot-path code complete, probe honest) |
| R2-W3 | `crates/lightr-cli/**` | sonnet | §4 verbs/flags wiring |
| R2-W4 | `crates/lightr-acceptance/**` | sonnet | §5 A17–A21 |
| critic | read-only | opus | suite vs §5 + parity rows |

Same gates/laws as v2 §10. vz REAL-BOOT validation (with an actual
kernel) is the S5 spike WITH the owner — R2 ships the machinery +
honest probes; the boot demo lands when the pack exists.
