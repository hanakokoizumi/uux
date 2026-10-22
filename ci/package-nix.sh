#!/usr/bin/env bash
set -euo pipefail
arch=${1:?architecture required}
bundle=${2:?bundle directory required}
destination=${3:-dist/packages}
case "$arch" in
    amd64) system=x86_64-linux ;;
    i386) system=i686-linux ;;
    aarch64) system=aarch64-linux ;;
    *) exit 1 ;;
esac
version=$(python3 ci/release-files.py)
output=$(nix-build packaging/nix/release.nix --no-out-link \
    --argstr system "$system" --arg bundle "$bundle")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$destination"
# nix-store exports paths in dependency order, including the runtime closure.
mapfile -t closure < <(nix-store --query --requisites "$output")
nix-store --export "${closure[@]}" > "$work/closure.nar"
printf '%s\n' "$output" > "$work/store-path"
install -m755 packaging/nix/install.sh "$work/install.sh"
tar -C "$work" -cJf "$destination/uux-$version-linux-$arch.nix.tar.xz" \
    closure.nar store-path install.sh
