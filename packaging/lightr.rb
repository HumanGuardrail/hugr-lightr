# lightr.rb — Homebrew formula template
#
# LICENSE: Apache-2.0 (ADR-0008 Accepted 2026-06-12). Formula is prepared but
# MUST NOT be pushed to a public tap until the GTM gate clears (whitepaper
# §9.8 — release timing after Runners M1). See packaging/README.md.
#
# Usage (after GTM gate is lifted):
#   brew tap hugr/tap
#   brew install lightr
#
# TODO: before publishing
#   1. Confirm the GTM timing call has been made.
#   2. Replace all __TODO_* placeholders below with real values from the
#      GitHub Release produced by .github/workflows/release.yml.
#      Asset URL pattern (release.yml § Package tarball):
#        https://github.com/<org>/hugr-lightr/releases/download/v<ver>/lightr-<ver>-<os>-<arch>.tar.gz
#      sha256 values are in the SHA256SUMS file attached to each GitHub Release,
#      and are also printed by packaging/release.sh for local builds.
#   3. Push this file to the hugr/homebrew-tap repo.

class Lightr < Formula
  desc "lightr — lightweight container runtime (local dev tool)"
  homepage "https://github.com/humangr/hugr-lightr"

  # TODO: set version from the published GitHub Release tag (e.g. "0.1.0")
  version "__TODO_VERSION__"

  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      # TODO: replace with real values once .github/workflows/release.yml
      # has produced a signed (or -unsigned) release for v<ver>.
      # URL pattern: https://github.com/<org>/hugr-lightr/releases/download/v<ver>/lightr-<ver>-darwin-arm64.tar.gz
      url "__TODO_URL_DARWIN_ARM64__"
      sha256 "__TODO_SHA256_DARWIN_ARM64__"
    else
      # URL pattern: https://github.com/<org>/hugr-lightr/releases/download/v<ver>/lightr-<ver>-darwin-x86_64.tar.gz
      url "__TODO_URL_DARWIN_X86_64__"
      sha256 "__TODO_SHA256_DARWIN_X86_64__"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      # Linux arm64 not in the current release matrix — add runner if needed.
      # URL pattern: https://github.com/<org>/hugr-lightr/releases/download/v<ver>/lightr-<ver>-linux-arm64.tar.gz
      url "__TODO_URL_LINUX_ARM64__"
      sha256 "__TODO_SHA256_LINUX_ARM64__"
    else
      # URL pattern: https://github.com/<org>/hugr-lightr/releases/download/v<ver>/lightr-<ver>-linux-x86_64.tar.gz
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
