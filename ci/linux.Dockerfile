# All release architectures use the same glibc 2.36 baseline.
ARG BASE_IMAGE=debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
FROM ${BASE_IMAGE}
ARG TARGETARCH
ARG RUST_VERSION=1.99.1
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates clang curl git libclang-dev pkg-config \
    protobuf-compiler nasm xz-utils patchelf python3 file \
    libasound2-dev libpulse-dev libopus-dev libzimg-dev libva-dev libdrm-dev \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxkbcommon-dev \
    libwayland-dev libegl1-mesa-dev libgl1-mesa-dev \
    fontconfig fonts-dejavu-core libgl1-mesa-dri xvfb xauth \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH
RUN case "$TARGETARCH" in \
      amd64) host=x86_64-unknown-linux-gnu ;; \
      386) host=i686-unknown-linux-gnu ;; \
      arm64) host=aarch64-unknown-linux-gnu ;; \
      *) exit 1 ;; \
    esac && \
    curl --fail --location --retry 5 "https://static.rust-lang.org/rustup/dist/$host/rustup-init" -o /tmp/rustup-init && \
    curl --fail --location --retry 5 "https://static.rust-lang.org/rustup/dist/$host/rustup-init.sha256" -o /tmp/rustup-init.sha256 && \
    cd /tmp && sha256sum --check rustup-init.sha256 && chmod +x rustup-init && \
    ./rustup-init -y --no-modify-path --default-host "$host" \
      --default-toolchain "$RUST_VERSION" --profile minimal --component rustfmt,clippy && \
    chmod -R a+rX /opt/rustup /opt/cargo && rm /tmp/rustup-init*
COPY ci/build-media.sh /opt/build-media.sh
RUN bash /opt/build-media.sh "$TARGETARCH"
RUN apt-get update && apt-get install -y --no-install-recommends libxkbcommon-x11-0 libvulkan-dev \
    && rm -rf /var/lib/apt/lists/*
ENV PKG_CONFIG_PATH=/opt/uux-media/lib/pkgconfig
ENV LD_LIBRARY_PATH=/opt/uux-media/lib
ENV CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
WORKDIR /work
