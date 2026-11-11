# Releasing

How to publish a release of agents-cockpit, for example `0.1.0`.
A release has four files:

| File | What it is |
| --- | --- |
| `agents-cockpit-0.1.0-x86_64-linux` | The daemon and command line for Linux x86_64, static: it runs on any distribution |
| `agents-cockpit_0.1.0_amd64.deb` | The app for Ubuntu 22.04 and newer, and Debian 12, with the daemon beside it |
| `agents-cockpit-desktop-0.1.0-windows-x64.zip` | The app for Windows |
| `SHA256SUMS` | The checksum of each file |

The Linux files are built on a Linux machine with Docker, the zip on a Windows machine.

## 1. Set the version

1. On `main`, set `version` under `[workspace.package]` in [Cargo.toml](../Cargo.toml).
2. Run `cargo check -q --workspace`, which writes the new version into `Cargo.lock`.
3. If the protocol's messages changed since the last release, check that `PROTOCOL_VERSION` in [crates/cockpit_protocol/src/session.rs](../crates/cockpit_protocol/src/session.rs) changed too.
4. Run `just check`, then commit: `git commit -am "Release 0.1.0"`.
5. Tag and push:

   ```sh
   git tag -a v0.1.0 -m "agents-cockpit 0.1.0"
   git push origin main v0.1.0
   ```

## 2. Build the Linux files

```sh
nix develop -c just release
```

The recipe refuses a tree with uncommitted changes. It builds `HEAD` inside an Ubuntu 22.04 container ([contrib/release/Dockerfile](../contrib/release/Dockerfile), [contrib/release/build.sh](../contrib/release/build.sh)) and writes the two files and `SHA256SUMS` to `dist/`. The first run takes about ten minutes. Later runs reuse two Docker volumes, `agents-cockpit-release-registry` and `agents-cockpit-release-target`.

The app inside the `.deb` knows the commit it was built from, so its "Update" button builds that commit on a server.

## 3. Build the Windows zip

On the Windows machine, in PowerShell, in a checkout of the repository:

```powershell
git fetch --tags
git checkout v0.1.0
cargo build --release -p cockpit_desktop
$version = "0.1.0"
New-Item -ItemType Directory -Force dist | Out-Null
Compress-Archive -Force -Path target\release\agents-cockpit-desktop.exe, LICENSE `
    -DestinationPath "dist\agents-cockpit-desktop-$version-windows-x64.zip"
```

Copy the zip into `dist/` on the Linux machine, then add it to the checksums:

```sh
cd dist && sha256sum agents-cockpit* > SHA256SUMS
```

## 4. Check the files

1. Install the `.deb` on Ubuntu or Debian (`sudo apt install ./agents-cockpit_0.1.0_amd64.deb`), start the app, and open the Machines dialog: it says "This app is version 0.1.0".
2. Run `./agents-cockpit-0.1.0-x86_64-linux --version` on a server.
3. Unzip the Windows build and start it.

## 5. Publish

Write the release notes in a file outside the repository: what changed, and the Claude Code version the release was tested with (`claude --version`). Then:

```sh
gh release create v0.1.0 dist/* --title "agents-cockpit 0.1.0" --notes-file <notes file>
```
