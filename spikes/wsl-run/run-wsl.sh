#!/usr/bin/env bash
# WSL2 engine validation harness.
#
# Run on a REAL Windows 10/11 box with WSL2 enabled and at least one registered
# distro. This harness CANNOT be validated on macOS or Linux — the WSL2 engine
# is a Windows-only path. Do NOT run on a machine without WSL2; it will fail
# loud at Step 1.
#
# Usage (from WSL bash or Git-Bash, inside the repo):
#   bash spikes/wsl-run/run-wsl.sh
#
# Exits 0 only when ALL assertions pass; non-zero on any failure (locate,
# probe, import, assertion, or prerequisite).
#
# See spikes/wsl-run/README.md for prerequisites and what each assertion proves.

set -euo pipefail

# ── Resolve repo root ──────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# ── Logging helpers ────────────────────────────────────────────────────────────
pass_count=0
fail_count=0

log_step() {
    printf '[WSL] %-58s' "$1 ..."
}

log_pass() {
    echo "PASS"
    pass_count=$(( pass_count + 1 ))
}

log_fail() {
    echo "FAIL"
    fail_count=$(( fail_count + 1 ))
    # Print the reason on the next line and immediately abort — fail fast.
    echo "[WSL] REASON: $1" >&2
    print_summary
    exit 1
}

print_summary() {
    echo "[WSL] ─────────────────────────────────────────────────────────────"
    if [ "${fail_count}" -eq 0 ]; then
        echo "[WSL] ALL ASSERTIONS PASSED — WSL2 engine round-trip validated"
        echo "[WSL] exit-code passthrough proven (A1: exit 0, A2: exit 7)"
    else
        echo "[WSL] FAILED: ${fail_count} assertion(s) failed, ${pass_count} passed"
        echo "[WSL] WSL2 engine validation remains UNVERIFIED until all assertions pass green."
    fi
}

# ── Step 1: Locate lightr.exe; probe WSL2 availability ────────────────────────
#
# On Windows the binary is lightr.exe. We look for it on PATH first (an
# installed build) then fall back to the repo's release target directory.
# `lightr engine ls` must report "wsl    available" — the honest probe_wsl
# check (wsl.exe -l -q finds at least one registered distro). If not, the
# engine is unusable and there is no point running the assertions.
log_step "Step 1: Locating lightr.exe + engine ls shows wsl available"

LIGHTR=""
if command -v lightr.exe > /dev/null 2>&1; then
    LIGHTR="lightr.exe"
elif [ -x "${REPO_ROOT}/target/release/lightr.exe" ]; then
    LIGHTR="${REPO_ROOT}/target/release/lightr.exe"
elif command -v lightr > /dev/null 2>&1; then
    # Under WSL bash the .exe suffix may be omitted when on PATH.
    LIGHTR="lightr"
elif [ -x "${REPO_ROOT}/target/release/lightr" ]; then
    LIGHTR="${REPO_ROOT}/target/release/lightr"
else
    log_fail "lightr.exe not found on PATH and not at ${REPO_ROOT}/target/release/lightr.exe.
  Build it: cargo build --release (on Windows, from a MSVC or MinGW shell)
  or download a pre-built release binary and place it on PATH."
fi

ENGINE_LS=$("${LIGHTR}" engine ls 2>&1) || true
if ! echo "${ENGINE_LS}" | grep -q "^wsl.*available"; then
    log_fail "engine ls does not report wsl available.
  Output: ${ENGINE_LS}
  Fix: ensure WSL2 is installed and at least one distro is registered.
  Run: wsl --install   (or: wsl --install -d Ubuntu)
  Then re-open the terminal and re-run this harness."
fi
log_pass

# ── Step 2: Obtain an Alpine rootfs ref ───────────────────────────────────────
#
# Mirror the s5 approach: prefer a pre-saved tar (ALPINE_TAR env var), then
# skopeo, then docker. The WSL2 engine execs into the default distro, so the
# rootfs needs to be importable from the Windows host. skopeo and docker can
# run inside the WSL2 distro (they are Linux tools); this script runs under
# WSL bash or Git-Bash, so the commands work in both environments.
ALPINE_REF="alpine"
ALPINE_OCI_DIR="/tmp/wsl-alpine-oci"

log_step "Step 2: Importing Alpine OCI image as ref '${ALPINE_REF}'"
if [ -n "${ALPINE_TAR:-}" ]; then
    if [ ! -f "${ALPINE_TAR}" ]; then
        log_fail "ALPINE_TAR=${ALPINE_TAR} is set but file does not exist"
    fi
    "${LIGHTR}" oci import "${ALPINE_TAR}" --name "${ALPINE_REF}" 2>&1 \
        || log_fail "lightr oci import failed for ALPINE_TAR=${ALPINE_TAR}"
elif command -v skopeo > /dev/null 2>&1; then
    rm -rf "${ALPINE_OCI_DIR}"
    skopeo copy \
        --override-arch amd64 \
        --override-os linux \
        "docker://alpine:latest" \
        "oci:${ALPINE_OCI_DIR}" 2>&1 \
        || log_fail "skopeo copy failed — check network connectivity"
    "${LIGHTR}" oci import "${ALPINE_OCI_DIR}" --name "${ALPINE_REF}" 2>&1 \
        || log_fail "lightr oci import failed for OCI dir ${ALPINE_OCI_DIR}"
elif command -v docker > /dev/null 2>&1; then
    docker pull --platform linux/amd64 alpine:latest 2>&1 \
        || log_fail "docker pull alpine failed — check Docker Desktop is running"
    ALPINE_TAR_TMP="/tmp/wsl-alpine.tar"
    docker save alpine:latest > "${ALPINE_TAR_TMP}" \
        || log_fail "docker save alpine failed"
    "${LIGHTR}" oci import "${ALPINE_TAR_TMP}" --name "${ALPINE_REF}" 2>&1 \
        || log_fail "lightr oci import failed for docker tar ${ALPINE_TAR_TMP}"
    rm -f "${ALPINE_TAR_TMP}"
else
    log_fail "Neither skopeo nor docker is available.
  Install skopeo inside WSL2 (e.g. apt install skopeo) or set ALPINE_TAR=/path/to/alpine.tar
  (see README.md §Prerequisites)."
fi
log_pass

# ── Assertion 1: echo returns exit 0, stdout has 'wsl-ok', not 255 ────────────
#
# Proves the full WSL2 engine round-trip:
#   lightr.exe hands the rootfs ref to the WSL2 engine → the engine translates
#   the Windows rootfs path to its /mnt/<drive>/… WSL2 view → invokes a Linux
#   `lightr` inside the default distro with `--engine ns --rootfs <wsl-path>` →
#   the ns model (unshare + pivot_root) runs /bin/echo → exit 0 arrives back.
# Exit 255 = GUEST_NO_REPORT_CODE or a WSL invocation failure — explicitly NOT
# a pass. A non-zero exit means the round-trip is broken.
log_step "Assertion 1: echo exits 0, stdout has 'wsl-ok', not 255"

ECHO_OUT=""
ECHO_EXIT=0
ECHO_OUT=$("${LIGHTR}" run --engine wsl --rootfs "${ALPINE_REF}" -- /bin/echo wsl-ok 2>/dev/null) \
    || ECHO_EXIT=$?

if [ "${ECHO_EXIT}" -eq 255 ]; then
    log_fail "exit code 255 — WSL2 invocation failed or the in-distro ns engine never reported
  its exit code. The WSL2 round-trip is broken. Check: is a Linux lightr on the
  distro PATH? Does the distro support unprivileged user namespaces?"
fi
if [ "${ECHO_EXIT}" -ne 0 ]; then
    log_fail "expected exit 0, got ${ECHO_EXIT}. stdout: '${ECHO_OUT}'"
fi
if ! echo "${ECHO_OUT}" | grep -q "wsl-ok"; then
    log_fail "stdout does not contain 'wsl-ok'. actual stdout: '${ECHO_OUT}'"
fi
log_pass

# ── Assertion 2: non-zero guest exit code flows accurately ────────────────────
#
# Proves the REAL exit code (7) travels the full chain: in-distro ns engine
# writes exit 7 → wsl.exe propagates ExitStatus → lightr.exe surfaces it.
# A constant 0 (fabricated success) or 255 (invocation failure) would be
# caught here. This closes the exit-code passthrough correctness claim for the
# WSL2 engine path.
log_step "Assertion 2: guest 'exit 7' flows as real exit code 7 (not 0, not 255)"

SH_EXIT=0
"${LIGHTR}" run --engine wsl --rootfs "${ALPINE_REF}" -- /bin/sh -c 'exit 7' \
    > /dev/null 2>&1 || SH_EXIT=$?

if [ "${SH_EXIT}" -ne 7 ]; then
    log_fail "expected exit 7, got ${SH_EXIT}.
  If 0: exit code is being fabricated (broken passthrough).
  If 255: WSL2 invocation failure or no exit reported.
  Any other value: incorrect exit-code mapping in the wsl or ns engine."
fi
log_pass

# ── Summary ────────────────────────────────────────────────────────────────────
print_summary
