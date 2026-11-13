# Install from source

Build agents-cockpit yourself when no release file fits your computer: Windows while a release has no Windows zip, macOS, a Linux other than Ubuntu and Debian, or an ARM server.

You need a copy of the repository and Rust. Install Rust with [rustup](https://rustup.rs), then:

```sh
git clone https://github.com/Henauxg/agents-cockpit.git
cd agents-cockpit
```

To build a release rather than the latest code, check out its tag first: `git checkout v0.1.0`.

## The app

### Linux

Install the WebKitGTK and GTK development packages. On Ubuntu and Debian:

```sh
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
    libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libxdo-dev libssl-dev
```

Then build and install the app in `~/.local/bin`:

```sh
cargo install --locked --path crates/cockpit_desktop --root ~/.local
```

To find it in your desktop's menu, install its entry and icon:

```sh
install -Dm644 contrib/agents-cockpit-desktop.desktop ~/.local/share/applications/agents-cockpit-desktop.desktop
install -Dm644 crates/cockpit_desktop/assets/agents-cockpit.svg ~/.local/share/icons/hicolor/scalable/apps/agents-cockpit.svg
```

With Nix, `nix profile install 'github:Henauxg/agents-cockpit'` does all of these steps. The command builds the latest code. Add the release's tag to the address to build that release, as in `github:Henauxg/agents-cockpit/v0.1.0`.

### Windows

Install the Visual Studio Build Tools with the "Desktop development with C++" workload, which Rust's installer offers. Then, in PowerShell:

```powershell
cargo build --release --locked -p cockpit_desktop
```

The app is `target\release\agents-cockpit-desktop.exe`. It needs the WebView2 runtime, which Windows 10 and 11 include.

### macOS

Nobody has tested the app on macOS yet. `cargo build --release --locked -p cockpit_desktop` can work there, and reports are welcome.

## The daemon, for a server

On the server, or on any Linux machine of the same CPU type, install a C linker first (on Ubuntu and Debian: `sudo apt install build-essential`), then:

```sh
cargo install --locked --path crates/agents_cockpit --root ~/.local
```

This puts `agents-cockpit` in `~/.local/bin`, where [contrib/agents-cockpit.service](../contrib/agents-cockpit.service) looks for it. Copy that file to `~/.config/systemd/user/`, then follow "On a server over SSH" in [README.md](../README.md) from step 2.

A server with Rust and this checkout can also be updated from the app's "Update" button: [development.md](./development.md) says how.
