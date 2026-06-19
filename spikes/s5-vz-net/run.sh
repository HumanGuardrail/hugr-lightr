#!/usr/bin/env bash
# S5-NET — vz container networking validation harness (WP-NET2).
#
# Proves the flagship Docker-parity case end-to-end on a real microVM:
#   `lightr run -d -p HOST:CONTAINER --engine vz --rootfs <linux-image> -- <server>`
# boots a Linux container in a microVM (Apple Virtualization.framework), the
# guest publishes its DHCP IP, the host forwards HOST→guest:CONTAINER, an HTTP
# round-trip reaches the in-guest server, and `lightr stop` tears it ALL down
# (port closed, clean exit, no leaked supervisor/VM process).
#
# Run on ANY Mac with Virtualization.framework (macOS 12+). vz virtualizes the
# NATIVE arch (Intel → x86_64 guest, Apple Silicon → arm64 guest); the harness
# derives the guest arch from the host. DO NOT run on a Mac without VZ support.
#
# Usage:   bash spikes/s5-vz-net/run.sh
#
# Exits 0 only when ALL assertions pass; non-zero on any failure (build,
# codesign, prerequisite, or assertion). See EXPECTED.md for the parity mapping.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

case "$(uname -m)" in
    arm64 | aarch64) GUEST_ARCH="aarch64" ;;
    x86_64 | amd64)  GUEST_ARCH="x86_64" ;;
    *) echo "[S5-NET] unsupported host arch $(uname -m)" >&2; exit 1 ;;
esac

HOST_PORT="${LIGHTR_S5NET_PORT:-18080}"
EXPECT_BODY="lightr-vz-net"
BIN="${REPO_ROOT}/target/debug/lightr"
RUN_ID=""

pass_count=0
fail_count=0
log_step() { printf '[S5-NET] %-52s' "$1 ..."; }
log_pass() { echo "PASS"; pass_count=$(( pass_count + 1 )); }
log_info() { echo "[S5-NET] $1"; }
cleanup() {
    # Best-effort teardown so a failed run never leaks a VM/supervisor.
    if [ -n "${RUN_ID}" ]; then "${BIN}" stop "${RUN_ID}" >/dev/null 2>&1 || true; fi
    pkill -f "lightr __supervise" >/dev/null 2>&1 || true
}
log_fail() {
    echo "FAIL"; fail_count=$(( fail_count + 1 ))
    echo "[S5-NET] REASON: $1" >&2
    cleanup
    echo "[S5-NET] ─────────────────────────────────────────────────────"
    echo "[S5-NET] FAILED: ${fail_count} failed, ${pass_count} passed — F-304 vz -p remains unproven."
    exit 1
}
trap cleanup EXIT

# Start from a clean slate: a prior aborted run's supervisor (each owns a VM)
# would compete for vmnet/CPU and slow this run's teardown. Reap any stale ones.
pkill -f "lightr __supervise" >/dev/null 2>&1 || true
sleep 1

# ── Step 1: toolchain ─────────────────────────────────────────────────────────
log_step "Step 1: toolchain (cargo + swiftc)"
[ -f "${HOME}/.cargo/env" ] && { . "${HOME}/.cargo/env"; }
command -v cargo  >/dev/null 2>&1 || log_fail "cargo not found — install Rust via rustup"
command -v swiftc >/dev/null 2>&1 || log_fail "swiftc not found — install Xcode (vz shim needs swiftc)"
command -v curl   >/dev/null 2>&1 || log_fail "curl not found"
log_pass

# ── Step 2: build + codesign the vz CLI ───────────────────────────────────────
log_step "Step 2: build lightr --features vz"
cargo build -p lightr-cli --features vz >/tmp/s5net-build.log 2>&1 || log_fail "vz build failed (see /tmp/s5net-build.log)"
[ -x "${BIN}" ] || log_fail "binary not at ${BIN}"
log_pass

log_step "Step 3: codesign (com.apple.security.virtualization)"
codesign --force --sign - --entitlements packaging/vz.entitlements "${BIN}" >/dev/null 2>&1 \
    || log_fail "codesign failed"
codesign -d --entitlements - "${BIN}" 2>&1 | grep -q virtualization \
    || log_fail "entitlement not present after codesign"
log_pass

# ── Step 4: ensure a linux pack is installed (kernel + the CURRENT init) ───────
# The guest PID1 must be THIS build (it publishes the IP on the net path). If a
# pack is absent OR predates this source tree, rebuild + install one.
log_step "Step 4: ensure linux pack (kernel + current lightr-init)"
PACK_DIR="${LIGHTR_HOME:-${HOME}/.lightr}/packs/linux"
if [ ! -f "${PACK_DIR}/kernel" ] || [ ! -f "${PACK_DIR}/initrd" ]; then
    log_fail "no linux pack at ${PACK_DIR} — build one with scripts/build-linux-pack.sh (needs a kernel; see README) then 'lightr engine install-pack <dir>'"
fi
# Rebuild + reinstall the initrd from the current init so publish_ip is present.
cargo zigbuild -p lightr-init --bin lightr-init --target "${GUEST_ARCH}-unknown-linux-musl" --release \
    >/tmp/s5net-init.log 2>&1 || log_fail "init musl cross-build failed (need cargo-zigbuild; see /tmp/s5net-init.log)"
cargo run -q -p lightr-engine --example assemble-pack -- \
    --kernel "${PACK_DIR}/kernel" \
    --init "target/${GUEST_ARCH}-unknown-linux-musl/release/lightr-init" \
    --out build/s5net-pack --arch "${GUEST_ARCH}" >/tmp/s5net-pack.log 2>&1 \
    || log_fail "assemble-pack failed (see /tmp/s5net-pack.log)"
"${BIN}" engine install-pack build/s5net-pack >/dev/null 2>&1 || log_fail "install-pack failed"
log_pass

# ── Step 5: ensure an alpine rootfs ref ───────────────────────────────────────
log_step "Step 5: ensure 'alpine' rootfs ref"
if ! "${BIN}" oci pull --name alpine alpine >/tmp/s5net-pull.log 2>&1; then
    log_fail "could not pull alpine into the store (network? see /tmp/s5net-pull.log)"
fi
log_pass

# ── Step 6: launch the published container ────────────────────────────────────
# A minimal HTTP server in busybox: each connection gets a fixed 200 response.
log_step "Step 6: run -d -p ${HOST_PORT}:80 --engine vz --rootfs alpine"
SERVER="while true; do printf 'HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n${EXPECT_BODY}' | nc -l -p 80; done"
OUT="$("${BIN}" run -d -p "${HOST_PORT}:80" --engine vz --rootfs alpine -- sh -c "${SERVER}" 2>&1)" \
    || log_fail "run -d failed: ${OUT}"
RUN_ID="$(echo "${OUT}" | grep -oE 'id=[0-9-]+' | cut -d= -f2)"
[ -n "${RUN_ID}" ] || log_fail "no run id printed (got: ${OUT})"
log_pass
log_info "run id = ${RUN_ID}"

# ── Step 7: HTTP round-trip through the guest (poll: boot + DHCP + listen) ─────
log_step "Step 7: HTTP round-trip via 127.0.0.1:${HOST_PORT}"
GOT=""
for _ in $(seq 1 60); do
    R="$(curl -s --max-time 2 "http://127.0.0.1:${HOST_PORT}/" 2>/dev/null || true)"
    if [ -n "${R}" ]; then GOT="${R}"; break; fi
    sleep 0.5
done
[ "${GOT}" = "${EXPECT_BODY}" ] || log_fail "expected '${EXPECT_BODY}', got '${GOT}' (status: $(cat "${LIGHTR_HOME:-${HOME}/.lightr}/run/${RUN_ID}/status" 2>/dev/null))"
log_pass
log_info "guest IP = $(cat "${LIGHTR_HOME:-${HOME}/.lightr}/run/${RUN_ID}/rootfs/.lightr-ip" 2>/dev/null)"

# ── Step 8: stop tears it down (port closed) ──────────────────────────────────
log_step "Step 8: stop ⇒ clean exit + port closed"
# `lightr stop` exits with the stopped run's code (143 = 128+SIGTERM), exactly
# like the native detached path — non-zero is the EXPECTED result for a
# signal-terminated server, not a failure. The teardown proof below is the
# closed port + the 'exited' status, not stop's own exit code.
"${BIN}" stop "${RUN_ID}" >/dev/null 2>&1 || true
sleep 1
AFTER="$(curl -s --max-time 3 "http://127.0.0.1:${HOST_PORT}/" 2>/dev/null || true)"
[ -z "${AFTER}" ] || log_fail "port still reachable after stop (got '${AFTER}')"
STATUS="$(cat "${LIGHTR_HOME:-${HOME}/.lightr}/run/${RUN_ID}/status" 2>/dev/null || true)"
echo "${STATUS}" | grep -q "exited" || log_fail "status not 'exited' after stop (got '${STATUS}')"
log_pass

# ── Step 9: no leaked supervisor/VM ───────────────────────────────────────────
log_step "Step 9: no leaked supervisor/VM process"
# Poll: the supervisor exits after the VZ framework finishes VM teardown, which
# can lag stop() by a second or two. A genuine LEAK never disappears; a clean
# teardown converges to 0 well within the window.
LEAK="?"
for _ in $(seq 1 20); do
    # `pgrep` exits 1 on zero matches — which is the SUCCESS case here — so guard
    # it with `|| true` (else `set -e`/`pipefail` would abort before we PASS).
    LEAK="$( { pgrep -f "lightr __supervise" || true; } | wc -l | tr -d ' ' )"
    [ "${LEAK}" = "0" ] && break
    sleep 0.5
done
[ "${LEAK}" = "0" ] || log_fail "${LEAK} supervisor process(es) still alive after stop"
RUN_ID=""  # stopped cleanly; nothing for the trap to tear down
log_pass

echo "[S5-NET] ─────────────────────────────────────────────────────"
echo "[S5-NET] ALL ASSERTIONS PASSED — F-304 vz \`-p\` CLOSED (guest arch: ${GUEST_ARCH})"
echo "[S5-NET] container reachable via published port + clean teardown."
