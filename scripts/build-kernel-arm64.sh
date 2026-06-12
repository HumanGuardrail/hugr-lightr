#!/usr/bin/env bash
#
# build-kernel-arm64.sh — reproducible arm64 microVM kernel for the `vz` engine
# (Apple Virtualization.framework on Apple Silicon Macs).
#
# Produces  <out>/Image  — the kernel image the vz engine boots on arm64 — by
# CROSS-COMPILING the pinned Linux source inside a linux/amd64 container with the
# aarch64-linux-gnu toolchain (native host speed; NO arm64 emulation). Also emits
# <out>/vmlinux and <out>/kernel.config for inspection.
#
# STATUS (honesty): this is the by-construction twin of the RUN-VALIDATED
# scripts/build-kernel-x86.sh (identical flow; only ARCH=arm64 + CROSS_COMPILE +
# the uncompressed `Image` output differ). It has NOT yet been run end-to-end on
# this Intel host (the Docker VM saturated after a long build session). Run it on
# a clean Docker daemon or on the Apple Silicon target before relying on the Image.
#
# ── arm64 vs x86 (reasoning) ─────────────────────────────────────────────────
# Apple's VZLinuxBootLoader on arm64 boots the **uncompressed `Image`**
# (arch/arm64/boot/Image) — the arm64 Linux boot protocol, NOT the x86 bzImage
# path (see build-kernel-x86.sh) and NOT PVH (x86-only). arm64 is Apple's primary
# VZ target (their open-source Containerization ships an arm64 Image kernel), so
# this is the well-trodden path. The boot-critical config is identical in intent
# to x86: virtio-PCI transport + virtio-console (hvc0) + virtiofs + FUSE, all =y.
#
# The host-side shim fixes that unblocked the Intel boot — a dedicated dispatch
# queue (not .main) and VZSingleDirectoryShare — live in the SHARED Swift shim
# and are arch-independent, so they already apply here. The only arch-specific
# difference is this kernel image format (Image, below).
#
# Usage:  scripts/build-kernel-arm64.sh [--out <dir>]   (default: build/linux-pack-arm64)
#
# Gates: bash -n clean; shellcheck-clean if shellcheck is present.

set -euo pipefail

readonly KERNEL_VERSION="6.18.5"
readonly KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VERSION}.tar.xz"
readonly KERNEL_SHA256="189d1f409cef8d0d234210e04595172df392f8cb297e14b447ed95720e2fd940"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUT_DIR="${REPO_ROOT}/build/linux-pack-arm64"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --out) OUT_DIR="$2"; shift 2 ;;
        -h | --help) sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "build-kernel-arm64: unknown arg: $1" >&2; exit 2 ;;
    esac
done

command -v docker >/dev/null 2>&1 || {
    echo "build-kernel-arm64: docker is required (cross-build runs in a linux/amd64 container)." >&2
    exit 3
}

mkdir -p "${OUT_DIR}"

read -r -d '' INNER <<'INNER_EOF' || true
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq build-essential bc bison flex libelf-dev libssl-dev \
    xz-utils curl ca-certificates kmod gcc-aarch64-linux-gnu >/dev/null
cd /tmp
curl -fsSLO "__KERNEL_URL__"
echo "__KERNEL_SHA256__  linux-__KERNEL_VERSION__.tar.xz" | sha256sum -c -
tar xf "linux-__KERNEL_VERSION__.tar.xz"
cd "linux-__KERNEL_VERSION__"
export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
make defconfig >/dev/null
scripts/config \
    --enable HYPERVISOR_GUEST \
    --enable VIRTIO --enable VIRTIO_PCI --enable VIRTIO_CONSOLE \
    --enable VIRTIO_BLK --enable VIRTIO_NET --enable VIRTIO_FS --enable FUSE_FS \
    --enable BLK_DEV_INITRD --enable DEVTMPFS --enable DEVTMPFS_MOUNT \
    --enable PRINTK --enable TTY
make olddefconfig >/dev/null
echo "[config] boot-critical options:"
grep -E "^CONFIG_(VIRTIO_PCI|VIRTIO_CONSOLE|VIRTIO_FS|FUSE_FS|BLK_DEV_INITRD|DEVTMPFS)=" .config
make -j"$(nproc)" Image 2>&1 | tail -4
cp arch/arm64/boot/Image /out/Image
cp vmlinux /out/vmlinux
cp .config /out/kernel.config
echo "BUILD_DONE Image=$(wc -c </out/Image) vmlinux=$(wc -c </out/vmlinux)"
INNER_EOF
INNER="${INNER//__KERNEL_URL__/${KERNEL_URL}}"
INNER="${INNER//__KERNEL_SHA256__/${KERNEL_SHA256}}"
INNER="${INNER//__KERNEL_VERSION__/${KERNEL_VERSION}}"

echo "build-kernel-arm64: cross-building linux-${KERNEL_VERSION} Image (aarch64) ..." >&2
docker run --platform linux/amd64 -v "${OUT_DIR}:/out" debian:bookworm bash -c "${INNER}"

echo
echo "── arm64 microVM kernel ready ───────────────────────────────────"
echo "  Image   : ${OUT_DIR}/Image     (this is what the vz engine boots on Apple Silicon)"
echo "  vmlinux : ${OUT_DIR}/vmlinux    (inspection only)"
echo "  config  : ${OUT_DIR}/kernel.config"
echo
echo "  On an Apple Silicon Mac, assemble + run:"
echo "    cargo run -p lightr-engine --example assemble-pack -- \\"
echo "      --kernel ${OUT_DIR}/Image --init <aarch64 lightr-init> \\"
echo "      --out build/vz-pack-arm64 --arch aarch64 --kernel-version ${KERNEL_VERSION}"
echo "─────────────────────────────────────────────────────────────────"
