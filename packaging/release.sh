#!/usr/bin/env bash
# release.sh — local release-build recipe for lightr
#
# LICENSE GATE: This script builds release artifacts but MUST NOT upload them
# until ADR-0008 (license) is Accepted. See docs/adr/0008-license.md.
#
# What it does:
#   1. Builds --release for the host target
#   2. Strips the binary
#   3. Computes sha256
#   4. Produces packaging/dist/lightr-<version>-<os>-<arch>.tar.gz
#   5. Prints the sha256 (needed for Homebrew formula + install.sh)
#   6. Prints a reminder that publishing is gated on ADR-0008
#
# What it does NOT do:
#   - Does not run `gh release create` or upload anything
#   - Does not modify Cargo.toml or any source file
#
# Founder-Mac PATH workaround:
#   If `cargo` is not on PATH (rustup proxy may fail on some macOS setups),
#   uncomment and use the direct toolchain path:
#
#   export PATH="$HOME/.rustup/toolchains/1.96.0-x86_64-apple-darwin/bin:$PATH"
#
# The CI workflow uses `rustup` normally (GitHub-hosted runners, no workaround needed).

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

BINARY_NAME="lightr"
DIST_DIR="$(cd "$(dirname "$0")" && pwd)/dist"

# Detect Rust toolchain; apply founder-Mac workaround if needed.
if ! command -v cargo > /dev/null 2>&1; then
    TOOLCHAIN_PATH="$HOME/.rustup/toolchains/1.96.0-x86_64-apple-darwin/bin"
    if [ -d "$TOOLCHAIN_PATH" ]; then
        echo "=> cargo not on PATH; applying rustup toolchain workaround: $TOOLCHAIN_PATH"
        export PATH="$TOOLCHAIN_PATH:$PATH"
    else
        echo "ERROR: cargo not found and toolchain path '$TOOLCHAIN_PATH' does not exist." >&2
        echo "       Install Rust via rustup or adjust TOOLCHAIN_PATH in this script." >&2
        exit 1
    fi
fi

# ---------------------------------------------------------------------------
# Detect version from workspace Cargo.toml
# ---------------------------------------------------------------------------

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Extract version from [workspace.package] section
VERSION="$(grep -A 10 '\[workspace\.package\]' "$REPO_ROOT/Cargo.toml" \
    | grep '^version' \
    | head -1 \
    | sed 's/.*= *"\(.*\)"/\1/')"

if [ -z "$VERSION" ]; then
    echo "ERROR: could not extract version from $REPO_ROOT/Cargo.toml" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Detect OS and arch
# ---------------------------------------------------------------------------

OS_RAW="$(uname -s)"
ARCH_RAW="$(uname -m)"

case "$OS_RAW" in
    Darwin) OS_TAG="darwin" ;;
    Linux)  OS_TAG="linux"  ;;
    *)      echo "ERROR: unsupported OS: $OS_RAW" >&2; exit 1 ;;
esac

case "$ARCH_RAW" in
    arm64|aarch64) ARCH_TAG="arm64"  ;;
    x86_64|amd64)  ARCH_TAG="x86_64" ;;
    *)             echo "ERROR: unsupported arch: $ARCH_RAW" >&2; exit 1 ;;
esac

TARBALL_NAME="${BINARY_NAME}-${VERSION}-${OS_TAG}-${ARCH_TAG}.tar.gz"
CHECKSUM_NAME="${TARBALL_NAME}.sha256"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

echo "=> Building ${BINARY_NAME} ${VERSION} (${OS_TAG}/${ARCH_TAG}) ..."
(cd "$REPO_ROOT" && cargo build --release -p lightr-cli)

BUILT_BIN="$REPO_ROOT/target/release/$BINARY_NAME"
if [ ! -f "$BUILT_BIN" ]; then
    echo "ERROR: expected binary not found at $BUILT_BIN" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Strip
# ---------------------------------------------------------------------------

echo "=> Stripping binary ..."
if command -v strip > /dev/null 2>&1; then
    strip "$BUILT_BIN"
else
    echo "   (strip not found; skipping)"
fi

# ---------------------------------------------------------------------------
# Package
# ---------------------------------------------------------------------------

mkdir -p "$DIST_DIR"

# Copy binary into a staging area so the tarball root contains just `lightr`
STAGE_DIR="$(mktemp -d)"
# STAGE_DIR is set once above; expansion at trap-set time is intentional here.
# shellcheck disable=SC2064
trap "rm -rf '$STAGE_DIR'" EXIT INT TERM

cp "$BUILT_BIN" "$STAGE_DIR/$BINARY_NAME"
chmod 755 "$STAGE_DIR/$BINARY_NAME"

TARBALL_PATH="$DIST_DIR/$TARBALL_NAME"
echo "=> Creating tarball: $TARBALL_PATH ..."
tar -czf "$TARBALL_PATH" -C "$STAGE_DIR" "$BINARY_NAME"

# ---------------------------------------------------------------------------
# Checksum
# ---------------------------------------------------------------------------

echo "=> Computing sha256 ..."
if command -v sha256sum > /dev/null 2>&1; then
    SHA256="$(sha256sum "$TARBALL_PATH" | awk '{print $1}')"
elif command -v shasum > /dev/null 2>&1; then
    SHA256="$(shasum -a 256 "$TARBALL_PATH" | awk '{print $1}')"
else
    echo "ERROR: no sha256 utility found" >&2
    exit 1
fi

echo "$SHA256  $TARBALL_NAME" > "$DIST_DIR/$CHECKSUM_NAME"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "artifact ready at $TARBALL_PATH"
echo "sha256: $SHA256"
echo ""
echo "=> Update packaging/install.sh and packaging/lightr.rb with the above sha256."
echo ""
echo "PUBLISHING IS GATED (ADR-0008): do not run 'gh release create' or upload"
echo "any artifact until docs/adr/0008-license.md is status: Accepted."
