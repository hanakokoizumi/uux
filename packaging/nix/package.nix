{ lib, rustPlatform, pkg-config, protobuf, patchelf, ffmpeg_9-headless,
  alsa-lib, libpulseaudio, opus, callPackage }:
let
  manifest = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
  runtime = callPackage ./runtime.nix { };
in rustPlatform.buildRustPackage {
  pname = "uux";
  inherit (manifest.package) version;
  src = lib.cleanSourceWith {
    src = ../..;
    filter = path: type:
      !(builtins.elem (baseNameOf path) [ "target" "dist" ".ci-cache" "result" ])
      && lib.cleanSourceFilter path type;
  };
  cargoLock.lockFile = ../../Cargo.lock;
  nativeBuildInputs = [ pkg-config protobuf patchelf rustPlatform.bindgenHook ];
  buildInputs = [ ffmpeg_9-headless alsa-lib libpulseaudio opus ] ++ runtime;
  doCheck = true;
  postInstall = ''
    install -Dm644 assets/uux.svg $out/share/icons/hicolor/scalable/apps/uux.svg
    install -Dm644 assets/uux.png $out/share/icons/hicolor/256x256/apps/uux.png
    install -Dm644 packaging/uux.desktop $out/share/applications/uux.desktop
  '';
  # eframe loads the window-system libraries dynamically. Keep a real binary at
  # current_exe() so GUI-generated MCP configurations also work without a wrapper.
  postFixup = ''
    patchelf --add-rpath '${lib.makeLibraryPath runtime}' $out/bin/uux
  '';
  meta = {
    description = "Native UU remote desktop client";
    mainProgram = "uux";
    platforms = [ "x86_64-linux" "i686-linux" "aarch64-linux" ];
  };
}
