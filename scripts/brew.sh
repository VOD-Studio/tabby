#!/usr/bin/env bash
# Points packaging/homebrew/tabby-mihomo.rb at the version in Cargo.toml: url
# is the source archive GitHub serves for its tag, so the tag must be pushed
# first, and sha256 is that archive's.
#
# To publish, copy the formula into Formula/ of a clone of
# https://github.com/VOD-Studio/homebrew-tabby, commit and push there.
#
# usage: scripts/brew.sh
# needs: cargo, curl, sha256sum or shasum
set -euo pipefail

cd "$(dirname "$0")/.."

die() {
    echo "brew: $*" >&2
    exit 1
}

for tool in cargo curl; do
    command -v "$tool" >/dev/null || die "$tool is needed"
done
if command -v sha256sum >/dev/null; then
    sha256() { sha256sum "$@"; }
elif command -v shasum >/dev/null; then
    sha256() { shasum -a 256 "$@"; }
else
    die "sha256sum or shasum is needed"
fi

formula=packaging/homebrew/tabby-mihomo.rb
version=$(cargo pkgid | sed 's/.*[#@]//')
tag=v$version
homepage=$(sed -n 's/^  homepage "\(.*\)"$/\1/p' "$formula")
[ -n "$homepage" ] || die "no homepage in $formula"
old_url=$(sed -n 's/^  url "\(.*\)"$/\1/p' "$formula")
old_sum=$(sed -n 's/^  sha256 "\(.*\)"$/\1/p' "$formula")
archive=$homepage/archive/refs/tags/$tag.tar.gz

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "brew: fetching $archive"
# GitHub's downloads time out (504) now and then: five tries, 10 s apart.
for try in 1 2 3 4 5; do
    curl --fail --location --silent --show-error -o "$work/source.tar.gz" "$archive" && break
    [ "$try" = 5 ] && die "cannot fetch $archive (is $tag pushed?)"
    sleep 10
done
sum=$(sha256 "$work/source.tar.gz" | cut -d' ' -f1)

if [ "$old_url" = "$archive" ]; then
    # The same release must not change under the formula.
    [ "$old_sum" = "$sum" ] || die "the archive of $tag changed: $old_sum is now $sum"
else
    # Written whole, not with sed -i, whose flags differ on macOS.
    sed -e "s|^  url \".*\"\$|  url \"$archive\"|" \
        -e "s|^  sha256 \".*\"\$|  sha256 \"$sum\"|" "$formula" >"$work/formula.rb"
    cat "$work/formula.rb" >"$formula"
fi
echo "brew: $formula is $tag; try it with: brew install --build-from-source $formula"
