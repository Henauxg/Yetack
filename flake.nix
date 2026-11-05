{
  description = "agents-cockpit: run Claude Code agents on a server and steer them from a desktop app";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
    in
    {
      devShells = forEachSystem (pkgs:
        let
          # The code builds on stable. Only rustfmt comes from nightly, for the
          # unstable options of docs/standards/rust/rustfmt.toml.
          stable = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rust-src" "rust-analyzer" ];
          };
          nightlyRustfmt = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.rustfmt);
          # Dioxus desktop renders in WebKitGTK on Linux.
          desktopLibraries = with pkgs; [
            gtk3
            webkitgtk_4_1
            libsoup_3
            glib
            xdotool
            openssl
          ];
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [ nightlyRustfmt stable pkgs.pkg-config ];
            buildInputs = desktopLibraries;
            # Settings another project's environment may export would send builds to its
            # target folder or replace this repository's clippy config.
            shellHook = ''
              unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR RUSTFLAGS CLIPPY_CONF_DIR
            '';
          };
        });
    };
}
