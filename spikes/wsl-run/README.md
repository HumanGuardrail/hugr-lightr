# WSL2 Engine Validation — Runbook

**Owner/hardware-gated.** This runbook requires a real **Windows 10/11 box
with WSL2 enabled and at least one registered distro**. It cannot be run on
macOS or Linux — the WSL2 engine is a Windows-only path. Until it passes
green on real Windows hardware, the WSL2 engine path remains `// WIN-PATH`
(unvalidated).

---

## Prerequisites

| Item | Requirement |
|------|-------------|
| OS | Windows 10 (Build 19041+) or Windows 11 |
| WSL2 | Enabled: `wsl --install` or via Windows Features |
| Distro | At least one registered distro (e.g. Ubuntu) — `wsl -l -q` must list it |
| Linux `lightr` | A `lightr` Linux binary installed **inside** the default WSL2 distro, on its `PATH` (the WSL2 engine invokes it via `wsl.exe -- lightr run --engine ns …`) |
| `lightr.exe` | The Windows build of `lightr` on PATH, or built at `target/release/lightr.exe` via `cargo build --release` |
| Alpine rootfs | One of: (a) set `ALPINE_TAR=/path/to/alpine.tar`; (b) `skopeo` in WSL2; (c) Docker Desktop running |

**No build step is required if you already have `lightr.exe` on PATH.** The
harness locates it automatically.

---

## How to Run

From WSL bash (inside the repo) or Git-Bash on Windows:

```bash
bash spikes/wsl-run/run-wsl.sh
```

To supply a pre-saved Alpine tar (avoids skopeo/docker):

```bash
ALPINE_TAR=/path/to/alpine.tar bash spikes/wsl-run/run-wsl.sh
```

Exits 0 on all-pass; non-zero on the first failure. Failure prints the
assertion that failed, the actual vs. expected value, and stops immediately.

---

## What Each Assertion Proves

| Assertion | Command | Pass condition | What it closes |
|-----------|---------|----------------|----------------|
| A1 | `lightr run --engine wsl --rootfs alpine -- /bin/echo wsl-ok` | exit 0 AND stdout contains `wsl-ok` AND exit != 255 | The full WSL2 engine round-trip: `lightr.exe` hands off to `wsl.exe`, the in-distro `ns` engine runs the command, exit 0 returns intact |
| A2 | `lightr run --engine wsl --rootfs alpine -- /bin/sh -c 'exit 7'` | exit code == 7 | Real exit-code passthrough: the guest's code travels `ns engine → wsl.exe → lightr.exe` without fabrication; 0 or 255 would fail this |

Exit 255 is never a pass — it indicates either a WSL invocation failure or
that the in-distro process never reported its exit code.

---

## Notes

- The WSL2 engine (`wsl_impl::WslEngine`) translates the Windows rootfs path
  to its `/mnt/<drive>/…` WSL2 view, then runs `lightr run --engine ns
  --rootfs <wsl-path>` inside the default distro. The `ns` model
  (unshare + pivot_root) provides the actual isolation.
- This runbook is **owner/hardware-gated**: it must be run by the owner (or a
  friend with a Windows box) on real Windows+WSL2 hardware. The lead cannot
  substitute a CI emulation for this path.
- Related: `spikes/s5-vz-boot/run-s5.sh` is the macOS vz-engine analog of
  this runbook.
