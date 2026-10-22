#!/usr/bin/env bash
set -euo pipefail
python3 -m unittest discover -s ci -p 'test_*.py'
for script in ci/*.sh; do bash -n "$script"; done
sh -n packaging/nix/install.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --fail --location --retry 5 --retry-all-errors --connect-timeout 20 --max-time 300 \
    https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz \
    -o "$work/actionlint.tar.gz"
echo "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8  $work/actionlint.tar.gz" | sha256sum --check
tar -xf "$work/actionlint.tar.gz" -C "$work" actionlint
"$work/actionlint" -color
