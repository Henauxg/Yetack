# Handoff

The state of the work, for the next agent or session.
Read AGENTS.md, [plan.md](plan.md) and [architecture.md](architecture.md) first: they describe what the code does.
This file says where the work stands, how it was done, and what is open.
Update it when you hand over.

## Where the work is

- All work is on the branch `ccr-8be27d05-6ge2s2`, pushed. `main` has not moved: the branch is about 150 commits ahead of it, and no pull request exists yet. Ask the user before opening one or merging.
- CI (`.github/workflows/ci.yml`) runs on every push, on Linux and Windows. Check its latest run on GitHub after you push: the Windows job is the only real Windows test, and it caught several failures that no Linux run showed.
- CI is green on both platforms from `b57f7cd` on, the Windows job's tests of every crate included. Before it, the Windows job failed from the accounts commits on, on four account tests whose home folder was a Unix path.
- The protocol version is 4. An app and a daemon of different versions refuse each other at `Hello`, so they are rebuilt together.

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

## How the user works

- The user decides; agents propose. Ask before a design change, a protocol change, or anything outside the request, with a recommendation first. The user often answers "ok" to a numbered list: map the answers by number.
- For a new screen or a large UI change, make an HTML mockup first (self-contained, reusing the app's `style.css`, in the scratchpad, with Playwright screenshots), send it, and build only once the user picked.
- After a feature: `/review fix` (one agent for the daemon side and one for the desktop crate work well in parallel), then `/review-standard fix`, then push, then check CI. Each bug fix comes with a test that failed first.
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

## Several machines, in progress

The user approved option A of the sidebar mockup: one panel per machine, folding, with no coloured dot per machine. Done so far (docs/architecture.md, "Several machines"):

- `[[machines]]` in config.toml; a connection task per machine and a router; app agent ids with the machine in the high bits (`machine.rs`, `map_agents` in the protocol).
- `MachineState` per machine; snapshots and events apply to their machine; `Outgoing.to` names the machine of a request about no agent.
- The grouped sidebar; the new agent dialog's Machine row (this computer's accounts only).

Left, in the order planned:

1. Accounts per machine: `state/accounts.rs` reads `machines.this` everywhere. A server's agent shows this computer's account labels, and the Accounts dialog needs a machine picker (the user agreed to one).
2. The header's plan usage chip, the MCP servers' hidden list (`settings.set`) and the undo of a close should follow the agent's machine; the machine load is polled for this computer alone.
3. A Machines dialog that adds and removes `[[machines]]` (mocked in the same round), and "Retry now" on a panel.
4. The "Where should agents run?" screen takes the whole window even when servers are configured: it should show inside this computer's panel then.
5. Run `/review` and `/review-standard` on the whole machines range, and check it on Windows with a real server.

## Open points

Decisions not taken yet, and things nobody could check here. The ideas for later are in [todos.md](todos.md).

- Details updates carry the whole subagent list, up to about 70 KB, at each subagent tool call, and the event logs keep them. Batching them per agent every 250 to 500 ms was proposed, not decided.
- An interrupted foreground subagent may show "failed" rather than "Stopped by you": it depends on what Claude Code writes, and no capture shows it.
- Windows was tested by the user only for starting agents in the app. Toasts (whether their threads end when Windows sends no answer), Job Objects on real Windows, and `CREATE_NO_WINDOW` were checked by CI tests or not at all.
- macOS was never run. Its sign-in state is `unknown` (Keychain), and its leftover check records nothing (no `/proc`).
- Choosing a folder trusts it: in the app's mode `claude` shows no trust prompt, so a repository's `.mcp.json` servers start at once. The user chose to document this rather than add a confirmation.
- Cloud sessions (claude.ai/code) cannot be shown in the app: no public API, and the terms forbid handling the user's claude.ai credentials. `claude --teleport` into a local session, then "Resume a session", is the supported route; the user said not now.
