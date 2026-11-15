# Handoff

The state of the work, for the next agent or session.
Read AGENTS.md, [plan.md](plan.md) and [architecture.md](architecture.md) first: they describe what the code does.
This file says where the work stands, how it was done, and what is open.
Update it when you hand over.

## Where the work is

- Work goes on short branches, each in its own worktree (see AGENTS.md), and is merged into `main` once the user agrees.
- CI (`.github/workflows/ci.yml`) runs on Linux and Windows on each push to `main` and on each pull request. For another branch, start it by hand (`gh workflow run ci.yml --ref <branch>`, or from the Actions tab), and ask the user first. The Windows job is the only real Windows test, and it caught several failures that no Linux run showed.
- CI is green on both platforms from `b57f7cd` on, the Windows job's tests of every crate included. Before it, the Windows job failed from the accounts commits on, on four account tests whose home folder was a Unix path.
- The protocol version is 11: `Welcome` carries the daemon's commit (`daemon_commit`), details go out as `DetailsUpdated`, with only the parts and subagents that changed, and an agent's summary carries its Claude Code version (`claude_code_version`). An app and a daemon of different versions refuse each other at `Hello`, so they are rebuilt together.

## Updating any machine's daemon

This work is in `main`. The user approved its screens after three rounds of HTML mockups, which were removed at the merge. Step 2 of [packaging.md](packaging.md) builds on it, on branch `feat/server-daemon-install`: a third plan that sends the app's own Linux daemon over SSH, beside the build on the server (`ServerPlan::daemon_file`), a probe of who owns a server's daemon before its update, and the install of a daemon from "Add a server". packaging.md lists what it does and what is not built.

What it does:

- The daemon sends its commit in `Welcome` and writes its process into `serve.lock`. On Unix, `cockpit_daemon::stop_lock_holder` stops that process and no other.
- The update's state and flow, the same for every machine (`state/update_offer.rs`, `state/update_plan.rs`, `state/daemon_update.rs`, `service_update.rs`). A server runs `contrib/update-daemon.sh`. Local's background service is stopped, then started again with the app's own program. When agents are busy, a confirmation offers "Update when they are idle" and "Update now", which gives the script `--even-if-busy`.
- Machine panels even when Local is alone, "Local" for this computer, a strip of machine troubles when agents are grouped by project, the Machines dialog as a grid, and compact menus in the composer (`state/machine_status.rs`).
- The update screens, from the mockups (`ui/daemon_update.rs`): the cards with the daemon's build beside the app's, the "0.1.0 → 0.2.0" version column, the confirmation with each busy agent's status, and an older daemon's line with "Update" alone. A refusal names the daemon's release and commit, so the cards can show them. The demo shows each state with `--demo-update <state>` and `--demo-machines <scene>`.
- An idle agent whose background tasks still run counts as busy, as the user decided: in `busy_agents`, in the confirmation ("runs 2 background tasks"), and in the script's check, through the TASKS column of `agents-cockpit list`.
- Updating Local checks its agents as the script does on a server. While Local's daemon refuses the app, "Update Local" always asks first, since the app cannot list the agents: "Update now" or "Cancel". While the app is connected, it reads the resting agents' raw logs first, and an agent that may hold a scheduled job (more `CronCreate` than `CronDelete` calls since the daemon started) counts as busy: the confirmation lists it with "may hold a scheduled job". "Update when they are idle" waits only for the agents that run something, since a job may never end; the restart reads the logs again, and refuses with "Update now" on its screen while a job may be held. The demo shows these with `--demo-update local-confirm-jobs`, `local-holds-jobs` and `local-confirm-unseen`.
- Local's update no longer stays on "Updating…" when another program restarts the old daemon during the stop, such as a systemd unit with `Restart=always`: the app counts its connections, and judges a connection made since the stop once the restart ends.
- The script's stop signals no process that got the daemon's pid since the script found it: it compares the start time from `/proc/<pid>/stat` right before `kill`.
- Fixes from `/review fix` on these changes. A second click on "Update Local" during the read of the logs does nothing, and the button says "Checking jobs…". The restart refuses when an agent started a turn during its read. A clock that was ahead when the daemon started no longer hides a scheduled job.
- The app's stop (`stop_lock_holder`) waits for the daemon's process to end, through its pidfd, not for the lock to be free. A program that starts the old daemon again at once, such as a systemd unit with `RestartSec=0`, no longer makes it fail with "did not stop".
- The script finds the daemon in `/proc/locks`, as the process that holds the lock on `serve.lock`. A process that only has the file open, such as the daemon's child between fork and exec, gets no signal, and the stop does not wait for it.

To check Unix code without CI, cross-build the daemon crates for `x86_64-unknown-linux-musl`, with `rust-lld` as the linker, and run their test binaries in WSL. The tests embed Windows paths through `env!`, so give them absolute paths for the run, and don't commit that change. The `update_daemon` tests fail in about 4 of 10 full runs there, on `main` too: rerun a failing test alone.

## What the branch added

Each item is described in architecture.md and was reviewed with `/review fix` and `/review-standard fix`.

- The review pass on the "/" palette anywhere in a message, Ctrl+O (`agent.list_dir`, `agent.find_files`), the title emoji grid and the machine load view.
- Sidebar: status chips only for statuses agents have; each row's time and task progress (`AgentSummary.last_activity_at`, `turn_started_at`).
- New agent dialog: folder completion from the daemon (`folders.list`); one highlight per list, the pointer moving the keyboard highlight (every list: resume, recent folders, folders, Ctrl+O, Ctrl+K, the "/" palette); lists scroll to the highlighted row; a field takes the focus when the dialog opens.
- The agent map (Ctrl+M or the composer button): every subagent of the agent in a dialog, built from an approved mockup. `AgentDetails.subagents` comes from the daemon.
- A faster agent switch: the click's frame draws the chat's latest 25 rows, the next frame the rest.
- The app runs the daemon in itself (`InAppDaemon`, an in-memory transport) or starts the background service, from a "No daemon answers" screen. A choice holds for the run unless "Use this every time" is ticked.
- Windows: the workspace builds and tests on Windows; the app runs agents in itself there. Job Objects end an agent's whole process tree; `claude.exe` is found before an npm `claude.cmd`; file browsing, the machine load and the stop of a command work on Windows; the release app has no console window and logs to a rotated file; notifications are toasts; paths follow the daemon machine's rules (`PathStyle`).
- Crash safety: a panic in the app stops the agents it runs before it exits; a daemon stops a leftover `claude` of a crashed daemon (same pid, start time and, on Linux, boot) before it resumes the agent.
- Several Claude accounts: each account is a `CLAUDE_CONFIG_DIR` folder with its own login; agents, past sessions and plan usage are per account; the daemon removes `CLAUDE_CODE_OAUTH_TOKEN` from every agent, and `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` unless the account takes its credentials from the environment. The app never reads a credential. Screens built from an approved mockup, with the Tag label style.
- The protocol's bool fields became enums (`CredentialSource`, `TurnOutcome`, `Delivery`, ...); old files on disk still read.
- Steering: a message sent while the agent works goes to `claude` at once, and Claude reads it at its next step (Alt+Enter queues it instead). Messages Claude has not read show pinned above the composer, from `AgentDetails.waiting_prompts`. Checked against Claude Code 2.1.319 by hand; the fake `claude` only covers the case where the message waits for the turn's end.
- Details updates: after an agent's first details event, a change sends only the parts that changed, and of the subagents only those that changed (`DetailsUpdated`). A subagent's tool call on an agent with 200 subagents sends about 470 bytes instead of 72 KB.

## How the user works

- The user decides; agents propose. Ask before a design change, a protocol change, or anything outside the request, with a recommendation first. The user often answers "ok" to a numbered list: map the answers by number.
- For a new screen or a large UI change, make an HTML mockup first (self-contained, reusing the app's `style.css`, in the scratchpad, with Playwright screenshots), send it, and build only once the user picked.
- After a feature: `/review fix` (one agent for the daemon side and one for the desktop crate work well in parallel), then `/review-standard fix`, then push. CI runs on the push to `main` (see "Where the work is"). Each bug fix comes with a test that failed first.
- Commits: plain English subject in the imperative, short body, no `Co-Authored-By` or other trailer. No em-dashes anywhere. Comments only for what code cannot show.
- The user tests on a Windows laptop: they clone the repository, `git pull`, then `cargo run -p cockpit_desktop`. Claude Code is installed there with the native installer (`claude.exe` in `%USERPROFILE%\.local\bin`, not on PATH).

## Working in a cloud container

These notes are about Claude Code's cloud sessions, where most of this branch was built.

- `nix develop` fails there: it cannot download its Rust overlay. Install the desktop's libraries with apt instead: `libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`.
- Keep the Rust toolchain as new as CI's (`rustup update stable`): a newer clippy in CI once failed a build that passed locally.
- No nightly toolchain is installed, so the standard's nightly rustfmt options (`imports_granularity`, ...) were never applied on this branch. Run `cargo fmt` in the dev shell on a machine with Nix, once, and commit the result.
- Windows checks: `rustup target add x86_64-pc-windows-msvc x86_64-pc-windows-gnu`; `cargo clippy --target x86_64-pc-windows-msvc` checks `cfg(windows)` code; tests for the GNU target run under Wine (`apt-get install wine mingw-w64`). Wine has no git, cannot open the app's window, show toasts or read symbolic links back. The desktop's GNU test binary needs `WebView2Loader.dll` next to it.
- The disk allowance is small. Use `CARGO_INCREMENTAL=0`, delete `target/*/incremental` and old targets, and never copy the app's binary around.
- On-screen checks: the demo (`agents-cockpit-desktop --demo`) under Xvfb with openbox, wmctrl and xdotool, screenshots with `import -window root`. Close the window with wmctrl: xdotool's close aborts GTK.
- An agent started with `isolation: "worktree"` gets a worktree of `main`: its first step must check out the working branch. Two agents in one checkout work if each owns separate crates and commits only its own files.

## Checking the app on Windows

The demo can be driven by a script, which sees and acts on the real page:

1. Build, then start the demo with its own WebView2 data folder and a debugging port: `WEBVIEW2_USER_DATA_FOLDER=<a scratch folder> WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333 target/debug/agents-cockpit-desktop.exe --demo`. The data folder matters when the user's own app runs: WebView2 keeps one browser per data folder, and that browser keeps the arguments it started with, so the port would not open.
2. Drive it over the Chrome DevTools Protocol from Node 22, which has `fetch` and `WebSocket` built in: `http://127.0.0.1:9333/json` lists the page, then `Runtime.evaluate`, `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`, `Input.insertText` and `Page.captureScreenshot`. A selection is made with `document.createRange` and `getSelection().addRange`.
3. Stop the demo before the next build: Windows locks a running program's file.

The window shows on the user's screen meanwhile.

## Several machines

The user approved option A of the sidebar mockup: one panel per machine, folding, with no coloured dot per machine. How it works is in docs/architecture.md, "Several machines".

It is merged into `main`, but the user has not fully run it yet. Left:

- Check it on Windows with a real server: add one in the Machines dialog, start an agent there, stop the server, and watch "Retry now". The user tests with WSL as the server, reached over SSH on localhost. The daemon there needs Claude Code (the `claude` command) on the `PATH` of the terminal that runs `serve`: Claude Desktop does not provide it.
- Branch deletion returns 403 from these sessions, so the user deletes old branches.

## Open points

Decisions not taken yet, and things nobody could check here. The ideas for later are in [todos.md](todos.md).

- An interrupted foreground subagent may show "failed" rather than "Stopped by you": it depends on what Claude Code writes, and no capture shows it.
- Windows was tested by the user only for starting agents in the app. Toasts (whether their threads end when Windows sends no answer), Job Objects on real Windows, and `CREATE_NO_WINDOW` were checked by CI tests or not at all.
- macOS was never run. Its sign-in state is `unknown` (Keychain), and its leftover check records nothing (no `/proc`).
- Choosing a folder trusts it: in the app's mode `claude` shows no trust prompt, so a repository's `.mcp.json` servers start at once. The user chose to document this rather than add a confirmation.
- Cloud sessions (claude.ai/code) cannot be shown in the app: no public API, and the terms forbid handling the user's claude.ai credentials. `claude --teleport` into a local session, then "Resume a session", is the supported route; the user said not now.
