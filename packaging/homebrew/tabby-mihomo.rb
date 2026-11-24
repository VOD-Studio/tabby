# The Homebrew formula of tabby, for the tap VOD-Studio/homebrew-tabby
# (`brew install vod-studio/tabby/tabby-mihomo`), using the GitHub release
# binaries. Run `make brew` after the release is published to update the
# version and checksums. The name distinguishes it from the Tabby terminal.
class TabbyMihomo < Formula
  desc "Terminal UI for the mihomo (Clash.Meta) kernel"
  homepage "https://github.com/VOD-Studio/tabby"
  license "MIT"

  release = "0.4.0"
  version release
  release_url = "#{homepage}/releases/download/v#{release}/tabby-#{release}"

  on_macos do
    depends_on macos: :monterey

    on_arm do
      url "#{release_url}-aarch64-apple-darwin.tar.gz"
      sha256 "d1d530d8095261f7f75981811cde590d7b54b848d29564233acd3d8187c0556e"
    end
    on_intel do
      url "#{release_url}-x86_64-apple-darwin.tar.gz"
      sha256 "f524403f86cd8d5137286b2a81530836154c6df44bb0c1d4f57a410aa1001530"
    end
  end

  on_linux do
    on_arm do
      url "#{release_url}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c636e149832a01bdcb08bd8314019edc6936840e447b276d3637ef5092634765"
    end
    on_intel do
      url "#{release_url}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "be0dfd441b140e67d2b73343b49d315475916b69168009db0fdd686e4ad694f3"
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
