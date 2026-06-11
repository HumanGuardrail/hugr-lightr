# S5 vz-boot Validation — Provisioning Checklist

**Purpose:** Run this on a rented ARM Mac to validate the real Virtualization.framework
microVM boot path. Until it passes green, parity-audit rows F-205 and F-206 remain yellow.
This document does NOT claim a boot has been validated; it is the runbook to perform that
validation.

---

## 1. Provision a Target Machine

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

### 2.4 Install cross-compilation toolchain for lightr-init

```
# musl cross-linker for aarch64 Linux — needed by build-linux-pack.sh
brew install filosottile/musl-cross/musl-cross
# Verify:
aarch64-linux-musl-gcc --version
```

### 2.5 Install skopeo (or have docker available)

The `run-s5.sh` script imports an Alpine OCI image. Provide one of:

- **skopeo** (no daemon needed, recommended):
  ```
  brew install skopeo
  skopeo --version
  ```
- **docker** (alternative):
  ```
  # Use Docker Desktop for Mac, then:
  docker pull alpine
  docker save alpine > /tmp/alpine.tar
  ```

### 2.6 Clone the repo

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

# Step 2 — Build and install the Linux kernel pack
bash scripts/build-linux-pack.sh --arch arm64
# The script downloads/builds the kernel and lightr-init, assembles the pack,
# and installs it to $LIGHTR_HOME/packs/linux (default: ~/.lightr/packs/linux).
# If the cross-toolchain is absent it will print exactly what to install.

# Step 3 — Verify the engine sees the pack
./target/release/lightr engine ls
# Expected line:  vz    available    vz engine ready (pack: ~/.lightr/packs/linux)

# Step 4 — Import a tiny Alpine OCI image
# Option A — skopeo (no docker daemon):
skopeo copy docker://alpine:latest oci:/tmp/alpine-oci
./target/release/lightr oci import /tmp/alpine-oci --name alpine

# Option B — docker save (if Docker is available):
docker pull alpine
docker save alpine > /tmp/alpine.tar
./target/release/lightr oci import /tmp/alpine.tar --name alpine

# Step 5 — Run the validation harness
bash spikes/s5-vz-boot/run-s5.sh
```

---

## 4. Expected Output

`run-s5.sh` prints a structured PASS/FAIL summary. A green run looks like:

```
[S5] Building lightr --features vz ...        PASS
[S5] Installing linux pack ...                PASS
[S5] Importing alpine OCI image ...           PASS
[S5] Assertion 1: echo exits 0 with s5-boot-ok in stdout ...   PASS
[S5] Assertion 2: exit 7 flows as real guest exit code ...      PASS
[S5] ─────────────────────────────────────────
[S5] ALL ASSERTIONS PASSED — F-205 / F-206 CLOSED (on this ARM host)
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
| A1 (exit 0, stdout ok, not 255) | F-205 | vz microVM boots and runs a guest command end-to-end via Virtualization.framework |
| A2 (exit 7) | F-206 | Guest exit code flows accurately over vsock — no fabricated or hardcoded value |

**Until this runbook is executed green on a real ARM Mac, F-205 and F-206 remain yellow (unvalidated).**
