{
  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    utils,
    ...
  } @ inputs:
    utils.lib.eachDefaultSystem
    (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            inputs.fenix.overlays.default
          ];
        };
      in {
        devShells = let
          components = [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
            "llvm-tools"
            "rust-analyzer"
          ];
          packages = with pkgs; [
            # Development
            grcov
            pre-commit

            cargo-nextest
          ];
        in rec {
          default = stable;
          stable = pkgs.mkShell {
            nativeBuildInputs = with pkgs; ([
                (fenix.stable.withComponents components)
              ]
              ++ lib.optionals stdenv.isLinux [pkg-config]);
            buildInputs = with pkgs; ([]
              ++ lib.optionals stdenv.isLinux [
                openssl
              ]);
            inherit packages;
          };
        };
      }
    );
}
