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
#        curl, sha256sum (or shasum on macOS), gzip, GNU tar (gtar on macOS),
#        git; gh too with GH_TOKEN set, which then fetches mihomo's release
#        asset as a signed-in user. Linux and macOS targets are built on a
#        machine of their own system.
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
x86_64-apple-darwin)
    asset=mihomo-darwin-amd64-v1-$MIHOMO_TAG.gz
    sum=$MIHOMO_SHA256_DARWIN_AMD64
    ;;
aarch64-apple-darwin)
    asset=mihomo-darwin-arm64-$MIHOMO_TAG.gz
    sum=$MIHOMO_SHA256_DARWIN_ARM64
    ;;
*)
    echo "dist: no mihomo build for $target" >&2
    exit 1
    ;;
esac

for tool in cargo cargo-about curl gzip git; do
    command -v "$tool" >/dev/null || {
        echo "dist: $tool is needed" >&2
        exit 1
    }
done
# macOS has shasum, and its tar is bsdtar: GNU tar is gtar there.
if command -v sha256sum >/dev/null; then
    sha256() { sha256sum "$@"; }
elif command -v shasum >/dev/null; then
    sha256() { shasum -a 256 "$@"; }
else
    echo "dist: sha256sum or shasum is needed" >&2
    exit 1
fi
tar=$(command -v gtar || command -v tar || true)
"${tar:-tar}" --version 2>/dev/null | grep -q 'GNU tar' || {
    echo "dist: GNU tar is needed (gtar on macOS: brew install gnu-tar)" >&2
    exit 1
}
case $target in
# The oldest macOS the build runs on; mihomo's own needs 12 too.
*-apple-darwin) export MACOSX_DEPLOYMENT_TARGET=12.0 ;;
esac

# Copies file $2 to $3 with mode $1, making its directory (BSD install has
# no -D).
put() {
    mkdir -p "$(dirname "$3")"
    install -m "$1" "$2" "$3"
}

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

# Runs "$@" up to five times, 10 s apart: GitHub's downloads time out (504)
# now and then, mostly for anonymous users when it is busy.
retry() {
    local try
    for try in 1 2 3 4 5; do
        "$@" && return
        [ "$try" = 5 ] || sleep 10
    done
    return 1
}

# Downloads $2 into $cache/$1 unless it is there already.
fetch() {
    [[ -s $cache/$1 ]] && return
    retry curl --fail --location --silent --show-error -o "$cache/$1.part" "$2"
    mv "$cache/$1.part" "$cache/$1"
}

# Downloads the asset $1 of mihomo's release $MIHOMO_TAG into $cache/$1.
# With $GH_TOKEN set (the release workflow) it comes through gh, as a
# signed-in user.
fetch_asset() {
    [[ -s $cache/$1 ]] && return
    if [ -z "${GH_TOKEN:-}" ]; then
        fetch "$1" "https://github.com/MetaCubeX/mihomo/releases/download/$MIHOMO_TAG/$1"
        return
    fi
    command -v gh >/dev/null || {
        echo "dist: GH_TOKEN is set, so gh is needed" >&2
        exit 1
    }
    retry gh release download "$MIHOMO_TAG" --repo MetaCubeX/mihomo \
        --pattern "$1" --output "$cache/$1.part" --clobber
    mv "$cache/$1.part" "$cache/$1"
}

# Before the build: a download that fails should not cost a build.
echo "dist: fetching mihomo $MIHOMO_TAG"
fetch_asset "$asset"
if [ "$(sha256 "$cache/$asset" | cut -d' ' -f1)" != "$sum" ]; then
    rm -f "$cache/$asset"
    echo "dist: $asset does not match the sha256 in packaging/mihomo.env" >&2
    exit 1
fi
fetch "$source_name" "https://github.com/MetaCubeX/mihomo/archive/refs/tags/$MIHOMO_TAG.tar.gz"
# Nothing after this downloads: the build scripts of the dependencies and
# cargo-about do not get the token.
unset GH_TOKEN

echo "dist: building $target"
cargo build --release --locked --target "$target"

# tabby alone.
plain=$work/plain/$name
put 755 "target/$target/release/tabby" "$plain/bin/tabby"
put 644 README.md "$plain/share/doc/tabby/README.md"
put 644 LICENSE "$plain/share/licenses/tabby/LICENSE"
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
"$tar" -xzOf "$cache/$source_name" "mihomo-${MIHOMO_TAG#v}/LICENSE" >"$full/share/licenses/tabby/mihomo/LICENSE"
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
    "$tar" --create --directory "$1" \
        --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
        "$name" | gzip -n >"$2"
}
pack "$work/plain" "dist/$name.tar.gz"
pack "$work/full" "dist/$name-with-mihomo.tar.gz"
cp "$cache/$source_name" "dist/$source_name"

(cd dist && sha256 -- *.tar.gz >SHA256SUMS)
echo "dist: done"
ls -l dist
