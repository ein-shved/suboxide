{
  description = "Suboxide API-compatible music streaming server in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {self, ...} @ inputs:
    inputs.flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [
          self.overlays.default
        ];
      };
    in {
      packages = {
        inherit (pkgs) suboxide;
        default = pkgs.suboxide;
      };

      checks = {
        default = self.packages.${system}.default;
        clippy = pkgs.stdenv.mkDerivation {
          name = "clippy";
          src = ./.;
          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
          ];
          buildInputs = with pkgs; [
            openssl
          ];
          buildPhase = ''
            cargo clippy --all-targets -- -D warnings
          '';
          installPhase = "mkdir -p $out; touch $out/done";
        };
        fmt = pkgs.stdenv.mkDerivation {
          name = "fmt";
          src = ./.;
          nativeBuildInputs = with pkgs; [rustToolchain];
          buildPhase = ''
            cargo fmt -- --check
          '';
          installPhase = "mkdir -p $out; touch $out/done";
        };
      };

      devShells.default = pkgs.mkShell {
        packages = with pkgs; [
          rustToolchain
          openssl
          pkg-config
          cargo-deny
          cargo-edit
          cargo-watch
          rust-analyzer
        ];

        env = {
          # Required by rust-analyzer
          RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
        };
      };
    })
    // {
      overlays.default = final: prev: let
        rustToolchain = with inputs.fenix.packages.${prev.stdenv.hostPlatform.system};
          combine (
            with stable; [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ]
          );
      in {
        inherit rustToolchain;
        suboxide = final.callPackage ./nix/package.nix {};
      };

      nixosModules = {
        default = import ./nix/module.nix;
        suboxide = self.nixosModules.default;
      };
    };
}
