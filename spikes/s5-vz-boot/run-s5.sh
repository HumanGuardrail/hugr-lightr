#!/usr/bin/env bash
# S5 vz-boot validation harness.
#
# Run on an ARM Mac (mac2.metal / mac2-m2.metal or MacStadium M1/M2).
# DO NOT run on Intel or on a Mac without Virtualization.framework support.
#
# Usage:
#   bash spikes/s5-vz-boot/run-s5.sh
#
# Exits 0 only when ALL assertions pass.
# Exits non-zero on any failure (build, assertion, or prerequisite).
#
# See spikes/s5-vz-boot/README.md for full provisioning steps.
# See spikes/s5-vz-boot/EXPECTED.md for what each assertion proves.

set -euo pipefail

# ── Resolve repo root ──────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

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
        echo "[S5] ALL ASSERTIONS PASSED — F-205 / F-206 CLOSED (on this ARM host)"
    else
        echo "[S5] FAILED: ${fail_count} assertion(s) failed, ${pass_count} passed"
        echo "[S5] F-205 and F-206 remain YELLOW until all assertions pass green."
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

# ── Step 2: Build + install the linux pack via W3 script ──────────────────────
#
# scripts/build-linux-pack.sh (produced by WP W3) assembles the kernel + initrd
# for the vz engine and installs them to $LIGHTR_HOME/packs/linux.
# The --arch arm64 flag selects the aarch64-unknown-linux-musl target for
# lightr-init.
log_step "Step 2: Building + installing linux pack (arm64)"
BUILD_PACK_SCRIPT="${REPO_ROOT}/scripts/build-linux-pack.sh"
if [ ! -f "${BUILD_PACK_SCRIPT}" ]; then
    log_fail "${BUILD_PACK_SCRIPT} not found — ensure WP W3 (s5-runbook) has been merged"
fi
bash "${BUILD_PACK_SCRIPT}" --arch arm64 2>&1
# Verify the pack is now visible to the engine.
ENGINE_LS=$("${LIGHTR}" engine ls 2>&1)
if ! echo "${ENGINE_LS}" | grep -q "^vz.*available"; then
    log_fail "engine ls reports vz unavailable after pack install. Output: ${ENGINE_LS}"
fi
log_pass

# ── Step 3: Import a tiny Alpine OCI image ────────────────────────────────────
#
# Prefer skopeo (no daemon, copies directly from a registry to an OCI layout).
# Fall back to a pre-saved docker tar if ALPINE_TAR is set in the environment
# (set it when a docker save was produced on another machine and copied over).
#
# To produce alpine.tar without docker on the target machine:
#   On any machine with docker:     docker save alpine > alpine.tar
#   Or with skopeo:                 skopeo copy docker://alpine:latest oci:/tmp/alpine-oci
#     then on the target:           lightr oci import /tmp/alpine-oci --name alpine
#
ALPINE_REF="alpine"
ALPINE_OCI_DIR="/tmp/s5-alpine-oci"

log_step "Step 3: Importing Alpine OCI image as ref '${ALPINE_REF}'"
if [ -n "${ALPINE_TAR:-}" ]; then
    # Use a pre-produced docker save tar (set ALPINE_TAR=/path/to/alpine.tar).
    if [ ! -f "${ALPINE_TAR}" ]; then
        log_fail "ALPINE_TAR=${ALPINE_TAR} is set but file does not exist"
    fi
    "${LIGHTR}" oci import "${ALPINE_TAR}" --name "${ALPINE_REF}" 2>&1
elif command -v skopeo > /dev/null 2>&1; then
    # skopeo: copy from registry to local OCI layout, then import.
    rm -rf "${ALPINE_OCI_DIR}"
    skopeo copy \
        --override-arch arm64 \
        --override-os linux \
        "docker://alpine:latest" \
        "oci:${ALPINE_OCI_DIR}" 2>&1
    "${LIGHTR}" oci import "${ALPINE_OCI_DIR}" --name "${ALPINE_REF}" 2>&1
elif command -v docker > /dev/null 2>&1; then
    # docker: pull + save to a tar, then import.
    docker pull --platform linux/arm64 alpine:latest 2>&1
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
# This proves the full boot path:
#   kernel loads → lightr-init PID1 mounts rootfs → spawns /bin/echo →
#   writes exit frame over vsock → host reads i32(0) from the frame.
#
# Exit code 255 is GUEST_NO_REPORT_CODE — the vsock chain is broken (the VM
# booted but PID1 never sent an exit frame). 255 is explicitly NOT a pass.
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
# This proves the REAL exit code (7) arrives over vsock and is NOT a hardcoded
# or fabricated value. If the engine always returned 0 (the old fake behaviour)
# or always returned 255 (no-report fallback), this assertion would catch it.
#
# The code path: lightr-init spawns /bin/sh → shell exits 7 → init writes
# i32(7) as a little-endian frame to CID_HOST:1024 → VsockExitReceiver reads
# it via read_exit_frame → VzEngine::run returns 7.
log_step "Assertion 2: guest 'exit 7' flows as real exit code 7 (not 0, not 255)"

# Capture the exit code without aborting the script (set -e is active).
SH_EXIT=0
"${LIGHTR}" run --engine vz "@img/${ALPINE_REF}" -- /bin/sh -c 'exit 7' \
    > /dev/null 2>&1 || SH_EXIT=$?

if [ "${SH_EXIT}" -ne 7 ]; then
    log_fail "expected exit 7, got ${SH_EXIT}. If 0: exit code was fabricated (old fake behaviour). If 255: vsock chain broken (GUEST_NO_REPORT_CODE). F-206 NOT closed."
fi
log_pass

# ── Summary ────────────────────────────────────────────────────────────────────
print_summary
