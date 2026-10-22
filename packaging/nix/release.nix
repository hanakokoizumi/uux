{ system, bundle }:
let
  lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
  pkgs = import (builtins.fetchTree lock.nodes.nixpkgs.locked) { inherit system; };
in pkgs.callPackage ./binary.nix { inherit bundle; }
