#!/usr/bin/env bash
# S5 vz-boot-arm64 validation harness.
#
# Run on an ARM Mac (mac2.metal / mac2-m2.metal or MacStadium M1/M2).
# DO NOT run on Intel or on a Mac without Virtualization.framework support.
#
# Usage:
#   bash spikes/s5-vz-boot-arm64/run-s5-arm64.sh
#
# Exits 0 only when ALL assertions pass.
# Exits non-zero on any failure (build, codesign, assertion, or prerequisite).
#
# See spikes/s5-vz-boot-arm64/README.md for full provisioning steps.
# See spikes/s5-vz-boot-arm64/EXPECTED.md for what each assertion proves.

set -euo pipefail

# ── Resolve repo root ──────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# ── Logging helpers ────────────────────────────────────────────────────────────
pass_count=0
fail_count=0

log_step() {
    printf '[S5-ARM64] %-55s' "$1 ..."
}

log_pass() {
    echo "PASS"
    pass_count=$(( pass_count + 1 ))
}

log_fail() {
    echo "FAIL"
    fail_count=$(( fail_count + 1 ))
    # Print the reason on the next line and immediately abort — fail fast.
    echo "[S5-ARM64] REASON: $1" >&2
    print_summary
    exit 1
}

print_summary() {
    echo "[S5-ARM64] ─────────────────────────────────────────────────────────"
    if [ "${fail_count}" -eq 0 ]; then
        echo "[S5-ARM64] ALL ASSERTIONS PASSED — F-205 / F-206 CLOSED (on this ARM host)"
    else
        echo "[S5-ARM64] FAILED: ${fail_count} assertion(s) failed, ${pass_count} passed"
        echo "[S5-ARM64] F-205 and F-206 remain YELLOW until all assertions pass green."
    fi
}

# ── Step 1: Build lightr --features vz ────────────────────────────────────────
#
# Founder-Mac PATH workaround: rustup installs cargo to ~/.cargo/bin, which is
# not always on $PATH when a script is invoked by SSH or a CI wrapper. Source
# the env file unconditionally so cargo is always found.
log_step "Step 1: Building lightr --features vz"
if [ -f "${HOME}/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env"
fi
# Verify cargo is available after sourcing
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

# ── Step 2: Ad-hoc codesign with the Virtualization.framework entitlement ─────
#
# macOS requires a process to hold the com.apple.security.virtualization
# entitlement before it can allocate a VZVirtualMachine. Without this, the
# first VM allocation fails with an authorization error at runtime.
#
# packaging/vz.entitlements is created by the wave lead (not this WP); if it
# is missing the harness fails clearly here rather than later with a confusing
# VZ error.
#
# The `-s -` flag requests an ad-hoc (self-signed) identity — no developer
# certificate is required. This is sufficient for local validation on any Mac.
log_step "Step 2: Codesigning lightr with vz entitlement (ad-hoc)"
VZ_ENTITLEMENTS="${REPO_ROOT}/packaging/vz.entitlements"
if [ ! -f "${VZ_ENTITLEMENTS}" ]; then
    log_fail "packaging/vz.entitlements not found at ${VZ_ENTITLEMENTS} — this file is created by the wave lead; ensure the wave is fully merged before running this harness"
fi
if ! command -v codesign > /dev/null 2>&1; then
    log_fail "codesign not found — this harness must run on macOS with Xcode Command Line Tools installed"
fi
codesign -s - \
    --entitlements "${VZ_ENTITLEMENTS}" \
    --force \
    "${LIGHTR}" 2>&1 \
    || log_fail "codesign failed — check that ${VZ_ENTITLEMENTS} is a valid plist entitlements file"
log_pass

# ── Step 3: Build the linux pack, THEN install it ─────────────────────────────
#
# scripts/build-linux-pack.sh assembles kernel + initrd (lightr-init as /init)
# into a pack DIRECTORY; it does NOT install. We then run
# `lightr engine install-pack <dir>`, which validates the pack (verify_pack:
# cpio /init executable, non-empty kernel) and copies it to
# $LIGHTR_HOME/packs/linux — the path probe_vz checks.
#
# KERNEL: build the arm64 Image first with `scripts/build-kernel-arm64.sh`
# (cross-compiles linux-6.18.5 in a container → build/linux-pack-arm64/Image),
# then export LIGHTR_KERNEL="$(pwd)/build/linux-pack-arm64/Image". It is passed
# through to --kernel and the from-source build is skipped. Apple's VZ on arm64
# boots the UNCOMPRESSED `Image` (not a bzImage/vmlinux ELF — those are x86).
#
# --arch aarch64 selects the aarch64-unknown-linux-musl guest target.
# (Note: the build script accepts 'aarch64', not 'arm64'.)
log_step "Step 3: Building + installing linux pack (aarch64)"
BUILD_PACK_SCRIPT="${REPO_ROOT}/scripts/build-linux-pack.sh"
if [ ! -f "${BUILD_PACK_SCRIPT}" ]; then
    log_fail "${BUILD_PACK_SCRIPT} not found — ensure the wave is merged"
fi
PACK_DIR="${REPO_ROOT}/build/linux-pack-arm64"
if [ -n "${LIGHTR_KERNEL:-}" ]; then
    if [ ! -f "${LIGHTR_KERNEL}" ]; then
        log_fail "LIGHTR_KERNEL=${LIGHTR_KERNEL} does not exist"
    fi
    bash "${BUILD_PACK_SCRIPT}" --arch aarch64 --out "${PACK_DIR}" --kernel "${LIGHTR_KERNEL}" 2>&1 \
        || log_fail "build-linux-pack.sh failed (see output above)"
else
    bash "${BUILD_PACK_SCRIPT}" --arch aarch64 --out "${PACK_DIR}" 2>&1 \
        || log_fail "build-linux-pack.sh failed — no kernel toolchain in-host. Run scripts/build-kernel-arm64.sh first, then re-run with LIGHTR_KERNEL=\$(pwd)/build/linux-pack-arm64/Image"
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

# ── Step 4: Import a tiny arm64 Alpine OCI image ──────────────────────────────
#
# Prefer skopeo (no daemon, copies directly from a registry to an OCI layout).
# Pass --override-arch arm64 --override-os linux to ensure an arm64 image is
# fetched (critical: the test must run a native arm64 binary inside the VM).
# Fall back to a pre-saved docker tar if ALPINE_TAR is set in the environment
# (set it when a docker save was produced on another machine and copied over).
#
# To produce alpine.tar with an arm64 image without docker on the target machine:
#   On any machine with docker:
#     docker pull --platform linux/arm64 alpine
#     docker save alpine > alpine.tar
#   Or with skopeo:
#     skopeo copy --override-arch arm64 --override-os linux \
#       docker://alpine:latest oci:/tmp/alpine-oci-arm64
#     then on the target:
#       lightr oci import /tmp/alpine-oci-arm64 --name alpine
#
ALPINE_REF="alpine"
ALPINE_OCI_DIR="/tmp/s5-alpine-oci-arm64"

log_step "Step 4: Importing arm64 Alpine OCI image as ref '${ALPINE_REF}'"
if [ -n "${ALPINE_TAR:-}" ]; then
    # Use a pre-produced docker save tar (set ALPINE_TAR=/path/to/alpine.tar).
    if [ ! -f "${ALPINE_TAR}" ]; then
        log_fail "ALPINE_TAR=${ALPINE_TAR} is set but file does not exist"
    fi
    "${LIGHTR}" oci import "${ALPINE_TAR}" --name "${ALPINE_REF}" 2>&1
elif command -v skopeo > /dev/null 2>&1; then
    # skopeo: copy from registry as arm64/linux to local OCI layout, then import.
    rm -rf "${ALPINE_OCI_DIR}"
    skopeo copy \
        --override-arch arm64 \
        --override-os linux \
        "docker://alpine:latest" \
        "oci:${ALPINE_OCI_DIR}" 2>&1
    "${LIGHTR}" oci import "${ALPINE_OCI_DIR}" --name "${ALPINE_REF}" 2>&1
elif command -v docker > /dev/null 2>&1; then
    # docker: pull arm64 image + save to a tar, then import.
    docker pull --platform linux/arm64 alpine:latest 2>&1
    ALPINE_TAR_TMP="/tmp/s5-alpine-arm64.tar"
    docker save alpine:latest > "${ALPINE_TAR_TMP}"
    "${LIGHTR}" oci import "${ALPINE_TAR_TMP}" --name "${ALPINE_REF}" 2>&1
    rm -f "${ALPINE_TAR_TMP}"
else
    log_fail "Neither skopeo nor docker is available. Install skopeo (brew install skopeo) or set ALPINE_TAR=/path/to/alpine.tar (see README.md §2.6)"
fi
log_pass

# ── Assertion 1: echo returns exit 0, stdout has 's5-boot-ok', not 255 ────────
#
# This proves the full boot path:
#   kernel loads → lightr-init PID1 mounts the rootfs virtiofs share → chroots →
#   spawns /bin/echo → writes its exit code to EXIT_FILE on the share → host
#   reads it back.
#
# Exit code 255 is GUEST_NO_REPORT_CODE — no EXIT_FILE (the VM booted but PID1
# never wrote its exit code). 255 is explicitly NOT a pass.
log_step "Assertion 1: echo exits 0, stdout has 's5-boot-ok', not 255"

ECHO_EXIT=0
ECHO_OUT=$("${LIGHTR}" run --engine vz --rootfs "${ALPINE_REF}" -- /bin/echo s5-boot-ok 2>/dev/null) || ECHO_EXIT=$?

if [ "${ECHO_EXIT}" -eq 255 ]; then
    log_fail "exit code 255 = GUEST_NO_REPORT_CODE — no EXIT_FILE on the rootfs share; PID1 never wrote its exit code. F-205 NOT closed."
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
# This proves the REAL exit code (7) arrives via the EXIT_FILE channel and is NOT
# a hardcoded or fabricated value. If the engine always returned 0 (the old fake
# behaviour) or always returned 255 (no-report fallback), this assertion catches it.
#
# The code path: lightr-init spawns /bin/sh → shell exits 7 → init writes "7" to
# EXIT_FILE on the rootfs share → VzEngine::run reads it back after the VM stops.
log_step "Assertion 2: guest 'exit 7' flows as real exit code 7 (not 0, not 255)"

# Capture the exit code without aborting the script (set -e is active).
SH_EXIT=0
"${LIGHTR}" run --engine vz --rootfs "${ALPINE_REF}" -- /bin/sh -c 'exit 7' \
    > /dev/null 2>&1 || SH_EXIT=$?

if [ "${SH_EXIT}" -ne 7 ]; then
    log_fail "expected exit 7, got ${SH_EXIT}. If 0: exit code was fabricated (old fake behaviour). If 255: no EXIT_FILE (GUEST_NO_REPORT_CODE). F-206 NOT closed."
fi
log_pass

# ── Summary ────────────────────────────────────────────────────────────────────
print_summary
