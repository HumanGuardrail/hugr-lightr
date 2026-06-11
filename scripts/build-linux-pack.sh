#!/usr/bin/env bash
#
# build-linux-pack.sh — reproducible recipe for a Lightr "linux pack".
#
# A pack is what the `vz` engine boots: a Linux `kernel` + an `initrd` whose
# `/init` is the `lightr-init` PID1 binary, plus a `pack.json` manifest. This
# script assembles a STRUCTURALLY-VALID pack end to end:
#
#   1. detect the guest cross-target toolchain (musl);
#   2. build `lightr-init` for the guest target;
#   3. obtain a minimal Linux kernel suitable for Virtualization.framework;
#   4. assemble  <out>/kernel + <out>/initrd + <out>/pack.json;
#   5. print a verify_pack-style structural summary.
#
# HONESTY: this box (Intel macOS, no Linux cross-toolchain by default) cannot
# build a real kernel. Where a required tool is absent the script DETECTS it,
# prints the EXACT install/command to fix it, and EXITS non-zero. It never
# fabricates a kernel or claims a boot. The kernel source is NAMED + PINNED
# (see KERNEL_* below) so the build is reproducible once the toolchain exists.
#
# ── Kernel source (NAMED + PINNED) ───────────────────────────────────────────
# We track the kernel Apple's open-source Containerization project builds for
# Virtualization.framework (apple/containerization, kernel/Makefile): mainline
# Linux from kernel.org, built with Apple's container-optimized arm64 config
# (virtiofs + AF_VSOCK in-tree — exactly what the vz engine needs). We pin the
# tarball + its sha256 so the fetch is verifiable and reproducible.
#
#   source : https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-<ver>.tar.xz
#   config : apple/containerization  kernel/config-arm64
#
# Alternative (documented, not automated here): consume Apple's prebuilt
# Containerization kernel binary directly (shipped via their `container`
# tooling) and pass it with --kernel to skip the from-source build.
#
# Usage:
#   scripts/build-linux-pack.sh [--out <dir>] [--arch <aarch64|x86_64>]
#                               [--kernel <prebuilt-vmlinux>]
#
#   --out     output pack directory       (default: ./build/linux-pack)
#   --arch    guest architecture          (default: host arch mapped to a
#                                           linux musl target; aarch64|x86_64)
#   --kernel  use this prebuilt kernel image instead of building one from
#             source (skips the kernel BUILD step; still assembles+verifies).
#
# Gates: shellcheck-clean (if shellcheck present) and `bash -n` clean.

set -euo pipefail

# ── Pinned kernel coordinates ────────────────────────────────────────────────
# Matches apple/containerization kernel/Makefile (v6.x line). Bump together
# with the sha256 when tracking a new pin.
readonly KERNEL_VERSION="6.18.5"
readonly KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VERSION}.tar.xz"
readonly KERNEL_SHA256="189d1f409cef8d0d234210e04595172df392f8cb297e14b447ed95720e2fd940"
# Apple's container-optimized arm64 kernel config (raw, pinned to a tag would
# be stricter; main is documented here and easy to pin once a tag is chosen).
readonly KERNEL_CONFIG_URL="https://raw.githubusercontent.com/apple/containerization/main/kernel/config-arm64"

# ── Repo + output layout ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

OUT_DIR="${REPO_ROOT}/build/linux-pack"
ARCH=""
PREBUILT_KERNEL=""

die() {
    echo "build-linux-pack: error: $*" >&2
    exit 1
}

note() {
    echo "build-linux-pack: $*" >&2
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1
}

# ── Arg parsing ──────────────────────────────────────────────────────────────
while [ "$#" -gt 0 ]; do
    case "$1" in
        --out)
            [ "$#" -ge 2 ] || die "--out requires a value"
            OUT_DIR="$2"
            shift 2
            ;;
        --arch)
            [ "$#" -ge 2 ] || die "--arch requires a value"
            ARCH="$2"
            shift 2
            ;;
        --kernel)
            [ "$#" -ge 2 ] || die "--kernel requires a value"
            PREBUILT_KERNEL="$2"
            shift 2
            ;;
        -h | --help)
            sed -n '2,40p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
done

# ── Resolve arch → guest musl target triple ──────────────────────────────────
if [ -z "${ARCH}" ]; then
    case "$(uname -m)" in
        arm64 | aarch64) ARCH="aarch64" ;;
        x86_64 | amd64) ARCH="x86_64" ;;
        *) die "unsupported host arch $(uname -m); pass --arch aarch64|x86_64" ;;
    esac
fi

case "${ARCH}" in
    aarch64) TARGET="aarch64-unknown-linux-musl" ;;
    x86_64) TARGET="x86_64-unknown-linux-musl" ;;
    *) die "unsupported --arch '${ARCH}' (want aarch64 or x86_64)" ;;
esac

note "target arch     : ${ARCH}"
note "guest triple    : ${TARGET}"
note "output pack dir : ${OUT_DIR}"

# ── cargo presence ───────────────────────────────────────────────────────────
if ! need_cmd cargo; then
    die "cargo not found on PATH. Install Rust (https://rustup.rs) or, in this
  repo's environment, run:
      export PATH=\"\$HOME/.rustup/toolchains/1.96.0-x86_64-apple-darwin/bin:\$PATH\""
fi

# ── 1. Detect the guest cross-target toolchain ───────────────────────────────
# The std library for the musl target must be installed (rustup component). If
# rustup is present we can check/instruct precisely; otherwise we instruct
# generically and exit (no faking).
detect_rust_target() {
    if need_cmd rustup; then
        if rustup target list --installed 2>/dev/null | grep -qx "${TARGET}"; then
            note "rust target     : ${TARGET} (installed)"
            return 0
        fi
        die "guest Rust target '${TARGET}' is not installed. Install it with:
      rustup target add ${TARGET}
  (musl targets need no extra C toolchain to build a static lightr-init.)"
    fi
    # No rustup: we cannot enumerate targets. A bare cargo+rustc may still have
    # the target's std; we let the build attempt surface the precise error
    # rather than guess. Warn so the failure mode is understood.
    note "rustup not found — cannot pre-verify the '${TARGET}' std component."
    note "if the build below fails for a missing std, install rustup and run:"
    note "    rustup target add ${TARGET}"
}
detect_rust_target

# ── 2. Build lightr-init for the guest target ────────────────────────────────
note "building lightr-init for ${TARGET} ..."
if ! cargo build -p lightr-init --release --target "${TARGET}" \
    --manifest-path "${REPO_ROOT}/Cargo.toml"; then
    die "failed to build lightr-init for ${TARGET}.
  If the error mentions a missing std for ${TARGET}, run:
      rustup target add ${TARGET}
  A musl cross-LINKER is only needed if lightr-init grows C deps; the pure-Rust
  static build does not. If a linker error appears, install one, e.g.:
      brew install FiloSottile/musl-cross/musl-cross   # macOS
      # then set: CARGO_TARGET_${TARGET//-/_}_LINKER (uppercased) to the cc"
fi

INIT_BIN="${REPO_ROOT}/target/${TARGET}/release/lightr-init"
[ -f "${INIT_BIN}" ] || die "expected lightr-init at ${INIT_BIN} but it is missing"
note "lightr-init     : ${INIT_BIN}"

# ── 3. Obtain a minimal Linux kernel ─────────────────────────────────────────
# Either the caller supplied a prebuilt kernel (--kernel), or we build one from
# the pinned source. The from-source build needs a Linux cross-build toolchain
# that this macOS host does not ship; we DETECT + INSTRUCT + EXIT (no fake).
KERNEL_IMG=""

if [ -n "${PREBUILT_KERNEL}" ]; then
    [ -f "${PREBUILT_KERNEL}" ] || die "--kernel '${PREBUILT_KERNEL}' does not exist"
    [ -s "${PREBUILT_KERNEL}" ] || die "--kernel '${PREBUILT_KERNEL}' is empty"
    KERNEL_IMG="${PREBUILT_KERNEL}"
    note "kernel (prebuilt): ${KERNEL_IMG}"
else
    note "no --kernel given: building from pinned source ${KERNEL_URL}"

    # Tools the from-source kernel build needs. None of these ship on a stock
    # Intel macOS; detect them all and instruct precisely.
    MISSING=()
    for tool in make flex bison; do
        need_cmd "${tool}" || MISSING+=("${tool}")
    done
    # A Linux-targeting cross compiler. On macOS the kernel cannot be built
    # natively; you build it inside a Linux builder (Docker/OrbStack/Lima) or
    # with a linux-gnu cross toolchain.
    if ! need_cmd "${ARCH}-linux-gnu-gcc" && ! need_cmd docker && ! need_cmd lima; then
        MISSING+=("a Linux kernel build environment (docker/lima/OrbStack or ${ARCH}-linux-gnu-gcc)")
    fi

    if [ "${#MISSING[@]}" -ne 0 ]; then
        {
            echo "build-linux-pack: cannot build the kernel from source on this host."
            echo "  Missing: ${MISSING[*]}"
            echo
            echo "  The Linux kernel does not cross-build natively on macOS. Choose ONE:"
            echo
            echo "  (A) Build inside a Linux builder (recommended — mirrors Apple's"
            echo "      containerization kernel/Makefile, which builds in a container):"
            echo "        - install Docker Desktop / OrbStack / Lima"
            echo "        - in a linux/${ARCH} container with build-essential + flex + bison:"
            echo "            curl -fsSLO ${KERNEL_URL}"
            echo "            echo '${KERNEL_SHA256}  linux-${KERNEL_VERSION}.tar.xz' | sha256sum -c -"
            echo "            tar xf linux-${KERNEL_VERSION}.tar.xz && cd linux-${KERNEL_VERSION}"
            echo "            curl -fsSL ${KERNEL_CONFIG_URL} -o .config"
            echo "            make olddefconfig && make -j\"\$(nproc)\""
            echo "            # arm64 image: arch/arm64/boot/Image ; x86: arch/x86/boot/bzImage"
            echo "        - then re-run with: --kernel <path-to-built-Image>"
            echo
            echo "  (B) Reuse Apple's prebuilt Containerization kernel binary and pass"
            echo "      it with: --kernel <path-to-vmlinux>"
            echo
            echo "  No kernel was fabricated. Exiting non-zero."
        } >&2
        exit 3
    fi

    # If we get here a builder exists; do a verifiable fetch into a workdir.
    # (The actual in-container compile is environment-specific and intentionally
    # left to path (A) above; we still pin+verify the source tarball here so the
    # reproducible inputs are exercised.)
    WORK="$(mktemp -d)"
    trap 'rm -rf "${WORK}"' EXIT
    TARBALL="${WORK}/linux-${KERNEL_VERSION}.tar.xz"
    note "fetching pinned kernel source ..."
    if need_cmd curl; then
        curl -fsSL "${KERNEL_URL}" -o "${TARBALL}" || die "kernel source fetch failed"
    elif need_cmd wget; then
        wget -q "${KERNEL_URL}" -O "${TARBALL}" || die "kernel source fetch failed"
    else
        die "neither curl nor wget available to fetch ${KERNEL_URL}"
    fi

    note "verifying sha256 ..."
    if need_cmd sha256sum; then
        echo "${KERNEL_SHA256}  ${TARBALL}" | sha256sum -c - >/dev/null \
            || die "kernel source sha256 mismatch — refusing to proceed"
    elif need_cmd shasum; then
        got="$(shasum -a 256 "${TARBALL}" | awk '{print $1}')"
        [ "${got}" = "${KERNEL_SHA256}" ] \
            || die "kernel source sha256 mismatch (got ${got}) — refusing to proceed"
    else
        die "no sha256sum/shasum to verify the kernel source"
    fi
    note "kernel source verified (linux-${KERNEL_VERSION}, sha256 ok)"

    die "kernel source fetched + verified, but the in-host compile is not
  performed on macOS (see path (A) above to build inside a Linux builder, then
  re-run with --kernel <built-image>). No fake kernel produced."
fi

# ── 4. Assemble the pack (kernel + initrd + pack.json) ───────────────────────
note "assembling pack into ${OUT_DIR} ..."
cargo run -q -p lightr-engine --example assemble-pack \
    --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --kernel "${KERNEL_IMG}" \
    --init "${INIT_BIN}" \
    --out "${OUT_DIR}" \
    --arch "${ARCH}" \
    --kernel-version "${KERNEL_VERSION}" \
    || die "pack assembly/verification failed"

# ── 5. Final structural summary (verify_pack-style) ──────────────────────────
echo
echo "── linux pack ready ─────────────────────────────────────────────"
echo "  dir     : ${OUT_DIR}"
echo "  kernel  : $(wc -c <"${OUT_DIR}/kernel" | tr -d ' ') bytes"
echo "  initrd  : ${OUT_DIR}/initrd  (newc cpio, /init = lightr-init)"
echo "  manifest: ${OUT_DIR}/pack.json"
echo
echo "  install with:  lightr engine install-pack ${OUT_DIR}"
echo "─────────────────────────────────────────────────────────────────"
