#!/usr/bin/env bash
set -euo pipefail
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cc -std=c11 ci/vulkan-abi.c -o "$work/system"
cc -std=c11 -Ivendor/ffmpeg-sys-next/hwcontext_stubs ci/vulkan-abi.c -o "$work/shim"
"$work/system" > "$work/expected"
"$work/shim" > "$work/actual"
diff -u "$work/expected" "$work/actual"
