#!/usr/bin/env bash
# S5-VZ-SWITCH — ADR-0018 keystone END-TO-END validation (WP-C10). THROWAWAY.
#
# Builds + codesigns the s5-vz-switch example (a Rust harness in lightr-run,
# gated `required-features=["vz"]`) and runs it. The harness boots REAL alpine
# microVMs and drives the merged Design-C networking library end-to-end:
#   STEP 1  one VM leases its registry IP from vswitch/dhcp.rs (busybox udhcpc)
#   STEP 2  two VMs reach each other by IP over the mesh (L2 switching)
#   STEP 3  curl-by-name round-trips via vswitch/dns.rs
#   STEP 4  teardown clean (no leaked switch threads / VM procs)
#
# Run on a REAL Intel/Apple-Silicon Mac with Virtualization.framework.
# Usage:  bash spikes/s5-vz-switch/run.sh
#
# IMPORTANT: the EXAMPLE binary is what creates the VZVirtualMachine, so the
# entitlement must be ad-hoc-signed onto the EXAMPLE binary (not just `lightr`).
# Re-codesign after every rebuild (linking voids the signature).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

export PATH="${HOME}/.rustup/toolchains/1.96.0-x86_64-apple-darwin/bin:${HOME}/.cargo/bin:${PATH}"
[ -f "${HOME}/.cargo/env" ] && . "${HOME}/.cargo/env"

ENTITLEMENTS="${REPO_ROOT}/packaging/vz.entitlements"

echo "[S5-SWITCH] toolchain: $(command -v cargo) / $(rustc --version)"
command -v swiftc >/dev/null 2>&1 || { echo "[S5-SWITCH] swiftc missing (vz shim needs it)"; exit 1; }
[ -f "${ENTITLEMENTS}" ] || { echo "[S5-SWITCH] missing ${ENTITLEMENTS}"; exit 1; }

# A linux pack + an 'alpine' rootfs ref must already be installed (this is a
# validation harness, not a provisioning script). Build lightr to check/provision.
echo "[S5-SWITCH] building lightr (vz) for prerequisite checks ..."
cargo build -p lightr-cli --features vz 2>&1 | tail -3
LIGHTR="${REPO_ROOT}/target/debug/lightr"
codesign --force --sign - --entitlements "${ENTITLEMENTS}" "${LIGHTR}" >/dev/null 2>&1 || true

PACK_DIR="${LIGHTR_HOME:-${HOME}/.lightr}/packs/linux"
if [ ! -f "${PACK_DIR}/kernel" ] || [ ! -f "${PACK_DIR}/initrd" ]; then
    echo "[S5-SWITCH] no linux pack at ${PACK_DIR} — install one first (see spikes/s5-vz-boot/README.md)"; exit 1
fi
# Ensure the alpine ref exists; pull only if missing (and a network is available).
if ! "${LIGHTR}" oci pull --name alpine alpine:latest >/dev/null 2>&1; then
    # pull failed (offline?) — only fatal if the ref is also absent.
    if ! ls "${LIGHTR_HOME:-${HOME}/.lightr}/store/refs-names" 2>/dev/null | grep -q .; then
        echo "[S5-SWITCH] no 'alpine' ref and pull failed (offline?). Provision an alpine rootfs ref first."; exit 1
    fi
    echo "[S5-SWITCH] (alpine pull skipped/failed; using the existing 'alpine' ref in the store)"
fi

# Build + codesign the EXAMPLE binary (the thing that boots the VMs).
echo "[S5-SWITCH] building example s5-vz-switch ..."
cargo build -p lightr-run --features vz --example s5-vz-switch 2>&1 | tail -3
EXAMPLE_BIN="$(ls -t "${REPO_ROOT}"/target/debug/examples/s5-vz-switch 2>/dev/null | head -1)"
[ -x "${EXAMPLE_BIN}" ] || { echo "[S5-SWITCH] example binary not found"; exit 1; }
echo "[S5-SWITCH] codesigning example: ${EXAMPLE_BIN}"
codesign --force --sign - --entitlements "${ENTITLEMENTS}" "${EXAMPLE_BIN}"
codesign -d --entitlements - "${EXAMPLE_BIN}" 2>&1 | grep -q virtualization \
    || { echo "[S5-SWITCH] entitlement missing after codesign"; exit 1; }

# Reap any stale supervisors so they don't compete for vmnet/CPU.
pkill -f "lightr __supervise" >/dev/null 2>&1 || true

echo "[S5-SWITCH] ─────────────────────────────────────────────────────"
"${EXAMPLE_BIN}"
RC=$?
echo "[S5-SWITCH] ─────────────────────────────────────────────────────"
exit "${RC}"
