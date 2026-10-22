{
  description = "UUX native Linux client";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/02c087f3fe673c9e317f206a627cb6b910336dbe";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "i686-linux" "aarch64-linux" ];
      forSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in rec {
          uux = pkgs.callPackage ./packaging/nix/package.nix { };
          default = uux;
        });
      apps = forSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.uux}/bin/uux";
          meta.description = "UUX remote desktop client";
        };
      });
      checks = forSystems (system: { inherit (self.packages.${system}) uux; });
    };
}
