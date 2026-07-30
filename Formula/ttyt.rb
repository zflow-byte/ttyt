# Homebrew formula for ttyt.
#
# Not yet usable via `url`/`sha256` as-is: the repo now has a real GitHub
# remote (https://github.com/zflow-byte/ttyt) but no tagged release yet.
# Once a `v0.1.0` tag exists:
#   1. Replace `sha256` below with the real tarball hash:
#        curl -L https://github.com/zflow-byte/ttyt/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
#   2. To test locally before publishing a tap:
#        brew install --build-from-source ./Formula/ttyt.rb
class Ttyt < Formula
  desc "TUI serial/network console for Cisco, Dell OS10, Aruba CX, Comware, JunOS"
  homepage "https://github.com/zflow-byte/ttyt"
  url "https://github.com/zflow-byte/ttyt/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_REAL_SHA256_AFTER_TAGGING_A_RELEASE"
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
