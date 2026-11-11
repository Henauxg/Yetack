# agents-cockpit

Run Claude Code agents on your computer and on your servers, and watch and steer them all from one desktop app.

![The app with agents on three machines: the sidebar lists them by machine, the middle shows one agent's conversation with a reply under way, the right column its tasks, subagents and messages](docs/images/app.png)

- **Several machines.** The app connects to this computer and to any number of servers over SSH. Each machine has its own panel in the sidebar.
- **Several Claude accounts.** A personal and a work subscription work side by side. You pick the account of each agent, and the app shows the plan usage of each account.
- **Sessions that persist on remote machines.** The agents run in a daemon on the server, so they keep working when the app closes, the laptop sleeps or the connection drops. When the app reconnects, it shows what the agents did meanwhile.
- **Steer while they work.** Answer approvals, questions and plans from the app. A message you send while an agent works reaches Claude at its next step.
- **Reply to a passage.** Select text in the chat to quote it, and write a comment beside it. Paste images into a message.
- **See what they do.** A map shows the subagents, and a block beside the chat shows the background tasks. The files pane shows the files each agent changed or read, with their diffs.
- **Notifications.** The app tells you when an agent needs you or finishes.

Claude Code runs unmodified, signed in with your own account. The app never sees your credentials. It reaches a server with your SSH key, and the daemon opens no network port.

## Install

Download the files from the [releases page](https://github.com/Henauxg/agents-cockpit/releases).

| Your computer | File | How |
| --- | --- | --- |
| Ubuntu 22.04 or newer, Debian 12 | `agents-cockpit_<version>_amd64.deb` | `sudo apt install ./agents-cockpit_<version>_amd64.deb` |
| Windows 10 or 11 | `agents-cockpit-desktop-<version>-windows-x64.zip`, when the release has one | Unzip it anywhere and run `agents-cockpit-desktop.exe` |
| NixOS, or Linux with Nix | | `nix profile install github:Henauxg/agents-cockpit` |
| Anything else | | Build it: [docs/install-from-source.md](docs/install-from-source.md) |

The `.deb` installs the app (`agents-cockpit-desktop`) and the daemon and command line (`agents-cockpit`).

You also need Claude Code on each machine where agents run. Install [Claude Code](https://claude.com/claude-code), then run `claude` once and sign in.

## Local or remote agents

### On this computer

Start the app. The first time, it shows "Where should agents run?":

- **Run agents in this app**: the agents stop when the app closes, and come back the next time it runs them. This is the only choice on Windows.
- **Start the background service here** (Linux): the agents keep running after the app closes.

"Use this every time" is ticked, so the app keeps your choice. Untick it to be asked again at the next start.

### On a server over SSH

The server needs Linux on x86_64 (for another CPU, build the daemon: [docs/install-from-source.md](docs/install-from-source.md)), SSH access with a key, and Claude Code signed in. You need two files from the release page: `agents-cockpit-<version>-x86_64-linux` and `agents-cockpit.service`. On Linux, the `.deb` also put the daemon at `/usr/bin/agents-cockpit`.

1. From your computer, copy them to the server. Replace `devbox` with the server's name in your SSH config:

   ```sh
   ssh devbox 'mkdir -p ~/.local/bin ~/.config/systemd/user'
   scp agents-cockpit-<version>-x86_64-linux devbox:.local/bin/agents-cockpit
   scp agents-cockpit.service devbox:.config/systemd/user/
   ssh devbox 'chmod +x ~/.local/bin/agents-cockpit'
   ```

2. On the server, start the daemon as a service, and let it run after you log out:

   ```sh
   systemctl --user daemon-reload
   systemctl --user enable --now agents-cockpit
   loginctl enable-linger "$USER"
   systemctl --user status agents-cockpit
   ```

   Without linger, systemd stops your services when your last session on the server ends, and the agents stop with them. On some servers `loginctl enable-linger` needs `sudo`.

3. In the app, open **Machines** (bottom left), and add the server:
   - **SSH host**: `devbox`, as you would type it after `ssh`.
   - **Path of agents-cockpit on the server**: `/home/<you>/.local/bin/agents-cockpit`.

The server's agents show in a panel of their own in the sidebar.

### Update a server

The app and the daemon must speak the same protocol. When they don't, the server's row says "Runs an older version".

Update it by copying the new daemon over the old one, then restarting the service. The restart stops the agents, and the daemon resumes them when it starts again, so do it while they are idle:

```sh
scp agents-cockpit-<version>-x86_64-linux devbox:.local/bin/agents-cockpit.new
ssh devbox 'chmod +x ~/.local/bin/agents-cockpit.new && mv ~/.local/bin/agents-cockpit.new ~/.local/bin/agents-cockpit && systemctl --user restart agents-cockpit'
```

The app's "Update" button builds the daemon from source on the server, so it works only on a server with Rust and a checkout of this repository: [docs/development.md](docs/development.md).

## When something goes wrong

[docs/troubleshooting.md](docs/troubleshooting.md) covers an app that cannot reach a server, a service that does not start, a `claude` the daemon cannot find, agents that stop when you log out, and a server that runs another version.

## License

agents-cockpit is licensed under the [Mozilla Public License 2.0](LICENSE). [CONTRIBUTING.md](CONTRIBUTING.md) says how contributions are licensed. To build it or work on it, read [docs/development.md](docs/development.md).
