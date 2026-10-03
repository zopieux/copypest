{
  description = "copypest, a minimalist clipboard manager for x11";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
      x11Libs =
        pkgs: with pkgs; [
          libGL
          libx11
          libxcb
          libxcursor
          libxrandr
          libxi
          libxkbcommon
        ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        rec {
          copypest = pkgs.rustPlatform.buildRustPackage {
            pname = "copypest";
            version = "local";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = with pkgs; [
              pkg-config
              makeWrapper
            ];
            buildInputs = x11Libs pkgs;
            postInstall = ''
              wrapProgram $out/bin/copypest \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (x11Libs pkgs)}
            '';
            doCheck = false;
            meta.mainProgram = "copypest";
          };
          default = copypest;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (x11Libs pkgs);
            inputsFrom = [ self.packages.${system}.copypest ];
            buildInputs = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              xvfb-run
              xdotool
              maim
              xclip
              imagemagick
            ];
          };
        }
      );
    };
}
