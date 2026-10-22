#!/usr/bin/env bash
set -euo pipefail
arch=${1:?amd64, i386 or aarch64 required}
case "$arch" in
    amd64) expected='x86_64-unknown-linux-gnu' ;;
    i386) expected='i686-unknown-linux-gnu' ;;
    aarch64) expected='aarch64-unknown-linux-gnu' ;;
    *) exit 1 ;;
esac
rustc -vV | grep -Fx "host: $expected"
export CARGO_HOME="/work/.ci-cache/cargo-$arch"
export CARGO_TARGET_DIR="/work/target/ci/$arch"
export CARGO_BUILD_JOBS=${BUILD_JOBS:-4}
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR"
# Cargo checks Rust fingerprints itself. Also invalidate native outputs when
# the compiler/media environment changes, including across fallback cache keys.
environment=$( { rustc -vV; pkg-config --modversion libavcodec libavutil libavformat libavfilter libswscale; sha256sum ci/linux.Dockerfile ci/build-media.sh; } | sha256sum | cut -d' ' -f1)
stamp="$CARGO_TARGET_DIR/uux-build-environment"
if [[ -f "$stamp" && $(cat "$stamp") != "$environment" ]]; then
    cargo clean --target-dir "$CARGO_TARGET_DIR"
fi
printf '%s\n' "$environment" > "$stamp"
bash ci/check-vulkan-abi.sh
if [[ ${RUN_CHECKS:-0} == 1 ]]; then
    cargo fmt --all --check
    cargo clippy --locked --all-targets -- -D warnings
fi
# Run the same offline device-independent tests on every CPU architecture.
cargo test --locked
cargo build --locked --release
state=$(mktemp -d)
trap 'rm -rf "$state"' EXIT
"$CARGO_TARGET_DIR/release/uux" --help
timeout 30 xvfb-run -a "$CARGO_TARGET_DIR/release/uux" \
    --state-dir "$state/state" --screenshot "$state/window.png" --quit-after 3
test -s "$state/window.png"
python3 ci/bundle-linux.py --arch "$arch" --binary "$CARGO_TARGET_DIR/release/uux" \
    --media /opt/uux-media --output "dist/bundle-$arch"
