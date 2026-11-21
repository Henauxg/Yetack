# Releasing

How to publish a release of Yetack, for example `0.1.0`.
A release has up to nine files:

| File | What it is |
| --- | --- |
| `yetack-0.1.0-x86_64-linux` | The daemon and command line for Linux x86_64, static: it runs on any distribution |
| `yetack-0.1.0-aarch64-linux` | The same for ARM servers |
| `yetack_0.1.0_amd64.deb` | The app for Ubuntu 22.04 and newer, and Debian 12, with the daemon beside it |
| `yetack-0.1.0-1.x86_64.rpm` | The same files as the `.deb`, for Fedora and openSUSE |
| `yetack-desktop-0.1.0-x86_64-linux.tar.gz` | The app for any other Linux x86_64 with glibc 2.35 or newer, with the daemon and an `install.sh` |
| `yetack-desktop-0.1.0-windows-x64.zip` | The app for Windows, with the Linux daemon beside it, when a Windows machine is at hand |
| `yetack-desktop-0.1.0-windows-x64.exe` | The app of the zip alone, which the Windows app's update downloads with the Linux daemon |
| `yetack.service` | The systemd user service that runs the daemon on a server |
| `SHA256SUMS` | The checksum of each file |

The Windows zip and app are built on Windows. The other files are built in Docker, on Linux or on Windows. A release can go out without them: the README sends Windows users to [install-from-source.md](./install-from-source.md) until a release has one, and the Windows app's update says the release has no Windows app.

The release builds in Ubuntu 22.04, which has `libxdo.so.3`. Newer distributions renamed it `libxdo.so.4`, and a program built against `.so.3` does not start there. So the tarball and the `.rpm` carry their own libxdo, which Dioxus's menus link and the app never calls. The app's HTTPS requests go through rustls, with ring for its cryptography, and no OpenSSL. Both are compiled into the app, so their fixes reach users only with a Yetack release. CI runs `cargo deny check advisories` on each push and each Monday (`.github/workflows/advisories.yml`). When it reports an advisory for `rustls`, `rustls-webpki`, `rustls-platform-verifier` or `ring`, update the crate and make a release soon after, even one with nothing else in it.

## 1. Set the version

1. On `main`, set `version` under `[workspace.package]` in [Cargo.toml](../Cargo.toml).
2. Run `cargo check -q --workspace`, which writes the new version into `Cargo.lock`.
3. If the protocol's messages changed since the last release, check that `PROTOCOL_VERSION` in [crates/cockpit_protocol/src/session.rs](../crates/cockpit_protocol/src/session.rs) changed too.
4. Check the release against the current Claude Code. Update Claude Code (`claude update`), then run the app from this checkout. Start an agent, and take it through a turn with a command, an approval, a question, a plan, a message sent while it works, and a stop. When each shows as it should, set `TESTED_VERSION` in [crates/cockpit_desktop/src/state/claude_code_version.rs](../crates/cockpit_desktop/src/state/claude_code_version.rs) to the version `claude --version` prints. The app shows a note on each agent whose Claude Code is newer than that version.
5. Run `just check`, then commit: `git commit -am "Release 0.1.0"`.
6. Tag and push:

   ```sh
   git tag -a v0.1.0 -m "Yetack 0.1.0"
   git push origin main v0.1.0
   ```

## 2. Build the Windows zip, if you can

Skip this step when no Windows machine is at hand. The zip holds the app and the static Linux daemon, `yetack-x86_64-linux`, which the app's "Update" sends to servers. `just linux-daemon` builds that daemon and copies it beside the app. It needs the musl target once: `rustup target add x86_64-unknown-linux-musl`.

`YETACK_PACKAGE` tells the app that it comes from the zip, so it updates itself from later releases. That update downloads the app alone, `yetack-desktop-<version>-windows-x64.exe`, with the Linux daemon and `SHA256SUMS`, so the app goes beside the zip.

On the Windows machine, in PowerShell, in a checkout of the repository:

```powershell
git fetch --tags
git checkout v0.1.0
$env:YETACK_PACKAGE = "zip"
cargo build --release -p cockpit_desktop
just linux-daemon
$version = "0.1.0"
New-Item -ItemType Directory -Force dist | Out-Null
Compress-Archive -Force -Path target\release\yetack-desktop.exe, target\release\yetack-x86_64-linux, LICENSE `
    -DestinationPath "dist\yetack-desktop-$version-windows-x64.zip"
Copy-Item target\release\yetack-desktop.exe "dist\yetack-desktop-$version-windows-x64.exe"
```

If you build the Linux files on another machine, copy the zip and the app into `dist/` of its checkout.

## 3. Build the Linux files

On Linux, with the tag checked out:

```sh
nix develop -c just release
```

On Windows, start Docker Desktop, open Git Bash in the checkout of step 2, and run `just release`. The recipe is a shell script, and `just` finds no shell to run it from PowerShell.

The recipe refuses a tree with uncommitted changes. It builds `HEAD` inside an Ubuntu 22.04 container ([contrib/release/Dockerfile](../contrib/release/Dockerfile), [contrib/release/build.sh](../contrib/release/build.sh)), writes the Linux files to `dist/`, and writes `SHA256SUMS` for every file there, the Windows zip and app included. It replaces the Linux files of an earlier run, keeps this version's zip and Windows app, and refuses to run while `dist/` holds any other file, such as an older zip.

The first run takes about ten minutes. Later runs reuse two Docker volumes, `yetack-release-registry` and `yetack-release-target`, and still rebuild every crate of the repository.

The app inside the `.deb` knows the commit it was built from. Its "Update" button sends the package's `/usr/bin/yetack` to a server, which then builds nothing.

## 4. Check the files

1. Install the `.deb` on Ubuntu or Debian (`sudo apt install ./dist/yetack_0.1.0_amd64.deb`), start the app, and open Settings: the About section, at the end of the General tab, says "Version 0.1.0".
   Then install the `.rpm` on Fedora (`sudo dnf install ./dist/yetack-0.1.0-1.x86_64.rpm`), and the tarball on another distribution such as Arch (`./install.sh` in its folder), and check the same.
2. Copy `dist/yetack-0.1.0-x86_64-linux` to a server and run it with `--version`. Do the same with `dist/yetack-0.1.0-aarch64-linux` on an ARM server, or in an ARM container (`docker run --rm --platform linux/arm64 -v "$PWD/dist:/dist" alpine /dist/yetack-0.1.0-aarch64-linux --version`).
   From Git Bash on Windows, run the x86_64 daemon in WSL (`wsl ./dist/yetack-0.1.0-x86_64-linux --version`). The ARM container needs a Windows path, and `MSYS_NO_PATHCONV=1` so that Git Bash leaves the container's paths alone: `MSYS_NO_PATHCONV=1 docker run --rm --platform linux/arm64 -v "$(pwd -W)/dist:/dist" alpine /dist/yetack-0.1.0-aarch64-linux --version`.
3. If there is a Windows zip, unzip it on Windows and start the app.

## 5. Publish

Write the release notes in a file outside the repository: what changed, and the Claude Code version the release was tested with (`TESTED_VERSION`, from step 1). Then, from the checkout's root:

```sh
version=0.1.0
zip="dist/yetack-desktop-$version-windows-x64.zip"
exe="dist/yetack-desktop-$version-windows-x64.exe"
gh release create "v$version" --title "Yetack $version" --notes-file <notes file> \
    "dist/yetack-$version-x86_64-linux" "dist/yetack-$version-aarch64-linux" \
    "dist/yetack_${version}_amd64.deb" \
    "dist/yetack-$version-1.x86_64.rpm" "dist/yetack-desktop-$version-x86_64-linux.tar.gz" \
    dist/yetack.service dist/SHA256SUMS $(test -e "$zip" && echo "$zip") $(test -e "$exe" && echo "$exe")
```
