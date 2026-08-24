{
  description = "Rust flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    devenv = {
      url = "github:cachix/devenv";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      nixpkgs,
      flake-parts,
      devenv,
      fenix,
      ...
    }:
    let
      overlay =
        final: _prev:
        let
          fenixPkgs = fenix.packages.${final.stdenv.hostPlatform.system};
        in
        {
          rustToolchain = fenixPkgs.combine (
            (with fenixPkgs.stable; [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ])
            ++ [
              fenixPkgs.targets.wasm32-unknown-unknown.stable.rust-std
            ]
          );
        };
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        devenv.flakeModule
      ];

      systems = nixpkgs.lib.systems.flakeExposed;

      flake.overlays.default = overlay;

      perSystem =
        {
          system,
          ...
        }:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ overlay ];
            config = {
              android_sdk.accept_license = true;
              allowUnfree = true;
            };
          };
        in
        {
          _module.args.pkgs = pkgs;

          devenv.shells.default = {
            packages =
              (with pkgs; [
                rustToolchain
                openssl
                pkg-config
                ffmpeg-full
                cargo-bundle
                cargo-deny
                cargo-edit
                cargo-watch
                cmake
                git
                ninja
                rust-analyzer
                zlib
              ])
              ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
                pkgs.rustPlatform.bindgenHook
                pkgs.alsa-lib
                pkgs.dbus
                pkgs.libxkbcommon
                pkgs.vulkan-loader
                pkgs.wayland
                pkgs.wayland-protocols
                pkgs.zenity
              ]
              ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
                pkgs.actool
              ];

            env = {
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";

              LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.isLinux (
                pkgs.lib.makeLibraryPath [
                  pkgs.alsa-lib
                  pkgs.dbus
                  pkgs.libxkbcommon
                  pkgs.vulkan-loader
                  pkgs.wayland
                  pkgs.ffmpeg-full
                ]
              );
            };

            git-hooks.hooks = {
              rustfmt = {
                enable = true;
                package = pkgs.rustToolchain;
              };

              clippy = {
                enable = true;
                packageOverrides = {
                  cargo = pkgs.rustToolchain;
                  clippy = pkgs.rustToolchain;
                };
              };
            };
          };
        };
    };
}
