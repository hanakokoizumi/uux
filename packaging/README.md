# 构建与发布

工作流位于 `.github/workflows/linux.yml`，以 `uu-desktop` 为仓库根目录。

| 目标 | 可执行程序 | deb / rpm / pacman | Nix 二进制包 |
| --- | --- | --- | --- |
| Linux amd64 | ✓ | ✓ | ✓ |
| Linux i386（i686 / SSE2） | ✓ | ✓ | ✓ |
| Linux aarch64 | ✓ | ✓ | ✓ |

Windows、macOS 尚未移植，本轮不发布对应产物。i386、aarch64 的 pacman 包供支持这些架构的发行版使用。

## 触发

- PR、`main` 更新、手动运行：检查、三架构构建、测试、打包，结果保留为 Actions artifacts。
- 推送与 `Cargo.toml` 版本一致的标签（如 `v0.2.0`）：全部成功后自动发布 GitHub Release。
- 发布必须包含 15 个架构产物、UUX 源码、FFmpeg 源码及 `SHA256SUMS`。先上传草稿再发布；失败不会发布缺少产物的版本。已发布版本不会被覆盖。

检查包含格式、Clippy、Rust 单元测试、工作流校验、打包测试、X11 GUI 启动及全新 Debian / Fedora / Arch 容器安装。不会登录账号或连接远端设备。

## 安装

普通二进制压缩包解压后运行 `./uux`，保留旁边的 `lib/`。系统需 glibc **2.36+**（Debian 12 / Ubuntu 24.04 或更新）、ALSA、PulseAudio 客户端库、Opus、Wayland/X11、OpenGL/EGL、fontconfig。发行版安装包会声明对应依赖；不要求 KDE。

包内含固定版本 FFmpeg 9.0.2 和 zimg，支持 H.264/H.265、色彩转换、HDR 到 SDR 及 VA-API；64 位构建还包含 NVDEC 接口，实际硬件解码取决于显卡及驱动。原生 HDR 输出和现有程序的其他功能限制不因打包而改变。

```sh
# Debian / Ubuntu
apt install ./uux-*-linux-amd64.deb
# Fedora
dnf install ./uux-*-linux-amd64.rpm
# Arch
pacman -U ./uux-*-linux-amd64.pkg.tar.zst
```

Nix 二进制包带运行依赖闭包，先解压 `*.nix.tar.xz`，然后运行 `./install.sh`。需要本机已安装 Nix。多用户 Nix 的未签名闭包需由管理员先执行 `nix-store --import < closure.nar`，再由普通用户运行安装脚本；脚本不会降低系统签名要求。NixOS 以外的图形驱动兼容由本机 Nix/OpenGL 配置负责。源码构建可用 `nix build .#uux`，`flake.lock` 固定依赖。

## 本地复现

```sh
docker build --platform linux/amd64 --build-arg TARGETARCH=amd64 \
  -f ci/linux.Dockerfile -t uux-builder:amd64 .
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/work" \
  -e RUN_CHECKS=1 uux-builder:amd64 bash ci/build-linux.sh amd64
bash ci/install-nfpm.sh "$PWD/.ci-cache/tools"
PATH="$PWD/.ci-cache/tools:$PATH" python3 ci/package-linux.py \
  --arch amd64 --bundle dist/bundle-amd64
bash ci/package-nix.sh amd64 ./dist/bundle-amd64
```

i386 使用 `linux/386`、`TARGETARCH=386`；aarch64 使用原生 ARM64 主机、`linux/arm64`、`TARGETARCH=arm64`。发布包以 Debian 12 构建，不使用维护者机器上的库。
