# HuGR Lightr — As-Built Architecture

> **Status: design / pre-GA.** Every statement here is grounded in actual
> source files and ADRs. Comparisons are stated as targets, not measurements
> (CLAUDE.md tense discipline).

---

## 1  Crate Graph

```
                          ┌─────────────────┐
                          │  lightr-cli      │  binary `lightr`
                          │  (clap, dispatch)│
                          └────────┬─────────┘
                                   │ deps on everything below
           ┌───────────────────────┼──────────────────────────┐
           │                       │                          │
  ┌────────▼──────┐  ┌─────────────▼──────┐  ┌──────────────▼────┐
  │  lightr-build  │  │   lightr-run        │  │  lightr-oci        │
  │  Dockerfile +  │  │   memo/spawn/ps/    │  │  bridge crate;     │
  │  compose YAML  │  │   logs/stop/exec_in │  │  pull/push/import  │
  └───────┬────────┘  └──────────┬──────────┘  └──────────┬────────┘
          │                      │                         │
          │           ┌──────────▼───────────┐             │
          │           │  lightr-engine        │             │
          │           │  EngineKind / probe / │             │
          │           │  engine_for / ExecSpec│             │
          │           └──────────┬────────────┘             │
          │                      │                          │
          └──────────────────────┼──────────────────────────┘
                                 │ all crates dep on:
           ┌──────────────┬──────┴──────────┬───────────────┐
           │              │                  │               │
  ┌────────▼────┐  ┌──────▼──────┐  ┌───────▼─────┐  ┌─────▼──────┐
  │ lightr-index │  │ lightr-store│  │  lightr-init │  │lightr-views│
  │ stat-index   │  │ Store (CAS/ │  │  Linux PID1  │  │ O(1) view  │
  │ scan/snap/   │  │ AC / refs / │  │  + file-chan  │  │ plan/solid │
  │ hydrate/undo │  │ CoW ladder) │  │  constants   │  │ backends   │
  └──────┬───────┘  └──────┬──────┘  └──────────────┘  └────────────┘
         │                  │
         └────────┬──────────┘
                  │
        ┌─────────▼────────┐
        │   lightr-core     │
        │  Digest / Manifest│
        │  Entry / RefRecord│
        │  LightrError      │
        └──────────────────┘

  ┌──────────────────┐   ┌─────────────────────────────┐
  │ lightr-acceptance │   │  lightr-init (bin/init)      │
  │  A1-A8 e2e suite  │   │  Linux cfg-gated binary      │
  │  invokes `lightr` │   │  (#[cfg(target_os="linux")]) │
  └──────────────────┘   └─────────────────────────────┘
```

**Dependency law (ADR-0001):** `lightr-cli` depends on all library crates;
nothing depends on `lightr-cli`; `lightr-acceptance` invokes only the
compiled binary (no crate dep). Network code is quarantined in `lightr-oci`
and the planned `lightr-wire`; core crates link zero async runtime (ADR-0011).

### Crate roles and key public types

| Crate | Role | Key public surface |
|---|---|---|
| `lightr-core` | Frozen contract layer — types only, no I/O (`#![forbid(unsafe_code)]`) | `Digest`, `Manifest`, `Entry`, `RefRecord`, `ref_key`, `validate_ref_name`, `LightrError`, `ResourceLimits`, `MANIFEST_MAGIC`, `OUTPUT_CAP_BYTES`, `REF_KEY_DOMAIN` |
| `lightr-store` | Local CAS/AC/refs on disk; CoW ladder; gc advisory locking | `Store`, `CowRung`, `WriteGuard`, `GcGuard` |
| `lightr-index` | Stat-index; scan/snapshot/hydrate/status/time-axis ops | `Index`, `scan`, `snapshot`, `hydrate`, `hydrate_verified`, `status`, `gc`, `GcReport`, `bisect`, `diff_manifests`, `parse_lrr1`, `undo`, `WalkReport`, `SnapshotReport`, `HydrateReport`, `StatusReport`, `DiffReport` |
| `lightr-run` | Memo key, exec, spawn_detached, supervise, ps/logs/stop/exec_in, networking stubs | `RunSpec`, `RunHandle`, `RunOutcome`, `Mount`, `PortMap`, `StoreFile`, `LogStream`, `VzMemoKey`, `DeepMemoConfig`, `run_memoized`, `run_memoized_with`, `predict`, `run_vz_memoized`, `vz_memo_key`, `run_memoized_deep`, `spawn_detached`, `spawn_detached_engine`, `spawn_detached_with_health`, `supervise`, `ps`, `logs`, `stop`, `exec_in`, `RestartPolicy`, `NetworkRegistry`, `MacAddr`, `Member`, `NetworkId`, `Subnet` |
| `lightr-engine` | `EngineKind` enum, `Engine` trait, `probe`, `engine_for`, per-engine impls + Swift shim | `EngineKind`, `EngineCaps`, `ExecSpec`, `NativeEngine`, `probe`, `pack_status`, `engine_for`, `GUEST_PATH` |
| `lightr-oci` | Bridge crate — OCI pull/push/import over HTTP (ureq); sha256 in Digest wrapper | `pull`, `push`, `import_layout`, `ImportReport`, `PushReport` |
| `lightr-views` | O(1) view planning + solidifier; real backends cfg-gated and intentionally unwired | `ViewPlan`, `PlanEntry`, `EntryKind`, `plan_view`, `Solidifier`, `ViewBackend`, `FakeBackend`, `solidify_step` |
| `lightr-build` | Dockerfile graph (step-memoized) + hand-rolled compose YAML parser + lazy-compose supervisor | `build`, `parse_dockerfile`, `parse_compose`, `compose_up`, `compose_down`, `compose_supervise`, `step_reads_clock_or_net`, `BuildReport`, `BuildStep`, `Compose`, `ComposeHandle`, `Instr`, `Service`, `ServiceSpec`, `StackSpec` |
| `lightr-cli` | Binary `lightr` — clap dispatch + handlers; exit codes 0/1/2 | `lightr_home`, `emit_event`, `PlanCmd` |
| `lightr-init` | Linux PID1 library + host-portable constants for the file-channel protocol; `bin/init.rs` is `#[cfg(target_os="linux")]` | `InitSpec`, `ROOTFS_TAG`, `ROOTFS_DEST`, `CMD_FILE`, `EXIT_FILE`, `STDOUT_FILE`, `STDERR_FILE`, `IP_FILE`, `GUEST_PATH`, `SPAWN_FAILED_CODE` |
| `lightr-acceptance` | End-to-end test suite A1-A8; no library dep — invokes compiled binary | (test crate) |

Sources: `crates/<name>/src/lib.rs` for each crate.

---

## 2  Engine Seam

### EngineKind — the four isolation tiers

Defined in `crates/lightr-engine/src/engine/kind.rs`:

```rust
pub enum EngineKind { Native, Ns, Vz, Wsl }
```

`EngineKind::platform_default()` returns `Vz` on macOS, `Ns` on Linux,
`Wsl` on Windows, `Native` elsewhere.

`EngineKind::all()` iterates all four variants in display order — consumed by
`lightr engine ls`.

### Engine trait and engine_for

`crates/lightr-engine/src/engine/mod.rs`:

```rust
pub trait Engine {
    fn run(&self, spec: &ExecSpec) -> Result<i32>;
}

pub fn engine_for(kind: EngineKind) -> Result<Box<dyn Engine>>
```

`engine_for` calls `probe(kind)` first; an unavailable engine returns
`Err(LightrError::InvalidRef("engine <kind>: <detail>"))` — fail closed,
never a silent skip.

### ExecSpec — the per-run descriptor

`crates/lightr-engine/src/engine/spec.rs`. Fields:

| Field | Type | Notes |
|---|---|---|
| `cwd` | `&Path` | Working directory on the host |
| `command` | `&[String]` | Argv |
| `rootfs` | `Option<&Path>` | `None` for native; CoW-materialized tree for ns/vz |
| `limits` | `ResourceLimits` | F-203 caps; NOT part of any memo key |
| `net` | `bool` | Enables NAT NIC + `ip=dhcp` on vz; other engines ignore |
| `net_fd` | `Option<RawFd>` | ADR-0018: guest-side fd of `socketpair(AF_UNIX, SOCK_DGRAM)` for the mesh NIC (eth1); `None` = single-NAT-NIC path |
| `net_mac` | `Option<[u8;6]>` | MAC for the mesh NIC, assigned by the network registry |

### probe — side-effect-free capability checks

`crates/lightr-engine/src/engine/probe.rs`. `probe(kind) -> EngineCaps`.

- **Native**: always `available = true`.
- **Ns**: `cfg(target_os="linux")` only; honest non-Linux arm names the host OS and returns unavailable.
- **Vz**: `cfg(all(target_os="macos", feature="vz"))` AND the linux pack (`kernel` + `initrd`) must exist under `$LIGHTR_LINUX_PACK` / `$LIGHTR_HOME/packs/linux` / `~/.lightr/packs/linux`. Missing pack → `available = false` with the install command.
- **Wsl**: `cfg(target_os="windows")` only; `wsl.exe -l -q` must return at least one distro; honest non-Windows arm names the host OS.

### The vz path — Apple Virtualization.framework

**Rust side:** `crates/lightr-engine/src/engine/vz.rs`  
**Swift shim:** `crates/lightr-engine/shim/vz.swift` (compiled to a static lib by `build.rs` when `feature = "vz"`; default builds never reach this file)

The shim exports the C symbol `lightr_vz_run`; the Rust side declares it as `extern "C"`. The shim uses `VZVirtualMachine` (Apple Virtualization.framework) to boot a Linux microVM:

- `VZLinuxBootLoader` pointing at a **bzImage** kernel + initrd from the linux pack. A raw `vmlinux` ELF (even a PVH one) is rejected by VZ with "Internal Virtualization error" — bzImage is required.
- A writable virtiofs share at tag `rootfs` (the CoW rootfs dir).
- A read-only virtiofs share at tag `store` (the host store root).
- One `VZNATNetworkDeviceAttachment` (eth0, internet egress), opt-in via `LIGHTR_VZ_NET`.
- Optionally a second `VZFileHandleNetworkDeviceAttachment` over `net_fd` (eth1, mesh, ADR-0018) when `net_fd >= 0`.

**Why files, not vsock:** macOS has no host `AF_VSOCK`. The kernel cmdline cannot carry arguments with spaces. The host/guest channel is two small files on the shared writable virtiofs rootfs (`crates/lightr-init/src/lib.rs`):

| Constant | Written by | Content |
|---|---|---|
| `CMD_FILE = "/.lightr-cmd"` | host (before boot) | `InitSpec` JSON (command, cwd, env, net flag) |
| `EXIT_FILE = "/.lightr-exit-code"` | guest PID1 (after run) | decimal exit code |
| `STDOUT_FILE = "/.lightr-stdout"` | guest PID1 | captured stdout for memo replay |
| `STDERR_FILE = "/.lightr-stderr"` | guest PID1 | captured stderr for memo replay |
| `IP_FILE = "/.lightr-ip"` | guest PID1 (when `net=true`) | primary IPv4 for port-forward |

The shim returns a **VM-lifecycle status** (0 = clean stop, -1 = boot/config failure), never the guest's exit code. `VzEngine::run` reads `EXIT_FILE` after the VM stops; a missing file produces exit code 255, not a fabricated 0 (`GUEST_NO_REPORT_CODE`).

The guest PID1 (`crates/lightr-init`) is a Rust static binary (~1 MB target). The library is fully host-portable, parameterised over `GuestOps` and `ExitSink`; the real Linux syscalls are `#[cfg(target_os="linux")]`-gated in `bin/init.rs`.

`GUEST_PATH` (`/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`) is a single const in `lightr-init/src/lib.rs`, re-exported from `lightr-engine` as `pub use lightr_init::GUEST_PATH`. Both the engine and the vz memo key reference this const — they can never drift.

---

## 3  CAS + Memo Model

### Content-addressed store (lightr-store, ADR-0009)

Default root: `~/.lightr/store` (override: `LIGHTR_STORE_DIR`).

```
~/.lightr/store/
  objects/<2-hex>/<62-hex>   # CAS blobs — write-once, immutable, chmod read-only
  refs/<2-hex>/<62-hex>      # RefRecord binaries, keyed by ref_key(name)
  ac/<2-hex>/<62-hex>        # Action Cache values, keyed by run-key digest
  .gc.lock                   # advisory flock: writers take SHARED, gc takes EXCLUSIVE
```

`Store::open` (`crates/lightr-store/src/lib.rs`) probes the CoW ladder at init
via `store::cow::probe_rung` (`crates/lightr-store/src/store/cow.rs`):

```
macOS:   clonefile (CowRung::Clone)
Linux:   FICLONE reflink (CowRung::Reflink) → copy_file_range (CowRung::CopyRange)
Windows: FSCTL_DUPLICATE_EXTENTS_TO_FILE (CowRung::RefsBlockClone)
fallback: std::fs::copy (CowRung::Copy)
```

The winning rung is stored in `Store.rung` and surfaced to `--explain`.

All writes are temp-file + rename (POSIX-atomic). `get` re-hashes the blob on
read; a mismatch returns `LightrError::Integrity` and leaves the file on disk
(stores are evidence; deletion is the cache's semantics, not the store's).

### Core types (lightr-core)

- **`Digest`** (`crates/lightr-core/src/core/digest.rs`): 32-byte BLAKE3 wrapper
  (`pub struct Digest(pub [u8; 32])`). `Digest::of_bytes` uses `blake3::hash`;
  `Digest::of_file` uses `blake3::Hasher::update_mmap_rayon`. Note: `lightr-oci`
  stores sha256 bytes in the same wrapper for OCI blobs; those callsites are
  annotated `// sha256 bytes stored in Digest (not blake3)`.

- **`Manifest`** (`crates/lightr-core/src/core/manifest.rs`): binary format `LMF1`
  (4-byte magic + u32 version + u64 total_size + u32 entry_count + path-sorted
  entries). mmap-able; JSON only at borders (`--json`, wire).

- **`Entry`**: three variants — `File { path, mode, size, digest }`,
  `Symlink { path, target }`, `Dir { path }`.

- **`RefRecord`** (`crates/lightr-core/src/core/refrecord.rs`):
  `{ name, root: Digest, parent: Option<Digest>, created_at_unix: u64, tool_version: String }`.
  The `parent` chain enables `undo`/`diff`/`bisect` (F-401/402).
  `ref_key(name)` produces the store lookup key with domain separation via
  `REF_KEY_DOMAIN`.

### Stat-index (lightr-index, ADR-0010)

Binary, mmap-able, path-sorted index stored under `~/.lightr/index/<root-hash>`,
never inside the user's tree. Maps `path -> (size, mtime_ns, inode, mode) ->
digest`. Stat match => trust digest; mismatch => rehash (BLAKE3, rayon).
Racily-clean entries (mtime == index write-time) are re-verified (the git rule).
Feeds `status`, `snapshot`, and `run` memo-key assembly.

The index is a cache, not truth: deletable at any time; cold rebuild is the
full-walk path. Source: `crates/lightr-index/src/lib.rs`.

### Memo key and replay (lightr-run)

`crates/lightr-run/src/run/memo.rs`. Key assembled by `assemble_key` in exact
BLAKE3 order:

1. Domain separator `b"lightr/run/v1\0"`
2. For each input path (or `spec.cwd` if `spec.inputs` is empty):
   `scan(abs_path) -> WalkReport.manifest.digest()`; hash `(rel_path_bytes + \0 + digest.0)`
3. Command args: for each arg, `(arg.len() as u64).to_le_bytes() + arg.bytes()`
4. Env keys (sorted): present = `key=value\0`; absent = `key\x01`
5. `OS-ARCH` string (e.g. `macos-x86_64`)
6. Mounts in order: `ref_name bytes + [0x02] + current root digest bytes`

The key is a `Digest`; it is looked up in the AC. A **HIT** replays
`RunOutcome { exit_code, stdout, stderr }` from the AC record immediately.
A **MISS** runs the command, captures output (capped at `OUTPUT_CAP_BYTES`),
writes the AC record, and returns `RunOutcome`.

**`predict`** assembles the key without executing (dry-run for `lightr plan run`).

**Ports, resource limits, and healthcheck config are not part of the memo key**
(runtime parameters; the `ports_excluded_from_key` test enforces this).

**vz memo** (`crates/lightr-run/src/run/vzmemo.rs`): `VzMemoKey` adds
vz-specific inputs (linux pack digest, `GUEST_PATH`). The vz path stores
`{exit, stdout, stderr}` in the AC and replays them on a HIT.

**Deep memo** (`crates/lightr-run/src/run/deepmemo.rs`, ADR-0016): opt-in via
`--deep-memo`. Memoizes per child process inside the process tree, keyed on
`(argv, env-subset, cwd-rel, read-set file digests, platform)`. On Linux, the
read-set comes from the FS view layer + per-run mount namespace; on macOS via
spawn-shim interposition (degrades honestly to whole-run memo when interposition
fails). First consumer: `lightr build` (each Dockerfile instruction = a
deep-memo'd run).

---

## 4  Daemonless Lifecycle

**Principle 1 (CLAUDE.md): "No daemon, ever — nothing runs when nothing runs; `ps` proves it."**

No coordinator process. No socket bound at idle. Each `lightr` invocation is a
one-shot CLI process.

### Memoized run (synchronous)

`run_memoized` / `run_memoized_with` in `crates/lightr-run/src/run/memo.rs`:
assemble key → AC lookup → HIT returns immediately → MISS hydrates + runs + stores.

### Detached runs and the supervisor re-exec pattern

`crates/lightr-run/src/run/spawn.rs` — `spawn_detached` / `spawn_detached_engine`.

1. A run dir is created under `$LIGHTR_HOME/runs/<id>/`.
2. The `RunSpec` is persisted as `spec.json`.
3. The binary re-execs itself as `lightr __supervise <run_dir>` (detached, no controlling terminal).
4. The supervisor (`crates/lightr-run/src/run/supervise.rs`) owns the child process lifetime, writing exit code + logs to the run dir.

For vz runs, `spawn_detached_engine` boots the microVM inside the supervisor; the
supervisor also reads `IP_FILE` and forwards published ports from
`127.0.0.1:host_port` to the guest's DHCP IP (WP-NET2).

### ps / logs / stop / exec_in

- **`ps`** (`crates/lightr-run/src/run/ps.rs`): walks `$LIGHTR_HOME/runs/`, reads `spec.json` + `pid` + `exit` + `health` per run dir, returns `Vec<RunInfo>`.
- **`logs`** (`crates/lightr-run/src/run/logs.rs`): streams stdout/stderr from log files in the run dir.
- **`stop`** (`crates/lightr-run/src/run/stop.rs`): sends SIGTERM/SIGKILL to the pid in the run dir.
- **`exec_in`** (`crates/lightr-run/src/run/exec.rs`): enters a running container (ns: `nsenter`; vz: planned).

### OS-supervisor unit templates (lightr-run::restart, ADR-0017)

`crates/lightr-run/src/restart/mod.rs` — pure module, **no I/O**. Provides
`RestartPolicy` (`No`, `Always`, `OnFailure { max: u32 }`, `UnlessStopped`) and
template generators `launchd_plist(...)` and `systemd_unit(...)` that return
`String`. The I/O (write + register) lives in `lightr-cli::handlers::supervise`.
Lightr generates the unit and tells the user the opt-in command; it ships no daemon.

### Compose supervisor (lightr-build::compose, ADR-0015)

`lightr compose up` binds per-service listeners (a few KB each) and resumes each
service from its suspended state on first packet — an idle "running" stack costs
~0 RAM and `up` returns in milliseconds. `compose_supervise`
(`crates/lightr-build/src/build/`) is the ephemeral per-stack supervisor: it owns
the bind/accept/proxy loop and self-exits when `$stack_dir/stop` appears or the TTL
fires. `--eager` restores immediate-start semantics per service.

---

## 5  Networking (ADR-0018)

### Why a userspace L2 switch

Each `lightr run --engine vz` is a separate microVM. Apple's
`VZNATNetworkDeviceAttachment` isolates guests behind independent NATs — two VMs
in separate processes on the same `192.168.64.x` subnet cannot reach each other.
`VZVmnetNetworkDeviceAttachment` (true guest L2) requires macOS 26+; this host is
15.3.2. `VZBridgedNetworkDeviceAttachment` requires the restricted entitlement
`com.apple.vm.networking`. Only `VZFileHandleNetworkDeviceAttachment` requires
no extra entitlement beyond `com.apple.security.virtualization`, which
`packaging/vz.entitlements` already signs.

### Design (ADR-0018 Decision)

1. **Userspace L2 switch** (`crates/lightr-run/src/vswitch/switch.rs`): pure-Rust
   MAC-learning switch. `forward(frame, from, table) -> ForwardDecision` is a pure
   function — known-unicast to the learned port, broadcast/multicast/unknown/ARP
   flooded to all other ports, frames under 14 bytes dropped. No I/O; fully
   unit-testable with crafted Ethernet frames.

2. **Dual-NIC: eth0 (NAT) + eth1 (mesh)**: a container on a user network keeps
   the existing `VZNATNetworkDeviceAttachment` (eth0, internet egress) and gains a
   `VZFileHandleNetworkDeviceAttachment` (eth1, mesh). Each member VM's mesh NIC
   connects via one half of a `socketpair(AF_UNIX, SOCK_DGRAM)` — one datagram
   == one Ethernet frame. A container on no user network is byte-for-byte the
   existing single-NAT-NIC path (zero regression). `ExecSpec.net_fd` carries the
   guest-side fd; `ExecSpec.net_mac` carries the deterministic MAC from the
   network registry.

3. **Network registry** (`crates/lightr-run/src/network/registry.rs`): on-disk,
   `flock`-guarded membership under `$LIGHTR_HOME/net/<id>/`. Mutators (`create`,
   `join`, `leave`) take `LOCK_EX`; readers (`members`) take `LOCK_SH`. Writes are
   atomic (temp + fsync + rename + parent fsync). Corrupt `members.json` fails
   closed (`InvalidData`), never a silent empty-membership. Source:
   `crates/lightr-run/src/network/`.

4. **Embedded DHCP + DNS**: the L2 switch is the DHCP-advertised DNS resolver
   (option 6 = switch IP). It answers A records for container/service names and
   forwards unknown queries upstream. The supervisor also injects `/etc/hosts`,
   `/etc/hostname`, and `resolv.conf` into the guest rootfs before boot
   (`--add-host`, `--dns`, `--hostname` support).

5. **Daemonless lifecycle**: the switch is network-scoped — born lazily by the
   first member's supervisor, reference-counted in the network registry, stopped
   when the last member leaves. Nothing of ours is resident between runs.

6. **CLI surface**: `lightr network create|ls|rm <name>`; `run --network <name>`;
   `compose.yml` `networks:` key.

**Spike gate (ADR-0018 status)**: `VZFileHandleNetworkDeviceAttachment` over a
datagram socket is gated on a de-risk spike. GREEN unblocks integration WPs
C1-C7; RED falls back to a host-relay alternative. ADR-0018 will be updated on
outcome, never silently.

---

## 6  O(1) Views (lightr-views, ADR-0013)

The **shipped** materialization path is CoW hydrate via `lightr_index::hydrate`:
for each manifest entry, clone the object from the store to the destination using
the best available `CowRung`. This is the runtime today.

The **planned** O(1) backends are `cfg`-gated modules in `lightr-views`
(`crates/lightr-views/src/lib.rs`): `composefs` (Linux, EROFS metadata over the
store + overlay upper), `nfsloopback` (macOS, EdenFS-proven NFS-loopback
in-process server), `projfs` (Windows, ProjFS). All compile but return
`ErrorKind::Unsupported` — they are intentionally not yet wired into the run
path, pending ADR-0013 S1/S3 spike validation.

`plan_view` walks every manifest entry (path-sorted) into a `ViewPlan` without
touching disk — only records `(path, kind, digest)` per entry. `Solidifier` owns
the promote-on-access policy: background-promote hot files to native CoW clones;
when fully solid, unmount (zero steady-state indirection). `FakeBackend` is the
host test double.

---

## 7  OCI Bridge (lightr-oci, ADR-0011)

`lightr-oci` is the only crate with network code (ureq; async/tokio allowed
here, forbidden in core). It handles `pull`, `push`, `import_layout`. OCI layers
are pulled and unpacked once into CAS file objects; they never live locally as a
runtime model. For OCI blobs the 32-byte sha256 hash is stored directly in the
`Digest` wrapper; every such callsite is annotated `// sha256 bytes stored in
Digest (not blake3)`. Exit-code mapping: `Integrity`/`InvalidManifest`/`Io`/
`Registry`/`NotFound`/`TooLarge` -> exit 1; `InvalidRef`/`RefNotFound` -> exit 2.
Source: `crates/lightr-oci/src/lib.rs`.

---

## 8  Run -> Memo -> Engine Flow

```
lightr run [--engine vz] @ref -- cmd args
     |
     +-- 1. Resolve ref
     |       lightr_store::Store::ref_get(ref_key(name)) -> RefRecord.root (Digest)
     |
     +-- 2. Memo key assembly   (lightr_run::run_memoized)
     |       lightr_index::scan(inputs) -> manifest digest per input
     |       BLAKE3( "lightr/run/v1\0" | inputs | cmd | env | OS-ARCH | mounts )
     |       -> Digest key
     |
     +-- 3a. AC HIT
     |        replay RunOutcome { exit_code, stdout, stderr } from AC record
     |        return immediately; zero execution
     |
     +-- 3b. AC MISS
              |
              +-- lightr_index::hydrate(manifest -> cwd)   [CoW clone objects]
              |
              +-- lightr_engine::engine_for(kind)          [probe; fail closed]
                    |
                    +-- Native -> NativeEngine::run(spec)
                    |     std::process::Command; stdout/stderr captured
                    |
                    +-- Ns    -> ns_engine_box()
                    |     clone(CLONE_NEWUSER | CLONE_NEWPID | ...)
                    |     pivot_root into spec.rootfs
                    |
                    +-- Vz    -> VzEngine::run(spec)
                    |     write InitSpec JSON -> rootfs/CMD_FILE
                    |     lightr_vz_run(kernel, initrd, rootfs, store, ...) [Swift shim]
                    |       VZVirtualMachine boots; virtiofs rootfs + store
                    |       guest PID1 (lightr-init) reads CMD_FILE, runs command
                    |       guest writes exit -> EXIT_FILE
                    |       guest writes stdout/stderr -> STDOUT_FILE/STDERR_FILE
                    |     read EXIT_FILE -> exit code (255 if missing)
                    |
                    +-- Wsl   -> wsl.exe -- cmd
                          WSL2 utility VM (the OS's, not ours)
              |
              +-- store RunOutcome in AC -> return
```

---

## 9  CoreLink Relationship

Per `CLAUDE.md` and ADR-0011:

- **Pure client of CoreLink Cache**: Lightr calls `/v1/cas` and `/v1/ac` exactly
  like `clw`; it does not fork or modify `corelink-server`.
- **Stage 1 is offline-absolute**: the local `Store` is the CAS/AC; no server is
  contacted.
- **Stage 2** (planned): the `lightr-wire` bridge crate (async, tokio, clw
  path-deps — ADR-0002 narrowed to bridge crates only by ADR-0011) syncs local
  objects to CoreLink in background. FastCDC chunking at the wire border; local
  store stays file-level.
- **Engine lineage from `corelink-runners`**: the `Engine` trait shape (spawn /
  probe / exec / teardown, fail-closed lifecycle) follows
  `corelink-runners/src/isolation.rs`. In the cloud, Lightr is what a runner
  lease executes; Runners is the fabric, Lightr is the runtime.
- **clw path-deps** (ADR-0002, narrowed by ADR-0011): only `lightr-oci` and the
  planned `lightr-wire` may take `clw-*` path-dependencies. Core crates link zero
  network code.
- **CoreLink dedup**: intra-tenant at GA; cross-tenant is staged
  (`CAP-DEDUP-CROSS-TENANT`). Not claimed as live.

---

## 10  Cross-Platform Seams (ADR-0017)

| Platform | Default engine | Key OS-specific seams |
|---|---|---|
| macOS (Intel + Apple Silicon) | `vz` | `clonefile` CoW; `flock` advisory; Swift shim compiled with `feature="vz"` |
| Linux (x86_64 + aarch64) | `ns` | `FICLONE` reflink or `copy_file_range`; `clone(CLONE_NEWUSER...)` |
| Windows (x86_64) | `wsl` | `LockFileEx`/`UnlockFileEx`; `FlushFileBuffers`; `FSCTL_DUPLICATE_EXTENTS_TO_FILE`; `wsl.exe` dispatch |

Platform seams are `cfg`-gated and additive — the Unix path is untouched by
Windows additions. `windows-sys` is declared in `Cargo.toml` under
`[target.'cfg(windows)'.dependencies]` and never pulled on Unix builds.

Runtime validation on non-host platforms is runbook-gated; the code is written
against honest stubs that report unavailable with a reason rather than a silent
skip.

---

## 11  Key Invariants

1. **No daemon** — `ps` proves zero Lightr processes at idle. The WSL2 and macOS
   VZ utility VMs are the OS's; Lightr starts and owns no background service.
2. **No images** — OCI is an import format (`lightr-oci`). Local model is CAS
   objects + `Manifest` + `RefRecord`.
3. **Memoize-first** — the AC check precedes any provisioning (`run_memoized`
   assembles the key before hydrate or spawn).
4. **Fail closed** — unavailable engine -> `Err`; missing linux pack -> probe
   reports reason; corrupt CAS object -> `Integrity` error, evidence preserved,
   never silently deleted.
5. **No network code in core** — `lightr-oci` is the quarantine boundary; no
   async runtime in core crates (ADR-0011).
6. **Free local, forever** — Stage 1 touches no servers; `~/.lightr` is the only
   required resource.

---

*Sources: `crates/*/src/lib.rs`, `crates/lightr-engine/src/engine/kind.rs`,
`crates/lightr-engine/src/engine/spec.rs`, `crates/lightr-engine/src/engine/probe.rs`,
`crates/lightr-engine/src/engine/vz.rs`, `crates/lightr-engine/shim/vz.swift`,
`crates/lightr-init/src/lib.rs`, `crates/lightr-run/src/run/memo.rs`,
`crates/lightr-run/src/run/spawn.rs`, `crates/lightr-run/src/restart/mod.rs`,
`crates/lightr-run/src/network/`, `crates/lightr-run/src/vswitch/switch.rs`,
`crates/lightr-store/src/store/cow.rs`, `crates/lightr-core/src/core/digest.rs`,
`crates/lightr-core/src/core/manifest.rs`, `crates/lightr-core/src/core/refrecord.rs`,
`docs/adr/0001` through `docs/adr/0018`, `CLAUDE.md`.*
