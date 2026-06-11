# S5 vz-boot-arm64 — Expected Assertions and Parity-Audit Mapping

**Honesty statement:** This file describes what each assertion in `run-s5-arm64.sh`
proves and which parity-audit row it closes. None of these assertions have
been executed yet. Until `run-s5-arm64.sh` runs green on a real ARM Mac (EC2
`mac2.metal` / `mac2-m2.metal` or MacStadium M1/M2), **F-205 and F-206
remain yellow (unvalidated).**

---

## Assertion 1 — `lightr run --engine vz @img/alpine -- /bin/echo s5-boot-ok`

### What it asserts

1. Exit code is 0 (not 255, not any other non-zero value).
2. Standard output contains the string `s5-boot-ok`.
3. Exit code is explicitly NOT 255 (`GUEST_NO_REPORT_CODE`).

### What it proves

| Sub-check | Mechanism exercised | Proof |
|-----------|---------------------|-------|
| Exit 0 received | The full boot chain ran end-to-end: kernel loaded by Virtualization.framework, `lightr-init` PID1 mounted the rootfs, spawned `/bin/echo`, collected its exit status (0), wrote a 4-byte little-endian frame `[0, 0, 0, 0]` to `AF_VSOCK` at `CID_HOST:1024`, and the host `VsockExitReceiver` read it via `read_exit_frame`. `VzEngine::run` returned that `i32(0)`. | The exit code 0 came off the wire; it was not fabricated. The Swift shim no longer contains `exitCode = 0` (verified by test `swift_shim_has_no_fabricated_exit_code_zero`). |
| `s5-boot-ok` in stdout | The guest command actually executed inside the VM and its stdout was forwarded to the host (virtiofs or virtio-serial channel). | Confirms the guest filesystem mount and process execution are real. |
| Exit != 255 | `GUEST_NO_REPORT_CODE` (255) is the engine's explicit sentinel for "VM stopped but PID1 never sent a vsock frame". Receiving 0 instead of 255 proves the vsock channel is live and the frame was delivered. | Distinguishes a real success from a silent VM death where the engine falls back to 255. |

### Parity-audit row closed

**F-205** — `vz` engine boots a real microVM and runs a guest command end-to-end via
Virtualization.framework on Apple Silicon.

Status: `🟡` (unvalidated) until this assertion passes on ARM.
Status becomes `🟢` after a green run on ARM is recorded.

---

## Assertion 2 — `lightr run --engine vz @img/alpine -- /bin/sh -c 'exit 7'`

### What it asserts

Exit code received by the caller is exactly 7.

### What it proves

| Check | Mechanism exercised | Proof |
|-------|---------------------|-------|
| Exit 7 (not 0) | The engine does NOT fabricate a zero exit code. The old vz shim used `exitCode = 0` unconditionally. If that fabrication were still present, this assertion would receive 0, not 7. | The `read_exit_frame_is_the_sole_exit_code_source` test pins the source-level invariant; this assertion validates the full runtime path. |
| Exit 7 (not 255) | Exit 255 would mean `GUEST_NO_REPORT_CODE` — the guest ran but the vsock frame was never received. Receiving 7 proves the vsock channel carried the REAL non-zero code, not a fallback. | Distinguishes correct vsock delivery from a silent guest failure that maps to 255. |
| Exit 7 (not 1 or other) | The i32 frame is decoded correctly as little-endian via `read_exit_frame`. A decoding bug (e.g. big-endian, byte truncation) would produce a wrong value. | The frame tests in `vsock.rs` cover the parser; this runtime test covers the full path including the kernel, init, and vsock socket. |

### Code path traced

```
lightr CLI  →  VzEngine::run
              ├── VsockExitReceiver::bind()   (AF_VSOCK CID_ANY:1024 before boot)
              ├── lightr_vz_run(...)           (Swift shim boots VM; returns vm_status)
              │     vm_status = 0 (VM lifecycle: stopped cleanly)
              └── recv_handle.join()
                    └── VsockExitReceiver::recv()
                          └── read_exit_frame(&mut conn)
                                reads 4 bytes: [7, 0, 0, 0] (LE)
                                returns Ok(7)
              VzEngine::run returns Ok(7)
lightr exits with code 7
```

Inside the VM:
```
lightr-init PID1
  └── spawn_wait(["/bin/sh", "-c", "exit 7"], ...)
        └── /bin/sh exits with code 7
  └── VsockSink::write_exit_frame(7)
        └── writes [7, 0, 0, 0] to AF_VSOCK CID_HOST:1024
  └── PID1 exits
```

### Parity-audit row closed

**F-206** — Guest exit code flows accurately from the VM to the host process exit code;
no fabrication, no hardcoding, no truncation.

Status: `🟡` (unvalidated) until this assertion passes on ARM.
Status becomes `🟢` after a green run on ARM is recorded.

---

## Summary Table

| Assertion | Command | Expected result | Proves | Closes |
|-----------|---------|-----------------|--------|--------|
| A1 | `lightr run --engine vz @img/alpine -- /bin/echo s5-boot-ok` | exit 0, stdout has `s5-boot-ok`, exit != 255 | Full vz boot chain is live on arm64; vsock channel delivers real exit 0 | F-205 |
| A2 | `lightr run --engine vz @img/alpine -- /bin/sh -c 'exit 7'` | exit 7 (not 0, not 255) | Real guest exit code (non-zero) flows accurately over vsock; no fabricated value | F-206 |

**Until both assertions pass green on a real ARM Mac, F-205 and F-206 remain yellow.**

---

## Notes on the 255 Sentinel

`GUEST_NO_REPORT_CODE = 255` is defined in `crates/lightr-engine/src/lib.rs` (vz_impl).
It is returned when:
- The VM booted (`vm_status >= 0`) but
- The vsock receiver's `recv()` returned `Err(_)` — either the guest never connected,
  the connection closed before 4 bytes were written (short read), or the `SO_RCVTIMEO`
  backstop fired.

255 is never a fabricated success. It is an explicit "something is wrong" sentinel.
Assertion A1 checks `exit != 255` precisely to distinguish a real success from this fallback.
Assertion A2 checks `exit == 7` precisely to distinguish a real non-zero code from 255.

---

## Notes on the Codesign Step

The harness (Step 2 in `run-s5-arm64.sh`) ad-hoc codesigns the `lightr` binary with
`packaging/vz.entitlements` before any VM is allocated. This is required on ALL Macs —
Intel and Apple Silicon alike — to satisfy macOS's authorization check for
`com.apple.security.virtualization`. Without this step, `VZVirtualMachine` throws an
authorization error at allocation time, before any boot attempt.

The codesign uses an ad-hoc identity (`-s -`): no developer certificate is required,
making this runbook self-contained for any engineer on any Mac.
