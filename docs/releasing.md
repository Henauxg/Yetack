# Releasing

How to publish a release of agents-cockpit, for example `0.1.0`.
A release has up to five files:

| File | What it is |
| --- | --- |
| `agents-cockpit-0.1.0-x86_64-linux` | The daemon and command line for Linux x86_64, static: it runs on any distribution |
| `agents-cockpit_0.1.0_amd64.deb` | The app for Ubuntu 22.04 and newer, and Debian 12, with the daemon beside it |
| `agents-cockpit-desktop-0.1.0-windows-x64.zip` | The app for Windows, when a Windows machine is at hand |
| `agents-cockpit.service` | The systemd user service that runs the daemon on a server |
| `SHA256SUMS` | The checksum of each file |

The Windows zip is built on a Windows machine, the other files on a Linux machine with Docker. A release can go out without the zip: the README sends Windows users to [install-from-source.md](./install-from-source.md) until a release has one.

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

## 2. Build the Windows zip, if you can

Skip this step when no Windows machine is at hand. On the Windows machine, in PowerShell, in a checkout of the repository:

```powershell
git fetch --tags
git checkout v0.1.0
cargo build --release -p cockpit_desktop
$version = "0.1.0"
New-Item -ItemType Directory -Force dist | Out-Null
Compress-Archive -Force -Path target\release\agents-cockpit-desktop.exe, LICENSE `
    -DestinationPath "dist\agents-cockpit-desktop-$version-windows-x64.zip"
```

Copy the zip into `dist/` of the Linux machine's checkout.

## 3. Build the Linux files

On the Linux machine, with the tag checked out:

```sh
nix develop -c just release
```

The recipe refuses a tree with uncommitted changes. It builds `HEAD` inside an Ubuntu 22.04 container ([contrib/release/Dockerfile](../contrib/release/Dockerfile), [contrib/release/build.sh](../contrib/release/build.sh)), writes the Linux files to `dist/`, and writes `SHA256SUMS` for every file there, the Windows zip included. It replaces the Linux files of an earlier run, keeps this version's zip, and refuses to run while `dist/` holds any other file, such as an older zip.

The first run takes about ten minutes. Later runs reuse two Docker volumes, `agents-cockpit-release-registry` and `agents-cockpit-release-target`, and still rebuild every crate of the repository.

The app inside the `.deb` knows the commit it was built from, so its "Update" button builds that commit on a server.

## 4. Check the files

1. Install the `.deb` on Ubuntu or Debian (`sudo apt install ./dist/agents-cockpit_0.1.0_amd64.deb`), start the app, and open Settings: the About section, at the end of the General tab, says "Version 0.1.0".
2. Copy `dist/agents-cockpit-0.1.0-x86_64-linux` to a server and run it with `--version`.
3. If there is a Windows zip, unzip it on Windows and start the app.

## 5. Publish

Write the release notes in a file outside the repository: what changed, and the Claude Code version the release was tested with (`claude --version`). Then, from the checkout's root:

```sh
version=0.1.0
zip="dist/agents-cockpit-desktop-$version-windows-x64.zip"
gh release create "v$version" --title "agents-cockpit $version" --notes-file <notes file> \
    "dist/agents-cockpit-$version-x86_64-linux" "dist/agents-cockpit_${version}_amd64.deb" \
    dist/agents-cockpit.service dist/SHA256SUMS $(test -e "$zip" && echo "$zip")
```
