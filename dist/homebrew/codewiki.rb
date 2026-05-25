# T-511 — Homebrew formula for codewiki.
#
# To use (local tap):
#   brew install --formula ./dist/homebrew/codewiki.rb
#
# To submit to homebrew-core (after release):
#   1. Run dist/homebrew/update-sha256.sh to replace the REPLACE_AT_RELEASE_TIME placeholders.
#   2. Test locally: brew install --build-from-source ./codewiki.rb
#   3. Open a PR to homebrew/homebrew-core.
#
# Release workflow (release.yml) sed-patches the SHA256 placeholders before
# publishing the GitHub Release.

class Codewiki < Formula
  desc "Tree-sitter-powered code knowledge graph server for AI coding agents"
  homepage "https://github.com/0xsyncroot/codewiki"
  license "MIT"
  version "0.1.1"

  # ── macOS Apple Silicon ──────────────────────────────────────────────────────
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/0xsyncroot/codewiki/releases/download/v#{version}/codewiki-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_AT_RELEASE_TIME_MACOS_ARM64"
    else
      url "https://github.com/0xsyncroot/codewiki/releases/download/v#{version}/codewiki-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_AT_RELEASE_TIME_MACOS_X86_64"
    end
  end

  # ── Linux x86_64 ─────────────────────────────────────────────────────────────
  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/0xsyncroot/codewiki/releases/download/v#{version}/codewiki-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_AT_RELEASE_TIME_LINUX_X86_64"
    else
      url "https://github.com/0xsyncroot/codewiki/releases/download/v#{version}/codewiki-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_AT_RELEASE_TIME_LINUX_ARM64"
    end
  end

  # ── No build dependencies — pre-built binary ─────────────────────────────────

  def install
    bin.install "codewiki"
  end

  # ── Shell completions (generated at install time) ─────────────────────────────
  def post_install
    # Generate shell completions if the binary supports it.
    # codewiki --generate-completion bash > $(brew --prefix)/etc/bash_completion.d/codewiki
    # Suppressed in v1; add in v1.1 once clap_complete is wired.
  end

  test do
    # Verify the binary runs and reports the correct version.
    assert_match version.to_s, shell_output("#{bin}/codewiki --version")
  end
end
