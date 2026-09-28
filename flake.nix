{
  description = "WiVRn to Rebocap VR-mode bridge development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rebocap-compat = {
      url = "github:dogesoulseller/rebocap-linux-compat";
      flake = false;
    };
  };

  outputs = { nixpkgs, rust-overlay, rebocap-compat, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          # Small Windows helper from rebocap-linux-compat (MIT), run under Wine.
          bridge = pkgs.pkgsCross.mingwW64.stdenv.mkDerivation {
            pname = "rebocap-pipe-bridge";
            version = "unstable";
            src = rebocap-compat;
            dontConfigure = true;
            buildPhase = ''
              runHook preBuild
              $CC bridge/bridge.c -o bridge.exe -lws2_32
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              install -Dm755 bridge.exe "$out/bin/bridge.exe"
              runHook postInstall
            '';
          };
        in f pkgs bridge);
    in {
      packages = forAllSystems (pkgs: bridge:
        if pkgs.stdenv.hostPlatform.isx86_64 then { inherit bridge; } else { });
      devShells = forAllSystems (pkgs: bridge: {
        default = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.stable.latest.default.override {
              extensions = [ "rust-src" "rustfmt" "clippy" ];
            })
            pkgs.pkg-config
            pkgs.openxr-loader
          ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isx86_64 [ bridge ];
        };
      });
    };
}
