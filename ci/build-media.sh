#!/usr/bin/env bash
set -euo pipefail
arch=${1:?architecture required}
version=9.0.2
checksum=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
curl --fail --location --retry 5 --retry-all-errors --connect-timeout 20 --max-time 300 "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz" -o ffmpeg.tar.xz
echo "$checksum  ffmpeg.tar.xz" | sha256sum --check
tar -xf ffmpeg.tar.xz
hardware=(--enable-vaapi --enable-libdrm --enable-hwaccel=h264_vaapi,hevc_vaapi)
if [[ $arch == 386 ]]; then
    hardware+=(--arch=x86 --cpu=i686)
fi
if [[ $arch != 386 ]]; then
    curl --fail --location --retry 5 --retry-all-errors --connect-timeout 20 --max-time 300 \
        https://api.github.com/repos/FFmpeg/nv-codec-headers/tarball/n13.1.15.0 -o nv-codec.tar.gz
    echo '000b906c860fc00c5e65e21767e59034f25bb4090c371ea3642e6da13e381d7b  nv-codec.tar.gz' | sha256sum --check
    mkdir nv-codec
    tar -xf nv-codec.tar.gz -C nv-codec --strip-components=1
    make -C nv-codec PREFIX=/opt/uux-media install
    hardware+=(--enable-ffnvcodec --enable-cuda --enable-hwaccel=h264_nvdec,hevc_nvdec)
fi
cd "ffmpeg-$version"
PKG_CONFIG_PATH=/opt/uux-media/lib/pkgconfig ./configure \
    --prefix=/opt/uux-media --libdir=/opt/uux-media/lib \
    --disable-static --enable-shared --enable-pic --disable-doc --disable-programs \
    --disable-autodetect --disable-network --disable-everything \
    --disable-avdevice --disable-swresample --enable-avformat \
    --enable-decoder=h264,hevc --enable-parser=h264,hevc \
    --enable-filter=buffer,buffersink,format,scale,zscale,tonemap \
    --enable-libzimg "${hardware[@]}"
make -j "${BUILD_JOBS:-4}"
make install
mkdir -p /opt/uux-media/share/licenses
cp COPYING.LGPLv2.1 LICENSE.md /opt/uux-media/share/licenses/
cp /usr/share/doc/libzimg2/copyright /opt/uux-media/share/licenses/zimg-copyright
# Keep the exact source used for the dynamically linked media libraries available.
cp "$work/ffmpeg.tar.xz" /opt/uux-media/share/ffmpeg-source.tar.xz
