#!/usr/bin/env bash
# Cuts release <version> on a clean tree: runs the gate (make check), sets the
# version in Cargo.toml and Cargo.lock, adds its section to CHANGELOG.md
# (git-cliff, cliff.toml), commits `chore(release): v<version>` and tags that
# commit `v<version>` (annotated). Nothing is pushed: pushing the tag makes
# .github/workflows/release.yml build the archives and publish the release.
#
# usage: scripts/release.sh <version>      e.g. 0.2.0 or 1.0.0-rc.1
#        scripts/release.sh                prints the version git-cliff suggests
# needs: git-cliff (cargo install --locked git-cliff)
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=../packaging/mihomo.env
. packaging/mihomo.env

GIT_CLIFF=${GIT_CLIFF:-git-cliff}

die() {
    echo "release: $*" >&2
    exit 1
}

command -v "$GIT_CLIFF" >/dev/null || die "git-cliff is needed (cargo install --locked git-cliff)"
if [ $# -ne 1 ] || [ -z "$1" ]; then
    echo "usage: $0 <version>" >&2
    echo "git-cliff suggests: $("$GIT_CLIFF" --bumped-version 2>/dev/null)" >&2
    exit 2
fi

version=${1#v}
tag=v$version
semver='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
[[ $version =~ $semver ]] || die "$version is not X.Y.Z or X.Y.Z-<pre-release>"
[ -z "$(git status --porcelain)" ] || die "the working tree has changes"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    die "$tag exists already"
fi

make check

if [ "$(cargo pkgid | sed 's/.*[#@]//')" != "$version" ]; then
    # The first `version = ` at the start of a line is [package]'s: the
    # dependencies' versions sit in inline tables or after their names.
    sed -i "0,/^version = \".*\"\$/s//version = \"$version\"/" Cargo.toml
    cargo update --workspace --offline --quiet
    [ "$(cargo pkgid | sed 's/.*[#@]//')" = "$version" ] || die "could not set the version in Cargo.toml"
fi

# Only the new section is generated: earlier ones may have been edited by hand.
if [ -f CHANGELOG.md ]; then
    TABBY_MIHOMO_TAG=$MIHOMO_TAG "$GIT_CLIFF" --unreleased --tag "$tag" --prepend CHANGELOG.md
else
    TABBY_MIHOMO_TAG=$MIHOMO_TAG "$GIT_CLIFF" --tag "$tag" --output CHANGELOG.md
fi

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit --quiet -m "chore(release): $tag"
git tag -a "$tag" -m "tabby $version" -m "The -with-mihomo archives ship mihomo $MIHOMO_TAG."

branch=$(git symbolic-ref --short HEAD)
echo "release: $tag is committed and tagged; publish with: git push --atomic origin $branch $tag"
