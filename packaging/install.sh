#!/usr/bin/env sh
# install.sh — lightr installer (curl|sh)
#
# LICENSE GATE: This installer is prepared but MUST NOT be made public until
# ADR-0008 (license) is Accepted. The binary ships license=UNLICENSED,
# publish=false. See docs/adr/0008-license.md.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/<org>/hugr-lightr/main/packaging/install.sh | sh
#
# Detects OS (Darwin/Linux) + arch (arm64/x86_64), downloads the matching
# release tarball from RELEASES_URL, verifies sha256, and installs `lightr`
# to ~/.local/bin (or /usr/local/bin with a prompt).

set -eu

# ---------------------------------------------------------------------------
# PLACEHOLDER — replace both values when a real release is published after
# ADR-0008 is Accepted. The script detects the placeholder and fails loudly.
# ---------------------------------------------------------------------------
RELEASES_URL="__PLACEHOLDER__RELEASES_URL__"
VERSION="__PLACEHOLDER__VERSION__"
# ---------------------------------------------------------------------------

BINARY_NAME="lightr"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
FALLBACK_INSTALL_DIR="/usr/local/bin"

# ── helpers ─────────────────────────────────────────────────────────────────

die() {
    printf 'ERROR: %s\n' "$1" >&2
    exit 1
}

info() {
    printf '=> %s\n' "$1"
}

# ── gate: fail loudly if placeholder URL is still set ───────────────────────

case "${RELEASES_URL}" in
    *__PLACEHOLDER__*)
        die "no published release yet (license-gated, ADR-0008)"
        ;;
esac

case "${VERSION}" in
    *__PLACEHOLDER__*)
        die "no published release yet (license-gated, ADR-0008)"
        ;;
esac

# ── detect OS and arch ──────────────────────────────────────────────────────

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
    Darwin) OS_TAG="darwin" ;;
    Linux)  OS_TAG="linux"  ;;
    *)      die "unsupported OS: ${OS}" ;;
esac

case "${ARCH}" in
    arm64|aarch64) ARCH_TAG="arm64"  ;;
    x86_64|amd64)  ARCH_TAG="x86_64" ;;
    *)             die "unsupported architecture: ${ARCH}" ;;
esac

TARBALL="${BINARY_NAME}-${VERSION}-${OS_TAG}-${ARCH_TAG}.tar.gz"
CHECKSUM_FILE="${TARBALL}.sha256"
DOWNLOAD_URL="${RELEASES_URL}/${VERSION}/${TARBALL}"
CHECKSUM_URL="${RELEASES_URL}/${VERSION}/${CHECKSUM_FILE}"

# ── download ─────────────────────────────────────────────────────────────────

TMP_DIR="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '${TMP_DIR}'" EXIT INT TERM

info "Downloading ${TARBALL} ..."

if command -v curl > /dev/null 2>&1; then
    curl -fsSL --output "${TMP_DIR}/${TARBALL}" "${DOWNLOAD_URL}" \
        || die "download failed: ${DOWNLOAD_URL}"
    curl -fsSL --output "${TMP_DIR}/${CHECKSUM_FILE}" "${CHECKSUM_URL}" \
        || die "checksum download failed: ${CHECKSUM_URL}"
elif command -v wget > /dev/null 2>&1; then
    wget -q -O "${TMP_DIR}/${TARBALL}" "${DOWNLOAD_URL}" \
        || die "download failed: ${DOWNLOAD_URL}"
    wget -q -O "${TMP_DIR}/${CHECKSUM_FILE}" "${CHECKSUM_URL}" \
        || die "checksum download failed: ${CHECKSUM_URL}"
else
    die "neither curl nor wget found; cannot download"
fi

# ── verify sha256 ────────────────────────────────────────────────────────────

info "Verifying sha256 checksum ..."

EXPECTED_HASH="$(awk '{print $1}' "${TMP_DIR}/${CHECKSUM_FILE}")"

if command -v sha256sum > /dev/null 2>&1; then
    ACTUAL_HASH="$(sha256sum "${TMP_DIR}/${TARBALL}" | awk '{print $1}')"
elif command -v shasum > /dev/null 2>&1; then
    ACTUAL_HASH="$(shasum -a 256 "${TMP_DIR}/${TARBALL}" | awk '{print $1}')"
else
    die "no sha256 utility found (sha256sum or shasum required)"
fi

if [ "${ACTUAL_HASH}" != "${EXPECTED_HASH}" ]; then
    die "checksum mismatch — download may be corrupt or tampered"
fi

info "Checksum OK."

# ── extract ──────────────────────────────────────────────────────────────────

tar -xzf "${TMP_DIR}/${TARBALL}" -C "${TMP_DIR}" \
    || die "failed to extract tarball"

EXTRACTED_BIN="${TMP_DIR}/${BINARY_NAME}"
if [ ! -f "${EXTRACTED_BIN}" ]; then
    die "binary '${BINARY_NAME}' not found in tarball"
fi
chmod +x "${EXTRACTED_BIN}"

# ── install ───────────────────────────────────────────────────────────────────

# Prefer ~/.local/bin (no sudo); fall back to /usr/local/bin with prompt.
if [ -d "${DEFAULT_INSTALL_DIR}" ] || mkdir -p "${DEFAULT_INSTALL_DIR}" 2>/dev/null; then
    INSTALL_DIR="${DEFAULT_INSTALL_DIR}"
else
    printf "Cannot write to %s. Install to %s? (requires sudo) [y/N] " \
        "${DEFAULT_INSTALL_DIR}" "${FALLBACK_INSTALL_DIR}"
    read -r ANSWER
    case "${ANSWER}" in
        y|Y) INSTALL_DIR="${FALLBACK_INSTALL_DIR}" ;;
        *)   die "installation cancelled" ;;
    esac
fi

if [ "${INSTALL_DIR}" = "${FALLBACK_INSTALL_DIR}" ]; then
    sudo install -m 755 "${EXTRACTED_BIN}" "${INSTALL_DIR}/${BINARY_NAME}" \
        || die "sudo install failed"
else
    install -m 755 "${EXTRACTED_BIN}" "${INSTALL_DIR}/${BINARY_NAME}" \
        || cp "${EXTRACTED_BIN}" "${INSTALL_DIR}/${BINARY_NAME}" \
        || die "install failed"
fi

info "${BINARY_NAME} installed to ${INSTALL_DIR}/${BINARY_NAME}"

# Remind user to add to PATH if needed
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        printf "\nNote: add '%s' to your PATH:\n" "${INSTALL_DIR}"
        printf "  export PATH=\"%s:\$PATH\"\n\n" "${INSTALL_DIR}"
        ;;
esac

info "Done. Run: ${BINARY_NAME} --version"
