{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-25.11";

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

        # pinned Rust toolchain; single source of truth is ./rust-toolchain.toml
        # (channel + components + profile), so the devshell matches CI and rustup.
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in {
        devShells.default = pkgs.stdenv.mkDerivation {
          name = "libdatadog-devshell";

          # The stdenv cc-wrapper injects -D_FORTIFY_SOURCE, which glibc rejects
          # when compiling without optimization. Some build scripts (e.g.
          # spawn_worker's trampoline.c) compile C at -O0 with -Werror, so the
          # resulting fortify #warning becomes a hard error. Disable fortify
          # hardening in the shell so those builds succeed.
          hardeningDisable = [ "fortify" "fortify3" ];

          buildInputs = [
            rust            # rustc + cargo + rustfmt + clippy, pinned via toolchain file
            pkgs.rust-cbindgen
            pkgs.cmake
            pkgs.autoconf
            pkgs.automake
            pkgs.libtool
          ];
        };
      }
    );
}
