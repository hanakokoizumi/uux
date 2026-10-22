#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")"
package=$(cat store-path)
if ! nix-store --check-validity "$package" > /dev/null 2>&1; then
    if ! nix-store --import < closure.nar > /dev/null; then
        printf '%s\n' 'Import failed. On multi-user Nix, ask an administrator to run:' \
            '  nix-store --import < closure.nar' \
            'Then run this installer again as your normal user.' >&2
        exit 1
    fi
fi
nix --extra-experimental-features 'nix-command flakes' profile install "$package"
