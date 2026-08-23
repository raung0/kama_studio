{
  description = "Rust flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      fenix,
      ...
    }:
    let
      overlay =
        final: prev:
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
            ]) ++ [ fenixPkgs.targets.wasm32-unknown-unknown.stable.rust-std ]
          );
        };
    in
    {
      overlays.default = overlay;
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
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
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.rustPlatform.bindgenHook
          ];

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
            ++ (with pkgs; lib.optionals stdenv.isDarwin [
              actool
            ])
            ++ (with pkgs; lib.optionals stdenv.isLinux [
              alsa-lib
              dbus
              libxkbcommon
              vulkan-loader
              wayland
              wayland-protocols
              zenity
            ]);

          shellHook = ''
            export RUST_SRC_PATH="${pkgs.rustToolchain}/lib/rustlib/src/rust/library"
            ${pkgs.lib.optionalString pkgs.stdenv.isLinux ''
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
                pkgs.alsa-lib
                pkgs.dbus
                pkgs.libxkbcommon
                pkgs.vulkan-loader
                pkgs.wayland
                pkgs.ffmpeg-full
              ]}:''${LD_LIBRARY_PATH:-}"
            ''}
          '';
        };
      }
    );
}
