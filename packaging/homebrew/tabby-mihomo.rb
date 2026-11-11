# The Homebrew formula of tabby, for the tap VOD-Studio/homebrew-tabby
# (`brew install vod-studio/tabby/tabby-mihomo`), built from the source
# archive of a release tag: what Homebrew builds has no quarantine to lift.
# `make brew` (scripts/brew.sh) points url and sha256 at the version in
# Cargo.toml once its tag is on GitHub. Like the AUR package it is not called
# tabby, the name of the Tabby terminal (a cask, which installs no `tabby`
# command).
class TabbyMihomo < Formula
  desc "Terminal UI for the mihomo (Clash.Meta) kernel"
  homepage "https://github.com/VOD-Studio/tabby"
  url "https://github.com/VOD-Studio/tabby/archive/refs/tags/v0.2.2.tar.gz"
  sha256 "e6571969e169e2c0ff1fdf081db1bd47aa5c71ec5b21dd904c7a62d5855568d4"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      tabby downloads the mihomo kernel itself on first start, or runs the one
      on your PATH (`brew install mihomo`), which Homebrew then keeps up to date.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/tabby -V")
  end
end
