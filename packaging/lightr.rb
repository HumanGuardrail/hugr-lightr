# lightr.rb — Homebrew formula template
#
# LICENSE GATE: This formula is prepared but MUST NOT be pushed to a public tap
# until ADR-0008 (license) is Accepted. The binary ships license=UNLICENSED,
# publish=false. See docs/adr/0008-license.md.
#
# Usage (after license gate is lifted):
#   brew tap hugr/tap
#   brew install lightr
#
# TODO: before publishing
#   1. Confirm ADR-0008 is Accepted with a concrete SPDX license identifier.
#   2. Replace all __TODO_* placeholders below with real values from the
#      GitHub Release (packaging/release.sh prints the sha256).
#   3. Push this file to the hugr/homebrew-tap repo.

class Lightr < Formula
  desc "lightr — lightweight container runtime (local dev tool)"
  homepage "https://github.com/humangr/hugr-lightr"

  # TODO: set version from the published GitHub Release tag (e.g. "0.1.0")
  version "__TODO_VERSION__"

  # LICENSE GATE — ADR-0008 must be Accepted before this formula is published.
  # Replace the license string below with the accepted SPDX identifier, e.g.:
  #   license "MIT"
  #   license "Apache-2.0"
  #   license "BUSL-1.1"
  license "__TODO_LICENSE__"  # BLOCKED: ADR-0008 not yet Accepted

  on_macos do
    if Hardware::CPU.arm?
      # TODO: replace with real URL and sha256 from `packaging/release.sh`
      url "__TODO_URL_DARWIN_ARM64__"
      sha256 "__TODO_SHA256_DARWIN_ARM64__"
    else
      # TODO: replace with real URL and sha256 from `packaging/release.sh`
      url "__TODO_URL_DARWIN_X86_64__"
      sha256 "__TODO_SHA256_DARWIN_X86_64__"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      # TODO: replace with real URL and sha256 from `packaging/release.sh`
      url "__TODO_URL_LINUX_ARM64__"
      sha256 "__TODO_SHA256_LINUX_ARM64__"
    else
      # TODO: replace with real URL and sha256 from `packaging/release.sh`
      url "__TODO_URL_LINUX_X86_64__"
      sha256 "__TODO_SHA256_LINUX_X86_64__"
    end
  end

  # No bottles — we ship a pre-built binary inside the tarball.
  # The tarball layout is: lightr (the binary at the root)
  bottle :unneeded

  def install
    bin.install "lightr"
  end

  test do
    output = shell_output("#{bin}/lightr --version").strip
    assert_match version.to_s, output
  end
end
