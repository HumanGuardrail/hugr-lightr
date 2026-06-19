#!/usr/bin/env bash
#
# build-init.sh — build lightr-init (guest PID 1) for a Linux musl target
# with NO docker, using cargo-zigbuild (zig as the cross-linker).
#
# lightr-init is the only build artifact that must cross-compile from macOS to
# a musl Linux guest binary; the kernel requires a Linux build environment
# (see build-kernel-x86.sh / build-kernel-arm64.sh for those options).
#
# Prerequisites (checked and reported with the exact fix command):
#   • zig            — brew install zig
#   • cargo-zigbuild — cargo install cargo-zigbuild
#   • rustup target  — rustup target add <triple>-unknown-linux-musl
#
# Usage:  scripts/build-init.sh [--arch <x86_64|aarch64>] [--out <path>]
#
#   --arch   guest CPU architecture (default: host arch mapped to the guest triple)
#   --out    destination path for the binary
#            (default: target/<triple>-unknown-linux-musl/release/lightr-init)
#
# Gates: bash -n clean; shellcheck-clean if shellcheck is present.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── defaults ─────────────────────────────────────────────────────────────────
# Map the host arch to the most useful guest target; override with --arch.
_host_arch="$(uname -m)"
case "${_host_arch}" in
    arm64 | aarch64) ARCH="aarch64" ;;
    x86_64)          ARCH="x86_64"  ;;
    *)               ARCH="aarch64" ;;  # reasonable default for Apple Silicon hosts
esac
OUT_PATH=""   # resolved after arg-parse when still empty

# ── arg parse ────────────────────────────────────────────────────────────────
while [ "$#" -gt 0 ]; do
    case "$1" in
        --arch)
            case "$2" in
                x86_64 | aarch64) ARCH="$2"; shift 2 ;;
                *) echo "build-init: --arch must be x86_64 or aarch64 (got: $2)" >&2; exit 2 ;;
            esac
            ;;
        --out) OUT_PATH="$2"; shift 2 ;;
        -h | --help) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "build-init: unknown arg: $1" >&2; exit 2 ;;
    esac
done

TARGET="${ARCH}-unknown-linux-musl"

# Resolve default out-path now that ARCH is settled.
if [ -z "${OUT_PATH}" ]; then
    OUT_PATH="${REPO_ROOT}/target/${TARGET}/release/lightr-init"
fi

# ── prerequisite checks ───────────────────────────────────────────────────────

# 1. zig
if ! command -v zig >/dev/null 2>&1; then
    echo "build-init: 'zig' not found." >&2
    echo "  Fix: brew install zig" >&2
    exit 3
fi

# 2. cargo-zigbuild — probe the binary directly. (Do NOT use
# `cargo zigbuild --version`: cargo-zigbuild's `zigbuild` subcommand rejects
# `--version` with "unexpected argument", so that check fails even when it IS
# installed. `command -v` mirrors the zig check above and is robust.)
if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "build-init: 'cargo-zigbuild' not found." >&2
    echo "  Fix: cargo install cargo-zigbuild" >&2
    exit 3
fi

# 3. target std — check the std lib dir directly via `rustc --print sysroot`.
# (Do NOT shell out to `rustup`: it may be absent even when the target std IS
# installed for the active toolchain, e.g. a pinned-toolchain PATH with no rustup
# proxy. The std dir is what `cargo zigbuild` actually needs.)
SYSROOT="$(rustc --print sysroot 2>/dev/null)"
if [ -z "${SYSROOT}" ] || [ ! -d "${SYSROOT}/lib/rustlib/${TARGET}" ]; then
    echo "build-init: target std '${TARGET}' not found under ${SYSROOT}/lib/rustlib." >&2
    echo "  Fix: rustup target add ${TARGET}" >&2
    exit 3
fi

# ── build ────────────────────────────────────────────────────────────────────
echo "build-init: building lightr-init for ${TARGET} ..." >&2

(
    cd "${REPO_ROOT}"
    cargo zigbuild -p lightr-init --release --target "${TARGET}"
)

BUILT="${REPO_ROOT}/target/${TARGET}/release/lightr-init"

# ── verify ELF + arch ────────────────────────────────────────────────────────
if ! command -v file >/dev/null 2>&1; then
    echo "build-init: 'file' not available; skipping ELF check." >&2
else
    FILE_OUT="$(file "${BUILT}")"
    echo "build-init: ${FILE_OUT}" >&2

    # Fail loud if the output is not a Linux ELF for the requested arch.
    case "${ARCH}" in
        aarch64)
            if ! echo "${FILE_OUT}" | grep -qE "ELF.*aarch64|ARM aarch64"; then
                echo "build-init: ERROR — output does not look like an aarch64 ELF." >&2
                echo "  file output: ${FILE_OUT}" >&2
                exit 4
            fi
            ;;
        x86_64)
            if ! echo "${FILE_OUT}" | grep -qE "ELF.*x86-64"; then
                echo "build-init: ERROR — output does not look like an x86-64 ELF." >&2
                echo "  file output: ${FILE_OUT}" >&2
                exit 4
            fi
            ;;
    esac
fi

# ── copy to requested out-path if different ──────────────────────────────────
if [ "${OUT_PATH}" != "${BUILT}" ]; then
    mkdir -p "$(dirname "${OUT_PATH}")"
    cp "${BUILT}" "${OUT_PATH}"
fi

echo
echo "── lightr-init ready (${ARCH}) ──────────────────────────────────────"
echo "  binary : ${OUT_PATH}"
echo
echo "  assemble a pack with:"
echo "    cargo run -p lightr-engine --example assemble-pack -- \\"
echo "      --kernel <kernel-image> --init ${OUT_PATH} \\"
echo "      --out build/vz-pack-${ARCH} --arch ${ARCH}"
echo "─────────────────────────────────────────────────────────────────────"
