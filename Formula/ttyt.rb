# Homebrew formula for ttyt.
#
# This is the canonical copy, kept in sync with the same file published
# at github.com/zflow-byte/homebrew-ttyt (the real tap end users install
# from: `brew tap zflow-byte/ttyt && brew install ttyt`). Verified against
# v0.1.3's real release tarball (build + `brew test` both passed): `sha256`
# below is the real hash, not a placeholder.
#
# To exercise this exact file locally instead of the published tap
# (current Homebrew rejects a bare formula path outside any tap):
#   mkdir -p "$(brew --repository)/Library/Taps/local/homebrew-test/Formula"
#   cp Formula/ttyt.rb "$(brew --repository)/Library/Taps/local/homebrew-test/Formula/"
#   brew install --build-from-source local/test/ttyt
#   brew test local/test/ttyt
#   brew uninstall ttyt && rm -rf "$(brew --repository)/Library/Taps/local/homebrew-test"
class Ttyt < Formula
  desc "TUI serial/network console for Cisco, Dell OS10, Aruba CX, Comware, JunOS"
  homepage "https://github.com/zflow-byte/ttyt"
  url "https://github.com/zflow-byte/ttyt/archive/refs/tags/v0.1.3.tar.gz"
  sha256 "2b44f738205ca8af6dad6dfa660348b3e489df73705811de04c52cd417ed30fa"
  license "MIT"
  head "https://github.com/zflow-byte/ttyt.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/ttyt-cli")
  end

  test do
    assert_match "ttyt", shell_output("#{bin}/ttyt --help")
  end
end
