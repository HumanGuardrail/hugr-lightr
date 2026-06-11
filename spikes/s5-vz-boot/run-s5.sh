#!/usr/bin/env bash
# S5 vz-boot validation harness.
#
# Run on ANY Mac with Virtualization.framework (macOS 12+). vz virtualizes the
# NATIVE arch: on Intel the guest is x86_64, on Apple Silicon it is arm64 — this
# harness derives the guest arch from the host. It is NOT Apple-Silicon-only.
# DO NOT run on a Mac without Virtualization.framework support.
#
# Usage:
#   bash spikes/s5-vz-boot/run-s5.sh
#
# Exits 0 only when ALL assertions pass; non-zero on any failure (build,
# codesign, assertion, or prerequisite).
#
# See spikes/s5-vz-boot/README.md for provisioning + prerequisites.

set -euo pipefail

# ── Resolve repo root ──────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# ── Resolve guest arch from host (vz virtualizes the native arch) ──────────────
# Virtualization.framework is NOT emulation: the guest arch must equal the host
# arch. Intel host -> x86_64 guest; Apple Silicon host -> arm64 guest.
case "$(uname -m)" in
    arm64 | aarch64) GUEST_ARCH="aarch64"; SKOPEO_ARCH="arm64" ;;
    x86_64 | amd64)  GUEST_ARCH="x86_64";  SKOPEO_ARCH="amd64" ;;
    *) echo "[S5] unsupported host arch $(uname -m)" >&2; exit 1 ;;
esac

# ── Logging helpers ────────────────────────────────────────────────────────────
pass_count=0
fail_count=0

log_step() {
    printf '[S5] %-55s' "$1 ..."
}

log_pass() {
    echo "PASS"
    pass_count=$(( pass_count + 1 ))
}

log_fail() {
    echo "FAIL"
    fail_count=$(( fail_count + 1 ))
    # Print the reason on the next line and immediately abort — fail fast.
    echo "[S5] REASON: $1" >&2
    print_summary
    exit 1
}

print_summary() {
    echo "[S5] ─────────────────────────────────────────────────────────"
    if [ "${fail_count}" -eq 0 ]; then
        echo "[S5] ALL ASSERTIONS PASSED — F-205 / F-206 CLOSED (guest arch: ${GUEST_ARCH})"
    else
        echo "[S5] FAILED: ${fail_count} assertion(s) failed, ${pass_count} passed"
        echo "[S5] F-205 and F-206 remain YELLOW until all assertions pass green."
    fi
}

# ── Step 1: Build lightr --features vz ────────────────────────────────────────
#
# Founder-Mac PATH workaround: rustup installs cargo to ~/.cargo/bin, not always
# on $PATH under SSH/CI wrappers. Source the env file so cargo is always found.
log_step "Step 1: Building lightr --features vz"
if [ -f "${HOME}/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env"
fi
if ! command -v cargo > /dev/null 2>&1; then
    log_fail "cargo not found — install Rust via rustup (see README.md §2.3)"
fi
# swiftc is required by the vz build (shim/vz.swift).
if ! command -v swiftc > /dev/null 2>&1; then
    log_fail "swiftc not found — install Xcode and run: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer (see README.md §2.2)"
fi
(
    cd "${REPO_ROOT}"
    cargo build --release --features vz 2>&1
)
LIGHTR="${REPO_ROOT}/target/release/lightr"
if [ ! -x "${LIGHTR}" ]; then
    log_fail "build produced no executable at ${LIGHTR}"
fi
log_pass

# ── Step 1b: Codesign with the virtualization entitlement ─────────────────────
#
# Virtualization.framework refuses to create a VM unless the process carries the
# com.apple.security.virtualization entitlement. Ad-hoc signing (-s -) attaches
# it for LOCAL execution with NO paid Apple Developer account. Without this the
# very first VZVirtualMachine call fails with a code-signing/entitlement error.
# Re-sign after every build (linking voids the prior signature).
log_step "Step 1b: Codesigning lightr with virtualization entitlement"
ENTITLEMENTS="${REPO_ROOT}/packaging/vz.entitlements"
if [ ! -f "${ENTITLEMENTS}" ]; then
    log_fail "missing entitlement plist at ${ENTITLEMENTS}"
fi
if ! command -v codesign > /dev/null 2>&1; then
    log_fail "codesign not found — install the Xcode command line tools"
fi
codesign --sign - --entitlements "${ENTITLEMENTS}" --force "${LIGHTR}" 2>&1 \
    || log_fail "codesign failed — could not attach the virtualization entitlement"
# Verify the entitlement actually landed on the binary.
if ! codesign -d --entitlements - "${LIGHTR}" 2>&1 | grep -q "com.apple.security.virtualization"; then
    log_fail "virtualization entitlement not present after codesign"
fi
log_pass

# ── Step 2: Build the linux pack, THEN install it ─────────────────────────────
#
# build-linux-pack.sh assembles kernel + initrd (lightr-init as /init) into a
# pack DIRECTORY for the host's guest arch; it does NOT install. We then run
# `lightr engine install-pack <dir>`, which validates the pack (verify_pack:
# cpio /init executable, non-empty kernel) and copies it to
# $LIGHTR_HOME/packs/linux — the path probe_vz checks.
#
# KERNEL: a from-source kernel build needs a Linux cross toolchain the build
# script detects and demands (it will NOT fake a kernel). To stay turnkey,
# pre-obtain a vmlinux for the host arch (see README §2.4) and export
# LIGHTR_KERNEL=/path/to/it; it is passed through to --kernel.
log_step "Step 2: Building + installing linux pack (${GUEST_ARCH})"
BUILD_PACK_SCRIPT="${REPO_ROOT}/scripts/build-linux-pack.sh"
if [ ! -f "${BUILD_PACK_SCRIPT}" ]; then
    log_fail "${BUILD_PACK_SCRIPT} not found — ensure the wave is merged"
fi
PACK_DIR="${REPO_ROOT}/build/linux-pack"
if [ -n "${LIGHTR_KERNEL:-}" ]; then
    if [ ! -f "${LIGHTR_KERNEL}" ]; then
        log_fail "LIGHTR_KERNEL=${LIGHTR_KERNEL} does not exist"
    fi
    bash "${BUILD_PACK_SCRIPT}" --arch "${GUEST_ARCH}" --out "${PACK_DIR}" --kernel "${LIGHTR_KERNEL}" 2>&1 \
        || log_fail "build-linux-pack.sh failed (see output above)"
else
    bash "${BUILD_PACK_SCRIPT}" --arch "${GUEST_ARCH}" --out "${PACK_DIR}" 2>&1 \
        || log_fail "build-linux-pack.sh failed — a kernel toolchain is missing. Pre-build a vmlinux and re-run with LIGHTR_KERNEL=/path/to/vmlinux (see README.md §2.4)"
fi
# Install the built pack so the engine can find it.
"${LIGHTR}" engine install-pack "${PACK_DIR}" 2>&1 \
    || log_fail "engine install-pack rejected the pack at ${PACK_DIR} (verify_pack failed)"
# Verify the pack is now visible to the engine.
ENGINE_LS=$("${LIGHTR}" engine ls 2>&1)
if ! echo "${ENGINE_LS}" | grep -q "^vz.*available"; then
    log_fail "engine ls reports vz unavailable after install-pack. Output: ${ENGINE_LS}"
fi
log_pass

# ── Step 3: Import a tiny Alpine OCI image ────────────────────────────────────
#
# Prefer skopeo (no daemon). Fall back to a pre-saved docker tar via ALPINE_TAR,
# or a local docker. The image arch matches the host (SKOPEO_ARCH).
ALPINE_REF="alpine"
ALPINE_OCI_DIR="/tmp/s5-alpine-oci"

log_step "Step 3: Importing Alpine OCI image as ref '${ALPINE_REF}'"
if [ -n "${ALPINE_TAR:-}" ]; then
    if [ ! -f "${ALPINE_TAR}" ]; then
        log_fail "ALPINE_TAR=${ALPINE_TAR} is set but file does not exist"
    fi
    "${LIGHTR}" oci import "${ALPINE_TAR}" --name "${ALPINE_REF}" 2>&1
elif command -v skopeo > /dev/null 2>&1; then
    rm -rf "${ALPINE_OCI_DIR}"
    skopeo copy \
        --override-arch "${SKOPEO_ARCH}" \
        --override-os linux \
        "docker://alpine:latest" \
        "oci:${ALPINE_OCI_DIR}" 2>&1
    "${LIGHTR}" oci import "${ALPINE_OCI_DIR}" --name "${ALPINE_REF}" 2>&1
elif command -v docker > /dev/null 2>&1; then
    docker pull --platform "linux/${SKOPEO_ARCH}" alpine:latest 2>&1
    ALPINE_TAR_TMP="/tmp/s5-alpine.tar"
    docker save alpine:latest > "${ALPINE_TAR_TMP}"
    "${LIGHTR}" oci import "${ALPINE_TAR_TMP}" --name "${ALPINE_REF}" 2>&1
    rm -f "${ALPINE_TAR_TMP}"
else
    log_fail "Neither skopeo nor docker is available. Install skopeo (brew install skopeo) or set ALPINE_TAR=/path/to/alpine.tar (see README.md §2.5)"
fi
log_pass

# ── Assertion 1: echo returns exit 0, stdout has 's5-boot-ok', not 255 ────────
#
# Proves the full boot path: kernel loads -> lightr-init PID1 mounts rootfs ->
# spawns /bin/echo -> writes exit frame over vsock -> host reads i32(0).
# Exit 255 = GUEST_NO_REPORT_CODE (vsock chain broken); explicitly NOT a pass.
log_step "Assertion 1: echo exits 0, stdout has 's5-boot-ok', not 255"

ECHO_OUT=$("${LIGHTR}" run --engine vz "@img/${ALPINE_REF}" -- /bin/echo s5-boot-ok 2>/dev/null) || ECHO_EXIT=$?
ECHO_EXIT="${ECHO_EXIT:-0}"

if [ "${ECHO_EXIT}" -eq 255 ]; then
    log_fail "exit code 255 = GUEST_NO_REPORT_CODE — the vsock chain is broken; PID1 never sent an exit frame. F-205 NOT closed."
fi
if [ "${ECHO_EXIT}" -ne 0 ]; then
    log_fail "expected exit 0, got ${ECHO_EXIT}. stdout: '${ECHO_OUT}'"
fi
if ! echo "${ECHO_OUT}" | grep -q "s5-boot-ok"; then
    log_fail "stdout does not contain 's5-boot-ok'. actual stdout: '${ECHO_OUT}'"
fi
log_pass

# ── Assertion 2: non-zero guest exit code flows accurately ────────────────────
#
# Proves the REAL exit code (7) arrives over vsock and is not fabricated. A
# constant 0 (old fake) or 255 (no-report) would be caught here.
log_step "Assertion 2: guest 'exit 7' flows as real exit code 7 (not 0, not 255)"

SH_EXIT=0
"${LIGHTR}" run --engine vz "@img/${ALPINE_REF}" -- /bin/sh -c 'exit 7' \
    > /dev/null 2>&1 || SH_EXIT=$?

if [ "${SH_EXIT}" -ne 7 ]; then
    log_fail "expected exit 7, got ${SH_EXIT}. If 0: exit code was fabricated (old fake behaviour). If 255: vsock chain broken (GUEST_NO_REPORT_CODE). F-206 NOT closed."
fi
log_pass

# ── Summary ────────────────────────────────────────────────────────────────────
print_summary
