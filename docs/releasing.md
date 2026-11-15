# Releasing

How to publish a release of Yetack, for example `0.1.0`.
A release has up to five files:

| File | What it is |
| --- | --- |
| `yetack-0.1.0-x86_64-linux` | The daemon and command line for Linux x86_64, static: it runs on any distribution |
| `yetack_0.1.0_amd64.deb` | The app for Ubuntu 22.04 and newer, and Debian 12, with the daemon beside it |
| `yetack-desktop-0.1.0-windows-x64.zip` | The app for Windows, with the Linux daemon beside it, when a Windows machine is at hand |
| `yetack.service` | The systemd user service that runs the daemon on a server |
| `SHA256SUMS` | The checksum of each file |

The Windows zip is built on a Windows machine, the other files on a Linux machine with Docker. A release can go out without the zip: the README sends Windows users to [install-from-source.md](./install-from-source.md) until a release has one.

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

On the Windows machine, in PowerShell, in a checkout of the repository:

```powershell
git fetch --tags
git checkout v0.1.0
cargo build --release -p cockpit_desktop
just linux-daemon
$version = "0.1.0"
New-Item -ItemType Directory -Force dist | Out-Null
Compress-Archive -Force -Path target\release\yetack-desktop.exe, target\release\yetack-x86_64-linux, LICENSE `
    -DestinationPath "dist\yetack-desktop-$version-windows-x64.zip"
```

Copy the zip into `dist/` of the Linux machine's checkout.

## 3. Build the Linux files

On the Linux machine, with the tag checked out:

```sh
nix develop -c just release
```

The recipe refuses a tree with uncommitted changes. It builds `HEAD` inside an Ubuntu 22.04 container ([contrib/release/Dockerfile](../contrib/release/Dockerfile), [contrib/release/build.sh](../contrib/release/build.sh)), writes the Linux files to `dist/`, and writes `SHA256SUMS` for every file there, the Windows zip included. It replaces the Linux files of an earlier run, keeps this version's zip, and refuses to run while `dist/` holds any other file, such as an older zip.

The first run takes about ten minutes. Later runs reuse two Docker volumes, `yetack-release-registry` and `yetack-release-target`, and still rebuild every crate of the repository.

The app inside the `.deb` knows the commit it was built from. Its "Update" button sends the package's `/usr/bin/yetack` to a server, which then builds nothing.

## 4. Check the files

1. Install the `.deb` on Ubuntu or Debian (`sudo apt install ./dist/yetack_0.1.0_amd64.deb`), start the app, and open Settings: the About section, at the end of the General tab, says "Version 0.1.0".
2. Copy `dist/yetack-0.1.0-x86_64-linux` to a server and run it with `--version`.
3. If there is a Windows zip, unzip it on Windows and start the app.

## 5. Publish

Write the release notes in a file outside the repository: what changed, and the Claude Code version the release was tested with (`TESTED_VERSION`, from step 1). Then, from the checkout's root:

```sh
version=0.1.0
zip="dist/yetack-desktop-$version-windows-x64.zip"
gh release create "v$version" --title "Yetack $version" --notes-file <notes file> \
    "dist/yetack-$version-x86_64-linux" "dist/yetack_${version}_amd64.deb" \
    dist/yetack.service dist/SHA256SUMS $(test -e "$zip" && echo "$zip")
```
