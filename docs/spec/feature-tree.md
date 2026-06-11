# Feature tree — the complete decomposition

- **Status:** canon, derived from whitepaper v2 (working backwards).
  Supersedes the matrix in `feature-parity.md` as the build-driving list
  (parity doc remains the directive record).
- Every feature: ring, mechanism, budget (where perf-bearing), acceptance
  anchor. A ring is claimed only when its features' acceptance + bench
  budgets are green (tense law).

## F-0xx — The content plane (store)

| F | Feature | Ring | Mechanism / budget | Acceptance |
|---|---|---|---|---|
| F-001 | File-level CAS objects (BLAKE3, immutable, sharded) | R0 | `objects/<2hex>/<62hex>`, write-once temp+rename | A1, A7 |
| F-002 | CoW ladder probe + materialization | R0 | clonefile→FICLONE→copy_file_range→copy; probed at store init; mode visible in `--explain` | A1, bench B3 |
| F-003 | Binary mmap manifests (path-sorted) | R0 | custom codec, no JSON hot path; parse ≈ 0 | A1 unit |
| F-004 | Fail-closed integrity | R0 | rehash on get; `Integrity` error, never silent-delete | A7 |
| F-005 | Refs + lineage (parent chain) | R0 | ref record: name→manifest digest+parent | A1, F-091 |
| F-006 | Big-object page-aligned chunking (VM states) | R2 | CodeSandbox-style page chunks; dedup across states | R2 bench |
| F-007 | fs-verity object sealing (Linux) | R2 | kernel-enforced integrity where available | R2 accept |
| F-008 | `lightr gc` — the one janitor | R1 | mark-and-sweep from refs; dry-run default | R1 accept |

## F-1xx — Index & verbs (the warp core)

| F | Feature | Ring | Mechanism / budget | Acceptance |
|---|---|---|---|---|
| F-101 | Git-style stat-index (mmap, racily-clean safe) | R0 | `~/.lightr/index/<root-hash>`; never pollutes user tree | A5 unit |
| F-102 | `snapshot` ≤100 ms warm @10k files | R0 | parallel ignore-walk + index; rehash only deltas | A1, bench B5 |
| F-103 | `hydrate` via CoW clone (R0) → O(1) view (R2) | R0/R2 | R0: parallel clonefile tree (≤150 ms/10k); R2: mounted view + solidifier | A1, bench B3 |
| F-104 | `status` ≤50 ms warm @10k | R0 | index diff | A5 |
| F-105 | `run` memoized, exit-code passthrough, HIT/MISS stderr | R0 | key = index digests of inputs ⊕ cmd ⊕ env ⊕ platform; exit-0-only memo; ≤5 MiB output cap | A2, A3 |
| F-106 | Memo replay ≤10 ms | R0 | AC record + stdout stream; sync core, no runtime spinup | bench B4 |
| F-107 | No-daemon invariant | R0 | nothing alive between invocations | A4 |
| F-108 | Offline-absolute (zero network code in core) | R0 | no net deps linked in core crates | A6 |
| F-109 | CLI overhead <5 ms (`--version`) | R0 | lazy init, no config read on hot verbs | bench B1 |

## F-2xx — Execution engines

| F | Feature | Ring | Mechanism / budget |
|---|---|---|---|
| F-201 | `native` engine (<5 ms, "not a sandbox" said loudly) | R0 | posix_spawn; run-owned control dir |
| F-202 | `exec/logs/ps/stop` peer-to-peer | R1 | per-run ctl socket, dies with process tree |
| F-203 | Resource limits | R1 | rlimits (native) / VM config (vz) |
| F-204 | `ns` engine (Linux, ~10–20 ms, no overlayfs) | R2 | clone3+pivot_root into CoW tree; cgroup v2; seccomp default |
| F-205 | `vz` engine: boot-once-per-machine, resume ~100–300 ms | R2 | golden state minted at install (background); per-project warm states |
| F-206 | Apple-derived guest kernel + Rust PID1 (~1 MB) | R2 | Containerization kernel base; virtiofs; balloon 128–256 MB |
| F-207 | Guest-side views over immutable store | R2 | virtiofs store mount + composefs view inside guest (boundary = cache, not protocol) |
| F-208 | Rosetta x86 images near-native | R2 | VZLinuxRosettaDirectoryShare; boot path (no resume) until Apple allows |
| F-209 | `fc` engine (cloud, ~5 ms resume) | R4 | Firecracker on Runners fabric, same Engine trait |

## F-3xx — OCI & ecosystem compat

| F | Feature | Ring | Mechanism |
|---|---|---|---|
| F-301 | `oci import/export` (layers unpacked once → refs) | R2 | registry client (quarantined async crate) |
| F-302 | Registry push/pull | R2 | OCI distribution spec |
| F-303 | Volumes/binds parity | R1 | dirs + CoW clones; named volumes = refs |
| F-304 | Networking parity at OrbStack's bar | R2 | per-container DNS, VPN coexistence, `-p` publish |
| F-305 | `lightr compose` lazy (listeners + resume-on-first-packet) | R3 | socket activation default; per-stack ephemeral supervisor (TTL) |
| F-306 | `lightr build` Dockerfile-compat, step-memoized | R3 | FS-view dependency oracle (Linux); spawn-shim nitro (macOS) |
| F-307 | docker CLI translation + ephemeral socket shim | R3 | `lightr docker …`; testcontainers-compatible, session-scoped |
| F-308 | Restart policies via OS supervisor | R3 | generated launchd/systemd units |
| F-309 | Healthchecks, secrets, configs | R3 | run-spec features, store-backed |

## F-4xx — Beyond Docker (the R4 ring they can't enter)

| F | Feature | Mechanism |
|---|---|---|
| F-401 | `undo` / `diff @ref@{time}` | lineage walk, view diff |
| F-402 | `bisect` with memoized tests | O(log n) over snapshot history, mostly AC hits |
| F-403 | Deep-memo nitro (`--deep-memo`) | process-tree memo via FS oracle / spawn-shim |
| F-404 | LAN mesh cache (peer store discovery) | mDNS; try-peer-before-remote |
| F-405 | Stage-2 sync (CoreLink push/pull) | clw FastCDC bridge, background |
| F-406 | Run-state snapshot/restore as refs | vz/fc states content-addressed |

## F-5xx — Agent-first surface (LLM-native, cross-ring)

| F | Feature | Ring | Mechanism |
|---|---|---|---|
| F-501 | `--json` on every verb (versioned schemas, terse) | R0 seed | machine output, TTY detection, no banners |
| F-502 | `--explain` (why-hit, what-materialized, CoW rung) | R0 seed | structured self-narration |
| F-503 | `plan` dry-run on mutating verbs | R1 | preview without side effects |
| F-504 | `--events ndjson` stream | R1 | orchestrator telemetry |
| F-505 | `lightr mcp` (runtime as MCP server) | R3 | run/snapshot/hydrate/diff as agent tools |
| F-506 | Agent sandbox profiles (attested, memoized) | R3/R4 | vz/fc + content-addressed inputs/outputs |
| F-507 | Determinism-as-trust (verify what ran, byte-for-byte) | R0+ | content addressing end-to-end |

## F-6xx — Product & distribution

| F | Feature | Ring | Mechanism |
|---|---|---|---|
| F-601 | Single binary ≤10 MB (mac arm64, linux x86_64/arm64) | R0 | LTO, strip; Linux pack (kernel+initramfs) lazy ref, not bundled |
| F-602 | `lightr bench --vs-docker` in the binary | R0 | indicator table on the user's machine; marketing weapon |
| F-603 | Microwave clause floor (1 core/512 MB/any POSIX FS) | R0 | copy-rung fallback; bench reports rung |
| F-604 | brew/curl/gh-releases, signed + notarized | gated | after ADR-0008 (license) + Runners M1 |
| F-605 | Zero telemetry (ADR-0007) | R0 | no network code, period |

## Budgets index (CI gates)

B1 `--version` <5 ms · B2 memo-hit run ≤10 ms · B3 hydrate(CoW) 10k files
≤150 ms warm · B4 replay ≤10 ms · B5 snapshot 10k warm ≤100 ms · B6 status
10k warm ≤50 ms · B7 binary ≤10 MB · B8 idle RSS between runs = 0 (no
process). Spike-calibrated additions: view-mount O(1) (R2), vz resume
(R2), compose-up ms (R3).
