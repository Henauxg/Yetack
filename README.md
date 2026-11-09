# agents-cockpit

Run several Claude Code agents on a server, and watch and steer them from a desktop app: their conversations, tool calls, approvals, questions and plans.

- The agents run in a daemon on the server, so they keep working when the app disconnects.
- The app reaches the daemon over SSH, or directly on the same machine. The daemon opens no network port.
- Claude Code runs unmodified, in its JSON mode, signed in with your own account.

Status: early development. See [docs/plan.md](docs/plan.md) and [docs/architecture.md](docs/architecture.md).

## Updating a server's daemon

An app and a daemon of different protocol versions refuse each other. This updates a server's daemon to one commit, from a checkout of this repository on your computer:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> < contrib/update-daemon.sh
```

`<path>` is the absolute path of `agents-cockpit` on the server, and `--source-dir` names the server's checkout when it is not `~/agents-cockpit`. The script builds outside the checkout, and refuses while an agent works, waits for an answer or may hold a scheduled job.

The server needs a way to restart the daemon: the `agents-cockpit` systemd user service, or a shell script at `~/.config/agents-cockpit/start-daemon` that starts the daemon in the background and returns.

## Development

```sh
nix develop
cargo test -q --workspace
```
