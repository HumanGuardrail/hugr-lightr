# S5-NET — vz container networking: expected assertions + parity mapping

**Status (2026-06-18, Intel x86_64, macOS):** `run.sh` ran **GREEN** end-to-end
on this box — all 9 assertions PASS. This closes the `vz` half of **F-304**
(`-p` for a Linux image). Re-run `bash spikes/s5-vz-net/run.sh` to reproduce; it
exits 0 only when every assertion below passes.

This is the networking sibling of `spikes/s5-vz-boot` (F-205/F-206, vz boot). It
proves the flagship Docker-parity case: **run a real Linux container with a
published port on a Mac, reach its server from the host, and tear it down
cleanly** — the case the Phase-1 guard used to reject as "Phase 2".

---

## What the harness proves

| Step | Assertion | Mechanism exercised |
|------|-----------|---------------------|
| 1–3 | toolchain present; `--features vz` builds; binary codesigned with `com.apple.security.virtualization` | the vz CLI is buildable + entitled (VZ refuses to start a VM without the entitlement) |
| 4 | a linux pack is installed whose `initrd` is the **current** `lightr-init` | the guest PID1 is this source tree's build — i.e. it contains `publish_ip` (writes the guest IP to `IP_FILE`); a stale initrd would never publish an IP |
| 5 | an `alpine` rootfs ref exists in the store | a real Linux image to boot as the container rootfs |
| 6 | `lightr run -d -p 18080:80 --engine vz --rootfs alpine -- <nc server>` prints `id=…` and returns immediately | the **detached vz path** routes through `spawn_detached_engine` → the supervisor boots the VM in-process (it does NOT block the CLI, unlike the old synchronous engine path that ignored `-d`) |
| 7 | `curl 127.0.0.1:18080` returns `lightr-vz-net` (the in-guest server's fixed 200 response) | END-TO-END: kernel `ip=dhcp` leased a NAT IP → guest PID1 published it to `IP_FILE` → the supervisor read it (`192.168.64.x`) and started a userspace forwarder `127.0.0.1:18080 → guest:80` → the host TCP round-trips through the forwarder into the busybox `nc` server inside the microVM |
| 8 | `lightr stop <id>` ⇒ status `exited`, and `curl` afterwards gets nothing | the supervisor's ctl handler writes the guest `EXIT_FILE`; the shim polls it and force-stops the VM (no new shim code); the forwarder is dropped (listener closed) ⇒ the published port is closed |
| 9 | no `lightr __supervise` process remains | the in-process VM dies with the supervisor; nothing is leaked (daemonless invariant holds for the container modality too) |

## Parity-audit row

**F-304** — networking (`-p`). The native detached path shipped in Phase 1
(`acceptance_net.rs`). This spike closes the **vz** path: `-p` for a Linux image
on macOS via the microVM, with a host→guest userspace forward and clean
teardown. Remaining Phase-2 items (container↔container networks, DNS/service
discovery, `-P`, udp, Linux `ns` veth/bridge) are tracked in `parity-audit.md`.

## Honesty notes

- vz virtualizes the **native** arch (Intel → x86_64 guest; this run was
  x86_64). The arm64 guest path shares the code but is validated separately
  (hardware-gated, `spikes/s5-vz-boot-arm64`).
- `lightr stop` exits with the stopped run's code (`143` = 128+SIGTERM), exactly
  like the native detached path — the harness asserts the closed port + `exited`
  status, not stop's own exit code.
- The in-guest server here is a fixed-response busybox `nc` loop (deterministic
  body for the assertion). A real image (nginx, etc.) publishes the same way —
  the forward is content-agnostic TCP.
