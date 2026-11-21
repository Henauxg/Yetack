# Servers

The app installs and updates a server's daemon by itself: see "Add a server" in the [README](../README.md). This page has the details, and the same steps without the app.

## What a server needs

Linux on x86_64 or ARM (aarch64), SSH access with a key, and Claude Code signed in. For another CPU, build the daemon as [install-from-source.md](./install-from-source.md) says.

## In the config file

The app keeps its servers in its config file: `~/.config/yetack/config.toml` (`$XDG_CONFIG_HOME/yetack/config.toml`), or `%APPDATA%\yetack\config.toml` on Windows. The Machines dialog writes them there, and you can write them yourself, one `[[machines]]` table per server:

```toml
[[machines]]
name = "devbox"                                  # what the sidebar calls it
host = "devbox"                                  # as you type it after `ssh`
remote_command = "/home/you/.local/bin/yetack"   # the daemon's absolute path on the server
```

The daemon must already be installed there. The app reads the file when it starts. A config file that is a symbolic link, as from home-manager or a dotfiles repository, stays one when the app saves it, and a read-only one, such as a link into the Nix store, is never written.

## Servers without systemd

On a server without systemd, such as Gentoo with OpenRC, the app's install starts the daemon on its own, apart from your SSH session, so it keeps running after you log out. Nothing starts it again after the server restarts: the app shows the line it can add to your crontab for that, and adds it only when you click **Add it to my crontab**.

## Keep the agents running after you log out

Without linger, systemd stops your services when your last session on the server ends, and the agents stop with the services. Turn it on once, on the server:

```sh
loginctl enable-linger "$USER"
```

On some servers, this command needs `sudo`. After an install, the app checks linger and says when it is off.

## Updating the daemon

The app and the daemon must speak the same protocol. If the protocols differ, the machine's row in the sidebar says "Runs an older version".

For a server, the app's "Update" button sends the app's own daemon to the server, checks it there, then restarts the daemon. The server needs no Rust and no checkout of this repository. The update runs beside the window, and the sidebar shows its progress under the server.

The button replaces only a daemon in `~/.local/share/yetack/bin` or `~/.local/bin`. A daemon from Nix or from a package is updated by its package manager, and the app gives the steps. The app sends the daemon for the server's CPU, x86_64 or ARM: from its own files, or downloaded from the release when the app is that release's build. Otherwise, such as for an app built from source without that daemon, or a server with another CPU, the daemon is built on the server. That server needs Rust and a checkout of this repository, as [development.md](./development.md) says.

For the background service on this computer, the Machines dialog offers **Update**, which restarts the service with the app's own version.

Before an update, the app names the agents a restart would interrupt, and can wait until they are idle.

## Install by hand

From the [releases page](https://github.com/Henauxg/yetack/releases), download `yetack-<version>-x86_64-linux` (`yetack-<version>-aarch64-linux` for an ARM server) and `yetack.service`. On Ubuntu and Debian, the `.deb` also put the daemon at `/usr/bin/yetack`.

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

## Update by hand

Copy the new daemon over the old one, then restart the service. The restart stops the agents, and the daemon resumes the agents when the daemon starts again. Restart while the agents are idle:

```sh
scp yetack-<version>-x86_64-linux devbox:.local/bin/yetack.new
ssh devbox 'chmod +x ~/.local/bin/yetack.new && mv ~/.local/bin/yetack.new ~/.local/bin/yetack && systemctl --user restart yetack'
```
