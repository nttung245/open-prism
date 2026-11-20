#!/usr/bin/env bash
# Builds the downloadable package of one release for one architecture:
#   target/dist/open-prism-<version>-linux-<arch>.tar.gz
# from what `just build` made (target/release/open-prism-api, web/.next/standalone) and the
# official Node.js build, checked against the SHA-256 written below.
#
# Usage: scripts/package-release.sh <version> <x64|arm64> [output dir]
#
# To update Node: change NODE_VERSION and both checksums, from
# https://nodejs.org/dist/v<version>/SHASUMS256.txt (the linux-x64 and linux-arm64 .tar.xz lines).
set -euo pipefail

NODE_VERSION=22.23.5
node_sha256() {
  case "$1" in
    x64) echo cdeb8670a0c81f51b4dc4ac61919f9b1ad917b304bf8221eb939570663d240f8 ;;
    arm64) echo 2babb9eb5264860d540849f4b064a6fbdebcdc323657d9d382ce23d313fd5e09 ;;
  esac
}

die() {
  echo "package-release: $*" >&2
  exit 1
}

[ $# -ge 2 ] && [ $# -le 3 ] || die "usage: $0 <version> <x64|arm64> [output dir]"
version=$1
arch=$2
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]] || die "not a version: $version"
case "$arch" in x64 | arm64) ;; *) die "architecture must be x64 or arm64, not $arch" ;; esac

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
out=${3:-$root/target/dist}
api=$root/target/release/open-prism-api
standalone=$root/web/.next/standalone
umask 022

# What `just build` makes, and from this version of the code.
[ -x "$api" ] || die "$api is missing: run \`just build\` first"
[ -f "$standalone/server.js" ] && [ -d "$standalone/.next/static" ] && [ -d "$standalone/public" ] ||
  die "$standalone is missing or incomplete: run \`just build\` first"
api_version=$("$api" --version) || die "cannot run $api"
[ "$api_version" = "open-prism-api $version" ] ||
  die "$api is \"$api_version\", not version $version: change the version or rebuild"

# Node.js: downloaded once into the cache, and checked every time it is used.
node_name=node-v$NODE_VERSION-linux-$arch
node_tar=$node_name.tar.xz
cache=${NODE_CACHE:-$root/target/node-dist}
expected=$(node_sha256 "$arch")
mkdir -p "$cache"
sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
if [ ! -f "$cache/$node_tar" ] || [ "$(sha256_of "$cache/$node_tar")" != "$expected" ]; then
  echo "Downloading $node_tar"
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$cache/$node_tar.part" \
    "https://nodejs.org/dist/v$NODE_VERSION/$node_tar"
  actual=$(sha256_of "$cache/$node_tar.part")
  if [ "$actual" != "$expected" ]; then
    rm -f "$cache/$node_tar.part"
    die "$node_tar has SHA-256 $actual, expected $expected: not packaging it"
  fi
  mv "$cache/$node_tar.part" "$cache/$node_tar"
fi

# The package folder, built next to the archive and replaced on every run.
name=open-prism-$version-linux-$arch
mkdir -p "$out"
out=$(cd -- "$out" && pwd -P)
work=$out/$name
rm -rf "$work" "$out/$name.tar.gz"
mkdir -p "$work/bin" "$work/node/bin" "$work/share" "$work/web"

install -m 755 "$root/packaging/open-prism" "$work/open-prism"
install -m 755 "$api" "$work/bin/open-prism-api"
cp -a "$standalone/." "$work/web/"
# Only node itself and its license: npm and the headers are not needed to run the web app.
tar -xJf "$cache/$node_tar" -C "$work/node" --strip-components=1 \
  "$node_name/bin/node" "$node_name/LICENSE"
install -m 644 "$root/packaging/open-prism.svg" "$work/share/open-prism.svg"
install -m 644 "$root/LICENSE" "$root/NOTICE" "$work/"
sed -e "s/{version}/$version/g" -e "s/{arch}/$arch/g" "$root/packaging/README.txt" > "$work/README.txt"
# Readable by all, writable by the owner only, whatever the build machine's umask was.
chmod -R u=rwX,go=rX "$work"

case "$(uname -m)" in x86_64) host=x64 ;; aarch64 | arm64) host=arm64 ;; *) host= ;; esac
if [ "$arch" = "$host" ]; then
  node_version=$("$work/node/bin/node" --version) || die "the packaged Node.js does not run"
  [ "$node_version" = "v$NODE_VERSION" ] || die "the packaged Node.js is $node_version, not v$NODE_VERSION"
fi

# Owned by root in the archive, not by whoever built it; executables keep their mode.
tar -C "$out" --sort=name --owner=0 --group=0 --numeric-owner -cf - "$name" | gzip -n -9 > "$out/$name.tar.gz.part"
mv "$out/$name.tar.gz.part" "$out/$name.tar.gz"
rm -rf "$work"
echo "$out/$name.tar.gz ($(du -h "$out/$name.tar.gz" | cut -f1))"
