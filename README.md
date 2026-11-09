# agents-cockpit

Run several Claude Code agents on a server, and watch and steer them from a desktop app: their conversations, tool calls, approvals, questions and plans.

- The agents run in a daemon on the server, so they keep working when the app disconnects.
- The app reaches the daemon over SSH, or directly on the same machine. The daemon opens no network port.
- Claude Code runs unmodified, in its JSON mode, signed in with your own account.

Status: early development. See [docs/plan.md](docs/plan.md) and [docs/architecture.md](docs/architecture.md).

## Updating a server's daemon

An app and a daemon of different protocol versions refuse each other. When a server's daemon is older than the app, the app offers "Update" on its screen, or in the sidebar for a machine in `[[machines]]`. It runs `contrib/update-daemon.sh` on the server over SSH, for the commit the app was built from. A Nix build of a tree with uncommitted changes has no commit, so it offers no update. A `cargo` build takes `HEAD`, whatever the tree holds.

You can also run the script by hand, from a checkout of this repository on your computer:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> < contrib/update-daemon.sh
```

`<path>` is the absolute path of `agents-cockpit` on the server. `--source-dir` names the server's checkout, by its absolute path, when it is not `~/agents-cockpit`; in the app's config, `source_dir` per machine does the same. The script builds outside the checkout, and refuses while an agent works, waits for an answer or may hold a scheduled job. Once started it finishes even when the connection is lost. Running it with `--cancel` makes the update stop before it installs, once its build ends. Once the install has started, `--cancel` says it is too late and exits with 1.

Its exit status is 0 when the daemon runs the new version, 1 when it failed before anything changed, 3 when it refused because of agents, and 4 when the install or the restart failed.

The server needs a way to restart the daemon: the `agents-cockpit` systemd user service, or a shell script at `~/.config/agents-cockpit/start-daemon` that starts the daemon in the background and returns.

## Development

```sh
nix develop
just          # lists the recipes
just run      # builds the daemon program and the app, then runs the app
just check    # what CI runs
```
