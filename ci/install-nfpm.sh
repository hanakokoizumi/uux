#!/usr/bin/env bash
set -euo pipefail
destination=${1:?installation directory required}
case $(uname -m) in
    x86_64) arch=x86_64; checksum=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750 ;;
    aarch64) arch=arm64; checksum=a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4 ;;
    *) echo 'Run the packager on an amd64 or aarch64 host.' >&2; exit 1 ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --fail --location --retry 5 --retry-all-errors --connect-timeout 20 --max-time 300 \
    "https://github.com/goreleaser/nfpm/releases/download/v2.48.0/nfpm_2.48.0_Linux_$arch.tar.gz" -o "$work/nfpm.tar.gz"
echo "$checksum  $work/nfpm.tar.gz" | sha256sum --check
mkdir -p "$destination"
tar -xf "$work/nfpm.tar.gz" -C "$destination" nfpm
