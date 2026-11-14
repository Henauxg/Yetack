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

Without Nix, install the packages that [install-from-source.md](./install-from-source.md) lists, then `cargo run --release -p cockpit_desktop`.

Releases are built as [releasing.md](./releasing.md) says.

## Updating a server's daemon

An app and a daemon of different protocol versions refuse each other. When a server's daemon is older than the app, the app offers "Update" on its screen, or in the sidebar for a machine in `[[machines]]`. It runs [contrib/update-daemon.sh](../contrib/update-daemon.sh) on the server over SSH, for the commit the app was built from. When the app carries a Linux daemon, the update sends it to a Linux x86_64 server, and the script installs it. Otherwise the script builds the daemon on the server, which then needs Rust, git and a checkout of this repository. A Nix build of a tree with uncommitted changes has no commit, so it offers no update. A `cargo` build takes `HEAD`, whatever the tree holds. The app replaces only a daemon in `~/.local/share/agents-cockpit/bin` or `~/.local/bin`. For a daemon from Nix, a package or another folder, its update card says how to update it by hand.

A build from source carries a daemon once `just linux-daemon` has run. The recipe cross-builds the static daemon for `x86_64-unknown-linux-musl` and copies it as `agents-cockpit-x86_64-linux` into `target/debug` and `target/release`, beside the app. The dev shell has the target. Outside it, add the target once with `rustup target add x86_64-unknown-linux-musl`. Run the recipe again after each commit: the script checks that the daemon prints the app's release and commit, and refuses an older file.

You can also run the script by hand, from a checkout of this repository on your computer:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> < contrib/update-daemon.sh
```

`<path>` is the absolute path of `agents-cockpit` on the server. `--source-dir` names the server's checkout, by its absolute path, when it is not `~/agents-cockpit`. In the app's config, `source_dir` per machine does the same. The script builds outside the checkout, and refuses while an agent works, waits for an answer or may hold a scheduled job. Once started, it finishes even when the connection is lost. Running it with `--cancel` makes the update stop before it installs, once its build ends. Once the install has started, `--cancel` says it is too late and exits with 1.

To install a daemon that is already on the server in place of a build, add `--built`:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> --built <file> --sha256 <sum> --version <text> < contrib/update-daemon.sh
```

Before anything changes, the script checks that `sha256sum <file>` gives `<sum>` and that `<file> --version` prints `agents-cockpit <text>`, and exits with 1 otherwise. `agents-cockpit --version` prints `agents-cockpit <release> (<commit>)`, so `<text>` is `<release> (<commit>)` with the full commit. The script deletes `<file>` when it ends.

Its exit status is:

- 0 when the daemon runs the new version;
- 1 when it failed before anything changed;
- 3 when it refused because of agents;
- 4 when the install or the restart failed;
- 5 when the new daemon did not start and the previous version runs again;
- 6 when another update is running.

The server needs a way to restart the daemon: the `agents-cockpit` systemd user service, or a shell script at `~/.config/agents-cockpit/start-daemon` that starts the daemon in the background and returns. [contrib/agents-cockpit.service](../contrib/agents-cockpit.service) is an example of the service, and [contrib/start-daemon](../contrib/start-daemon) an example of the script, which starts the daemon in tmux.

"Add a server" installs a daemon with [contrib/install-daemon.sh](../contrib/install-daemon.sh), the same way: `ssh <host> sh -s -- --built <file> --sha256 <sum> --version <text> < contrib/install-daemon.sh`. It checks the file as the update script does, installs `~/.local/share/agents-cockpit/bin/agents-cockpit`, writes and starts the `agents-cockpit` systemd user service, then prints whether linger is on and where the service finds `claude`. With `--check`, it prints only those two lines. It exits with 0 when the daemon runs, 1 when it failed before anything changed (a server without a systemd user session among them), and 4 when the install or the start failed.
