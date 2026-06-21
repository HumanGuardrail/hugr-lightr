#!/usr/bin/env bash
#
# build-kernel-x86.sh — reproducible x86_64 microVM kernel for the `vz` engine
# (Apple Virtualization.framework on Intel Macs).
#
# Produces  <out>/bzImage  — the kernel image the vz engine boots — from the
# pinned Linux source, built inside a linux/amd64 container (the kernel does not
# cross-build natively on macOS). Also emits <out>/vmlinux and <out>/kernel.config
# for inspection.
#
# ── WHY bzImage (not vmlinux), validated on Intel 2026-06-12 ──────────────────
# Apple's VZLinuxBootLoader on x86_64 boots via the x86 setup-header / real-mode
# protocol: it wants the **bzImage**. A raw `vmlinux` ELF — even one built with
# CONFIG_PVH=y carrying the XEN_ELFNOTE_PHYS32_ENTRY note (the Firecracker/Cloud-
# Hypervisor PVH direct-boot path) — is rejected by VZ with
#   VZErrorDomain Code=1 "Internal Virtualization error."
# whereas the bzImage boots to a clean stop. (PVH=y is left enabled below: it is
# harmless and keeps the same image PVH-bootable under other hypervisors; VZ
# simply uses the bzImage real-mode entry. A future slim-down may drop it.)
#
# The boot-critical kernel options (all =y, built-in — the initramfs holds only
# /init, so nothing is a module): virtio-PCI transport (VZ exposes every device
# as virtio-pci), virtio-console (the guest's only console, hvc0), virtiofs +
# FUSE (the rootfs/store shares + the host<->guest file channel).
#
# ── BOOT NETWORKING: two NICs, two DIFFERENT IP mechanisms (ADR-0018) ─────────
# A networked guest has a DUAL-NIC layout (ADR-0018 §Decision 2):
#   • eth0 — VZNATNetworkDeviceAttachment (internet egress). Its IP is acquired
#     by the KERNEL at boot from the `ip=dhcp` cmdline the vz shim appends
#     (crates/lightr-engine/shim/vz.swift). THIS is exactly what CONFIG_IP_PNP +
#     CONFIG_IP_PNP_DHCP enable — kernel-level IP autoconfig brings eth0 up with
#     an address BEFORE PID1 runs, so lightr-init reads the leased eth0 address
#     immediately on the boot path (crates/lightr-init/src/bin/init.rs).
#   • eth1 — VZFileHandleNetworkDeviceAttachment (the userspace L2 mesh switch).
#     Its IP is leased in USERSPACE, NOT by the kernel: the guest runs busybox
#     `udhcpc -i eth1` against the host-side switch's embedded DHCP server
#     (crates/lightr-run/src/vswitch/dhcp.rs). Kernel IP-PNP autoconfigures only
#     the FIRST NIC, so eth1 is deliberately a userspace-DHCP NIC — no kernel
#     change is needed (or possible) for the eth1 mesh IP.
# Both NICs are virtio-net (VZ exposes the file-handle NIC as virtio-pci too), so
# VIRTIO_NET (enabled below) is the SINGLE driver for eth0 AND eth1 — no extra
# driver is needed for the mesh NIC. The full split is validated end-to-end by
# crates/lightr-run/examples/s5-vz-switch.rs (eth0 kernel ip=dhcp + eth1 udhcpc
# lease from our switch).
#
# ── NO-DOCKER NOTES ──────────────────────────────────────────────────────────
# lightr-init (the guest PID 1 binary) is built DOCKER-FREE via
#   scripts/build-init.sh
# which cross-compiles to <arch>-unknown-linux-musl on macOS using zig as the
# linker (cargo-zigbuild). No container is needed for the init binary.
#
# The KERNEL, however, requires a Linux build environment (the kernel does not
# cross-build natively on macOS). Docker is one option (this script). Alternatives:
#   • Apple's Containerization framework ships a prebuilt arm64 VZ kernel that
#     works directly on Apple Silicon — no kernel build required for arm64.
#   • Build the kernel on a Linux target machine and copy out the bzImage.
# ─────────────────────────────────────────────────────────────────────────────
#
# Usage:  scripts/build-kernel-x86.sh [--out <dir>]        (default: build/linux-pack-x86)
#
# Gates: bash -n clean; shellcheck-clean if shellcheck is present.

set -euo pipefail

readonly KERNEL_VERSION="6.18.5"
readonly KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VERSION}.tar.xz"
readonly KERNEL_SHA256="189d1f409cef8d0d234210e04595172df392f8cb297e14b447ed95720e2fd940"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUT_DIR="${REPO_ROOT}/build/linux-pack-x86"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --out) OUT_DIR="$2"; shift 2 ;;
        -h | --help) sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "build-kernel-x86: unknown arg: $1" >&2; exit 2 ;;
    esac
done

command -v docker >/dev/null 2>&1 || {
    echo "build-kernel-x86: docker is required (the kernel builds in a linux/amd64 container)." >&2
    exit 3
}

mkdir -p "${OUT_DIR}"

# The in-container build recipe (kept inline so this script is self-contained).
read -r -d '' INNER <<'INNER_EOF' || true
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq build-essential bc bison flex libelf-dev libssl-dev \
    xz-utils curl ca-certificates kmod >/dev/null
cd /tmp
curl -fsSLO "__KERNEL_URL__"
echo "__KERNEL_SHA256__  linux-__KERNEL_VERSION__.tar.xz" | sha256sum -c -
tar xf "linux-__KERNEL_VERSION__.tar.xz"
cd "linux-__KERNEL_VERSION__"
make x86_64_defconfig >/dev/null
scripts/config \
    --enable PVH --enable XEN --enable XEN_PVH \
    --enable HYPERVISOR_GUEST --enable PARAVIRT \
    --enable VIRTIO --enable VIRTIO_PCI --enable VIRTIO_CONSOLE \
    --enable VIRTIO_BLK --enable VIRTIO_NET --enable VIRTIO_FS --enable FUSE_FS \
    --enable BLK_DEV_INITRD --enable DEVTMPFS --enable DEVTMPFS_MOUNT \
    --enable PRINTK --enable TTY \
    --enable IP_PNP --enable IP_PNP_DHCP
make olddefconfig >/dev/null
echo "[config] boot-critical options:"
grep -E "^CONFIG_(PVH|VIRTIO_PCI|VIRTIO_CONSOLE|VIRTIO_FS|FUSE_FS|BLK_DEV_INITRD|DEVTMPFS)=" .config
# ADR-0018: kernel-level DHCP autoconfig (`ip=dhcp` cmdline) brings up eth0 (the
# NAT egress NIC) at boot — see the BOOT NETWORKING block in the header. (eth1,
# the mesh NIC, leases separately in userspace via udhcpc; it does not depend on
# kernel IP-PNP.) Fail closed if either option silently drops from defconfig.
grep -q "^CONFIG_IP_PNP=y"      .config || { echo "[config] MISSING: CONFIG_IP_PNP=y"      >&2; exit 1; }
grep -q "^CONFIG_IP_PNP_DHCP=y" .config || { echo "[config] MISSING: CONFIG_IP_PNP_DHCP=y" >&2; exit 1; }
# VIRTIO_NET is the single driver for BOTH eth0 (NAT) and eth1 (file-handle mesh
# NIC — VZ exposes it as virtio-pci too). Without it neither NIC appears, so no
# boot IP on either. Fail closed.
grep -q "^CONFIG_VIRTIO_NET=y"  .config || { echo "[config] MISSING: CONFIG_VIRTIO_NET=y"  >&2; exit 1; }
make -j"$(nproc)" vmlinux bzImage 2>&1 | tail -4
cp vmlinux /out/vmlinux
cp arch/x86/boot/bzImage /out/bzImage
cp .config /out/kernel.config
echo "BUILD_DONE bzImage=$(wc -c </out/bzImage) vmlinux=$(wc -c </out/vmlinux)"
INNER_EOF
INNER="${INNER//__KERNEL_URL__/${KERNEL_URL}}"
INNER="${INNER//__KERNEL_SHA256__/${KERNEL_SHA256}}"
INNER="${INNER//__KERNEL_VERSION__/${KERNEL_VERSION}}"

echo "build-kernel-x86: building linux-${KERNEL_VERSION} bzImage in a linux/amd64 container ..." >&2
docker run --platform linux/amd64 -v "${OUT_DIR}:/out" debian:bookworm bash -c "${INNER}"

echo
echo "── x86_64 microVM kernel ready ──────────────────────────────────"
echo "  bzImage : ${OUT_DIR}/bzImage   (this is what the vz engine boots)"
echo "  vmlinux : ${OUT_DIR}/vmlinux   (inspection only; NOT bootable under VZ)"
echo "  config  : ${OUT_DIR}/kernel.config"
echo
echo "  assemble a pack with:"
echo "    cargo run -p lightr-engine --example assemble-pack -- \\"
echo "      --kernel ${OUT_DIR}/bzImage --init <lightr-init> \\"
echo "      --out build/vz-pack --arch x86_64 --kernel-version ${KERNEL_VERSION}"
echo "─────────────────────────────────────────────────────────────────"
