# Packaging plan

How agents-cockpit becomes something other people install, rather than build.
The first users are colleagues on Linux and Windows. The repository will be public later, so each choice keeps that door open.

## Decisions

These were settled with the user on 2026-11-11.

| Question | Decision |
| --- | --- |
| License | MPL-2.0, with contributions also licensed under MIT OR Apache-2.0, so the project can move to those later |
| Versions | A version tag per release. The daemon always tells the app its release, and the protocol version decides whether they work together. |
| Linux daemon | One static binary built for musl, x86_64 |
| Linux app | A `.deb`, built in an Ubuntu container. No AppImage for now. |
| Windows app | A `.zip`, not signed |
| macOS | No build. Users build from source. |
| Where release files are built | Locally, not in CI (the CI budget is used up) |
| Next step after the first release | The app installs and updates the daemon over SSH (step 2) |

## Step 1: a first release, v0.1.0

### 1.1 License

- Add `LICENSE` with the MPL-2.0 text, and set `license = "MPL-2.0"` in the root `Cargo.toml`.
- Every dependency is MIT, Apache-2.0 or similar, which MPL-2.0 code may use. A count of the licenses in `cargo metadata` confirmed it: five dependencies are MPL-2.0, and none requires the GPL.
- The copyright holder can release later versions under MIT or Apache-2.0. Versions already released stay available under MPL-2.0. Code from other contributors needs their agreement, so `CONTRIBUTING.md` says that a contribution is licensed under MPL-2.0 and also under MIT OR Apache-2.0.

### 1.2 Versions

- The workspace version in the root `Cargo.toml` is the release version. A release is a tag `v0.1.0` on `main`.
- `agents-cockpit --version` already prints it.
- The daemon always sends its release version in `Welcome` (`daemon_version`), and the protocol version went from 5 to 6. A daemon of protocol 5 or older refuses the app at `Hello`. The app then offers "Update", as it does for any protocol gap.
- The Machines dialog shows each daemon's version on its row, and the About section at the end of the Settings dialog's General tab shows the app's own version.
- The app offers "Update" when the daemon runs another build than the app, of any protocol. The update sends the app's own daemon, or builds the app's commit on the server ([update_script::app_commit](crates/cockpit_desktop/src/update_script.rs)) when the app carries none.

### 1.3 Release files

| File | What it holds | How it is built |
| --- | --- | --- |
| `agents-cockpit-0.1.0-x86_64-linux` | The daemon and CLI, one static binary | `cargo build --release --target x86_64-unknown-linux-musl -p agents_cockpit` inside an `ubuntu:22.04` container |
| `agents-cockpit_0.1.0_amd64.deb` | The app, the static daemon, the `.desktop` file and the icons | `cargo deb` in the same container |
| `agents-cockpit-desktop-0.1.0-windows-x64.zip` | `agents-cockpit-desktop.exe`, and the static daemon `agents-cockpit-x86_64-linux` that its updates send to servers | `cargo build --release -p cockpit_desktop` and `just linux-daemon` on the user's Windows laptop |
| `agents-cockpit.service` | The systemd user service, for a server | copied from [contrib/agents-cockpit.service](contrib/agents-cockpit.service) |
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

`just release` builds the Linux files and the checksums from the committed tree, in one container ([contrib/release/Dockerfile](contrib/release/Dockerfile)). [docs/releasing.md](docs/releasing.md) lists every step of a release, the Windows zip and `gh release create` included. Both files were checked: the daemon runs in bare Ubuntu 22.04, Debian 12 and Alpine containers, and the `.deb` installs in clean Ubuntu 22.04 and Debian 12 containers, where the app starts and draws.

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

Built on branch `feat/server-daemon-install`. The same idea as VS Code Remote: the server needs SSH and Claude Code, nothing else. [docs/architecture.md](docs/architecture.md) describes the code, under "Updating a machine's daemon".

### Where the binary comes from

The app carries the static daemon for Linux x86_64. It never downloads one. Nothing outside the app has to be trusted, and an install works offline. The app takes the first of these that exists (`server_daemon::carried`):

1. The path its build names in `AGENTS_COCKPIT_LINUX_DAEMON`. The `.deb` names `/usr/bin/agents-cockpit`, the daemon it already installs ([contrib/release/build.sh](contrib/release/build.sh)). On x86_64 Linux, the flake builds a static daemon (the package `daemon-static`) and names it in the app it builds.
2. The file `agents-cockpit-x86_64-linux` beside the app's program. The Windows zip holds it. For a build from source, `just linux-daemon` cross-builds it and copies it into `target/debug` and `target/release`.

An app without such a file still updates a server by building on it from its checkout. Linux aarch64 servers get a binary too once someone has one.

### Sending the binary

The app reads the file's SHA-256, then runs `ssh <host> sh -c '<script>'` with the file on stdin. The script writes it to a new file in `~/.cache/agents-cockpit/sent/` and prints its path. The file is mode 600 and the folder mode 700. Each send first deletes the files there older than an hour, which a send cut short leaves behind.

The script that installs the file checks it before anything changes: its checksum with `sha256sum`, then that `<file> --version` prints `agents-cockpit <release> (<commit>)`, the app's own build. A file that fails either check changes nothing (exit 1). The script deletes the file when it ends.

### First connection to a server

"Add a server" in the Machines dialog sets the server up when the form's path is left empty ([contrib/install-daemon.sh](contrib/install-daemon.sh)):

1. The app looks for `agents-cockpit` on the server: on the `PATH`, and in `~/.local/share/agents-cockpit/bin` and `~/.local/bin`. It also reads `uname -sm`. When it finds a daemon, it adds the server with that daemon's path, and the setup ends.
2. When the server has none, runs `Linux x86_64`, and the app carries a daemon, the app offers to install it and lists the two files it writes. Otherwise it says the server needs a build from source, as the README says.
3. The app sends the binary, then runs `install-daemon.sh --built <file> --sha256 <sum> --version <text>` over `ssh <host> sh -s`. The script checks the file. It needs a systemd user session: without one, it exits with 1 and points to [contrib/start-daemon](contrib/start-daemon).
4. It installs `~/.local/share/agents-cockpit/bin/agents-cockpit`, in a folder of mode 700. It writes `~/.config/systemd/user/agents-cockpit.service`, the unit of [contrib/agents-cockpit.service](contrib/agents-cockpit.service) with `ExecStart=%h/.local/share/agents-cockpit/bin/agents-cockpit serve`. It runs `systemctl --user daemon-reload` and `systemctl --user enable --now agents-cockpit`, then waits up to 30 seconds for `agents-cockpit list` to answer.
5. It reports whether linger is on (`linger yes` or `linger no`, from `loginctl show-user`), and where the service's `PATH` finds `claude` (`claude <path>` or `claude missing`).
6. Once the daemon runs, the server joins the app's config, with the installed path as its `remote_command`. The panel shows what is left to do: turn linger on with `loginctl enable-linger` (some servers want `sudo` before it), and install Claude Code. "Check again" runs the script with `--check`, which prints only the report.

The script exits with 0 when the daemon runs, 1 when it failed before anything changed, and 4 when the install or the start failed. The setup lives in the app's state, so closing the Machines dialog does not stop an install.

### Updates

1. Before the app offers "Update" for a server, it reads where the server's daemon is and who owns it (`server_daemon::probe`). The update card starts this check when it opens. The check resolves the configured path with `readlink -f`, or first finds the command on the `PATH`. It names the owner: `nix` for a path in `/nix/store`, `package <name>` when `dpkg -S` names a package, `app` for a binary in `~/.local/share/agents-cockpit/bin` or `~/.local/bin` whose folder the user can write, and `other` otherwise. It also reads `uname -sm` and the binary's `--version`.
2. The app replaces only an `app` daemon, at the path the check resolved. It sends its daemon only to a `Linux x86_64` server. A server of another system gets the build from its checkout, as before.
3. The update sends the binary, then runs [contrib/update-daemon.sh](contrib/update-daemon.sh) with `--built <file> --sha256 <sum> --version <text>`. The script checks the file, then installs it in place of a build, restarts the daemon and rolls back as before. Its exit codes stay as they are.
4. The card's steps are "Send <release> to <machine>", with the bytes sent, then "Check it on <machine>", "Restart the daemon" and "Reconnect". "Cancel the update" is not offered during the send.
5. The update runs beside the window. "Update" closes the card. The sidebar's line under the machine names the step ("Sending 0.1.0", "Checking 0.1.0", "Restarting") with a progress bar. "Show" opens the card again, and its "Hide" closes it. A failure does not open the card by itself.

### Installs that a package manager owns

Nix keeps the daemon in `/nix/store`, which is read-only, and the system's configuration decides its version. The `.deb` puts it in `/usr/bin`, which `apt` owns. The app never replaces such a daemon. Its update card says what to do instead, and the sidebar's link reads "Details" in place of "Update":

- From Nix: move the server to the release in its flake or its system configuration, then retry.
- From a package: the card gives the command `sudo apt install ./agents-cockpit_<release>_amd64.deb`, with a copy button and a button that opens the release page.
- Anywhere else: the card names the daemon's path and says to replace it by hand.

"Retry", "Check again" and an edit of the server's config make the app check the server again.

### Not built

- The first-run screen. The plan: when no config file exists, the app asks "Where do your agents run?" with two answers, "This computer" and "A server". For a server it runs the setup above, showing each step with its result. This screen gets an HTML mockup first, as every new screen does. Today a server is added from the Machines dialog.
- An install on a server without systemd. The install script tells the user to start the daemon with [contrib/start-daemon](contrib/start-daemon), and does not install it.
- The check that `claude` is signed in. The install reports only whether the service finds `claude`. The app's Accounts dialog shows the sign-in.
- The app does not look for newer releases, so a Nix or `.deb` install of the app is not told when one exists.

### Security

- The plan's rules hold: no network port, the SSH key is the only login.
- The binary comes from the app's own files. On the server, the script checks its checksum and its version before it replaces anything.
- The server's other users can read the scripts the app passes to `sh -c` (the check of the daemon and the receive script), so they hold nothing private. They hold no single quote and no backslash: Windows' ssh breaks the quoting of a remote command that holds a single quote.
- An install writes only under `~/.cache/agents-cockpit/`, `~/.local/share/agents-cockpit/` and `~/.config/systemd/user/` on the server. An update replaces only a daemon in `~/.local/share/agents-cockpit/bin` or `~/.local/bin`. The app never runs `sudo`.

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

1. Do any colleagues run servers on ARM (aarch64)?

The colleagues' distros are recent, so the `.deb` builds in `ubuntu:22.04`. That covers Ubuntu 22.04 and newer, and Debian 12.
