# Install from source

Build Yetack yourself when no release file fits your computer: Windows while a release has no Windows zip, macOS, a Linux on ARM, or a Linux older than glibc 2.35. Any other Linux x86_64 can take the release's tarball instead: see "Any other Linux" in [install.md](./install.md).

You need a copy of the repository and Rust. Install Rust with [rustup](https://rustup.rs), then:

```sh
git clone https://github.com/Henauxg/yetack.git
cd yetack
```

To build a release rather than the latest code, check out its tag first: `git checkout v0.1.0`.

## The app

### Linux

Install the WebKitGTK and GTK development packages. On Ubuntu and Debian:

```sh
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
    libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libxdo-dev
```

Then build and install the app in `~/.local/bin`:

```sh
cargo install --locked --path crates/cockpit_desktop --root ~/.local
```

To find it in your desktop's menu, install its entry and icon:

```sh
install -Dm644 contrib/yetack-desktop.desktop ~/.local/share/applications/yetack-desktop.desktop
install -Dm644 crates/cockpit_desktop/assets/yetack.svg ~/.local/share/icons/hicolor/scalable/apps/yetack.svg
```

With Nix, `nix profile install 'github:Henauxg/yetack'` does all of these steps. The command builds the latest code. Add the release's tag to the address to build that release, as in `github:Henauxg/yetack/v<version>`.

### Windows

Install the Visual Studio Build Tools with the "Desktop development with C++" workload, which Rust's installer offers. Then, in PowerShell:

```powershell
cargo build --release --locked -p cockpit_desktop
```

The app is `target\release\yetack-desktop.exe`. It needs the WebView2 runtime, which Windows 10 and 11 include.

### macOS

Nobody has tested the app on macOS yet. `cargo build --release --locked -p cockpit_desktop` can work there, and reports are welcome.

## The daemon, for a server

On the server, or on any Linux machine of the same CPU type, install a C linker first (on Ubuntu and Debian: `sudo apt install build-essential`), then:

```sh
cargo install --locked --path crates/yetack --root ~/.local
```

This puts `yetack` in `~/.local/bin`, where [contrib/yetack.service](../contrib/yetack.service) looks for it. Copy that file to `~/.config/systemd/user/`, then follow [servers.md](./servers.md), "Install by hand", from step 2.

The app's "Update" button can update this daemon later. On a Linux server with an x86_64 or ARM CPU, it sends the app's daemon for that CPU. An app built from source carries the x86_64 one once `just linux-daemon` has run. Otherwise the button builds the daemon on the server, which needs Rust and this checkout: [development.md](./development.md) says how.
