#!/usr/bin/env bash
# Builds tabby's release archives into dist/:
#
#   tabby-<version>-<target>.tar.gz              tabby alone
#   tabby-<version>-<target>-with-mihomo.tar.gz  tabby and the mihomo pinned in
#                                                packaging/mihomo.env
#   mihomo-<tag>-source.tar.gz                   that mihomo's source (GPL-3.0)
#   SHA256SUMS
#
# Both archives hold one directory laid out like a prefix (bin/tabby,
# lib/tabby/mihomo, share/...), to be unpacked into ~/.local or /usr/local
# with --strip-components=1: tabby looks for the kernel it ships at
# <prefix>/lib/tabby/mihomo, next to <prefix>/bin/tabby.
#
# usage: scripts/dist.sh [<rust target triple>]   (default: the host's)
# needs: cargo, cargo-about (cargo install --locked --features cli cargo-about),
#        curl, sha256sum, gzip, GNU tar, git
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=../packaging/mihomo.env
. packaging/mihomo.env

target=${1:-$(rustc -vV | sed -n 's/^host: //p')}
case $target in
x86_64-*-linux-*)
    asset=mihomo-linux-amd64-v1-$MIHOMO_TAG.gz
    sum=$MIHOMO_SHA256_AMD64
    ;;
aarch64-*-linux-*)
    asset=mihomo-linux-arm64-$MIHOMO_TAG.gz
    sum=$MIHOMO_SHA256_ARM64
    ;;
*)
    echo "dist: no mihomo build for $target" >&2
    exit 1
    ;;
esac

for tool in cargo cargo-about curl sha256sum gzip tar git; do
    command -v "$tool" >/dev/null || {
        echo "dist: $tool is needed" >&2
        exit 1
    }
done

version=$(cargo pkgid | sed 's/.*[#@]//')
name=tabby-$version-$target
# A release is the commit tagged v<version>, built from a clean tree: a
# released version must never be packed twice with different contents.
# Anything else is a snapshot, packed with a warning.
tags=$(git tag --points-at HEAD --list 'v*')
if [ -n "$tags" ]; then
    grep -qxF "v$version" <<<"$tags" || {
        echo "dist: HEAD is tagged ${tags//$'\n'/ }, but Cargo.toml says $version" >&2
        exit 1
    }
    [ -z "$(git status --porcelain)" ] || {
        echo "dist: v$version is a release, but the working tree has changes" >&2
        exit 1
    }
else
    echo "dist: HEAD is not tagged v$version: packing a snapshot, not a release" >&2
fi
source_name=mihomo-$MIHOMO_TAG-source.tar.gz
# The time of the last commit for every file: the same commit packs the same
# archives, and the shipped kernel of two releases differs in its time.
epoch=$(git log -1 --format=%ct)
cache=target/dist-cache
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p dist "$cache"

# Downloads $1 into $cache/$2 unless it is there already.
fetch() {
    [[ -s $cache/$2 ]] && return
    curl --fail --location --silent --show-error -o "$cache/$2.part" "$1"
    mv "$cache/$2.part" "$cache/$2"
}

echo "dist: building $target"
cargo build --release --locked --target "$target"

echo "dist: fetching mihomo $MIHOMO_TAG"
fetch "https://github.com/MetaCubeX/mihomo/releases/download/$MIHOMO_TAG/$asset" "$asset"
if ! echo "$sum  $cache/$asset" | sha256sum --check --quiet; then
    rm -f "$cache/$asset"
    echo "dist: $asset does not match the sha256 in packaging/mihomo.env" >&2
    exit 1
fi
fetch "https://github.com/MetaCubeX/mihomo/archive/refs/tags/$MIHOMO_TAG.tar.gz" "$source_name"

# tabby alone.
plain=$work/plain/$name
install -Dm755 "target/$target/release/tabby" "$plain/bin/tabby"
install -Dm644 README.md "$plain/share/doc/tabby/README.md"
install -Dm644 LICENSE "$plain/share/licenses/tabby/LICENSE"
cargo about generate --locked --fail --target "$target" \
    --config packaging/about.toml \
    --output-file "$plain/share/licenses/tabby/THIRD-PARTY-LICENSES" \
    packaging/about.hbs
chmod 644 "$plain/share/licenses/tabby/THIRD-PARTY-LICENSES"

# And with the kernel.
full=$work/full/$name
mkdir -p "$(dirname "$full")"
cp -a "$plain" "$full"
install -d "$full/lib/tabby" "$full/share/licenses/tabby/mihomo"
gzip -dc "$cache/$asset" >"$full/lib/tabby/mihomo"
chmod 755 "$full/lib/tabby/mihomo"
tar -xzOf "$cache/$source_name" "mihomo-${MIHOMO_TAG#v}/LICENSE" >"$full/share/licenses/tabby/mihomo/LICENSE"
cat >"$full/share/licenses/tabby/mihomo/SOURCE" <<EOF
lib/tabby/mihomo is mihomo $MIHOMO_TAG, unmodified, from the release asset
  $asset
of https://github.com/MetaCubeX/mihomo. It is a separate program that tabby
runs, licensed under the GNU General Public License version 3 (LICENSE next
to this file).

Its source code is published next to this archive as
  $source_name
and at
  https://github.com/MetaCubeX/mihomo/archive/refs/tags/$MIHOMO_TAG.tar.gz
EOF
chmod 644 "$full/share/licenses/tabby/mihomo/"*

# Packs directory $name of $1 into $2. `gzip -n` keeps the time out of the
# gzip header.
pack() {
    tar --create --directory "$1" \
        --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
        "$name" | gzip -n >"$2"
}
pack "$work/plain" "dist/$name.tar.gz"
pack "$work/full" "dist/$name-with-mihomo.tar.gz"
cp "$cache/$source_name" "dist/$source_name"

(cd dist && sha256sum -- *.tar.gz >SHA256SUMS)
echo "dist: done"
ls -l dist
