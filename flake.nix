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
    { self, nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
      # Dioxus desktop renders in WebKitGTK on Linux.
      desktopLibraries = pkgs: with pkgs; [
        gtk3
        webkitgtk_4_1
        libsoup_3
        glib
        xdotool
        openssl
      ];
    in
    {
      packages = forEachSystem (pkgs:
        let
          toolchain = pkgs.rust-bin.stable.latest.minimal;
          rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
          common = {
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # The tests run in the dev shell: some start processes the build sandbox forbids.
            doCheck = false;
          };
        in
        rec {
          # The daemon and the command line, for the server.
          daemon = rustPlatform.buildRustPackage (common // {
            pname = "agents-cockpit";
            cargoBuildFlags = [ "-p" "agents_cockpit" ];
            meta.mainProgram = "agents-cockpit";
          });

          # The desktop app, with its desktop entry and icon, for the laptop.
          desktop = rustPlatform.buildRustPackage (common // {
            pname = "agents-cockpit-desktop";
            cargoBuildFlags = [ "-p" "cockpit_desktop" ];
            # The commit this app is built from, which the daemon's update builds on a server.
            # Empty for a tree with uncommitted changes: the app then offers no update.
            AGENTS_COCKPIT_COMMIT = self.rev or "";
            nativeBuildInputs = [ pkgs.pkg-config pkgs.wrapGAppsHook3 ];
            buildInputs = desktopLibraries pkgs;
            # GTK names the window after the binary, so a Wayland desktop finds the entry, and its
            # icon, by the binary's name.
            postInstall = ''
              install -Dm644 contrib/agents-cockpit-desktop.desktop \
                $out/share/applications/agents-cockpit-desktop.desktop
              install -Dm644 crates/cockpit_desktop/assets/agents-cockpit.svg \
                $out/share/icons/hicolor/scalable/apps/agents-cockpit.svg
              install -Dm644 crates/cockpit_desktop/assets/agents-cockpit-256.png \
                $out/share/icons/hicolor/256x256/apps/agents-cockpit.png
            '';
            meta.mainProgram = "agents-cockpit-desktop";
          });

          default = desktop;
        });

      devShells = forEachSystem (pkgs:
        let
          # The code builds on stable. Only rustfmt comes from nightly, for the
          # unstable options of docs/standards/rust/rustfmt.toml.
          stable = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rust-src" "rust-analyzer" ];
          };
          nightlyRustfmt = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.rustfmt);
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [ nightlyRustfmt stable pkgs.pkg-config pkgs.just pkgs.shellcheck ];
            buildInputs = desktopLibraries pkgs;
            # Settings another project's environment may export would send builds to its
            # target folder or replace this repository's clippy config.
            shellHook = ''
              unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR RUSTFLAGS CLIPPY_CONF_DIR
            '';
          };
        });
    };
}
