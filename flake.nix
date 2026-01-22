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
        description = "Agentic Code Combo";
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            inputs.fenix.overlays.default
          ];
        };
        lib = pkgs.lib;
        git_dirty =
          if (self.sourceInfo ? rev)
          then "false"
          else "true";
        git_commit_sha =
          self.sourceInfo.rev or (
            if (self.sourceInfo ? dirtyRev)
            then lib.strings.removeSuffix "-dirty" self.sourceInfo.dirtyRev
            else "unknown"
          );
        git_last_modified = toString self.sourceInfo.lastModified or "unknown";
      in {
        packages = rec {
          default = code-combo;
          code-combo = let
            toolchain = (pkgs.fenix.stable).minimalToolchain;
            rustPlatform = pkgs.makeRustPlatform {
              cargo = toolchain;
              rustc = toolchain;
            };
            meta = builtins.fromTOML (builtins.readFile ./Cargo.toml);
            inherit (meta.package) name;
            inherit (meta.workspace.package) version;
          in
            rustPlatform.buildRustPackage {
              pname = name;
              inherit version;
              meta = {
                inherit description;
                mainProgram = "coco";
              };
              src = ./.;
              logLevel = "trace";
              env = {
                GIT_COMMIT_SHA = git_commit_sha;
                GIT_DIRTY = git_dirty;
                SOURCE_DATE_EPOCH = git_last_modified;
              };
              cargoLock = {
                lockFile = ./Cargo.lock;

                outputHashes = {
                  "tree-sitter-diff-0.1.0" = "sha256-8rYLNGgoZSvvfqO2++nAgFKmvbkKJ3m+9B8bTXp6Us4=";
                  "tui-textarea-0.7.0" = "sha256-3ENi0XCVkhJAj9mgMXXkCY2FZ1VcVrSjfidBCsYdfMA=";
                };
              };
              cargoBuildFlags = ["-p" "coco-tui"];

              preCheck = ''
                cargo build --bin coco
              '';

              nativeCheckInputs = with pkgs; [
                bash
              ];
              cargoTestFlags = [
                "--all"
                "--no-capture"
              ];
              useNextest = true;
            };
          run-test = pkgs.writeShellApplication {
            name = "run-test";
            text = builtins.readFile ./scripts/run-test.sh;
          };
          run-cov = pkgs.writeShellApplication {
            name = "run-cov";
            text = builtins.readFile ./scripts/run-cov.sh;
          };
        };
        apps = rec {
          default = coco;
          coco = {
            type = "app";
            meta = {
              inherit description;
            };
            program = lib.getExe self.packages.${system}.code-combo;
          };
        };
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
          packages = with pkgs;
            [
              # Development
              grcov
              prek

              cargo-nextest
              cargo-flamegraph
            ]
            ++ (with self.packages.${system}; [
              run-test
              run-cov
            ]);
        in rec {
          default = stable;
          stable = pkgs.mkShell {
            nativeBuildInputs = with pkgs; ([
                (fenix.stable.withComponents components)
              ]
              ++ lib.optionals stdenv.isLinux [pkg-config]);

            inherit packages;

            shellHook = ''
              # Unset SOURCE_DATE_EPOCH to prevent reproducible build timestamps during development.
              # This allows timestamps to reflect the current time, which is useful for development workflows.
              unset SOURCE_DATE_EPOCH
            '';
          };
        };
      }
    );
}
