# Homebrew formula for ttyt.
#
# Not yet usable as-is: `url`/`sha256`/`homepage` below are placeholders
# because this project has no GitHub remote yet (local git repo only, see
# changes.log). Once the repo is pushed and a `v0.1.0` tag exists:
#   1. Replace <org>/ttyt with the real GitHub path in `homepage`,
#      `url`, and `head`.
#   2. Replace `sha256` with the real tarball hash:
#        curl -L <url> | shasum -a 256
#   3. To test locally before publishing a tap:
#        brew install --build-from-source ./Formula/ttyt.rb
class Ttyt < Formula
  desc "TUI serial/network console for Cisco, Dell OS10, Aruba CX, Comware, JunOS"
  homepage "https://github.com/<org>/ttyt"
  url "https://github.com/<org>/ttyt/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_REAL_SHA256_AFTER_TAGGING_A_RELEASE"
  license "MIT"
  head "https://github.com/<org>/ttyt.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/ttyt-cli")
  end

  test do
    assert_match "ttyt", shell_output("#{bin}/ttyt --help")
  end
end
