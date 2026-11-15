{
  description = "yetack: run Claude Code agents on a server and steer them from a desktop app";

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
            # The commit the tree comes from, which the daemon tells the app, and which the app's
            # update builds on a server. Empty for a tree with uncommitted changes: the app then
            # offers no update. The sandbox has no git to ask.
            YETACK_SOURCE_COMMIT = self.rev or "";
          };
          # The daemon the app sends to servers: static, so that it runs on any x86_64 Linux.
          linuxDaemon = pkgs.pkgsStatic.rustPlatform.buildRustPackage (common // {
            pname = "yetack-static";
            cargoBuildFlags = [ "-p" "yetack" ];
            meta.mainProgram = "yetack";
          });
          carriesDaemon = pkgs.stdenv.hostPlatform.system == "x86_64-linux";
        in
        {
          # The daemon and the command line, for the server.
          daemon = rustPlatform.buildRustPackage (common // {
            pname = "yetack";
            cargoBuildFlags = [ "-p" "yetack" ];
            meta.mainProgram = "yetack";
          });

          # The desktop app, with its desktop entry and icon, for the laptop.
          desktop = rustPlatform.buildRustPackage (common // {
            pname = "yetack-desktop";
            cargoBuildFlags = [ "-p" "cockpit_desktop" ];
            nativeBuildInputs = [ pkgs.pkg-config pkgs.wrapGAppsHook3 ];
            buildInputs = desktopLibraries pkgs;
            # The daemon the app's update sends to a server, which then builds nothing. An app of
            # another system carries none: its update builds on the server.
            YETACK_LINUX_DAEMON =
              pkgs.lib.optionalString carriesDaemon "${linuxDaemon}/bin/yetack";
            # GTK names the window after the binary, so a Wayland desktop finds the entry, and its
            # icon, by the binary's name.
            postInstall = ''
              install -Dm644 contrib/yetack-desktop.desktop \
                $out/share/applications/yetack-desktop.desktop
              install -Dm644 crates/cockpit_desktop/assets/yetack.svg \
                $out/share/icons/hicolor/scalable/apps/yetack.svg
              install -Dm644 crates/cockpit_desktop/assets/yetack-256.png \
                $out/share/icons/hicolor/256x256/apps/yetack.png
            '';
            meta.mainProgram = "yetack-desktop";
          });

          default = self.packages.${pkgs.stdenv.hostPlatform.system}.desktop;
        } // pkgs.lib.optionalAttrs carriesDaemon {
          daemon-static = linuxDaemon;
        });

      devShells = forEachSystem (pkgs:
        let
          # The code builds on stable. Only rustfmt comes from nightly, for the
          # unstable options of docs/standards/rust/rustfmt.toml.
          stable = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rust-src" "rust-analyzer" ];
            # For `just linux-daemon`.
            targets = [ "x86_64-unknown-linux-musl" ];
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
