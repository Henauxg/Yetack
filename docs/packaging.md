# Packaging plan

How agents-cockpit becomes something other people install, rather than build.
The first users are colleagues on Linux and Windows. The repository will be public later, so each choice keeps that door open.

## Decisions

These were settled with the user on 2026-11-11.

| Question | Decision |
| --- | --- |
| License | MPL-2.0 (proposed, waiting for the user's OK) |
| Versions | A version tag per release. The app and the daemon match on the protocol version, and an update is offered on the release version, not the commit. |
| Linux daemon | One static binary built for musl, x86_64 |
| Linux app | A `.deb`, built in an Ubuntu container. No AppImage for now. |
| Windows app | A `.zip`, not signed |
| macOS | No build. Users build from source. |
| Where release files are built | Locally, not in CI (the CI budget is used up) |
| Next step after the first release | The app installs and updates the daemon over SSH (step 2) |

## Step 1: a first release, v0.1.0

### 1.1 License

- Add `LICENSE` with the MPL-2.0 text, and set `license = "MPL-2.0"` in the root `Cargo.toml`.
- Every dependency is MIT, Apache-2.0 or similar, which MPL-2.0 code may use. Run `cargo deny check licenses` once to confirm.

### 1.2 Versions

- The workspace version in the root `Cargo.toml` is the release version. A release is a tag `v0.1.0` on `main`.
- `agents-cockpit --version` already prints it. The app shows it in its About text and in the "No daemon answers" screen.
- The daemon sends its release version in `Hello`, next to `protocol_version`. The new field is optional, so an older daemon still parses, and the protocol version does not change for it.
- The app keeps refusing a daemon of another protocol version. It offers "Update" when the daemon's release version is older than its own, instead of comparing commits. A build from source without a tag keeps today's commit comparison ([crates/cockpit_desktop/build.rs](crates/cockpit_desktop/build.rs), [update_script::app_commit](crates/cockpit_desktop/src/update_script.rs)).

### 1.3 Release files

| File | What it holds | How it is built |
| --- | --- | --- |
| `agents-cockpit-0.1.0-x86_64-linux` | The daemon and CLI, one static binary | `cargo build --release --target x86_64-unknown-linux-musl -p agents_cockpit` |
| `agents-cockpit-desktop_0.1.0_amd64.deb` | The app, the daemon binary, the `.desktop` file and the icons | `cargo deb` inside an `ubuntu:22.04` container |
| `agents-cockpit-desktop-0.1.0-windows-x64.zip` | `agents-cockpit-desktop.exe` | `cargo build --release -p cockpit_desktop` on the user's Windows laptop |
| `SHA256SUMS` | The checksum of each file | `sha256sum` |

Why musl for the daemon:

- A binary built against glibc needs a glibc at least as new as the build machine's. Built on a 2026 distro, it fails on an Ubuntu 22.04 server with "version `GLIBC_2.39' not found".
- A binary built in `nix develop` also names a `/nix/store/...` loader that only Nix machines have.
- A static musl binary needs no library at all, so the same file runs on every x86_64 Linux. The daemon links no C library besides libc (`rustix`, `tokio`), so this costs nothing.
- musl's memory allocator is slower than glibc's. The daemon mostly waits on pipes and sockets, so this should not show. If it does, the daemon switches to `mimalloc`.

Why a `.deb` and not an AppImage:

- The app draws with the system's WebKitGTK. A `.deb` declares it as a dependency (`libwebkit2gtk-4.1-0`), so the package stays small and gets the distro's WebKit security fixes.
- An AppImage must carry WebKitGTK and its GPU libraries inside it (100 MB or more), and those break on some distros' graphics drivers.
- Cursor and VS Code ship an AppImage easily because they are Electron apps that carry their own Chromium anyway. Cursor first shipped only an AppImage, then added `.deb` and `.rpm`.
- The container build (Ubuntu 22.04) links against an old enough glibc for Ubuntu 22.04 and newer, and Debian 12.
- NixOS users already have `nix build` from [flake.nix](flake.nix). An AppImage or an `.rpm` comes when a colleague needs one.

A `just release` recipe builds the Linux files and the checksums. `docs/releasing.md` lists the steps:

1. Set the version in `Cargo.toml`, commit, tag.
2. `just release` on Linux.
3. Build the Windows zip on the laptop.
4. `gh release create v0.1.0 <files> --notes-file <notes>`.

### 1.4 README for users

The README starts with what the app does and a screenshot, then "Install", then the three setups, each with its exact commands:

1. **Only this computer.** Install the app and Claude Code, sign in to Claude Code, start the app. It runs the agents inside itself.
2. **A background service on this computer.** Agents keep running when the app closes. Install [contrib/agents-cockpit.service](contrib/agents-cockpit.service), then `systemctl --user enable --now agents-cockpit`.
3. **A server over SSH.** Copy the daemon binary to the server, install the service there, enable linger, then add the server in the app's Machines dialog.

A "Troubleshooting" section covers:

- `claude` not found: the service's `PATH` does not include where Claude Code is installed.
- Agents stop when you log out of the server: linger is off.
- The app cannot reach the server: the host name must work with `ssh <host>` alone, with a key and no password prompt.

The developer part moves to the end, or to `docs/development.md`.

## Step 2: the app installs and updates the daemon over SSH

The same idea as VS Code Remote: the server needs SSH and Claude Code, nothing else.

### Where the binary comes from

The app carries the daemon for Linux x86_64 beside itself (in the `.deb`, under `/usr/lib/agents-cockpit/`, and in the Windows zip). It never downloads one. Nothing outside the app has to be trusted, and an install works offline. Linux aarch64 servers get a binary too once someone has one.

### First connection to a server

1. `ssh <host> uname -sm` gives the system and the CPU. Anything other than `Linux x86_64` gets a clear message ("this server needs a build from source").
2. `ssh <host> '~/.local/share/agents-cockpit/bin/agents-cockpit --version'` says whether a daemon is installed, and which.
3. If none is installed, the app sends the binary over the SSH connection's stdin to a temporary file, checks its checksum on the server, makes it executable and moves it into place. The folder is mode 700.
4. It writes the systemd user unit, then `systemctl --user enable --now agents-cockpit`. A server without systemd gets [contrib/start-daemon](contrib/start-daemon) instead.
5. It reads `loginctl show-user "$USER" -p Linger`. When linger is off, the app shows the command to run, `loginctl enable-linger` (some servers want `sudo` before it), and why it matters. The app never runs `sudo` itself.
6. It checks that `claude` is on the service's `PATH` and signed in, and says what is missing.
7. The machine's `remote_command` in `config.toml` becomes the installed path.

### Updates

An update sends the new binary the same way, to a new file beside the old one. It then runs the waiting and restart half of [contrib/update-daemon.sh](contrib/update-daemon.sh): refuse while agents work, swap, restart, roll back if the new daemon does not start. The build half of the script is only used by machines that build from source. Its exit codes stay as they are.

### First-run screen

When no config file exists, the app asks "Where do your agents run?" with two answers: "This computer" and "A server". For a server it runs the steps above, showing each one with its result. This screen gets an HTML mockup first, as every new screen does.

### Security

- The plan's rules hold: no network port, the SSH key is the only login.
- The binary comes from the app's own files, and its checksum is checked on the server before it replaces anything.
- The app writes only under `~/.local/share/agents-cockpit/` and `~/.config/systemd/user/` on the server, and never runs `sudo`.

## Step 3: warn about an untested Claude Code version

The daemon reads Claude Code's JSON output, whose format can change between versions without notice.

- Each release records the Claude Code version it was tested with.
- The `system/init` line each agent starts with carries `claude_code_version`, which `claude_protocol` already parses. The daemon sends it to the app in the agent's details.
- When an agent's version is newer than the tested one, the app shows a quiet note on the agent ("Claude Code 2.3.1 is newer than this release was tested with"), not a modal.

## Later

- An AppImage or an `.rpm`, when a colleague needs one.
- Signing the Windows build, when there are Windows users besides the author.
- macOS: a build in CI, the `.app` bundle, notarization.
- A release built by CI on a tag, once the CI budget allows it.

## Open questions

1. The license: MPL-2.0, or another one?
2. Do any colleagues run servers on ARM (aarch64)?
3. Which is the oldest distro a colleague's laptop or server runs? It decides the container image for the `.deb`.
