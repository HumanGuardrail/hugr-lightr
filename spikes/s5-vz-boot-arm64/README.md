# S5 vz-boot-arm64 Validation — Provisioning Checklist

**Purpose:** Run this on a rented ARM Mac to validate the real Virtualization.framework
microVM boot path on Apple Silicon (arm64). Until it passes green, parity-audit rows F-205
and F-206 remain yellow.
This document does NOT claim a boot has been validated; it is the runbook to perform that
validation.

**Note:** This is the arm64 sibling of `spikes/s5-vz-boot/` (which targets the same ARM Mac
hardware but shares the same provisioning). The key differences are the `--arch aarch64` pack
build flag, the arm64 Alpine image, and the required ad-hoc codesign step that grants the
Virtualization.framework entitlement to the binary.

---

## 1. Provision a Target Machine

Both option A and option B below provide Apple Silicon hardware. This runbook requires
a physical ARM Mac running macOS 14+; Virtualization.framework cannot be emulated.

### Option A — AWS EC2 Dedicated Mac Host

| Item | Value |
|------|-------|
| Instance type | `mac2.metal` (M1) or `mac2-m2.metal` (M2) |
| AMI | Latest macOS 14 (Sonoma) AMI published by AWS |
| Minimum billing | **24 hours** — EC2 dedicated Mac hosts have a 24-hour minimum tenancy; you are billed for 24 h even if you stop the instance after 5 minutes |
| On-demand price (us-east-1, 2026) | ~$0.65/hr for `mac2.metal`; ~$0.87/hr for `mac2-m2.metal` — verify current pricing at https://aws.amazon.com/ec2/pricing/on-demand/ before provisioning |
| Total minimum cost | ~$15.60 (`mac2.metal`) or ~$20.88 (`mac2-m2.metal`) for the forced 24-hour lock-in |
| EBS volume | 100 GiB gp3 (Xcode + build cache) |
| Security group | Allow inbound SSH (port 22) from your IP |

Steps:
1. In the AWS console (or via `aws ec2 allocate-hosts`), allocate a Dedicated Host with instance family `mac2` or `mac2-m2` in a supported AZ.
2. Launch an instance on that host with the macOS 14 AMI.
3. Wait for the instance to reach the "running" state and pass both status checks (can take 10–15 minutes on first boot).
4. SSH in: `ssh -i <key.pem> ec2-user@<public-ip>`

### Option B — MacStadium Dedicated Mac

| Item | Value |
|------|-------|
| Hardware | Mac Mini M1 or M2 (request explicitly) |
| OS | macOS 14+ (request explicitly; MacStadium will provision) |
| Billing | Hourly or monthly plans — check https://www.macstadium.com/pricing |
| Typical hourly | ~$0.50–$0.80/hr; no forced multi-day minimum like EC2 |
| Access | SSH or Orka platform |

Steps:
1. Sign up at https://www.macstadium.com and provision an M1/M2 Mac with macOS 14+.
2. SSH in using the credentials MacStadium provides.

---

## 2. Machine Prerequisites

Run all steps below on the target ARM Mac.

### 2.1 Verify macOS version

```
sw_vers -productVersion
# must be 14.x or later — Virtualization.framework requires macOS 12+;
# macOS 14 is the supported target for this spike
```

### 2.2 Install Xcode (required for swiftc)

Virtualization.framework bindings are compiled from Swift (`shim/vz.swift`).
`swiftc` ships with Xcode, not with the standalone Command Line Tools.

```
# Install Xcode from the App Store, or via xcodes (faster):
brew install robotsandpencils/made/xcodes
xcodes install --latest
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
xcodebuild -version   # must print Xcode 15+ / Build version 15x
swiftc --version      # must print something — not "command not found"
```

### 2.3 Install Rust toolchain

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source "$HOME/.cargo/env"
rustup target add aarch64-unknown-linux-musl   # guest init cross-compile target
rustup show   # confirm stable aarch64-apple-darwin is active
```

### 2.4 Toolchain + kernel for the pack (READ — not fully turnkey)

The pack = a Linux **kernel** + an **initrd** (with `lightr-init` as `/init`).
Two prerequisites, one of which is not native to macOS:

**(a) lightr-init cross-compile (musl) — easy:**
```
rustup target add aarch64-unknown-linux-musl
brew install filosottile/musl-cross/musl-cross   # provides aarch64-linux-musl-gcc
aarch64-linux-musl-gcc --version
```

**(b) the kernel — pick ONE (the from-source path needs a Linux builder):**
- **Recommended (turnkey): bring a prebuilt `vmlinux`.** Build/obtain an
  arm64 kernel once with the Apple Containerization config
  (`github.com/apple/containerization`, `kernel/config-arm64`; virtiofs +
  AF_VSOCK enabled), then point the harness at it:
  ```
  export LIGHTR_KERNEL=/path/to/vmlinux
  ```
  `build-linux-pack.sh` skips its from-source build and uses this.
- **From source (heavy):** `build-linux-pack.sh` fetches + sha-verifies Linux
  6.18.5 but **does not compile a kernel on macOS** — kernel `make` needs a
  Linux build environment (docker/lima or a Linux box). The script detects
  the missing toolchain and exits with instructions rather than faking a
  kernel. If you go this route, build the kernel in a Linux container/VM
  (inside an `linux/arm64` container) and export `LIGHTR_KERNEL` to the result.

> Honest note: until a `vmlinux` exists (via either path), Step 2 below will
> stop with a clear message — it never proceeds with a fake kernel.

### 2.5 Verify packaging/vz.entitlements exists (REQUIRED for codesign)

The harness will ad-hoc codesign the `lightr` binary with the Virtualization.framework
entitlement before running any VM. This requires `packaging/vz.entitlements` to exist
in the repo root.

```
# In the repo root:
ls packaging/vz.entitlements
# Must print: packaging/vz.entitlements
# If it is missing, the harness will exit with a clear error.
# This file is created by the wave lead — do not create it yourself.
```

The entitlement grants `com.apple.security.virtualization` which macOS requires for
any process that calls Virtualization.framework APIs. Without this, `VZVirtualMachine`
throws an authorization error on Apple Silicon.

### 2.6 Install skopeo (or have docker available)

The `run-s5-arm64.sh` script imports an arm64 Alpine OCI image. Provide one of:

- **skopeo** (no daemon needed, recommended):
  ```
  brew install skopeo
  skopeo --version
  ```
- **docker** (alternative):
  ```
  # Use Docker Desktop for Mac, then:
  docker pull --platform linux/arm64 alpine
  docker save alpine > /tmp/alpine.tar
  ```

### 2.7 Clone the repo

```
git clone https://github.com/humangr/hugr-lightr.git
cd hugr-lightr
```

---

## 3. Exact Command Sequence

Run in order. All commands assume `cwd = hugr-lightr/`.

```bash
# Step 1 — Build lightr with the vz feature (requires Xcode + swiftc)
source "$HOME/.cargo/env"        # founder-Mac PATH workaround: rustup not on $PATH by default
cargo build --release --features vz

# Step 2 — Ad-hoc codesign with the Virtualization.framework entitlement
#   macOS requires this on ANY Mac (Apple Silicon and Intel) before a process
#   can call VZVirtualMachine. Without it, the first VM allocation fails with
#   an authorization error.
#   packaging/vz.entitlements must exist (see §2.5).
codesign -s - \
    --entitlements packaging/vz.entitlements \
    --force \
    ./target/release/lightr
codesign -dv ./target/release/lightr 2>&1 | grep -E "Entitlements|virtualization"
# Expected: com.apple.security.virtualization = true

# Step 3 — Build the pack, THEN install it (two distinct steps)
#   The build script assembles kernel+initrd into ./build/linux-pack — it does
#   NOT install. install-pack then validates (verify_pack) + copies the pack to
#   ~/.lightr/packs/linux (the path the vz engine probes). Bring a kernel per §2.4:
export LIGHTR_KERNEL=/path/to/vmlinux      # see §2.4(b); omit only if building from source in a Linux env
bash scripts/build-linux-pack.sh --arch aarch64 --out ./build/linux-pack \
     ${LIGHTR_KERNEL:+--kernel "$LIGHTR_KERNEL"}
./target/release/lightr engine install-pack ./build/linux-pack

# Step 4 — Verify the engine sees the pack (only AFTER install-pack)
./target/release/lightr engine ls
# Expected line:  vz    available    vz engine ready (pack: ~/.lightr/packs/linux)

# (Or skip Steps 1–4 manual sequence and just run the harness, which does all
#  of this + the boot assertions: `bash spikes/s5-vz-boot-arm64/run-s5-arm64.sh`)

# Step 5 — Import a tiny arm64 Alpine OCI image
# Option A — skopeo (no docker daemon):
skopeo copy \
    --override-arch arm64 \
    --override-os linux \
    docker://alpine:latest oci:/tmp/alpine-oci-arm64
./target/release/lightr oci import /tmp/alpine-oci-arm64 --name alpine

# Option B — docker save (if Docker is available):
docker pull --platform linux/arm64 alpine
docker save alpine > /tmp/alpine.tar
./target/release/lightr oci import /tmp/alpine.tar --name alpine

# Step 6 — Run the validation harness
bash spikes/s5-vz-boot-arm64/run-s5-arm64.sh
```

---

## 4. Expected Output

`run-s5-arm64.sh` prints a structured PASS/FAIL summary. A green run looks like:

```
[S5-ARM64] Building lightr --features vz ...                          PASS
[S5-ARM64] Codesigning lightr with vz entitlement ...                 PASS
[S5-ARM64] Building + installing linux pack (aarch64) ...             PASS
[S5-ARM64] Importing arm64 Alpine OCI image ...                       PASS
[S5-ARM64] Assertion 1: echo exits 0, stdout has 's5-boot-ok', not 255 ...   PASS
[S5-ARM64] Assertion 2: guest 'exit 7' flows as real exit code 7 (not 0, not 255) ...   PASS
[S5-ARM64] ─────────────────────────────────────────
[S5-ARM64] ALL ASSERTIONS PASSED — F-205 / F-206 CLOSED (on this ARM host)
```

Any failure prints the failing assertion, actual vs. expected values, and exits non-zero.

---

## 5. Pass / Fail Criteria

| # | Assertion | Pass | Fail |
|---|-----------|------|------|
| A1 | `lightr run --engine vz @img/alpine -- /bin/echo s5-boot-ok` | exit 0 AND stdout contains `s5-boot-ok` AND exit != 255 | exit 255 (no-report fallback hit — vsock chain broken) OR exit != 0 OR stdout missing `s5-boot-ok` |
| A2 | `lightr run --engine vz @img/alpine -- /bin/sh -c 'exit 7'` | exit code == 7 | any value other than 7 (proves the real guest code flows, not a hardcoded value) |

Exit code 255 is the engine's `GUEST_NO_REPORT_CODE` sentinel — it means the VM booted but the
guest's PID1 never sent a vsock exit frame. Receiving 255 on assertion A1 means the vsock chain
is broken and F-205 is NOT closed.

---

## 6. What Green Closes

| Assertion green | Parity-audit row | Feature |
|-----------------|------------------|---------|
| A1 (exit 0, stdout ok, not 255) | F-205 | vz microVM boots and runs a guest command end-to-end via Virtualization.framework on Apple Silicon |
| A2 (exit 7) | F-206 | Guest exit code flows accurately over vsock — no fabricated or hardcoded value |

**Until this runbook is executed green on a real ARM Mac, F-205 and F-206 remain yellow (unvalidated).**
