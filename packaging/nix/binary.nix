{ lib, stdenv, stdenvNoCC, autoPatchelfHook, patchelf, callPackage,
  alsa-lib, libpulseaudio, opus, libva, libdrm, bundle }:
let
  info = builtins.fromJSON (builtins.readFile (bundle + "/build.json"));
  runtime = callPackage ./runtime.nix { };
  architectures = { x86_64-linux = "amd64"; i686-linux = "i386"; aarch64-linux = "aarch64"; };
in assert info.architecture == architectures.${stdenvNoCC.hostPlatform.system};
stdenvNoCC.mkDerivation {
  pname = "uux";
  inherit (info) version;
  src = bundle;
  nativeBuildInputs = [ autoPatchelfHook patchelf ];
  buildInputs = [ alsa-lib libpulseaudio opus libva libdrm stdenv.cc.cc.lib ] ++ runtime;
  dontBuild = true;
  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin $out/lib/uux $out/share/doc/uux
    install -m755 uux $out/bin/uux
    cp -r lib/. $out/lib/uux/
    cp -r share/. $out/share/
    cp build.json $out/share/doc/uux/
    runHook postInstall
  '';
  appendRunpaths = map (p: "${lib.getLib p}/lib") runtime;
  doInstallCheck = true;
  installCheckPhase = ''
    $out/bin/uux --help > /dev/null
  '';
  meta = {
    description = "Native UU remote desktop client (release binary)";
    mainProgram = "uux";
    platforms = [ "x86_64-linux" "i686-linux" "aarch64-linux" ];
  };
}
