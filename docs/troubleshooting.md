# Troubleshooting

The commands below use `devbox` for the server: replace it with your server's name in your SSH config.

## The app cannot reach the server

The app runs `ssh devbox` without a terminal, so it cannot type a password or answer a question.

1. On your computer, run `ssh devbox true`. It must end at once, with no password prompt and no question about the host key. If it asks something, set up a key (`ssh-copy-id devbox`) and answer the host key question once.
2. In the app's Machines dialog, the path of `agents-cockpit` must be the full path on the server, such as `/home/<you>/.local/bin/agents-cockpit`. Check it with `ssh devbox ls -l /home/<you>/.local/bin/agents-cockpit`.

## The service does not start

On the server, `journalctl --user -u agents-cockpit -e` shows why.

- `status=203/EXEC` means systemd cannot run the binary: it is missing, or not executable. Run `chmod +x ~/.local/bin/agents-cockpit`, then `systemctl --user restart agents-cockpit`.
- If `agents-cockpit` is somewhere else than `~/.local/bin`, change `ExecStart` in `~/.config/systemd/user/agents-cockpit.service`, then `systemctl --user daemon-reload`.

## The server's agents do not start, and the daemon cannot find `claude`

The service's `PATH` is in `~/.config/systemd/user/agents-cockpit.service`. Add the folder that holds `claude` (run `command -v claude` on the server), then:

```sh
systemctl --user daemon-reload
systemctl --user restart agents-cockpit
```

## The agents stop when you log out of the server

Linger is off, so systemd stops your services when your last session on the server ends. On the server, run `loginctl enable-linger "$USER"` (some servers want `sudo` before it).

## A machine's row says "Runs an older version" or "Runs a newer version"

The app and the server's daemon come from different releases and do not speak the same protocol. Update the older one, as the "Update a server" section of [README.md](../README.md) says.
