# Homebrew formula for ttyt.
#
# Verified against v0.1.0's real release tarball (build + `brew test` both
# passed): `sha256` above is the real hash, not a placeholder.
#
# Current Homebrew (6.x) rejects `brew install --build-from-source
# ./Formula/ttyt.rb` on a bare path -- it now requires every formula to
# live in a tap. Without publishing a separate `homebrew-ttyt` tap repo,
# the way to exercise this file locally is a throwaway local tap:
#   mkdir -p "$(brew --repository)/Library/Taps/local/homebrew-test/Formula"
#   cp Formula/ttyt.rb "$(brew --repository)/Library/Taps/local/homebrew-test/Formula/"
#   brew install --build-from-source local/test/ttyt
#   brew test local/test/ttyt
#   brew uninstall ttyt && rm -rf "$(brew --repository)/Library/Taps/local/homebrew-test"
class Ttyt < Formula
  desc "TUI serial/network console for Cisco, Dell OS10, Aruba CX, Comware, JunOS"
  homepage "https://github.com/zflow-byte/ttyt"
  url "https://github.com/zflow-byte/ttyt/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "4348a5ff108a4a2d633178b57c955d494374b75337d0b433bf61fc73d5f421aa"
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
