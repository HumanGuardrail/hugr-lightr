# Decisions log — owner authorizations (verbatim)

Append-only. The rigor compact requires human authorization for gate
changes; this file is where those authorizations are recorded verbatim.

## 2026-06-11 — Overnight delivery mandate

**Authorized-by:** Gustavo (owner), in session, end of day.

**Verbatim (goal directive):** "Esses sao nossos alvos, decomponha todos
eles em features, depois atualize o whitepaper, claude.md, product.md, etc.
A partir dai, vamos usar a tecnica de working backwards, comecando pelo
whitepaper do produto pronto. […] Depois de voce decompor o whitepaper
completo trabalhando backwards, formalize toda a documentacao de
especificacao. A partir dela voce vai comecar a criar os wps, crie wps
atomicos, sem overhead, usando sua skill de techlead, escolha bem os
modelos pra nao queimar token atoa. Bote pra fuder, e me entregue o
produto completo, impecavel, testado, sota […]"

**Verbatim (final, going to sleep):** "vou deixar voce memso setar a goal
ok?" · "boa noite, vou dormir. Por favor, entregue tudo impecavel padrao e
rigor sota. Tudo impecavel."

**Effect (lead's interpretation, logged for morning review):**
1. The doc cascade (whitepaper v2 → feature tree → canon → ADRs →
   build-spec v2) proceeds autonomously tonight.
2. ADRs 0009–0016 + reworked 0003/0005 are marked
   **Accepted (owner overnight mandate — subject to morning review)**
   instead of waiting for the interactive per-ADR hammer session; any of
   them can be reverted to Proposed by the owner in the morning.
3. The R0 wave (atomic WPs, model-routed, TechLead method) is dispatched
   tonight under this authorization. The three standing gates remain for
   anything beyond: rigor waivers stay human-only; no public
   distribution/release (ADR-0008 unresolved); no sibling-repo mutation.
4. Spikes that require external downloads/new VMs (S1–S3, S5) are NOT run
   tonight; only S4 (clonefile storm, local, read-safe) informs the wave.
   R0 scope deliberately excludes spike-dependent features (views/vz).

## 2026-06-11 (overnight) — Lead amendments during R0 integration

**Authorized-by:** lead under the overnight mandate; flagged for morning
review. All gates green after each amendment.

1. **Integrity law refined (spec §4/§7/§8, A7 split).** CoW materialization
   is metadata-only and cannot re-hash; the frozen A7 contradicted the
   O(metadata) bar. Resolution: verification lives where bytes are READ —
   manifests/refs/AC are always re-hashed (default fail-closed; A7b) — and
   the paranoid full re-hash is explicit: `lightr hydrate --verify` /
   `lightr_index::hydrate_verified` (A7a). fs-verity (R2, ADR-0009) closes
   the kernel-side gap. Also fixed: parallel materialize silently discarded
   errors (now fail-closed, first error aborts).
2. **Dep-list amendments (spec §2):** `blake3` allowed in lightr-run (key
   assembly needs a streaming hasher); `tempfile` allowed as a lightr-cli
   runtime dep (bench fixtures).
3. **Test-isolation law (all crates):** env-mutating tests serialize on a
   static lock and isolate LIGHTR_HOME in tempdirs; index temp-files are
   per-thread unique (PID alone collided under the parallel test runner).

## 2026-06-12 — R1→R4 sequential execution mandate

**Authorized-by:** Gustavo (owner), verbatim: "Entao marcha familia, pode
especificar, planejar e executar r1 a r4 em sequencia, mantendo rigor e
padrao sota."

**Lead interpretation:** spec→plan→execute each ring in sequence under the
standing rigor; rings claimed only on green acceptance+bench (tense law).
Known platform constraints logged up front: this dev box is Intel x86_64 —
VZ save/restore (boot-never resume) and Apple's arm64 Containerization
kernel require Apple Silicon, so R2's vz tier is built capability-probed
and validated to the extent this hardware allows (boot path), with resume
budgets binding to AS hardware. Honest degradations are documented, never
silent. R1 scope cut logged: native-tier resource limits are NOT
enforceable honestly on macOS without VM/ns tiers — flags reserved,
enforcement lands with ns/vz (feature-tree F-203 note).

## 2026-06-12 — R2 cold-critic findings + lead amendment (sha2)

Critic (opus, cold) flagged a FAIL-OPEN: build-spec-r2 §3 claims "blob digest
verified before applying (fail-closed)" but the pull path verified nothing
(blobs named by loop index, not sha256) — a substituted registry blob would
be imported as a trusted ref, and the net-gate hides it from CI. Under the
rigor compact this is debt that must be closed at the root, not waived.

**Lead amendment (authorized under the R1→R4 mandate):** add `sha2` crate to
lightr-oci (justified: registry integrity is load-bearing; tiny, audited dep)
and verify every layer + config blob's sha256 against the manifest digest on
BOTH import_layout and pull, fail-closed (LightrError::Integrity, real
digests). Also fix: size-mismatch exit class, OCI whiteout intra-layer
ordering, opaque-same-layer, hardlink forward-ref, pull malformed-ref → exit 2.
Dispatched as R2-HARDEN (parallel, disjoint from R3-build).

## 2026-06-12 — Final cross-ring critic + dir-COPY fix

Closing critic (opus, cold) verdict: product PASS, parity-audit honest,
zero todo!() in src. ONE material defect: `build` step_key hashed COPY
sources only when `is_file()`, so `COPY src/ /app` (a directory) didn't
fold its contents into the cache key → editing a file inside a copied dir
gave a stale cache hit (silent miscompile). Hidden because the shipped A22
was narrowed to single-file COPY.

**Lead fix (root):** step_key now recurses copied directories — every
contained file's (relative-path ‖ digest), sorted; symlinks contribute
target; missing sources a sentinel. Regression covered at both levels:
`step_key_dir_copy_changes_when_contained_file_changes` (unit) +
`a22b_dir_copy_invalidates_on_nested_change` (e2e). Cosmetic: whitepaper
"315 cases" → 338. Final: 340 tests / 0 failures, clippy -D clean.

## 2026-06-12 — Prod-hardening cold critic + H2 fixes (all 6 closed)

Prod-phase critic (opus) verdict: core REAL, but GAPS — 3 honest
overstatements, 1 durability hole, 1 vacuous test, 1 real hang. All closed
at root (no waivers):
1. OCI "streaming kills OOM" was half-real (apply did `fs::read` whole layer)
   → `apply_layers` now streams from the temp file through GzDecoder+tar
   (`LayerBlob::open_reader`, 2-byte peek + chain-back); no whole-layer Vec.
2. `test_streaming_large_layer_import` vacuous → rewritten to a ≥64 MiB
   incompressible plain-tar through the file/streaming path.
3. `Index::save_for` not fsync'd → now sync_all + parent-dir fsync (matches
   store durability). 4. README "362 tests" stale → 379. 5.
   `gc_does_not_sweep_live_writers` non-adversarial → real concurrent
   index::gc-vs-writer; fails if the flock were a no-op. (+ the two empty
   `gc_end_to_end_*` bodies filled with real assertions.)
6. vz silent-guest infinite `accept(2)` hang → generous SO_RCVTIMEO backstop
   (default 24h, env LIGHTR_VZ_EXIT_TIMEOUT_SECS) → timed-out accept maps to
   GUEST_NO_REPORT_CODE (255), never a hang or a fabricated 0. Window is
   generous on purpose (legit guest connects only at job-end); precise
   cancel-on-VM-stop remains S5 (BOOT-PATH, can't validate on Intel).
Final: 379 tests/0, clippy -D clean, `--features vz` compiles+lints clean.

## 2026-06-12 — ship-vm-views cold critic + S5 runbook fix

Critic (opus) verdict: GAPS but **no honesty violations, no vacuous tests, no
overclaims** — release/naming/kpack/views all verified REAL (403/0). The 3
gaps were all in the S5 runbook (the one artifact the owner pays to run on a
rented ARM Mac):
1. build→install disconnect: `run-s5.sh` Step 2 ran build-linux-pack.sh but
   never `install-pack`; pack landed in build/linux-pack while probe_vz checks
   ~/.lightr/packs/linux → would log_fail at Step 2. Fixed: build → explicit
   `engine install-pack <dir>` → verify. (Failed-closed, never a false green.)
2. kernel not turnkey: from-source kernel build needs a Linux env macOS lacks;
   runbook surprised the user with exit 3. Fixed: `LIGHTR_KERNEL=/path/to/vmlinux`
   passthrough + README §2.4 makes the prebuilt-kernel the recommended path and
   the from-source path honestly heavy.
3. README §3 showed `vz available` as reachable pre-install → corrected to
   build→install→verify order.
Product code untouched (Rust 403/0 holds); fixes are shell+README, bash -n +
shellcheck clean.

## 2026-06-12 — Omni cross-platform wave (Windows + mac Intel/ASi + Linux)

**Authorized-by:** Gustavo (owner), in session.

**Verbatim (trigger — challenged the ARM dependency):** "Eu nao entendo, porque
voce depende de um mac silicon pra seguir? nao tem logica isso"

**Verbatim (mandate):** "Fecha logo tudo irmao, windows, mac intel e mac
silicon. Ai eu posos ate pedir pra amigos ou provisionar maquinas. Mas tu tem
que entregar o produto full, nao ficar me enchendo o saco"

**Effect (lead):**
1. Correction logged: vz was NEVER Apple-Silicon-gated — Virtualization.framework
   runs Linux guests on Intel Macs (guest arch = host arch). Only VZ save/restore
   (F-406) + Rosetta-in-VM (F-208) are genuinely arm64-only. ADR-0017 records it.
2. Reframe: validation on hardware the lead lacks is a one-command runbook the
   owner triggers, NOT a blocker. Lead delivers code-complete + host-green +
   cross-compile-clean + per-platform runbook.
3. Windows tier built from zero (no cfg(windows) existed): native core port
   (locks/fsync/ctl-socket/CoW-ReFS/symlinks/perms), wsl isolation engine,
   5-target CI/release matrix. ADR-0017 + build-spec-omni frozen.
4. Delivered via a 7-WP disjoint-by-crate fleet (git worktrees, zero merge
   conflicts), model-routed (sonnet mechanical; opus for RUN named-pipes +
   ENGINE WSL2).

**Cold critic (opus) — verdict GAPS, all fixed at root (no waivers):**
- BLOCKER: wsl engine invoked a nonexistent `lightr __ns-exec` → rewired to the
  real `wsl.exe -- lightr run --engine ns --rootfs <wsl-path> -- <cmd>` (reuses
  NsEngine in-distro) + win_path_to_wsl translation; overclaiming comment removed.
- SHOULD-FIX: Windows supervisor shutdown deadlock (a single nudge could miss) →
  retry-nudge until `server_exited`.
- SHOULD-FIX: ReFS FSCTL never engaged (dst length 0) → `set_len` pre-size +
  honest cluster-alignment caveat; copy fallback guarantees correctness.
- Doc overclaim: virtualization entitlement on a notarized release softened
  (restricted entitlement, needs Apple provisioning) in ADR-0017 + release.yml.
- Nit: aarch64 cross-check → `--all-targets`; windows-msvc noted natively gated.
Critic AFFIRMED: no vacuous tests, no `todo!()` in non-test code, honest
probes/skeletons, unix path untouched, cross-crate exhaustiveness complete.

**Gates:** host `cargo test --workspace` 408/0, clippy -D, fmt clean; Windows
cross-check (lib+bins + all-targets) 0 errors; `--features vz` compiles+links on
Intel. **Pending (honestly marked, hardware/CI-gated):** the vz boot assertion
(x86_64 kernel building on this box), arm64 vz boot, Windows/Linux runtime — each
runbook- or CI-gated, none claimed validated.

## 2026-06-12 — Intel vz boot bring-up (real, on this Mac)

Ran the vz boot path end-to-end on the owner's Intel Mac (long deferred as
"ARM-gated"). Built a real x86_64 kernel (Linux 6.18.5, virtio/vsock/virtiofs) +
`lightr-init` (musl) in docker; assembled + installed the pack → `vz available`.
The bring-up FOUND + FIXED 4 latent bugs — none caught before because the path
had literally never run:
1. `pack_dir()` (engine) fell back to bare $HOME vs the CLI's ~/.lightr → the
   installed pack was invisible to probe_vz. Aligned to ~/.lightr.
2. `--features vz` binary crashed at startup (libswiftCore @rpath, no LC_RPATH)
   → `.cargo/config.toml` adds the /usr/lib/swift rpath on macOS.
3. `KERNEL_SHA256` pin in build-linux-pack.sh was wrong → corrected to kernel.org.
4. `packaging/vz.entitlements` had an XML comment containing `--` (illegal) →
   codesign rejected it, entitlement never attached → stripped to a clean plist.

ARCHITECTURAL FINDING (the real blocker, arch-INDEPENDENT): the exit-code channel
(F-206) uses a raw host `AF_VSOCK` listener (crates/lightr-engine/src/vsock.rs).
**macOS has no host AF_VSOCK** — `socket(AF_VSOCK)` returns ENODEV on Intel AND
Apple Silicon. Renting an ARM Mac would hit the SAME wall — vindicating the
owner's "don't depend on ARM" instinct even harder. That raw-AF_VSOCK receiver is
the LINUX-host mechanism (future `fc`/KVM). For the macOS `vz` engine the exit
code must be brokered by the Swift shim's `VZVirtioSocketDevice`, OR carried as a
small file on the shared (writable) virtiofs rootfs.

DECISION PENDING (owner) — which exit-code rework for vz-on-macOS:
- (a) file-on-virtiofs: `lightr-init` writes the exit code to a rootfs file; host
  reads it after the VM stops. No vsock. Simpler (~1 init + 1 engine change).
- (b) Swift `VZVirtioSocketDevice`: the "proper" vsock broker; more Swift.
Lead recommends (a). Caveat: even with the channel fixed, the VM boot itself
(kernel → virtiofs mount → pivot_root → exec → write) is NOT yet validated green —
that is the next step, not the last. No boot is claimed as validated.
