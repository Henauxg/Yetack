# Troubleshooting

The commands below use `devbox` for the server: replace it with your server's name in your SSH config.

## The app cannot reach the server

The app runs `ssh devbox` without a terminal, so it cannot type a password or answer a question.

1. On your computer, run `ssh devbox true`. It must end at once, with no password prompt and no question about the host key. If it asks something, set up a key (`ssh-copy-id devbox`) and answer the host key question once.
2. In the app's Machines dialog, the path of `yetack` must be the full path on the server, such as `/home/<you>/.local/bin/yetack`. Check it with `ssh devbox ls -l /home/<you>/.local/bin/yetack`.

## The service does not start

On the server, `journalctl --user -u yetack -e` shows why.

- `status=203/EXEC` means systemd cannot run the binary: it is missing, or not executable. Run `chmod +x ~/.local/bin/yetack`, then `systemctl --user restart yetack`.
- If `yetack` is somewhere else than `~/.local/bin`, change `ExecStart` in `~/.config/systemd/user/yetack.service`, then `systemctl --user daemon-reload`.

## The server's agents do not start, and the daemon cannot find `claude`

The service's `PATH` is in `~/.config/systemd/user/yetack.service`. Add the folder that holds `claude` (run `command -v claude` on the server), then:

```sh
systemctl --user daemon-reload
systemctl --user restart yetack
```

## The agents stop when you log out of the server

Linger is off, so systemd stops your services when your last session on the server ends. On the server, run `loginctl enable-linger "$USER"`. On some servers, it needs `sudo`.

## A machine's row says "Runs an older version" or "Runs a newer version"

The app and the machine's daemon come from different releases and do not speak the same protocol. Update the older one: the daemon with the row's Update link, or the app. When Nix, a package or another folder holds the daemon, the link reads "Details", and its card says how to update the daemon. [servers.md](./servers.md) says how to update a server by hand.

## A machine's row says "Update available." or "Runs another build."

The machine's daemon was built from another version than the app, and the app still uses it. "Runs another build." means the same release from another commit: the Machines dialog names both commits, and the app cannot tell which one is newer. The Update link puts the app's build in its place. When agents of the machine are busy, the app names them first, and the update can wait until they are idle.

## The app cannot stop this computer's background service

A background service started by an older version of the app does not record its process, so the app cannot tell which process to stop. Stop it yourself once, then retry the update:

1. Find it: `pgrep -af -- '--state-dir'`. It runs with `--state-dir`, your state folder (`~/.local/state/yetack` by default), and `serve`.
2. Stop it: `kill <pid>`. It stops as a service does, and the next daemon resumes its agents.
3. Click Retry in the app.

The daemon the app starts records its process, so the next update stops it by itself.

## The update says another build still answers

The app stopped the daemon and started its own version, but another program started the old daemon again, such as a systemd unit that restarts it whenever it stops. Stop that program, or point it at the app's version, then retry. For the systemd unit: `systemctl --user disable --now yetack`.

## A terminal starts the wrong shell

A terminal runs the shell your account logs in with, on the machine of the agent. On Windows, it runs PowerShell 7, or Windows PowerShell when PowerShell 7 is not installed. The ⌄ beside the "+" of the terminal's tabs opens a terminal with another shell of that machine. To change the default one, set `YETACK_SHELL` to its full path in the daemon's environment, then restart the daemon. On Windows, the daemon runs in the app: set it in your user's environment (`setx YETACK_SHELL <path>`), then start the app again. For the background service on Linux, put `Environment=YETACK_SHELL=/run/current-system/sw/bin/zsh` in a drop-in file (`systemctl --user edit yetack`).
