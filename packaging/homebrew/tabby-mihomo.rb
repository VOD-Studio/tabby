# The Homebrew formula of tabby, for the tap VOD-Studio/homebrew-tabby
# (`brew install vod-studio/tabby/tabby-mihomo`), using the GitHub release
# binaries. Run `make brew` after the release is published to update the
# version and checksums. The name distinguishes it from the Tabby terminal.
class TabbyMihomo < Formula
  desc "Terminal UI for the mihomo (Clash.Meta) kernel"
  homepage "https://github.com/VOD-Studio/tabby"
  license "MIT"

  release = "0.3.1"
  version release
  release_url = "#{homepage}/releases/download/v#{release}/tabby-#{release}"

  on_macos do
    depends_on macos: :monterey

    on_arm do
      url "#{release_url}-aarch64-apple-darwin.tar.gz"
      sha256 "2cd6b303b40871f5b4c6026862a5ca494bb13bb640dc4a52fc62d9e7fd59b1bf"
    end
    on_intel do
      url "#{release_url}-x86_64-apple-darwin.tar.gz"
      sha256 "d346a8a0a108611e80f4604ae4fb443578f9f2855ce253b6748f5e349880553c"
    end
  end

  on_linux do
    on_arm do
      url "#{release_url}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "66f300952e4e887f890741a78fa9a83389b405b0c9be28a0a40089fb8f665213"
    end
    on_intel do
      url "#{release_url}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "774641f383073943c83ab2287720d9372c37e72751445615bb755b968c546d38"
    end
  end

  def install
    bin.install "bin/tabby"
    prefix.install "share"
  end

  def caveats
    <<~EOS
      tabby downloads the mihomo kernel itself on first start, or runs the one
      on your PATH (`brew install mihomo`), which Homebrew then keeps up to date.
    EOS
  end

  test do
    assert_equal "tabby #{version}", shell_output("#{bin}/tabby -V").strip
    assert_predicate share/"licenses/tabby/THIRD-PARTY-LICENSES", :file?
  end
end
