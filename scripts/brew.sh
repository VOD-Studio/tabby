#!/usr/bin/env bash
# Updates packaging/homebrew/tabby-mihomo.rb for the version in Cargo.toml.
# The GitHub release must already have all the binary archives the formula uses.
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

work=$(mktemp -d "$(dirname "$formula")/.brew.XXXXXXXX")
trap 'rm -rf "$work"' EXIT

archive_sum() {
    local archive=$homepage/releases/download/$tag/tabby-$version-$1.tar.gz
    local try
    echo "brew: fetching $archive" >&2
    # GitHub's downloads time out (504) now and then: five tries, 10 s apart.
    for try in 1 2 3 4 5; do
        if curl --fail --location --silent --show-error -o "$work/archive.tar.gz" "$archive"; then
            sha256 "$work/archive.tar.gz" | cut -d' ' -f1
            return
        fi
        [ "$try" = 5 ] && die "cannot fetch $archive (is $tag published?)"
        sleep 10
    done
}

# Read the targets from the formula so each URL stays paired with its checksum.
release_line='^  release = "([^"]+)"$'
url_line='^[[:space:]]+url "#\{release_url\}-([a-z0-9_-]+)\.tar\.gz"$'
checksum_line='^([[:space:]]+sha256 )"([a-f0-9]{64})"$'
old_version=
target=
sum=
while IFS= read -r line; do
    if [[ $line =~ $release_line ]]; then
        old_version=${BASH_REMATCH[1]}
        line="  release = \"$version\""
    elif [[ $line =~ $url_line ]]; then
        [ -z "$target" ] || die "missing sha256 for $target in $formula"
        target=${BASH_REMATCH[1]}
    elif [[ $line =~ $checksum_line ]]; then
        [ -n "$old_version" ] && [ -n "$target" ] || die "unexpected sha256 in $formula"
        old_sum=${BASH_REMATCH[2]}
        sum=$(archive_sum "$target")
        if [ "$old_version" = "$version" ] && [ "$old_sum" != "$sum" ]; then
            die "the $target archive of $tag changed: $old_sum is now $sum"
        fi
        line="${BASH_REMATCH[1]}\"$sum\""
        target=
    fi
    printf '%s\n' "$line"
done <"$formula" >"$work/formula.rb"

[ -z "$target" ] || die "missing sha256 for $target in $formula"
[ -n "$sum" ] || die "no release assets in $formula"
if [ "$old_version" != "$version" ]; then
    mv "$work/formula.rb" "$formula"
fi
echo "brew: $formula uses the binary archives of $tag"
