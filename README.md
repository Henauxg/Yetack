# Yetack (Yet another agents cockpit)

Orchestrator to control multiple agents over multiple machines and accounts.

| ![The app with agents on three machines: the sidebar lists the agents by machine, the middle shows one agent's conversation with a reply under way that quotes three passages, each with its comment beside the text, and the right column shows the agent's tasks, subagents and messages](docs/images/machines.png) | ![The app with the agents grouped by project: one agent asks three questions, the machine's load is open under the header with each agent's cores and memory, the file view shows the changelog that the agent edited, and the files pane lists the agent's folder](docs/images/projects.png) |
| --- | --- |

Features:

- Multiple machines (local & remotes)
- Multiple accounts (work, personal)
- Persistent sessions via daemons
- Steering, questions, plans, approvals, MCPs, notifications
- File browsing, diffs, machine load
- Projects grouping
- Quote & reply intuitively
- Messages timeline
- Subagents views and map

## Install

Download the files from the [releases page](https://github.com/Henauxg/yetack/releases).

| Your computer | File | Command |
| --- | --- | --- |
| Ubuntu 22.04 or newer, Debian 12 | `yetack_<version>_amd64.deb` | `sudo apt install ./yetack_<version>_amd64.deb` |
| Fedora, openSUSE | `yetack-<version>-1.x86_64.rpm` | `sudo dnf install ./yetack-<version>-1.x86_64.rpm`, or `sudo zypper install` on openSUSE |
| Any other Linux x86_64: Gentoo, Arch... | `yetack-desktop-<version>-x86_64-linux.tar.gz` | See [Any other Linux](#any-other-linux) |
| Windows 10 or 11 | `yetack-desktop-<version>-windows-x64.zip`, if the release has one | Unzip the file and run `yetack-desktop.exe` |
| NixOS, or Linux with Nix | None | See [With Nix](#with-nix) |
| macOS, or Linux on ARM | None | Build the app: [docs/install-from-source.md](docs/install-from-source.md) |

The `.deb` and the `.rpm` install the app (`yetack-desktop`) and the daemon with its command line (`yetack`).

The app tells you when a new release is out, at the foot of its sidebar. The Windows app updates itself. The other installs get the steps. Settings › General › About turns the daily check off.

Nobody has run the app on macOS yet.

Each machine that runs agents also needs Claude Code. Install [Claude Code](https://claude.com/claude-code), then run `claude` once and sign in.

### Any other Linux

The tarball holds the app and the daemon, built for any Linux x86_64 with glibc 2.35 or newer. The app draws with your system's WebKitGTK 4.1, so install it first with your packages. On Gentoo, for example:

```sh
sudo emerge --ask --noreplace net-libs/webkit-gtk:4.1
```

Then unpack the tarball and run its `install.sh`, which copies the app, the daemon and the menu entry to `~/.local`:

```sh
tar xzf yetack-desktop-<version>-x86_64-linux.tar.gz
./yetack-desktop-<version>-x86_64-linux/install.sh
```

The script then checks that the app finds every library it needs. When one is missing, it names it, with the command that installs it on your distribution. `install.sh --prefix <folder>` installs elsewhere, and `install.sh --uninstall` removes the files. For a new release, do the same with the new tarball.

### With Nix

The repository is a Nix flake. Nix builds the app from the release's source on your computer, which takes a few minutes. The app, its menu entry and its icon then come with your Nix setup.

To try the app, add it to your user profile. If your Nix does not know `nix profile add`, write `install` in place of `add`.

```sh
nix profile add 'github:Henauxg/yetack/v<version>'
```

The profile keeps the version you added. For a new release, remove the app with `nix profile remove yetack`, then add the new version.

To install the app with the rest of your NixOS configuration, add the flake as an input of your system's flake:

```nix
# In your system's flake.nix
inputs.yetack.url = "github:Henauxg/yetack/v<version>";

# In your NixOS configuration
environment.systemPackages = [ inputs.yetack.packages.${pkgs.system}.default ];
```

For a new release, change the version in the input, run `nix flake update yetack`, and rebuild your system. A home-manager configuration takes the same package in `home.packages`.

## Local or remote agents

### Locally

Start the app. At the first start, the app asks "Where should agents run?":

- **Run agents in this app**: the agents stop when the app closes, and come back at the next start. On Windows, this choice is the only one.
- **Start the background service here** (Linux): the agents keep running after the app closes.

The box "Use this every time" is ticked, so the app keeps your choice. To get the question again at the next start, untick the box. On Linux, you can change the choice later in Settings (the gear at the foot of the sidebar), under Local: "In this app", "Background service", or "Ask", which asks again at a start where no service answers.

When the background service runs an older version than the app, the Machines dialog offers **Update**, which restarts the service with the app's own version. Before an update, the app names the agents a restart would interrupt, and can wait until they are idle.

### On a server over SSH

The server needs Linux on x86_64 or ARM (aarch64), SSH access with a key, and Claude Code signed in. For another CPU, build the daemon as [docs/install-from-source.md](docs/install-from-source.md) says. Once you add the server, its agents show in their own panel in the sidebar.

#### Install from the app

The app can install the daemon for you:

1. In the app, open **Machines** at the bottom left, and click **Add a server**.
2. Type the **SSH host**, as you type it after `ssh`, and leave **Path of yetack on the server** empty.
3. The app looks for `yetack` on the server, and adds the server with the one it finds. When the server has none, the app offers to install it, as a systemd user service when the server has a systemd user session.
4. Once the daemon runs, the app says what is left to do on the server: turn linger on (see [Keep the agents running after you log out](#keep-the-agents-running-after-you-log-out)), or install Claude Code.

On a server without systemd, such as Gentoo with OpenRC, the install starts the daemon on its own, apart from your SSH session, so it keeps running after you log out. Nothing starts it again after the server restarts: the app shows the line it can add to your crontab for that, and adds it only when you click **Add it to my crontab**.

#### Install by hand

From the releases page, download `yetack-<version>-x86_64-linux` (`yetack-<version>-aarch64-linux` for an ARM server) and `yetack.service`. On Ubuntu and Debian, the `.deb` also put the daemon at `/usr/bin/yetack`.

1. On your computer, copy the two files to the server. Replace `devbox` with the server's name in your SSH config:

   ```sh
   ssh devbox 'mkdir -p ~/.local/bin ~/.config/systemd/user'
   scp yetack-<version>-x86_64-linux devbox:.local/bin/yetack
   scp yetack.service devbox:.config/systemd/user/
   ssh devbox 'chmod +x ~/.local/bin/yetack'
   ```

2. On the server, start the daemon as a service:

   ```sh
   systemctl --user daemon-reload
   systemctl --user enable --now yetack
   systemctl --user status yetack
   ```

   Then turn linger on, as [Keep the agents running after you log out](#keep-the-agents-running-after-you-log-out) says.

3. In the app, open **Machines** at the bottom left, and add the server:
   - **SSH host**: `devbox`, as you type it after `ssh`.
   - **Path of yetack on the server**: `/home/<you>/.local/bin/yetack`.

#### Keep the agents running after you log out

Without linger, systemd stops your services when your last session on the server ends, and the agents stop with the services. Turn it on once, on the server:

```sh
loginctl enable-linger "$USER"
```

On some servers, this command needs `sudo`. After an install, the app checks linger and says when it is off.

#### Update the daemon

The app and the daemon must speak the same protocol. If the protocols differ, the server's row in the sidebar says "Runs an older version".

The app's "Update" button sends the app's own daemon to the server, checks it there, then restarts the daemon. The server needs no Rust and no checkout of this repository. The update runs beside the window, and the sidebar shows its progress under the server.

The button replaces only a daemon in `~/.local/share/yetack/bin` or `~/.local/bin`. A daemon from Nix or from a package is updated by its package manager, and the app gives the steps. The app sends the daemon for the server's CPU, x86_64 or ARM: from its own files, or downloaded from the release when the app is that release's build. Otherwise, such as for an app built from source without that daemon, or a server with another CPU, the daemon is built on the server. That server needs Rust and a checkout of this repository, as [docs/development.md](docs/development.md) says.

Before an update, the app names the agents a restart would interrupt, and can wait until they are idle.

To update the daemon by hand, copy the new daemon over the old one, then restart the service. The restart stops the agents, and the daemon resumes the agents when the daemon starts again. Restart while the agents are idle:

```sh
scp yetack-<version>-x86_64-linux devbox:.local/bin/yetack.new
ssh devbox 'chmod +x ~/.local/bin/yetack.new && mv ~/.local/bin/yetack.new ~/.local/bin/yetack && systemctl --user restart yetack'
```

## When something goes wrong

See [docs/troubleshooting.md](docs/troubleshooting.md).

## How it works

### Claude Code

The daemon runs the official `claude` program without its terminal interface: the daemon writes JSON messages to the program's standard input and reads JSON messages from its standard output. Claude Code runs unmodified and signs in with your own account. The app never sees your credentials.

The app reaches a server with your SSH key, and the daemon opens no network port.

## License

Yetack is under the [Mozilla Public License 2.0](LICENSE). [CONTRIBUTING.md](CONTRIBUTING.md) says how contributions are licensed. To build Yetack or work on it, read [docs/development.md](docs/development.md).
