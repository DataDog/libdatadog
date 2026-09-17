# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-26.05";

    # cross-platform convenience
    flake-utils.url = "github:numtide/flake-utils";

    # backwards compatibility with nix-build and nix-shell
    flake-compat.url = "https://flakehub.com/f/edolstra/flake-compat/1.tar.gz";

    # pinned, exact upstream Rust toolchains
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, flake-compat, rust-overlay }:
    # resolve for all platforms in turn
    flake-utils.lib.eachDefaultSystem (system:
      let
        # packages for this system platform, with the rust-overlay applied
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # A devshell for a given Rust toolchain (read from a toolchain file via
        # rust-overlay), with the rest of the build dependencies.
        mkDevShell = rust: pkgs.mkShell {
          name = "libdatadog-devshell";

          # The stdenv cc-wrapper injects -D_FORTIFY_SOURCE, which glibc rejects
          # when compiling without optimization. Some build scripts (e.g.
          # spawn_worker's trampoline.c) compile C at -O0 with -Werror, so the
          # resulting fortify #warning becomes a hard error. Disable fortify
          # hardening in the shell so those builds succeed.
          hardeningDisable = [ "fortify" "fortify3" ];

          nativeBuildInputs = [
            rust            # rustc + cargo + rustfmt + clippy, pinned via toolchain file
            pkgs.rust-cbindgen
            pkgs.cmake
            pkgs.autoconf
            pkgs.automake
            pkgs.libtool
          ];
        };
      in {
        # Default: the pinned stable toolchain (single source of truth is
        # ./rust-toolchain.toml), matching CI and rustup.
        devShells.default = mkDevShell (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml);

        # Nightly toolchain (./nightly-toolchain.toml) for the jobs that
        # genuinely need a nightly compiler. Use with `nix develop .#nightly`.
        devShells.nightly = mkDevShell (pkgs.rust-bin.fromRustupToolchainFile ./nightly-toolchain.toml);
      }
    );
}
