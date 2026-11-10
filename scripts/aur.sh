#!/usr/bin/env bash
# Points packaging/arch/PKGBUILD at the version in Cargo.toml and writes its
# .SRCINFO: _tag and pkgver follow the version (a pre-release such as
# 1.0.0-rc.1 becomes pkgver 1.0.0rc.1, which pacman orders before 1.0.0),
# pkgrel goes back to 1 for a new version, and the sum is that of the source
# archive GitHub serves for the tag, so the tag must be pushed first.
# A packaging-only change keeps the version: raise pkgrel by hand, then run
# this for .SRCINFO.
#
# To publish, copy PKGBUILD and .SRCINFO into a clone of
# ssh://aur@aur.archlinux.org/tabby-mihomo.git, commit and push there.
#
# usage: scripts/aur.sh
# needs: cargo, curl, sha256sum, makepkg (Arch Linux)
set -euo pipefail

cd "$(dirname "$0")/.."

die() {
    echo "aur: $*" >&2
    exit 1
}

for tool in cargo curl sha256sum makepkg; do
    command -v "$tool" >/dev/null || die "$tool is needed"
done

pkgbuild=packaging/arch/PKGBUILD
version=$(cargo pkgid | sed 's/.*[#@]//')
tag=v$version
pkgver=${version//-/}
url=$(sed -n "s/^url='\(.*\)'\$/\1/p" "$pkgbuild")
[ -n "$url" ] || die "no url= in $pkgbuild"
old_tag=$(sed -n 's/^_tag=//p' "$pkgbuild")
old_sum=$(sed -n "s/^sha256sums=('\(.*\)')\$/\1/p" "$pkgbuild")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
archive=$url/archive/refs/tags/$tag.tar.gz
echo "aur: fetching $archive"
# GitHub's downloads time out (504) now and then: five tries, 10 s apart.
for try in 1 2 3 4 5; do
    curl --fail --location --silent --show-error -o "$work/source.tar.gz" "$archive" && break
    [ "$try" = 5 ] && die "cannot fetch $archive (is $tag pushed?)"
    sleep 10
done
sum=$(sha256sum "$work/source.tar.gz" | cut -d' ' -f1)

if [ "$old_tag" = "$tag" ]; then
    # The same release must not change under the package.
    [ "$old_sum" = "$sum" ] || die "the archive of $tag changed: $old_sum is now $sum"
else
    sed -i -e "s/^_tag=.*/_tag=$tag/" -e "s/^pkgver=.*/pkgver=$pkgver/" \
        -e 's/^pkgrel=.*/pkgrel=1/' -e "s/^sha256sums=.*/sha256sums=('$sum')/" "$pkgbuild"
fi

(cd packaging/arch && makepkg --printsrcinfo >.SRCINFO)
echo "aur: $pkgbuild is $pkgver-$(sed -n 's/^pkgrel=//p' "$pkgbuild"); build it with: cd packaging/arch && makepkg"
