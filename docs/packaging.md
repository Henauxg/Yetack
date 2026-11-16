# Packaging plan

How Yetack becomes something other people install, rather than build.
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
- `yetack --version` already prints it.
- The daemon always sends its release version in `Welcome` (`daemon_version`), and the protocol version went from 5 to 6. A daemon of protocol 5 or older refuses the app at `Hello`. The app then offers "Update", as it does for any protocol gap.
- The Machines dialog shows each daemon's version on its row, and the About section at the end of the Settings dialog's General tab shows the app's own version.
- The app offers "Update" when the daemon runs another build than the app, of any protocol. The update sends the app's own daemon, or builds the app's commit on the server ([update_script::app_commit](crates/cockpit_desktop/src/update_script.rs)) when the app carries none.

### 1.3 Release files

| File | What it holds | How it is built |
| --- | --- | --- |
| `yetack-<version>-x86_64-linux` | The daemon and CLI, one static binary | `cargo build --release --target x86_64-unknown-linux-musl -p yetack` inside an `ubuntu:22.04` container |
| `yetack_<version>_amd64.deb` | The app, the static daemon, the `.desktop` file and the icons | `cargo deb` in the same container |
| `yetack-desktop-<version>-windows-x64.zip` | `yetack-desktop.exe`, and the static daemon `yetack-x86_64-linux` that its updates send to servers | `cargo build --release -p cockpit_desktop` and `just linux-daemon` on the user's Windows laptop |
| `yetack.service` | The systemd user service, for a server | copied from [contrib/yetack.service](contrib/yetack.service) |
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
2. **A background service on this computer.** Agents keep running when the app closes. Install [contrib/yetack.service](contrib/yetack.service), then `systemctl --user enable --now yetack`.
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

1. The path its build names in `YETACK_LINUX_DAEMON`. The `.deb` names `/usr/bin/yetack`, the daemon it already installs ([contrib/release/build.sh](contrib/release/build.sh)). On x86_64 Linux, the flake builds a static daemon (the package `daemon-static`) and names it in the app it builds.
2. The file `yetack-x86_64-linux` beside the app's program. The Windows zip holds it. For a build from source, `just linux-daemon` cross-builds it and copies it into `target/debug` and `target/release`.

An app without such a file still updates a server by building on it from its checkout. Linux aarch64 servers get a binary too once someone has one.

### Sending the binary

The app reads the file's SHA-256, then runs `ssh <host> sh -c '<script>'` with the file on stdin. The script writes it to a new file in `~/.cache/yetack/sent/` and prints its path. The file is mode 600 and the folder mode 700. Each send first deletes the files there older than an hour, which a send cut short leaves behind.

The script that installs the file checks it before anything changes: its checksum with `sha256sum`, then that `<file> --version` prints `yetack <release> (<commit>)`, the app's own build. A file that fails either check changes nothing (exit 1). The script deletes the file when it ends.

### First connection to a server

"Add a server" in the Machines dialog sets the server up when the form's path is left empty ([contrib/install-daemon.sh](contrib/install-daemon.sh)):

1. The app looks for `yetack` on the server: on the `PATH`, and in `~/.local/share/yetack/bin` and `~/.local/bin`. It also reads `uname -sm`. When it finds a daemon, it adds the server with that daemon's path, and the setup ends.
2. When the server has none, runs `Linux x86_64`, and the app carries a daemon, the app offers to install it and lists the two files it writes. Otherwise it says the server needs a build from source, as the README says.
3. The app sends the binary, then runs `install-daemon.sh --built <file> --sha256 <sum> --version <text>` over `ssh <host> sh -s`. The script checks the file. Without a systemd user session, it starts the daemon apart from the SSH session instead (see 5.5).
4. It installs `~/.local/share/yetack/bin/yetack`, in a folder of mode 700. It writes `~/.config/systemd/user/yetack.service`, the unit of [contrib/yetack.service](contrib/yetack.service) with `ExecStart=%h/.local/share/yetack/bin/yetack serve`. It runs `systemctl --user daemon-reload` and `systemctl --user enable --now yetack`, then waits up to 30 seconds for `yetack list` to answer.
5. It reports whether linger is on (`linger yes` or `linger no`, from `loginctl show-user`), and where the service's `PATH` finds `claude` (`claude <path>` or `claude missing`).
6. Once the daemon runs, the server joins the app's config, with the installed path as its `remote_command`. The panel shows what is left to do: turn linger on with `loginctl enable-linger` (some servers want `sudo` before it), and install Claude Code. "Check again" runs the script with `--check`, which prints only the report.

The script exits with 0 when the daemon runs, 1 when it failed before anything changed, and 4 when the install or the start failed. The setup lives in the app's state, so closing the Machines dialog does not stop an install.

### Updates

1. Before the app offers "Update" for a server, it reads where the server's daemon is and who owns it (`server_daemon::probe`). The update card starts this check when it opens. The check resolves the configured path with `readlink -f`, or first finds the command on the `PATH`. It names the owner: `nix` for a path in `/nix/store`, `package deb <name>` when `dpkg -S` names a package, `package rpm <name>` when `rpm -qf` does, `app` for a binary in `~/.local/share/yetack/bin` or `~/.local/bin` whose folder the user can write, and `other` otherwise. It also reads `uname -sm` and the binary's `--version`.
2. The app replaces only an `app` daemon, at the path the check resolved. It sends its daemon only to a `Linux x86_64` server. A server of another system gets the build from its checkout, as before.
3. The update sends the binary, then runs [contrib/update-daemon.sh](contrib/update-daemon.sh) with `--built <file> --sha256 <sum> --version <text>`. The script checks the file, then installs it in place of a build, restarts the daemon and rolls back as before. Its exit codes stay as they are.
4. The card's steps are "Send <release> to <machine>", with the bytes sent, then "Check it on <machine>", "Restart the daemon" and "Reconnect". "Cancel the update" is not offered during the send.
5. The update runs beside the window. "Update" closes the card. The sidebar's line under the machine names the step ("Sending 0.1.0", "Checking 0.1.0", "Restarting") with a progress bar. "Show" opens the card again, and its "Hide" closes it. A failure does not open the card by itself.

### Installs that a package manager owns

Nix keeps the daemon in `/nix/store`, which is read-only, and the system's configuration decides its version. The `.deb` puts it in `/usr/bin`, which `apt` owns. The app never replaces such a daemon. Its update card says what to do instead, and the sidebar's link reads "Details" in place of "Update":

- From Nix: move the server to the release in its flake or its system configuration, then retry.
- From a package: the card gives the command `sudo apt install ./yetack_<release>_amd64.deb`, with a copy button and a button that opens the release page.
- Anywhere else: the card names the daemon's path and says to replace it by hand.

"Retry", "Check again" and an edit of the server's config make the app check the server again.

### Not built

- The first-run screen. The plan: when no config file exists, the app asks "Where do your agents run?" with two answers, "This computer" and "A server". For a server it runs the setup above, showing each step with its result. This screen gets an HTML mockup first, as every new screen does. Today a server is added from the Machines dialog.
- The check that `claude` is signed in. The install reports only whether the service finds `claude`. The app's Accounts dialog shows the sign-in.

### Security

- The plan's rules hold: no network port, the SSH key is the only login.
- The binary comes from the app's own files. On the server, the script checks its checksum and its version before it replaces anything.
- The server's other users can read the scripts the app passes to `sh -c` (the check of the daemon and the receive script), so they hold nothing private. They hold no single quote and no backslash: Windows' ssh breaks the quoting of a remote command that holds a single quote.
- An install writes only under `~/.cache/yetack/`, `~/.local/share/yetack/` and `~/.config/systemd/user/` on the server. An update replaces only a daemon in `~/.local/share/yetack/bin` or `~/.local/bin`. The app never runs `sudo`.

## Step 3: warn about an untested Claude Code version

Built on branch `feat/claude-code-version`. The daemon reads Claude Code's JSON output, whose format can change between versions without notice.

- Each release records the Claude Code version it was tested with: `TESTED_VERSION` in [crates/cockpit_desktop/src/state/claude_code_version.rs](../crates/cockpit_desktop/src/state/claude_code_version.rs), 2.1.319 for now. [releasing.md](releasing.md) checks the release against the current Claude Code and updates it.
- The `system/init` line that starts each turn carries `claude_code_version`. The daemon sends it to the app in the agent's summary (`AgentSummary.claude_code_version`), which the header reads. The summary leaves it out until the agent's `claude` writes that line. The protocol moved to version 11 for it.
- When an agent's version is newer than the tested one, the header's folder line goes on with a quiet note: "Claude Code 2.3.1 is newer than this release was tested with (2.1.319)." Versions compare by their numbers, so 2.10.0 is newer than 2.9.9. An older or equal version, or one the app cannot read, shows nothing.

## Step 4: the app tells when a new release is out

Built on branch `feat/self-update`. [docs/architecture.md](architecture.md) describes the code, under "The app's own update".

- Once a day, the app asks GitHub's API for the repository's latest release. It saves when it last asked, so a restart does not ask again within the day. It waits at least ten seconds after it starts. The request names the app and its version (`User-Agent: yetack-desktop/<version>`) and nothing else. A failed check goes to the log, and the next one tries again.
- A release newer than the app shows a note at the foot of the sidebar, "0.4.0 is out", and a row in Settings › General › About. The note's cross, or the card's "Skip this version", hides that release for good. A newer one shows again. "Not now" hides that release's note until the next start. A newer release found before then shows.
- Settings › General › About has "Look for new releases", on by default. Turned off, the app asks GitHub nothing.
- The build says how the app was installed, in `YETACK_PACKAGE`: `zip` for the Windows release ([releasing.md](releasing.md), step 2), `deb`, `rpm` and `tarball` for the Linux release files ([contrib/release/build.sh](../contrib/release/build.sh)), `nix` for the flake. Any other build is one from source.
- The Windows zip updates itself. It downloads the release's `yetack-desktop-<version>-windows-x64.exe`, `yetack-<version>-x86_64-linux` and `SHA256SUMS`, only from the repository's release downloads, and checks both files against the checksums before anything moves. It then puts them in place of its own program and of the Linux daemon beside it, and restarts when the user says so. A failure leaves the app as it was. If the files cannot all go back, the update deletes none of them and says where each one is.
- The other installs get the steps: the `.deb`'s `apt` command, the `.rpm`'s `dnf` command, the tarball's `install.sh`, the Nix configuration to change, or the commands that build the release from a checkout.

## Step 5: more Linux systems, ARM servers, servers without systemd

Settled with the user on 2026-11-16, after a colleague asked how to install Yetack on Gentoo. The answer then needed a build from source, a daemon downloaded by hand, and tmux on a server without systemd. Each part below removes one of those.

### 5.1 The app as a tarball, for any Linux

The `.deb` serves Ubuntu and Debian alone. The release adds `yetack-desktop-<version>-x86_64-linux.tar.gz`: the `.deb`'s program, built again in the same container, with the static daemon, the desktop entry and the icons.

- It links against the system's WebKitGTK 4.1, as the `.deb` does, so it runs on any distro with glibc 2.35 or newer that has WebKitGTK 4.1: Gentoo, Fedora, Arch, openSUSE.
- Two libraries the `.deb` takes from the system changed their names on recent distros. Arch has `libssl.so.4` (OpenSSL 4). Arch, Fedora 45 and openSUSE Tumbleweed have `libxdo.so.4`. Ubuntu 22.04 builds against `.so.3` of both. The app cannot drop either: OpenSSL makes its HTTPS requests (the release check and the Windows update's downloads, through `ureq` and `native-tls`) and Dioxus's websocket, and Dioxus's menus need libxdo.
- The tarball's app links OpenSSL in (the `static-openssl` feature builds it from source). It still reads the system's root certificates, which `native-tls` finds in the distro's usual places. The tarball carries Ubuntu's `libxdo.so.3`. The app finds it through its `RUNPATH`, `$ORIGIN/lib:$ORIGIN/../lib/yetack`: `lib/` beside it in the tarball, `lib/yetack/` once installed. The loader searches a `RUNPATH` before the system's folders, so the carried copy always wins over a system's own `libxdo.so.3`. That keeps the library the app was built against.
- The cost: the distro's security updates no longer reach these two copies. An OpenSSL fix reaches the tarball only with the next Yetack release, so a release follows each OpenSSL security release that affects TLS clients. libxdo only sends input events to the local X server, and the app gives it no outside data, so its risk is small. The `.deb` and the `.rpm` link the system's OpenSSL.
- Its `install.sh` copies the files to `~/.local` (or `--prefix`), writes the program's full path in the desktop entry, and checks with `ldd` that every library is there. For a missing one, it names the packages to install on the distro that `/etc/os-release` names. `install.sh --uninstall` removes the files.
- The app built for it says `tarball` in `YETACK_PACKAGE`. Its update card gives the two commands that unpack the new tarball and run its `install.sh`.
- The daemon beside the app (`yetack-x86_64-linux`) goes to the servers, as the Windows zip's does. `install.sh` also installs it as `yetack`, the program the background service and the command line run.

Why it came only now: the first colleagues all ran Ubuntu or Debian, and a `.deb` brings its libraries through `apt`, where a tarball leaves them to the user.

### 5.2 The app as an `.rpm`

`yetack-<version>-1.x86_64.rpm`, built by `cargo generate-rpm` in the same container.

- It holds the `.deb`'s files, and the tarball's `libxdo.so.3` in `/usr/lib/yetack`: Fedora 45 and openSUSE Tumbleweed have only `libxdo.so.4`. The app's `RUNPATH` is `$ORIGIN/../lib/yetack`.
- It links the system's OpenSSL 3. Fedora 45 has it in `openssl3-libs`, beside OpenSSL 4.
- Its requirements are the sonames of the libraries the app and libxdo link, and glibc 2.35. `dnf` and `zypper` resolve them to the distro's packages. The automatic requirements are off: they named Ubuntu's `libicu*.so.70`, which other distros do not have.
- The app built for it says `rpm`. Its update card gives the `dnf` command, and says to use `zypper` on openSUSE. The daemon's probe asks `rpm -qf` too, so a server whose daemon the `.rpm` installed gets the same steps.

### 5.3 A daemon for ARM servers

The release adds `yetack-<version>-aarch64-linux`, the static daemon built for `aarch64-unknown-linux-musl`. The app carries it beside the x86_64 one, and a server whose `uname -m` says `aarch64` gets it. The `.deb`, the `.rpm` and the tarball carry both: the `.deb` and the `.rpm` in `/usr/lib/yetack/yetack-aarch64-linux`, which their app names in `YETACK_LINUX_DAEMON_AARCH64`, and the tarball beside its app. The Windows zip carries the x86_64 one alone: its update replaces that one, and its ARM servers get the release's daemon by the download of 5.4. The Rust toolchain's own linker builds it in the release container, so the build needs no C cross-compiler.

### 5.4 The app downloads the daemon it does not carry

An app built from source, or one without the daemon for a server's CPU, downloads the release's daemon for that CPU and the release's `SHA256SUMS` from the repository's release downloads, as the app's own update does. It checks the checksum, then sends the file as it sends a carried one, and the server's script checks its version. A build that is not a release (its version has no tag, or the release lacks the file) builds on the server, as today.

This changes step 2's rule that the app never downloads a daemon. The app's own update already trusts the same release files, so the download adds no new party to trust.

### 5.5 Servers without systemd

The app never sets a start at boot up by itself. It proposes it, and it does it only once the user agrees.

1. When `install-daemon.sh` finds no systemd user session, it still installs the binary, and starts the daemon detached from the SSH session (`setsid`). The agents then keep working while the app is closed, until the server restarts.
2. The setup panel then proposes "Start the daemon when the server starts", which adds one `@reboot` line to the user's crontab. It shows that line, and adds it only on a click. A server without `crontab` gets the line to put in its own init system.
3. As a last resort, when the app finds a server's daemon installed but not running, and no service starts it, the app offers to start it over SSH, detached. A systemd or crontab start stays the normal way: it keeps the agents working while the app is offline, through a restart of the server too.

## Later

- An AppImage, if the tarball or the `.rpm` do not fit someone.
- The tarball's install updating itself, as the Windows zip does.
- Signing the Windows build, when there are Windows users besides the author.
- macOS: a build in CI, the `.app` bundle, notarization.
- A release built by CI on a tag, once the CI budget allows it.

## The build's base system

The colleagues' distros are recent, so the `.deb` builds in `ubuntu:22.04`. That covers Ubuntu 22.04 and newer, and Debian 12.
