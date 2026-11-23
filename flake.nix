{
  description = "Yetack: run Claude Code agents on a server and steer them from a desktop app";

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
      # Dioxus desktop renders in WebKitGTK on Linux. Dictation records through ALSA.
      desktopLibraries = pkgs: with pkgs; [
        gtk3
        webkitgtk_4_1
        libsoup_3
        glib
        xdotool
        alsa-lib
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
            cargoLock = {
              lockFile = ./Cargo.lock;
              # Dioxus from the fork that Cargo.lock names (docs/todos.md, "Dioxus from crates.io again").
              outputHashes."dioxus-0.7.10" = "sha256-7YvuUyYdj7PF0M3QAxipo6r+1atNvLhH1fuPJkeBJEU=";
            };
            # The tests run in the dev shell: some start processes the build sandbox forbids.
            doCheck = false;
            # The commit the tree comes from, which the daemon tells the app, and which the app's
            # update builds on a server. Empty for a tree with uncommitted changes: the app then
            # offers no update. The sandbox has no git to ask.
            YETACK_SOURCE_COMMIT = self.rev or "";
          };
          system = pkgs.stdenv.hostPlatform.system;
          # A daemon the app sends to servers: static, so that it runs on any Linux of its CPU.
          # `static` builds it, natively or across from the other CPU.
          staticDaemon = static: static.rustPlatform.buildRustPackage (common // {
            pname = "yetack-static";
            cargoBuildFlags = [ "-p" "yetack" ];
            meta.mainProgram = "yetack";
          });
          # The app carries the daemon of each CPU, whichever system builds it.
          linuxDaemons = {
            x86_64 = staticDaemon
              (if system == "x86_64-linux" then pkgs.pkgsStatic else pkgs.pkgsCross.gnu64.pkgsStatic);
            aarch64 = staticDaemon
              (if system == "aarch64-linux" then pkgs.pkgsStatic else pkgs.pkgsCross.aarch64-multiplatform.pkgsStatic);
          };
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
            # cmake builds the speech model's C++ (transcribe.cpp) from a crate's build script.
            # The package itself is no cmake project.
            nativeBuildInputs = [ pkgs.pkg-config pkgs.wrapGAppsHook3 pkgs.cmake ];
            dontUseCmakeConfigure = true;
            buildInputs = desktopLibraries pkgs;
            # The daemons the app's update and install send to a server, which then builds
            # nothing.
            YETACK_LINUX_DAEMON = "${linuxDaemons.x86_64}/bin/yetack";
            YETACK_LINUX_DAEMON_AARCH64 = "${linuxDaemons.aarch64}/bin/yetack";
            # Tells the app that Nix installed it, so it says how to update instead of updating.
            YETACK_PACKAGE = "nix";
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

          default = self.packages.${system}.desktop;
          daemon-static = linuxDaemons.x86_64;
          daemon-static-aarch64 = linuxDaemons.aarch64;
        });

      devShells = forEachSystem (pkgs:
        let
          # The code builds on stable. Only rustfmt comes from nightly, for the
          # unstable options of docs/standards/rust/rustfmt.toml.
          stable = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rust-src" "rust-analyzer" ];
            # For `just linux-daemon`.
            targets = [ "x86_64-unknown-linux-musl" "aarch64-unknown-linux-musl" ];
          };
          nightlyRustfmt = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.rustfmt);
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [ nightlyRustfmt stable pkgs.pkg-config pkgs.just pkgs.shellcheck pkgs.cmake ];
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
