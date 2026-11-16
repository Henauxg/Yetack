# Development

Read [AGENTS.md](../AGENTS.md) first: it says how to develop, and points to the architecture and the coding standard.

## Build and run

Everything runs inside the dev shell, which has stable Rust, nightly rustfmt, clippy, just, shellcheck and the WebKitGTK libraries:

```sh
nix develop
just          # lists the recipes
just run      # builds and runs the app
just check    # the tests and clippy as CI runs them, and shellcheck on the scripts in contrib/
just fmt      # formats the code
```

The recipes run cargo with quiet flags, so the output stays short.
For a quick build without the tests, run `cargo check -q --workspace --all-targets`.
The coding standard's settings apply on their own: the workspace lints and `.cargo/config.toml` set clippy, and the root `rustfmt.toml` sets rustfmt, which must be the dev shell's nightly one.

Without Nix, install the packages that [install-from-source.md](./install-from-source.md) lists, then `cargo run --release -p cockpit_desktop`.

Releases are built as [releasing.md](./releasing.md) says.

## CI

CI (`.github/workflows/ci.yml`) runs on each push to `main` and on each pull request.
It runs plain cargo, without Nix, on Linux and on Windows: it checks, lints and tests the workspace, and keeps the Windows desktop app's release build as an artifact.
Its Windows job is the only real Windows test.
To run it on another branch, use `gh workflow run ci.yml --ref <branch>` or the Actions tab.

## Checking Unix code from Windows

The daemon crates have no C dependencies, so their Linux tests build on Windows and run in WSL.

1. Add the target once: `rustup target add x86_64-unknown-linux-musl`.
2. Build the test binaries with `rust-lld` as the linker:

   ```sh
   CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld CARGO_TARGET_DIR=target/musl \
     cargo test --no-run --target x86_64-unknown-linux-musl \
     -p cockpit_daemon -p yetack -p cockpit_client -p cockpit_protocol
   ```

   `cockpit_desktop` does not build this way, because WebKitGTK needs the Linux libraries.
3. Run the test binaries in WSL.

The tests find their programs and scripts through `env!`, which gives Windows paths that Linux reads as relative names.
For the run, replace those paths in the test files with absolute Linux paths, such as `BINARY` and `SCRIPT` in `update_daemon.rs` or the fake `claude` in `e2e.rs`, and don't commit that change.
The `update_daemon` tests fail in about 4 of 10 full runs in WSL, on `main` too, because a start or a stop of the daemon times out over `/mnt/c`.
Run a failing test again on its own.

## Driving the app on Windows

A script can see and act on the demo's real page:

1. Build, then start the demo with its own WebView2 data folder and a debugging port: `WEBVIEW2_USER_DATA_FOLDER=<a scratch folder> WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333 target/debug/yetack-desktop.exe --demo`.
   The data folder matters when your own app runs: WebView2 keeps one browser per data folder, and that browser keeps the arguments it started with, so the port would not open.
2. Drive it over the Chrome DevTools Protocol from Node 22, which has `fetch` and `WebSocket` built in.
   `http://127.0.0.1:9333/json` lists the page. Then use `Runtime.evaluate`, `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`, `Input.insertText` and `Page.captureScreenshot`.
   To select text, use `document.createRange` and `getSelection().addRange`.
3. Stop the demo before the next build, because Windows locks a running program's file.

The window shows on the screen meanwhile.

## Claude Code cloud sessions

- `nix develop` fails there, because it cannot download its Rust overlay. Install the desktop's libraries with apt instead: `libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`.
- Keep the Rust toolchain as new as CI's (`rustup update stable`): a newer clippy in CI can fail a build that passes there.
- To check Windows code, run `rustup target add x86_64-pc-windows-msvc x86_64-pc-windows-gnu`. `cargo clippy --target x86_64-pc-windows-msvc` checks the `cfg(windows)` code, and the tests for the GNU target run under Wine (`apt-get install wine mingw-w64`).
  Wine has no git, cannot open the app's window, show toasts or read symbolic links back. The desktop's GNU test binary needs `WebView2Loader.dll` next to it.
- The disk allowance is small. Use `CARGO_INCREMENTAL=0`, delete `target/*/incremental` and old targets, and never copy the app's binary around.
- To check the screen, run the demo (`yetack-desktop --demo`) under Xvfb with openbox, wmctrl and xdotool, and take screenshots with `import -window root`. Close the window with wmctrl, because xdotool's close aborts GTK.
- An agent started with `isolation: "worktree"` gets a worktree of `main`, so its first step is to check out the working branch.
- Deleting a branch on GitHub returns 403 from these sessions, so the user deletes old branches.

## Updating a server's daemon

[README.md](../README.md), "Update the daemon", says what the app's "Update" button does for the user. The button runs [contrib/update-daemon.sh](../contrib/update-daemon.sh) on the server over SSH, for the commit the app was built from. When the app has the daemon for the server's CPU (x86_64 or aarch64), carried beside it or downloaded from its release, the update sends it, and the script installs it. Otherwise the script builds the daemon on the server, which then needs Rust, git and a checkout of this repository. A Nix build of a tree with uncommitted changes has no commit, so it offers no update. A `cargo` build takes `HEAD`, whatever the tree holds.

A build from source carries a daemon once `just linux-daemon` has run. The recipe cross-builds the static daemon for `x86_64-unknown-linux-musl` and copies it as `yetack-x86_64-linux` into `target/debug` and `target/release`, beside the app. The dev shell has the target. Outside it, add the target once with `rustup target add x86_64-unknown-linux-musl`. Run the recipe again after each commit: the script checks that the daemon prints the app's release and commit, and refuses an older file.

You can also run the script by hand, from a checkout of this repository on your computer:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> < contrib/update-daemon.sh
```

`<path>` is the absolute path of `yetack` on the server. `--source-dir` names the server's checkout, by its absolute path, when it is not `~/yetack`. In the app's config, `source_dir` per machine does the same. The script builds outside the checkout, and refuses while an agent works, waits for an answer or may hold a scheduled job. Once started, it finishes even when the connection is lost. Running it with `--cancel` makes the update stop before it installs, once its build ends. Once the install has started, `--cancel` says it is too late and exits with 1.

To install a daemon that is already on the server in place of a build, add `--built`:

```sh
ssh <host> sh -s -- --commit <commit> --binary <path> --built <file> --sha256 <sum> --version <text> < contrib/update-daemon.sh
```

Before anything changes, the script checks that `sha256sum <file>` gives `<sum>` and that `<file> --version` prints `yetack <text>`, and exits with 1 otherwise. `yetack --version` prints `yetack <release> (<commit>)`, so `<text>` is `<release> (<commit>)` with the full commit. The script deletes `<file>` when it ends.

Its exit status is:

- 0 when the daemon runs the new version;
- 1 when it failed before anything changed;
- 3 when it refused because of agents;
- 4 when the install or the restart failed;
- 5 when the new daemon did not start and the previous version runs again;
- 6 when another update is running.

The server needs a way to restart the daemon: the `yetack` systemd user service, or a shell script at `~/.config/yetack/start-daemon` that starts the daemon in the background and returns. [contrib/yetack.service](../contrib/yetack.service) is an example of the service, and [contrib/start-daemon](../contrib/start-daemon) an example of the script, which starts the daemon in tmux.

"Add a server" installs a daemon with [contrib/install-daemon.sh](../contrib/install-daemon.sh), the same way: `ssh <host> sh -s -- --built <file> --sha256 <sum> --version <text> < contrib/install-daemon.sh`. It checks the file as the update script does, installs `~/.local/share/yetack/bin/yetack`, writes and starts the `yetack` systemd user service, then prints whether linger is on and where the service finds `claude`. With `--check`, it prints only those two lines. It exits with 0 when the daemon runs, 1 when it failed before anything changed (a server without a systemd user session among them), and 4 when the install or the start failed.
