# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
{
  description = "A dev environment with the tools needed to work on libdatadog.";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-25.11";
    flake-utils.url = "github:numtide/flake-utils";

    # pinned, exact upstream Rust toolchains
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # backwards compatibility with nix-build and nix-shell
    flake-compat.url = "https://flakehub.com/f/edolstra/flake-compat/1.tar.gz";
  };

  outputs = {
    nixpkgs,
    flake-utils,
    rust-overlay,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [(import rust-overlay)];
        };
        mkShell = rust-toolchain:
          pkgs.mkShell {
            name = "libdatadog-devshell";
            packages = [
              rust-toolchain
              pkgs.rust-cbindgen
              pkgs.cargo-nextest
              pkgs.cmake
              pkgs.autoconf
              pkgs.automake
              pkgs.libtool
              pkgs.alejandra
            ];
            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${rust-toolchain}/lib/rustlib/src/rust/library";
            };
          };
      in {
        # Default: the pinned stable toolchain (single source of truth is
        # ./rust-toolchain.toml), matching CI and rustup.
        # Use with `nix develop`
        devShells.default = mkShell (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml);

        # Nightly toolchain (./nightly-toolchain.toml) for the jobs that
        # genuinely need a nightly compiler.
        # Use with `nix develop .#nightly`.
        devShells.nightly = mkShell (pkgs.rust-bin.fromRustupToolchainFile ./nightly-toolchain.toml);
      }
    );
}
