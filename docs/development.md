# Development

Read [AGENTS.md](../AGENTS.md) first: it points to the plan, the architecture and the coding standard.

## Build and run

Everything runs inside the dev shell, which has the Rust toolchain and the WebKitGTK libraries:

```sh
nix develop
just          # lists the recipes
just run      # builds and runs the app
just check    # the tests, clippy and shellcheck
```

Without Nix, install stable Rust and the WebKitGTK 4.1 and GTK 3 development packages (on Ubuntu: `libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`), then `cargo run --release -p cockpit_desktop`.

The daemon and command line build with `cargo install --path crates/agents_cockpit`, which puts `agents-cockpit` in `~/.cargo/bin`.

Releases are built as [releasing.md](./releasing.md) says.

## Updating a server's daemon from source

An app and a daemon of different protocol versions refuse each other. When a server's daemon is older than the app, the app offers "Update" on its screen, or in the sidebar for a machine in `[[machines]]`. It runs [contrib/update-daemon.sh](../contrib/update-daemon.sh) on the server over SSH, for the commit the app was built from. The server needs Rust, git and a checkout of this repository. A Nix build of a tree with uncommitted changes has no commit, so it offers no update. A `cargo` build takes `HEAD`, whatever the tree holds.

You can also run the script by hand, from a checkout of this repository on your computer:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> < contrib/update-daemon.sh
```

`<path>` is the absolute path of `agents-cockpit` on the server. `--source-dir` names the server's checkout, by its absolute path, when it is not `~/agents-cockpit`. In the app's config, `source_dir` per machine does the same. The script builds outside the checkout, and refuses while an agent works, waits for an answer or may hold a scheduled job. Once started, it finishes even when the connection is lost. Running it with `--cancel` makes the update stop before it installs, once its build ends. Once the install has started, `--cancel` says it is too late and exits with 1.

Its exit status is:

- 0 when the daemon runs the new version;
- 1 when it failed before anything changed;
- 3 when it refused because of agents;
- 4 when the install or the restart failed;
- 5 when the new daemon did not start and the previous version runs again;
- 6 when another update is running.

The server needs a way to restart the daemon: the `agents-cockpit` systemd user service, or a shell script at `~/.config/agents-cockpit/start-daemon` that starts the daemon in the background and returns. [contrib/agents-cockpit.service](../contrib/agents-cockpit.service) is an example of the service, and [contrib/start-daemon](../contrib/start-daemon) an example of the script, which starts the daemon in tmux.
