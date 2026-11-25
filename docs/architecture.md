# Architecture

This document says how the code is split and why.
The [README](../README.md) says what the app does.
The Rust code follows [standards/rust/](standards/rust/README.md).

## Overview

```text
desktop app or CLI                       daemon (yetack serve, or in the app)
  cockpit_client                           cockpit_daemon
    transport:                               server: Unix socket, peer checks
      Local: the Unix socket       ──────►   hub: agents, sequence numbers, broadcast
      Ssh: ssh <host> yetack bridge          store: state folder, event logs
           (stdin/stdout, copied to the socket)
      InApp: an in-memory stream             claude process per agent:
           (the daemon in the app's process)   claude_protocol (JSON mode, control protocol)
```

The app reaches a remote daemon through `ssh <host> yetack bridge`, a local one through its socket, and one in its own process through an in-memory stream (see [The daemon in the app](#the-daemon-in-the-app)).
All three carry the same protocol, so everything above the transport is identical.

### Design choices

- Claude Code runs in JSON mode. The app draws messages, tool calls and approvals with HTML and CSS, so it needs them as data. A terminal tool such as [herdr](https://github.com/herdrdev/herdr) only sees Claude Code's screen as a grid of characters. Yetack takes herdr's other ideas: a daemon that outlives the client, an attach over SSH's stdin and stdout, and a snapshot followed by numbered events.
- The bridge does the job of `socat - UNIX-CONNECT:<socket>`, so the server needs no socat and the app's config needs no socket path. Forwarding the socket with `ssh -L` would leave a socket file on the laptop that any of its processes could use.
- The app is a desktop app in a webview. A web UI in a browser would need a port on the laptop, and any website open in that browser can send requests to localhost ports. Dioxus native (Blitz) has incomplete CSS support and cannot use the browser's libraries for markdown, code highlighting or diffs.
- Dioxus comes from a fork, `Henauxg/dioxus-fork`: 0.7.10 with `dioxus-desktop`'s websockets built without TLS. Upstream turns on tungstenite's `native-tls`, which links OpenSSL, though both websockets are plain TCP. With the fork and rustls for the app's own HTTPS, no build links OpenSSL. Cargo resolves the whole workspace, so even a server that builds only the daemon downloads the fork from GitHub, about 80 MB, once. [todos.md](todos.md) says when Dioxus comes back from crates.io.
- Out of scope: a git worktree per agent (the user picks the folder), and access from a phone or a browser.

## Crates

All crates live in `crates/` and are application code ([standards/rust/README.md](standards/rust/README.md#library-code-and-application-code)): nothing outside this repository calls them.

| crate | kind | owns |
| --- | --- | --- |
| `claude_protocol` | lib | Claude Code's JSON mode: types and parsing, no IO |
| `cockpit_protocol` | lib | The daemon API and its framing |
| `cockpit_client` | lib | A connection to a daemon, with reconnection |
| `cockpit_daemon` | lib | The daemon, and `InAppDaemon` to run it in the app |
| `yetack` | bin `yetack` | The command line: `serve`, `bridge`, client commands |
| `cockpit_desktop` | bin `yetack-desktop` | The Dioxus desktop app |
| `cockpit_voice` | lib | Dictation: the microphone, and speech to text on this computer |

Dependencies point one way:

```text
claude_protocol ◄── cockpit_daemon ──► cockpit_protocol ◄── cockpit_client
                      ▲      ▲                                  ▲      ▲
                      │      └────────────── yetack ────────────┘      │
                      └──────────────── cockpit_desktop ───────────────┘
                                              │
                                              ▼
                                        cockpit_voice
```

The app hands its in-app daemon the receiving end of the client's in-memory link, so `cockpit_daemon` and `cockpit_client` do not depend on each other outside tests.
`cockpit_protocol` does not depend on `claude_protocol`, so a change in Claude Code's wire format changes only `claude_protocol` and the daemon's conversion, never the app.
`cockpit_voice` holds the only C++ of the workspace, [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) on ggml, which builds with cmake and a C++ compiler. The app takes it through its `voice` feature, on by default: `cargo build -p cockpit_desktop --no-default-features` builds an app without dictation and without the C++. The daemon never depends on it.

## Data model (`cockpit_protocol`)

### Ids

Each id is a newtype ([standards/rust/api_design.md](standards/rust/api_design.md#give-every-persisted-id-its-own-type)).
`Seq` is one global number per event, from the hub.
`ItemId` numbers an agent's transcript items, so an update can target one.
`AccountId::DEFAULT` (0) is the default account.

### Agents

`AgentSummary` holds what the sidebar and the header show of one agent.

- `session_use` says whether `session` is the agent's own, or, for a fork that has not started its own session yet, the session it copies (`Fork`). Another agent may hold that session, so the app matches agents by their own session only (`app_state::own_session`): an undo or a resume never takes a fork for its original. A closed fork without its own session offers no undo, and the daemon keeps none of its choices.
- `repository` is the folder's git remote. `Repository::from_remote` writes every form of a remote the same way, so the app can group agents by it. The daemon reads it from the checkout's `.git` file, `commondir` and config: each only when it is a regular file of at most 64 KiB (1 MiB for the config), so that the agent cannot make the daemon wait on a FIFO or read gigabytes. On Windows it opens no device, and no share but the checkout's, that a `gitdir:` or `commondir` line names. It reads them in a task with a 10-second limit, one task per folder whatever the number of agents in it, never on the hub, since a `.git` file can point into a mount that never answers: a new agent takes the repository of another agent in its folder, if any, and a moved or restored one keeps its own, until the read comes back (`HubInput::RepositoryRead`).
- `claude_code_version` comes from the latest `system/init` line (see "Untested Claude Code" under [Desktop app](#desktop-app-cockpit_desktop)).
- `background_running` counts the background tasks still running. A task can outlive its turn, so an `Idle` agent can still have work.
- `turn_started_at` and `last_activity_at` let the app count a row's time by itself, so no event moves a clock. A turn Claude Code starts without a prompt (a resumed cut-off turn) starts at its first line.
- `quiet_since` is set after 5 s without a line (`thinking_tokens` included) while nothing waits for you. The hub checks once a second and the next line clears it, so a quiet stretch sends two summaries.

The daemon keeps the list's order, so every client shows the same one.
Only `agent.move` changes it.
Positions leave room between neighbours, so a move changes one agent.
`cockpit_protocol` holds the move, so the app reorders at once with the numbers the daemon will send.

`AgentStatus` is derived by the daemon, never by the app.
`NeedsYou` covers a pending approval, question or plan, and an account that needs a sign-in.

### Transcript

An agent's conversation is a list of `TranscriptItem`s, each with an `ItemId` and an optional `parent` (the subagent it nests under).
The daemon numbers items in transcript order, so the app can merge a history page with live appends by id.

- An `Edit` or `Write` call drops its old and new text when either is over 1 MiB, and marks its `FileEdit` `EditTexts::TooLarge`.
  The app then shows "Too large to show" in place of the diff, and the file view's diff starts after that edit.
  A `FileEdit` without the mark, from an older daemon or event log, reads as `Kept`.
- An `Approval` finds its tool call through `tool_use_id`, which Claude Code sends after the call's `tool_use`.
- Claude Code withdraws a pending request when the turn is interrupted, and the item becomes `Cancelled`.
- After a `conversation_reset`, Claude Code's total cost counts from 0.

`AgentDetails` holds what the side blocks and the agent map show, including the prompts Claude has not read yet.
Claude Code gives a foreground command a task too, so `agent.stop_task` can stop one command. Claude gets it as stopped (exit code 137) and goes on with its turn.
Claude Code reads a prompt sent during a step only once the step's running calls end (`AgentEntry::message_hold`).

### Subagents

`AgentDetails.subagents` lists every subagent at any depth, in start order.
A `Subagent`'s `item` is the `Agent` call that started it, which its own items name as their `parent`.

### Plan usage

`PlanUsage` holds the latest `rate_limit_event` per window.
The daemon keeps one per account.

### Accounts

An `Account` is a Claude Code config folder on the daemon's machine, with its login, user settings, sessions and skills.
A repository's own `.claude/`, `.mcp.json` and `CLAUDE.md` stay shared between accounts.
Its `credentials` are the folder's login (`sign_in`) or the daemon's own `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` (`environment`).
The default account uses the daemon's `CLAUDE_CONFIG_DIR`, else `.claude` in the home folder, and cannot be removed.
An agent keeps its account across stops, resumes and daemon restarts.
A message or file without an account reads as the default account.
`settings.json` and the event logs may hold `bool`s in place of `credentials`, and these still read.

## Protocol (`cockpit_protocol`)

### Framing

One JSON object per line, UTF-8, `\n` at the end.
A reader refuses a line over 16 MiB instead of buffering without bound.

### Session

1. The client sends `Hello { protocol_version, last_seq }`. An app and a daemon of different versions refuse each other.
2. The daemon answers `Welcome` or `Refused`. `Refused` keeps one shape in every protocol, so a client of any protocol reads it.
   Both carry `daemon_commit`, the git commit the daemon was built from (`build.rs`). Two builds of one release can differ, so the commit tells the app whether a daemon runs its own code (see "Updating a machine's daemon" under [Desktop app](#desktop-app-cockpit_desktop)).
3. The welcome replays every event after `last_seq` when the logs still hold them, else sends a `Snapshot`. It never does both, so no event is applied twice.
   With a replay, the welcome's `replayed_through` names the daemon's last event when the client joined. The events up to it happened while the client was away, and each live event comes after it. A daemon that predates the field leaves it out, and a client that predates it ignores it, so the protocol version stays.
4. Responses and events share the connection, in any order.

The snapshot holds no transcripts: the app asks for them with `agent.history`, a page at a time.

### Requests

The requests are the methods of one enum, with fixed strings ([standards/rust/api_design.md](standards/rust/api_design.md#declare-a-reusable-string-once)).
[Machine load](#machine-load), [Accounts](#accounts-1), [Terminals](#terminals) and "Working folder" describe theirs.
A daemon answers an unknown method with ``unknown method `<name>` ``. The app looks for that text to tell an older daemon, so a test pins it.

`agent.close` ends an agent even while its process runs, which `agent.stop` then `agent.remove` cannot do.
Removing the agent closes `claude`'s stdin. `claude` then ends its session file and exits, or is killed after 10 seconds, so `sessions.resume` can bring the session back.

A card sends its answer once. The daemon takes a repeat of an item's answer as done, and refuses a different one, since Claude Code has the first.

`agent.reload_skills` and `agent.reload_plugins` have the agent's running `claude` read its skill folders or its plugins again (see "Reloading skills and plugins" under [Claude Code processes](#claude-code-processes)).
Their answers, `SkillsReloaded` and `PluginsReloaded`, count the skills, or the plugins and the errors loading them, and name the commands the reload added or removed (`CommandChanges`).
The new list of commands goes to every client in a `CommandsChanged` event.

`agent.mcp_sign_in` starts a server's OAuth sign-in in the agent's `claude` (`mcp_authenticate`), and its answer `McpSignIn` holds the link of the sign-in page.
`agent.mcp_sign_in_callback` passes on the address the browser went back to, which carries the authorization code (`mcp_oauth_callback_url`). Claude Code answers once it holds the token, but does not reconnect the server after a sign-in finished this way. So the daemon then sends `mcp_reconnect`, and answers `Done` once Claude Code tried, whatever the outcome: the token is kept.

`agent.read_file`, `agent.list_dir` and `agent.find_files` reach any file the daemon's user can read, as an IDE does, since the agent's commands already can.
A path with `..` is refused.
A relative name is also searched for in each git checkout that holds a file the agent read or changed, unless that checkout is `/` or holds the home folder.
`agent.read_file` opens only a regular file, because opening a device can act on it (a serial port resets its board).
Each request checks again what it opened, so a link swapped in after the checks cannot reach a device or a FIFO.

On Windows, module `windows_path` checks paths as text, before any file system call:

- It compares names whatever their case, as Windows does, with one upper case character for each (`ß` stays `ß`).
- It refuses device paths and device names (`NUL`, `COM1`) in any folder.
- It refuses a network share that neither the agent's folder nor a checkout or file it worked on is on: opening a path on a share makes Windows send its host a hash of the user's login, and an agent's text can name any host. A symbolic link to a share is still followed when the path is resolved, then refused: the agent made the link and can reach that host itself. `folders.list` takes a share, since the user types it.
- A search leaves out a path that `git ls-files` lists outside its checkout, which an index file the agent wrote can hold.

`agent.read_file` sends at most 4 MiB of text, or 8 MiB of image or PDF. It takes a file for an image or a PDF by its extension. It adds the agent's edits of the file, since the app may not hold the messages that made them.
Its `git ls-files` search stops at 20 matches or 3 seconds, and leaves out a path that is not UTF-8, since the protocol carries paths as text.
`agent.list_dir` and `folders.list` read at most 20,000 entries, send at most 2,000, and run off the hub's task.
`folders.list` sends no file names, and leaves out a name that is not UTF-8, since the protocol carries paths as text.
The file system work of these requests, of the git requests and of `sessions.list` and `sessions.resume` runs on a blocking thread (module `blocking`). The request stops waiting at its time limit and answers with a timeout: 10 seconds for a file or a listing, 3 for `agent.find_files`, 30 for the sessions, and the git request's own. A thread that a stuck mount holds runs on until the mount answers. Once one folder has 8 such threads, or the daemon 128, the next work there is refused at once with a timeout, so that retries cannot take all of tokio's 512 blocking threads.

`agent.git_file`, `agent.git_blame`, `agent.git_log`, `agent.git_show` and `agent.git_status` read git for the file view (module `git`).
A path passes the checks of `agent.read_file`, then reaches git after `--`, relative to its checkout's top folder. A commit reaches git only as a `CommitHash` of 40 or 64 hexadecimal digits.
The agent can write its checkout's git config and `.gitattributes`, and through them make git run a program.
The daemon runs git without the user's approval and outside Claude Code's sandbox, so one builder makes every git command: no file system monitor, no optional locks (the agent's own git never waits on the index lock), no index refresh in `git diff` (writing the index runs the `post-index-change` hook), no pager, no pathspec magic, a deadline, and no console on Windows.
It also turns fetches off (`GIT_NO_LAZY_FETCH`, an empty `GIT_ALLOW_PROTOCOL`): a partial clone fetches the objects it lacks, and a fetch runs programs the config names.
The four requests on one file read commits only: a blob by its hash, the blame of a given commit, and a log without textconv, external diff or signature check (`log.showSignature` runs `gpg.program`).
`agent.git_status` alone reads the working tree. It turns off each filter the config defines and leaves submodules out, so a file under a filter may show as changed when it is not.
`-c` cannot name a filter whose name holds `=`, so the status refuses its checkout.
The status also names the remote the app links to (`origin`, else the first) and lists the commits of HEAD that none of its branches held at the last fetch, at most 100 (`git rev-list HEAD --not --remotes=<name>`).
`agent.git_show` says the same of its commit, which may have left HEAD's history since the status.
A file's text or a commit's change takes at most 5 seconds, a blame or a log 10, and a status 20, for at most 20 checkouts.

A history page is limited by bytes as well as by count, since images travel inline.
A page also carries the subagent calls its items run under that are older than the page (`ancestors`), so a subagent's work shows under its call when the call itself is far back, as for a subagent that runs in the background for hours. The app asks for the next older page before the first item of the page (`Transcript::loaded_from`), not before those calls, and its trim keeps the subagent calls of the items it keeps. Those calls come first in the chat, in a wrapper marked `data-early`, and the grey rows of older messages stand below them, where the older messages arrive. At the very top, the scroll script holds the first row past them while it shows, so an older page that arrives moves the view down and the loading stops. A view that holds one of those calls, as after a reveal of one or with a call taller than the view, asks for no older page: the page would arrive below the view and leave it at the top, and the chat would load page after page.
An item too large for any response comes as a `Notice` under its id, so paging moves on.
Titles and model names go into every snapshot, so the daemon refuses a title over 200 characters and a model name over 100.

### Events

Every event carries its `Seq`.
Slash commands and skills change only when `claude` starts, when it reloads them (`agent.reload_skills`, `agent.reload_plugins`), or when its watch of the skill folders sees a change, so they travel in `CommandsChanged`, apart from the details.

An agent's details go out whole (`DetailsChanged`) in a replay and as its first details event after the daemon starts, because a client may hold details the restarted daemon lost.
After that, `DetailsUpdated` carries only what changed. With 200 subagents (the most the daemon keeps), a tool call sends about 470 bytes instead of 72 KB.
`AgentDetails::changes_since` and `AgentDetails::apply` make and apply the changes, so the daemon and the app agree.

In a burst, a change waits until 200 ms have passed since the last one, and a later change replaces it.
A change of the waiting prompts goes out at once, since the app matches them against the transcript.

`agent.details` answers with `last_seq` and first sends any held change, so the answer is current.
An update can arrive before or after the answer that holds it, so the app keeps the updates that arrive while the details load and applies only those after `last_seq`.

## Daemon (`cockpit_daemon`)

### Entry points

The daemon starts the same way behind a socket and inside the app (module `daemon`).
`Daemon::prepare` takes `serve.lock` (see [Store](#store)) and loads the state. `PreparedDaemon::start` then starts the hub, which resumes the agents that were running.
`yetack serve` binds the socket between the two steps, so a failed bind starts no agent.

### The daemon in the app

`InAppDaemon` runs the daemon inside the desktop app, on a Tokio runtime of its own.
It uses the background service's state folder and `serve.lock`, so the two never serve one folder at once, and each resumes the agents the other left running. When the lock is held, it fails with `StoreError::AlreadyRunning`, and the app connects to that daemon's socket instead.
Clients reach it through a channel of `tokio::io::duplex` streams (`InAppLink`). No socket listens, so there is no peer check: an agent's command, the threat that check is for (see [Security](#security)), has nothing to connect to.
`InAppDaemon::shutdown` kills the agents' processes but leaves the agents marked as running, so the next daemon resumes them. When the app dies instead, the next daemon stops the `claude` processes left running (see [Store](#store)).
The shutdown blocks, so the app calls it from a thread outside any runtime.

### Tasks and ownership

The daemon is a set of tokio tasks that talk over channels. No state is shared behind a mutex.
The **Hub** owns every agent's state, numbers and logs the events, and sends them to every client. Its module `transcript` is the only place that turns Claude Code messages into transcript items.
The hub handles one input at a time, so a wait on an agent's folder on a mount that never answers would hold every agent and every client. Resolving a folder the app sends, checking an agent's folder, starting `claude` in it (module `hub::start`) and starting a terminal's shell there (module `hub::terminals`) run in tasks, as the file and git requests do, and the hub carries on when the answer comes back as a `HubInput`.
A **Claude process** task owns each running `claude`. The **Server** checks each peer, and a **Connection** task serves each client.
A client that reads too slowly is disconnected rather than buffered without bound. It reconnects with its last `Seq`.

### Claude Code processes

- Environment: the daemon's own, changed for the agent's account (module `accounts`):
  - `CLAUDE_CONFIG_DIR` is the account's folder. The default account's `claude` gets the daemon's own value, set or not.
  - `CLAUDE_CODE_OAUTH_TOKEN` is always removed: it would take the place of any folder's login.
  - `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` are removed, unless the account's `credentials` are `environment`. Claude Code prefers either one to the folder's login, so one of them in the daemon's environment would otherwise run every account.
  - The route switches (`ROUTE_ENV` in `accounts/route.rs`: `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`, `ANTHROPIC_BASE_URL`, the providers' own addresses and keys) are removed on the same rule: they would send every account's agents to that provider. An account picks a provider in its folder's `settings.json` instead. The providers' credentials, such as `AWS_PROFILE`, stay: Claude Code ignores them without a switch, and the agents' commands may need them.

  The daemon never reads or logs these values. For `accounts.list`, it only checks whether the key, the token and the route switches are set, and it never keeps a custom server's address.
  `claude mcp remove` gets the same environment.
- Command: `claude` with stream-json input and output, `--permission-prompt-tool stdio` and `--replay-user-messages` ([Run Claude Code programmatically](https://code.claude.com/docs/en/headless)), in the agent's folder.
  The daemon checks that the folder still exists first: a missing one fails the start with `NotFound`, so the app can offer to move the agent. Windows alone would only say that the directory name is not valid.
  Never `--bare`: bare mode ignores the subscription login and needs an API key.
  The daemon resumes its agents as it starts with `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`, so a turn its own stop cut off carries on. A user's resume, of an agent or a past session, goes without it, so a turn the user stopped stays stopped.
  Without a terminal, `claude` asks no trust question, and starts a repository's `.mcp.json` servers for any account. Choosing the folder in the app is what trusts it (see [Security](#security)).
- Start: checking the folder and starting `claude` both wait on the folder, since `claude` must enter it. They run on a blocking thread with a 10-second limit, counted with the folder's other reads (module `blocking`). A start past the limit fails with `the folder <path> did not answer`, shown as any failed start is: in the transcript of an agent the daemon resumes as it starts, else in the request's answer.
  `agent.start`, `agent.fork`, `agent.change_folder` and `sessions.resume` first resolve the folder the app or the session file names, in a task with the same limit.
  A request that starts `claude` (`agent.start`, `agent.resume`, `agent.fork`, `agent.change_folder`, `sessions.resume`) answers once `claude` runs or fails. A new agent is listed only once its `claude` runs. An agent the daemon resumes as it starts shows as running from the first snapshot.
  Meanwhile the agent's prompts wait in its queue, its control requests fail as for an exited agent, and a second start is refused. A second `sessions.resume` of the session a new agent starts on gets the first one's answer. A stop or a close gives the start up: each process has a generation, the stop moves the agent to the next one, and a process that starts for an older one is killed. The process task reports nothing until the hub runs it, so no line or exit of a process reaches the hub before the hub has the process.
- Binary: `claude` from `PATH`, or `YETACK_CLAUDE`. On Windows, relative folders of `PATH` are skipped.
  A `.cmd` or `.bat`, such as an npm `claude.cmd`, runs through the system folder's `cmd.exe` with `/d /v:off /s /c`. An argument holding a quote, a `%`, a line break or a NUL is refused, since `cmd.exe` would read it. The child of `cmd.exe`, the real `claude`, counts as `claude` itself (`Launch::Interpreter`).
  Every program the daemon starts on Windows, and the client's `ssh`, starts with `CREATE_NO_WINDOW`: the app has no console to share.
- On Unix, `claude` leads a process group of its own. On Windows, it starts suspended and joins a Job Object of its own (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, no breakaway) before it runs, so every process it starts is in the job.
  Either way, stopping the agent stops its MCP servers and commands too, and on Windows the job also ends when the app dies. When Windows cannot make the job, `claude` runs and stops alone.
- An agent given no effort runs at the one Claude Code picks, which the daemon asks for with `get_settings`. It does not read the settings files itself, since the user's own `effortLevel` counts only for some models.
- Effort and the session's name have no control request, so the daemon sends `/effort` and `/rename` as prompts.
  Claude Code runs them without Claude: they show no working agent, and their `result` ends no turn, even one a prompt sent meanwhile started (`hidden_prompt_sent` in `AgentView`).
  Claude Code restates the session's saved name as it starts, but the daemon keeps the agent's own, so a rename made while the agent was stopped stays.
- A prompt goes to stdin as a `user` message, never as an argument: other users can read arguments with `ps`.
  A prompt sent while the agent works is either queued in the daemon until a turn ends with every steered prompt read, or steered: written to stdin at once, for Claude Code to read at the next tool boundary or turn.
  The agent stays working until Claude Code has echoed (`isReplay`) every steered prompt. A slash command always queues, since Claude Code does not echo every one.
  After a stop between turns, a steered prompt whose text the transcript holds counts as read. A prompt still unread then, or when the process ends, becomes a "not sent" notice.
- Approvals, `AskUserQuestion` and `ExitPlanMode` come as `can_use_tool` control requests.
  This control protocol is not documented. The open-source Python Agent SDK implements it, in [`_internal/query.py`](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py) (`_handle_control_request`): it is the reference for its messages.
  Claude Code echoes control responses, so the daemon ignores one whose id it did not send.
- Reloading skills and plugins: in JSON mode, `/reload-skills` and `/reload-plugins` sent as prompts change nothing the daemon sees, since it learns the commands from `system/init` and the `initialize` answer. So `agent.reload_skills` and `agent.reload_plugins` send the `reload_skills` and `reload_plugins` control requests, which the Agent SDK does not document either.
  - `reload_skills` answers every skill. They become the agent's skill names. Among the commands, the skills the answer lists replace those it no longer lists, in their place, the new ones come last, and the other commands stay.
  - `reload_plugins` answers every command, the plugins' included, and they replace the agent's commands. The skill names stay those of the latest `system/init` or `reload_skills`, so a plugin's new skill shows as a command until the next turn's `system/init`.
  - Claude Code also sends a `system` line `commands_changed` with every command whenever its list changes: a skill file it watches (`~/.claude/skills`, the project's `.claude/skills`), or a reload. Its commands replace the agent's, marked as skills by the same names. So an edited skill reaches the "/" palette with no reload. Checked against Claude Code 2.1.316.
    On a reload, this line comes before the answer. So the hub keeps the commands as they were when the request left, and the answer's `CommandChanges` compares the new commands with those.
  - Claude Code also follows the skill folders it already knows by itself: a reload is for a new folder, and for the daemon's list.
- After `--resume`, Claude Code's `total_cost_usd` includes the session's past cost. The daemon reads that cost from the session file's last `cost-state` line, so each turn shows its own cost.
  The agent's cost is the higher of Claude Code's total and its own count: a process killed with the daemon writes no `cost-state`, and a `/clear` starts a session whose cost leaves out the one before.

### Store

Everything lives in `$XDG_STATE_HOME/yetack/`: the socket, `serve.lock`, `agents.json`, `settings.json` (the accounts included), `closed-sessions.json` (the choices of the agents closed before), and for each agent its events, its latest details and every line exchanged with `claude`. An MCP sign-in's callback address is logged without its query, which holds the authorization code.
`serve.lock` is held while a daemon runs, so a second daemon refuses to start and a leftover socket is safe to delete (see [The holder of `serve.lock`](#the-holder-of-servelock)).
The daemon leaves the umask alone, since `claude` and the agents' commands inherit it. It creates folders with mode 700 and files with mode 600. The folder protects the socket until the socket's mode is set, right after the bind.

A daemon that dies without stopping its agents leaves each `claude` running until its turn ends, and the next daemon would run a second `claude` on the same session.
So `Daemon::prepare` first stops each process recorded in `agents.json` that still runs with its recorded start time. On Unix, it sends SIGTERM to the group, then SIGKILL after 5 seconds.
On Windows, only a `claude` without a job can be left. `TerminateProcess` ends it and the processes under it. A process counts as a child only if it started after its parent and still names it once opened, since an ended process gives its id away.
On Linux, start times count from the boot, so the record also holds the boot id. macOS records nothing.

#### The holder of `serve.lock`

Once it holds `serve.lock`, the daemon writes its process into the file (module `lock_holder`), and it empties the file as it lets the lock go.
A daemon older than this record writes nothing, so the file may be empty, or name a killed daemon.

On Unix, the app uses it to stop this computer's background service for an update (see "Updating a machine's daemon" under [Desktop app](#desktop-app-cockpit_desktop)).
`stop_lock_holder` checks the recorded pid and start time (a record without a start time never passes). It sends SIGTERM through a pidfd, so a process that takes the pid after the check gets no signal, then waits up to 10 seconds for the process to end.
It does not wait for the lock to be free: a systemd unit with `RestartSec=0` may start the old daemon again at once.
`serve` leaves its agents marked as running on SIGTERM, so the next daemon resumes them.
Reading the holder takes a free lock for a moment, so a daemon that starts then refuses to start. On Windows, the lock keeps other processes from reading the file.

Before the stop, the app asks which agents may hold a scheduled job, which ends with their `claude`.
`may_hold_scheduled_job` follows `contrib/update-daemon.sh`: the agent's raw log holds more `CronCreate` than `CronDelete` calls since the daemon started. A job that runs once ends with no `CronDelete`, so the answer is only "may".
`lock_held_since` takes that start as the earlier of the lock file's last write and now less the holder's run time. The file's time alone is too late after the clock is set back.

### Past sessions

`sessions.list` lists the newest 100 sessions of all accounts. `sessions.resume` makes an agent that runs with the session's account, with the choices of the newer of two sources (`resumed_choices` in `hub/agents.rs`):
- The model, effort and permission mode the session's last agent had at its close. The daemon keeps them for the newest 500 closed sessions, in `closed-sessions.json` (module `hub::closed_choices`), so they outlive it. A saved session that does not read is left out. The file is written at each close, without waiting for the disk.
- The permission mode of the session file's last prompt. It wins when no agent of the daemon closed the session, or when the prompt came more than 10 seconds after the close, as when the session went on in a terminal. The 10 seconds are the time the closed `claude` has to exit. An undo always gets the kept choices. Claude Code writes the mode in force on each user line (`permissionMode`), with its `timestamp`. Only user lines count: Claude Code also writes a `permission-mode` line, which a `claude -p` run copies again at its exit without updating it. The model and effort then stay the defaults: the file records the model a reply used, not whether the user picked it.

The bypass permissions mode comes back only for the app's undo, a resume within 30 seconds of the close. A later resume gives the default mode, so that it never skips the permission checks unasked. From the file, the plan mode gives the default mode too, since the file does not record a plan's approval. An unknown mode name gives the default mode. A recorded default mode goes as `--permission-mode=default`: without the flag, the settings' default mode would apply.
A fork names the session it forks until its `claude` reports its own, so `sessions.resume` of that session never opens the fork: it resumes the agent that has the session, or makes a new one.
It reads the session file before the agent exists, through the same conversion as `claude`'s stdout (`PastTranscript`).
The newest 2,000 items show, with ids up to 2,001. The agent numbers its own items from 2,002, so the ids never clash.
The past items go in no event: `agent.history` serves them. A restarted daemon reads the same bytes again, since Claude Code only appends to the file.

### Accounts

The daemon keeps the accounts in `settings.json`, outside the `Settings` that `settings.set` replaces, so an app that predates accounts cannot erase them. Such an app speaks protocol version 1, and the daemon refuses it (see [Session](#session)).
An account's folder must be absolute and be no other account's, the default one's included, whatever its case on Windows and macOS. The default folder keeps part of its login outside it (`.claude.json` in the home folder, the Keychain on macOS), so the same folder named in `CLAUDE_CONFIG_DIR` would be another login.
The default account and an account that an agent uses cannot be removed. Ids are never reused.

The user signs in by running `claude auth login`, with `CLAUDE_CONFIG_DIR` set, in a terminal on the daemon's machine. The daemon writes this command for a POSIX shell, or for PowerShell on a Windows daemon, and names the program it runs (module `process::program`).
On Windows, it names plain `claude` only when PowerShell's own lookup finds the same file: PowerShell runs an npm install's `claude.ps1` first, and its execution policy may refuse it.

An account counts as signed in when `.credentials.json` exists in its folder, unless one of its agents waits on a sign-in: that agent's turn failed on the login after the file was last written, as when Claude Code could not refresh an expired OAuth session. The daemon never opens the file.
The wait of an agent whose account takes the daemon's API key or token does not count: its turn never tried the file.
On macOS, the login lives in the Keychain, so a missing file means `unknown`, and so does a file an agent waits on: a new sign-in may be in the Keychain.
A Console or `apiKeyHelper` login may keep its key elsewhere, so the check can say `not_signed_in` although `claude` works.
An API key never passes through the cockpit.

An agent without a valid login fails its first prompt with `authentication_failed`. The daemon shows the sign-in command in place of Claude Code's text, which names a command the cockpit has no place for.
The agent then needs you until a message goes to `claude`, or until `accounts.list` or the hub's check every 3 seconds finds the credentials file written after the failed turn.
A Keychain login or `environment` credentials leave no file to find, so only a message ends the wait.
The hub's check also sends `AccountsChanged` when an account's sign-in state changes.

Claude Code also marks a 403 from Cloudflare, in front of the API, `authentication_failed`. No sign-in fixes it, and it passes, so the daemon shows it as a refused request.
The daemon does not pass `--enable-auth-status`: its lines report a login in progress, not a missing one.

### Subagents for the agent map

The daemon builds `AgentDetails.subagents` from `claude`'s lines (module `transcript::subagents`).
An `Agent` or `Task` call starts a subagent, and the lines whose `parent_tool_use_id` names that call are its own.
A background subagent ends with its task, since its call returns at once. Claude Code may start a subagent in the background without `run_in_background` in its input, and its record of the call then says `"status": "async_launched"`: the daemon reads that, and drops the call's output, which holds only notes for Claude. The `<result>` of the task notification that ends a background subagent becomes the call's output. The turn's end stops the turn's foreground subagents.
The list keeps the running subagents and the latest ended ones, 200 in all.

`agent.stop_task` stops a subagent with a task. The control protocol cannot stop one foreground subagent without a task: only `agent.interrupt` stops it, with the whole turn.

### Machine load

`machine.load` answers the last five minutes of samples, for the server and each agent. A task samples every 3 seconds, only while clients ask.
On Linux, an agent's processes are its `claude` and the processes under it, found through the kernel's children lists, so the daemon reads no other process.
An agent's CPU includes the time of the children each process reaped, so a command that starts and ends between two samples still counts.
The `/proc` files stay open between samples, since a read costs about a tenth of an open. A file opened under `/proc/<pid>` stays bound to that process, so a reused pid is never read in its place.
The daemon walks at most 4,096 processes, and keeps at most 4,096 files open, no more than a quarter of its open-file limit.
On Windows, an agent's processes are those of its Job Object.

`agent.stop_process` ends one of an agent's processes, but never `claude` itself.
It checks that the process is the agent's, then acts through the same pidfd on Unix, or the same handle on Windows, so a reused pid is never hit.

### Terminals

`agent.terminal_open` starts a shell on a PTY in the agent's folder, with `TERM=xterm-256color` (module `hub::terminals`, crate `portable-pty`). The request names one of the shells `agent.terminals` lists, or none for the default one (module `hub::shells`). On Unix, the default is `YETACK_SHELL`, else the user's login shell from the system's user database, else `$SHELL`, else `/bin/sh`, and `SHELL` is set to the shell in its environment. `$SHELL` comes after the login shell because a `nix develop` shell sets it to a bash built without readline. The release daemon is a static musl build: its user database is `/etc/passwd`, or `nscd` where it runs. On Windows, the default is `YETACK_SHELL`, else PowerShell 7, else Windows PowerShell, else `cmd.exe`.
`agent.terminals` lists the agent's terminals, and the shells a terminal can run with the default one marked. On Unix, they are the interactive shells of `/etc/shells` (bash, zsh, fish and a few more, but not `sh`, `dash`, `rbash`, `tmux` or `nologin`), one per file, and the default one. The default one keeps its own name even when a listed shell is the same file, since a shell can act on its name: bash named `sh` is a POSIX shell. On Windows, they are PowerShell 7 (on `PATH`, else in Program Files), Windows PowerShell, `cmd.exe`, Git Bash (started with `--login -i`), WSL when it has a distribution, and `YETACK_SHELL`. Finding them reads files, so a task finds them under the limit of `blocking`, and the hub answers with the terminals open by then. The daemon runs only a shell of that list: it refuses another program with `<path> is not a shell of this machine`. The two fields are optional, so a daemon or an app that predates them speaks the same protocol: an older daemon lists no shells, and the app then offers no choice.
The shell enters the agent's folder as it starts, so it starts on a blocking thread with the limit of an agent's start: 10 seconds, counted with the folder's other reads (module `blocking`). Past the limit, the request fails with `the folder <path> did not answer`. The hub takes the terminal once the shell runs, and only then answers and tells the clients. A shell that starts past the limit, or after its agent closed, ends at once.
Three threads serve each shell, since a PTY's reads, writes and wait block: one reads the output, one writes what the user types (`terminal.input`), and one waits for the shell to end. The first two start once the hub takes the terminal, and the third reports nothing before then, so no output or exit reaches the hub before it has the terminal.
The hub keeps each terminal's last 512 KB of output, and sends what comes to every client as `ServerMessage::Terminal`.
That message has no `Seq` and no log: a shell can write megabytes a second, and an old screen is worth nothing in a replay.
A client whose queue is full misses it rather than being dropped. Output also skips a client whose queue is half full, since an event that finds the queue full drops the client.
Each output carries its byte `offset` in all the shell wrote. `terminal.attach` answers the kept output and its offset, so a client that shows a terminal again writes the kept output, then only the output past it.
The answer also lists the sizes the PTY took over the kept output (`TerminalSizeAt`), each from the byte it took it at, at most 256: its opening size, then each `terminal.resize`. A shell lays its output out for the PTY's size, as its cursor moves and its line ends show, and a client that wrote it all at another size would scatter it. A size taken before any more output replaces the one before, and the sizes no kept byte was written at go with that output. The field is optional, so the protocol's version stays: an older daemon sends none.
A terminal stays, with its output, after its shell ends, until `terminal.close`. Removing the agent closes its terminals, and stopping the daemon kills their shells.

### Server and peer checks

The daemon listens only on the Unix socket: no TCP port.
Before it reads any byte, it checks that the peer runs as its user (`SO_PEERCRED`), and that the peer's parent chain in `/proc` does not reach the daemon, so a command an agent runs cannot drive the daemon.
The walk accepts at a process of another user or one that `/proc` hides: the daemon's descendants all run as its user, and `/proc` always shows a user's own processes.
`yetack bridge` descends from `sshd`, not from the daemon, so it passes.
The daemon in the app has no socket to check (see [The daemon in the app](#the-daemon-in-the-app)).

## Client (`cockpit_client`)

- `Transport::Local` connects to the socket, and `Transport::InApp` to a daemon in the same process.
- `Transport::Ssh` runs `ssh -T -x -o BatchMode=yes -o ForwardAgent=no -o ClearAllForwardings=yes -o ControlPath=none -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -- <host> '<remote_command>' bridge`.
  The keepalives end ssh after a minute of silence, so a dead link shows as a lost connection.
  `BatchMode` makes a missing key fail at once, since the app cannot show a password prompt.
  The forwarding options forward nothing to the laptop, whatever the user's ssh config asks.
  `ControlPath=none` keeps the app out of the user's shared ssh connections, so neither cuts the other off, and the user's port forwards never run in the app's ssh. `ControlMaster=no` would not do: ssh still joins an existing master. A `ProxyJump` hop runs a second ssh, which these options do not reach, so that hop can still share a connection.
  `--` keeps ssh from reading the host or the command as an option, and the quotes keep the host's shell from reading the command.
- `Connection` reads the daemon at all times, so a response never waits behind events the app has not read.
  Up to 4096 events wait for the app. When the queue is full, they are dropped and `next_event` returns `CloseError::EventsLost`.
- `Session` reconnects with backoff, sending the last `Seq` it saw. After lost events, it sends none, so the daemon answers with a snapshot.

## Desktop app (`cockpit_desktop`)

- **Config** (module `config`): `~/.config/yetack/config.toml`, per machine, names the daemon to reach:

  ```toml
  [daemon]
  transport = "ssh"            # or "local", or "in_app"
  host = "build-box"
  remote_command = "/opt/yetack/bin/yetack"
  ```

  - `transport = "local"` takes an optional `socket`, and `start_service = true` to start the background service when the socket does not answer. `transport = "in_app"` runs the daemon in the app.
  - A missing file, or a file without `[daemon]`, means the local daemon at its default socket. `Config.daemon` is an `Option` because the "No daemon answers" screen treats a start without the section as the first one.
  - A `config.toml` that is a symbolic link (home-manager, a dotfiles repository) stays one: `save_daemon` writes the link's target, through a temporary file beside it with the target's mode. A read-only config, such as a link into the Nix store, is not written.
  - Each `[[machines]]` table names another machine the app reaches at the same time, over SSH. An SSH machine takes an optional `source_dir`: the git checkout its daemon's update builds from, `~/yetack` when unset.
- **Several machines** (module `machine`): `[daemon]` is machine 0, this computer ("Local"), and each `[[machines]]` table the next number.
  Each daemon numbers its own agents, so an agent's id in the app holds its machine in the high bits and the daemon's id in the low 20. Machine 0's agents keep the daemon's ids.
  Each machine has its own connection task, which translates the ids (`map_agents` in `cockpit_protocol`). A router hands each request to its agent's machine, or to the machine it names (`Outgoing.to`). `AppState.machines` holds each daemon's `MachineState`. Account ids collide across machines, so every account function takes the machine. An SSH machine that does not answer is tried again every 10 seconds.
  The Machines dialog (`ui/machines.rs`) adds, edits and removes servers, and the connection task writes the list back (`save_machines`). Removing a server leaves its agents running there. A renamed server keeps its preferences (`Prefs::rename_machine`).
  A new server with no daemon path gets a setup (`state/server_setup.rs`). It finds `yetack` with the update's probe, or installs a daemon the app carries or downloads with `contrib/install-daemon.sh`. The script runs the daemon as a systemd user service, or else with `setsid`, apart from the SSH session. Then it also writes `~/.config/yetack/start-daemon`, which an update runs to start the new daemon. The setup lives in `state.machines.setup`, so closing the dialog does not stop an install.
  A server that answers SSH while no daemon runs there offers to start it with the same script (`state/daemon_start.rs`). This is a last resort: a service, or an `@reboot` line in the user's crontab, keeps the agents working while the app is closed and after a reboot. So after such a start, the app offers to add that line when nothing starts the daemon at boot. The script leaves the crontab alone when `crontab -l` fails for another reason than an empty crontab, since a rewrite would lose the user's lines, or when the line holds a `%`, which cron reads as the end of the command.
  Only machine 0 has the "No daemon answers" screen. With servers configured, it shows as a dialog, so the servers stay usable.
- **Reaching the daemon** (module `reach`): the connection task turns the config's daemon into a transport.
  For `in_app`, it starts `InAppDaemon` with the default state folder (`InAppHost` keeps it), or connects to the socket when another daemon holds the lock.
  For `start_service`, it starts the service unless its socket answers.
- **"No daemon answers"** (`state/no_daemon.rs`): when the first connection to the daemon fails, the window shows this screen in place of its panes. It offers Retry, running the agents in the app, and starting the background service (Unix only).
  A choice holds for this run, unless "Use this every time" is ticked. Then `save_daemon` writes it into `[daemon]`, keeping the file's other settings and comments.
  The box starts ticked only when the config has no `[daemon]` section, since the choice is then a first setup. Otherwise it starts unticked, so the screen does not replace the config's choice unless the user asks.
  A connection that drops after it connected reconnects with no screen.
- **Where Local's agents run from the next start** (`state/settings.rs`): Settings offers "In this app", "Background service" and "Ask" (`StartDaemon`), which `save_daemon` writes as `in_app`, `local` with `start_service`, and `local` without it, keeping a socket the config names. A file without `[daemon]` shows as "Ask" (`Machines::configured` is `None`).
  The router writes the file (`MachineChange::LocalDaemon`) unless it names that daemon already, so quick clicks end on the last one. A file without the section takes even "Ask", so that the screen's box starts unticked from then on.
  Windows has no background service (`SERVICE_OFFERED`), so there the agents always run in the app.
- **Updating a machine's daemon** (`state/daemon_update.rs`, `state/update_plan.rs`, `server_daemon.rs`, `service_update.rs`, `contrib/update-daemon.sh`): the app updates every machine's daemon the same way, this computer's included.
  `MachineState.version_gap` (`GapKind`) compares the daemon with the app. A daemon of an older protocol refuses the app until it is updated. A daemon of an older release, or of the same release and another known commit, stays in use and can be updated: two commits of one release have no order the app knows. A newer daemon gets no update.
  Releases compare as Cargo versions, against `cockpit_daemon::RELEASE_VERSION`. The app's commit is the one `cockpit_daemon` was built from, since the app builds the daemon from its own tree. Without one, it offers no update. `build.rs` reads it from `YETACK_SOURCE_COMMIT` (empty under Nix for a tree with uncommitted changes), else from `git rev-parse HEAD`. It does not read `YETACK_COMMIT`: `cargo run` sets that for the app, and the agents the app starts would build with it.
  - Plans (`UpdatePlan`, set per machine by the connection task):
    - Send: the app sends a Linux daemon it carries for the server's CPU (`server_daemon::carried_daemons`), and the script checks and installs it.
    - Download (`release_daemon.rs`): an app that is a release's own build may download that release's daemon, checked against `SHA256SUMS`, to its own `.part` file so two downloads do not mix. It is that build only when GitHub says the release tag names exactly its commit: the script checks the daemon's version against the app's build, and a build between two releases has the earlier one's version.
    - Build: with no daemon to send, or after a failed download, the script builds the app's commit on the server.
    - Service (Unix): for this computer's background service, or a daemon at a state folder's socket, the app stops the daemon that holds the folder's lock and starts the service with its own program. The daemon in the app needs no update.
  - Who owns the daemon: before a server's update, a probe (`server_daemon::probe`) reads the server's system and its daemon's resolved path, version and owner (`DaemonOwner`: Nix, a package, the app, or other). The app owns a daemon in `~/.local/share/yetack/bin` or `~/.local/bin`, in a folder the user can write. It replaces only its own daemons, and never runs `sudo`.
  - Busy agents: a restart interrupts the agents that work, wait for an answer (other than a sign-in) or run background tasks. It also loses the resting agents' scheduled jobs, so an agent that may hold one counts as busy. The app asks first when agents are busy, and always while it cannot see Local's agents (`agents_unseen`). A wait for idle agents ignores scheduled jobs, since one may never end. The update checks again as it starts (`BusyAgents::Refuse`), unless the user chose "Update now" (`--even-if-busy`).
    A server's script reads `yetack list`, finding its columns by their headers, since an older binary's list lacks some. On this computer, the app reads the job holders from the resting agents' raw logs (`service_update::read_job_holders`), since an older daemon could not send them. That read grows with the logs, so the restart then checks again whether an agent started a turn.
  - This computer's service (`service_update.rs`): `restart` stops the daemon with `cockpit_daemon::stop_lock_holder` (see [The holder of `serve.lock`](#the-holder-of-servelock)), starts the service, and the app reconnects. Nothing cancels it.
    A program such as a systemd unit may restart the old daemon the moment it stops, and the app may reconnect to it before the stop returns. So the update judges the connections made since its stop started (`restart_ended`), and fails at once when one is another build (`OtherBuildAnswers`).
  - The update's state (`MachineState.update`) is per machine and outlives the connection's attempts, which an old daemon refuses while the build runs. A connection to the app's build ends any update. Otherwise a server's update lasts until its script ends, since the app may have reached the old daemon again. A daemon newer than the app ends the failure screen, since "Update again" would install an older version. A server's settings cannot be edited while its update runs.
  - A server's update runs in its own task (`update_server` in `connection.rs`), over plain SSH, since an old daemon refuses the app's protocol. A send streams the daemon to a receive script, which writes it to a new file (mode 600) in `~/.cache/yetack/sent/` and deletes what earlier sends cut short left there. Then `contrib/update-daemon.sh` runs from stdin (`ssh <host> sh -s`). Before anything changes, it checks a sent file's checksum and `--version`.
    The probe and the receive script go as the argument of `sh -c` (`cockpit_client::ssh_inline_command`), so that stdin stays free for the file. The server's other users can read the arguments, so these scripts hold nothing private. They hold no single quote and no backslash: Windows' ssh breaks the quoting of a remote command that holds one.
    "Cancel the update" runs the script with `--cancel` over a second ssh, until the restart starts. The running script then stops before it installs.
  - The script's safety rules:
    - Its lock, `lock.pid`, holds the owner's pid and start time, which tells a reused pid apart. It is moved into place from a temporary file, so no lock shows without its owner. A dead owner's lock is free, so a reboot or a SIGKILL blocks nothing. `flock` on `lock.pid.guard` makes reading and replacing the lock one step, so exactly one of several updates takes it.
    - It finds the daemon's pid from the `FLOCK` line on the inode of `serve.lock` in `/proc/locks`, comparing the inode only, since stat may give another device (as on Btrfs). This works because every daemon, an older one too, holds that lock in its own process: the kernel drops a dead process's lock from `/proc/locks`. Some sandboxed kernels list no locks, so the script first checks that a lock of its own shows, and refuses otherwise.
    - When it stops the daemon itself (with a start file), it signals the lock's holder and waits up to 30 seconds for the lock to be free. It rereads the holder's start time right before `kill`, so a pid that another process got since gets no signal.
    - When no daemon answers after the restart, it puts `<binary>.previous` back and restarts that. It does not roll back when another binary runs, since that would change nothing. A retry reads the agents with `<binary>.previous` when `<binary>` cannot, since a failed restart can leave the old daemon behind a new binary.
    - Over SSH without a terminal, a client that goes away only closes the pipes, and sends no signal. So the script ignores SIGHUP and SIGPIPE, and cargo writes through `tee` to `build.log`, so a lost connection cannot end the build or the install.
  - Exit statuses, each with its own failure screen:
    - 1: the old version still runs (after a failed check or send, or a cancel).
    - 3: busy agents refused the update.
    - 4: the daemon may not have come back, or another binary runs.
    - 5: the new version did not start, and the old one runs again.
    - 6: another update holds the lock (the user may close the app during an update and start another).
    - Any other, such as a lost ssh connection: the outcome is unknown.
- **The background service**: the app runs its own program again, as `<program> --state-dir <folder> serve`, so any build of the app can start it.
  It runs in a process group of its own, so Ctrl+C in the terminal that started the app does not reach it, and it outlives the app. Its stderr goes to `serve.log` in the app's state folder.
  The app waits up to 20 seconds for its socket. When the program is missing or the service stops at once, it shows the screen again with the log's last line.
  An update restarts it with the app's program (see "Updating a machine's daemon").
- **Closing the window**: the event loop ends the process right after its last event, without dropping anything, so the app stops the daemon in the app in that event (`LoopDestroyed`), waiting at most 5 seconds after the agents are killed.
- **The app's own update** (`state/app_update.rs`, `self_update.rs`, `ui/app_update.rs`): the app tells when a newer release is out. Only the Windows zip replaces itself: the build sets the install kind (`InstallKind`, from `option_env!("YETACK_PACKAGE")`).
  - The check asks GitHub's API for the latest release every 24 hours while its setting is on, and at once on "Check now" in Settings › About, whatever the setting. It goes over HTTPS through rustls, which trusts the system's root certificates. It reads at most 4 MB and sends only the `User-Agent` and `Accept` headers. The tag may hold only letters, digits, `.` and `-`, since the card shows it in commands to copy.
  - The Windows update downloads the release's Windows program, its Linux daemons for x86_64 and aarch64 and `SHA256SUMS`, and offers no update for a release that lacks one of them, since the zip carries both and a daemon left at the old commit fails a server's check. It downloads them only from under `https://github.com/Henauxg/yetack/releases/download/`, at most 200 MB together, as the release lists their sizes and as they download, and checks each file's SHA-256. A file may take 30 minutes at most, and 30 seconds without data end its download. It renames the running program and the daemons beside it to `<name>.old` and moves the new files in (`put_in_place`), since Windows lets a program rename its running file and not write over it. A failed move puts back the files moved so far. When they cannot all go back, the update deletes nothing, since one may be the only copy of the app. A `RunId` tells the runs apart, so a cancelled run's late news changes nothing.
  - The new program starts with `--after-update <pid>`, waits up to 30 seconds for the old app to end, puts back an `.old` file whose place is empty, and deletes the rest. Until the restart, the daemons beside the app are the new release's, so a server's update sends the `.old` one, which matches the running app (`server_daemon::carried`). A daemon the zip did not have, such as the aarch64 one in a zip from before it carried both, has no `.old`: while the app's program waits as `.old`, the app carries none for that CPU.
- **A panic** (`stop_on_panic`): while the app runs agents itself, the panic hook stops the daemon in the app and exits with status 101. Otherwise the agents' processes would outlive the app, and the next start would resume their sessions a second time.
  Only the first panic stops the daemon. The stop waits at most 1 second for the host's lock, which the panicking thread may hold. A panic on the window's thread waits for the stop, since that thread's unwinding would end the process. A panic on another thread unwinds at once, so its task's drop kills the processes the task holds.
- **Log** (module `logging`): the app logs to stderr, at the level `RUST_LOG` sets (`warn` by default). A release build on Windows has no stderr, so it logs to `app.log` in its state folder, rotated at 10 MiB over 5 files. A new run appends, so the rotation carries on across runs. A failed rotation (another program holds a file) tries again 10 MiB later, so no line is lost.
- **Notifications** (module `notifications`): at most one per agent, when it needs you or finishes. None shows while the window has the focus.
  An agent finishes once its turn has ended and no background task runs. After the last background task ends, the app waits 10 s (`FINISH_HOLD`) before it announces the finish: Claude Code usually starts a turn to read the task's result, and that turn's end is the real finish.
  The events a replay brings after a reconnect (up to `replayed_through`) raise no notification, since they happened while the app was away. They still mark a finished agent unread. A finish they hold marks it unread when its 10 s are over, without a notification. With a daemon that does not send `replayed_through`, every event can raise one.
  Levels (`state/notification_levels.rs`, saved in `prefs.json`): one for the app and one per agent, each "All", "Only when it needs you" or "Off". An alert shows only when both let it through. A change of level, or the window getting the focus back, reaches every machine's notifier (`ToDaemon::Notifications`), which closes the notifications no longer let through. Removing an agent drops its level, since a later agent may take its id. The config's `[notifications] enabled = false` sets the app's level until the user picks one in the window.
  On Linux they go through the freedesktop service, so Do Not Disturb holds them back.
  On Windows they are toasts from notify-rust. The app has no Start menu entry, so toasts show PowerShell's name and icon. A click reaches the app only while the toast is on screen, and a toast cannot be withdrawn.
- **Dictation** (`voice_desk.rs`, `voice_engine.rs`, `voice_models.rs`, `state/voice.rs`, `ui/voice.rs`, `ui/voice_tab.rs`, and the crate `cockpit_voice`): the user talks, and the text goes in the message, a quote's comment, a question's "Other" answer, or the new agent's first message. [docs/voice/investigation.md](voice/investigation.md) measured the choices below.
  - The talk key (`Command::Talk`, Ctrl+Space by default), held, records while held; tapped (under 300 ms), until pressed again. The mic before Send starts and stops a recording too. Enter stops it as the talk key does, and the next Enter sends (`KeyMode.enter_stops`). Esc drops a recording and its text. The page script takes the talk key itself (`KeyMode.talk`), since the other shortcuts send no key release. When the window loses the focus, the script stops a held recording, since a key that comes up in another window sends no release.
  - `cockpit_voice::Voice` owns a worker thread, which loads the models at the first recording and drops them after 5 minutes unused. It records the microphone chosen in Settings, or the system default, through `cpal`, evens out its level (the model misses quiet speech), resamples it to 16 kHz, and cuts the speech at pauses of 400 ms. The speech model (8-bit GGUF) transcribes each finished segment once, while the user goes on, and Silero VAD skips the segments without speech, on which a model can invent words. The text drops filler words (um, uh). The live text comes from transcribing the current segment again, at a step that follows the computer's speed: every half second on a fast CPU, every second or two on a slower one.
  - The language in Settings picks the speech model (`DictationLanguage`, `cockpit_voice::SpeechModel`): English, the default, takes Parakeet unified EN, which knows only English and so never switches language. 25 languages takes Parakeet Ultra, which picks the language of each segment and can write an English sentence in French. Both cost the same to run ([voice/investigation.md](voice/investigation.md), "English-only models"). A recording asks for the model of the language chosen, and the worker loads it in place of another.
  - A microphone that stops (`StreamInvalidated` or `DeviceNotAvailable`) gives way to the one the worker opens then, and the recording goes on with its text and its level control, and a new resampler for the new rate. On Windows a new default microphone, such as a headset plugged in, invalidates the stream of the old one. Other errors, a failed opening, or 8 openings in one recording end it with the text so far. A chosen microphone that is missing, at the start or later, gives way to the system default, and the card above the mic says so (`Event::Fallback`). Settings lists the microphones by name, off the interface thread, each time its Voice tab opens. The name is kept in `VoicePrefs.microphone`.
  - The worker's events reach `voice_engine`'s task, which updates `VoiceState` and the level, two signals of their own: the level changes 30 times a second, and only the meter reads it. The strip above the message shows the level, the time and the text, the words since the last pause lighter.
  - While a dictation runs into an agent not on screen, that agent's sidebar row shows a red mic in place of its status dot, and the agent on screen shows a one-line strip above its message (`AwayStrip`): the agent dictated to, a button back to it, and the talk key and Esc. Enter there sends that agent's own message. Closing the agent dictated to, or its removal by the daemon, cancels the dictation, whose text would have nowhere to go.
  - The final text goes in at the caret of the message, as one step of its undo history, when the message of the agent it was dictated to shows. Otherwise it goes at the end of that agent's draft. "Insert and send" (`AfterDictation`) puts it at the end and sends the message.
  - A recording started in a quote's comment field, by the talk key or the comment box's mic, writes into that comment (`Destination::Comment`, with the opening of the field). Its level and words show under the field, and its text is never sent. When the comment closes or another one opens, the recording stops and its text goes at the end of the message, unsent.
  - The "Other" field of a question card in the chat takes dictation the same way (`Destination::Answer`, with the question's item and index), from the talk key or the field's mic. The words show under the questions, so another tab leaves them on screen. The text goes in at the field's caret, or at the end of the draft's answer while the agent is not on screen, and it is never sent. Once the question is answered or withdrawn, the recording stops and its text goes at the end of the message. Enter anywhere on the card, with or without a modifier, in the message or on the page, stops the recording rather than answer, and so does a click on Send: the next one sends the answer with the words in. The agent map's cards take no dictation.
  - The new agent dialog's first message takes dictation too (`Destination::NewAgent`), for no agent yet: the talk key works in that field, and its mic has the card above it. Enter or Ctrl+Enter there, Enter in the dialog's other fields, and a click on Start stop the recording, and the next one starts the agent with the words in. The dialog's other tab, or "Accounts" opened from it, stops the recording, and the text goes at the end of the kept message. Closing the dialog drops the text, since the next opening starts from an empty message.
  - `voice::field_state` tells whether the field a dictation writes into is still open, covered by a dialog, closed, or gone with its agent or its dialog. `voice_desk` watches it: a field that closes or that a dialog covers stops the recording, and one that is gone cancels it. A field under a dialog takes the last words without the focus, which the dialog keeps, and a message there is never sent.
  - Esc during a dictation cancels it and does nothing else: the page's script stops it before a palette or a dialog sees it. The last words of a dictation a dialog stopped land anyway, and Esc then goes to the dialog.
  - The models are not in the release. A dictation in a language whose model is missing offers its download in a card above the mic. Settings' Voice tab lists the two languages, a row each with its model and the model's state: Download in the row of the language chosen, the download's progress, or Remove once the model is on disk. Silero goes with the last speech model. The files are pinned to a commit of their Hugging Face repositories, in `cockpit_voice::SpeechModel::files`, and go to `voice-models` in the app's state folder. Each file downloads through a `.part` file beside it, which takes the file's place only once its SHA-256 matches, and a `.sha256` marker beside it then tells a whole file at the next start without hashing 700 MB again.
  - The C++ builds for any x86_64 CPU with AVX2 and FMA, not for the building machine's (`TRANSCRIBE_CMAKE_ARGS` in `.cargo/config.toml`), and `cockpit_voice::supported` hides dictation on an older CPU. On Windows, `UseMultiToolTask` there makes Visual Studio's build compile its files in parallel.
- **Paths** (module `path_style`): the app follows the path rules of the daemon's machine, since the app on Windows may reach a Linux daemon over SSH. `PathStyle` works on text, so the Linux tests check both styles. A path's root tells its style. The new agent dialog's folder field has no agent yet, so it takes Unix over SSH (only Unix runs `yetack bridge`) and the app's own platform otherwise.
- **Preferences**: `prefs.json` in the app's state folder (see Platforms) keeps one viewer's choices. A missing or broken file means the defaults. The demo reads and saves none, since its agents take the ids of real ones.
- **Splitters** (`ui/splitter.rs`) resize the sidebar, the file view and the files pane. A page script follows the drag, and Rust hears of the new size once, on release.
- **List of open files** (`state/file_list.rs`, `ui/file_list.rs`, `Popover::FileList`): a button after the file view's tabs lists the open files, with a filter.
- **Back and forward in the file view** (`FileView.back` and `forward`, `FilePlace`, `actions::travel_files`): each other file shown is a step, a tab switch, a jump to a line from a link and a commit shown from a file's history included. Back goes to the place shown before, with the line a link marked, the commit it showed and its mode once settled (chosen, read, or brought back: a place left before its first read picks its mode again), and opens again a file whose tab closed; over the terminal's tab, it first shows the file under it; Forward goes to the place Back left, and a file opened since ends the way forward. Over the file view and the files pane, the mouse's back and forward buttons do it, and Alt+Left and Alt+Right outside text fields and dialogs (`Reach::Page`, bindable in Settings), so that Option+Left still moves by word in the message on macOS. The view keeps the last 50 places, for the session, and drops them when it closes. The page script takes the side buttons everywhere, so the web view never leaves the page. The scroll is not kept.
- **Tab menu** (`state/tab_menu.rs`, `ui/tab_menu.rs`, `Popover::TabMenu`): a right-click on a tab, or on its row in the list of open files, opens a menu of actions on the file and the tabs. "Show in the files pane" is off for a file the tree never shows (`file_tree::can_show`).
- **Messages on their way** (`state/sending.rs`): a sent message shows at once, dimmed, since the daemon's echo takes a round trip. The echo replaces it when it has the same text and as many images. A refused message's draft comes back.
  A message to a busy agent can wait in the app for a few seconds (`chat.steer_hold`, off by default), so the user can cancel it: once sent, Claude Code holds it and no request removes it. Held messages leave in the order sent. One still held when the app quits is not sent.
- **Esc and Stop** (`ui/composer.rs`, `state/shortcuts.rs`): one button is Stop while the agent is busy and the message empty, and Send otherwise. For a second after a send it stays an inactive Send, so a double click or the next key never stops the agent.
  Esc first closes what is open: the comment field, a menu or palette, a dialog, the find. Then, on a busy agent, it opens "Stop the agent", or stops at once when `chat.esc_stop` is `at_once`. Esc in another field stops nothing. A character typed into that dialog closes it and goes into the message, and the agent keeps working. Keys with Ctrl, Alt or Meta leave it open, since a shortcut may take them.
- **Files pane** (`state/file_tree.rs`, `ui/files_pane.rs`): it shows the selected agent's folder as a tree. Each agent keeps its own tree (`AgentEntry.tree`), and whether its pane is open (`FilesPanePrefs.agents`, by machine name and agent id, dropped with the agent).
  - "All" lists a folder with `agent.list_dir` when it opens, then the outside folders where the agent changed or read a file. Such a folder shows only those files unless it is a git checkout's top folder, so that a folder such as `/etc` does not bury them.
  - "Also worked in" lists each of its folders once even while closed: a folder the daemon says no longer exists (`Listing.gone`), as a worktree removed since, leaves it. It comes back, and lists again, once a later chat item has read or written a file in it. F5 and the pane's refresh list it again too, but an agent's step does not, so that a dozen removed worktrees do not list after each command. Its "Clear" hides the folders it lists, kept per agent in `FilesPanePrefs.cleared` with the agent's last chat item then: a folder shows again once a later item has read or written a file in it. `file_tree::footprint` leaves the hidden folders' files out, so every view and count of the pane follows. The header's changes chip still counts them.
  - The shown listings are read again when the agent finishes an edit or a Bash call, when it comes back on screen, and on F5.
  - `file_tree::reveal` keeps the row to show in `FileTree.reveal` until it shows, since a folder may wait for its listing.
- **Docks** (`state/docks.rs`, `ui/tab_drag.rs`): two panels of tabs. The side dock, where the file view shows, holds the agent's files and the terminals moved there. The bottom dock, under the chat, holds the other terminals. `AgentLayout` keeps, for each agent, the dock each terminal sits in, and each dock's order and pinned tabs.
  - A drag moves a tab along its row, or a terminal's tab to the other dock's row. While a terminal's tab is dragged, a closed dock shows an area to drop it on, which opens the dock. A file's tab stays in the side dock.
  - The pinned tabs come first, then the tabs the user moved, then the others in the order they came. A pinned tab shows a pin in place of its "×", and closing a group of tabs from the tab menu leaves it open. The file tab menu and the terminal tab menu (`Popover::TerminalTabMenu`) pin and unpin.
  - `state/saved_files.rs` saves each agent's files and layout in `FileViewPrefs.agents`, by machine and daemon agent id, after each change, and restores them when the app lists the agent: the shown file and its mode, the files opened besides the changed ones, the changed files closed, and the layout. Closing the file view closes its changed files for good: opening a file later does not bring their tabs back.
- **Terminals** (`state/terminals.rs`, `ui/terminal_pane.rs`, `terminal_feed.rs`): each terminal is a tab of a dock. `UiState.terminal_docks` holds the docks whose terminals show. It is not saved: terminals open hidden. A new terminal opens in the dock whose "+" opened it. A terminal with no dock saved, as one opened from another window, goes to the agent's default dock (`TerminalPrefs`, by machine and agent). Moving a terminal makes its dock the agent's default, and the default of the agents the app lists later, as those of a server still connecting. A bottom dock that shows with its agent draws at once (`terminal-instant`). The header's terminal button and Ctrl+ù hide the terminals on screen, else show both docks' terminals. While terminals show, the app lists the agent's terminals and opens one for an agent with none. The page's script puts each dock's shown terminal in that dock's screen.
  - In the side dock, opening a file shows the file. The shortcut first brings a terminal back over it, then hides it. The side dock's "×" closes its files and hides its terminals. Ctrl+W in the terminal goes to the shell, not to the file view. While a terminal is over a file, that file is not the shown one: the files pane and the list of open files do not mark it, and its find waits, taking no key, until the file shows again.
  - A `path:line` in a terminal's output underlines under the pointer, and Ctrl+click opens it in the file view. The page sends the row's text, one code point per cell, and the app finds the references with the chat's rules (`render::line_refs`) and answers their columns. The page drops an answer for a row that changed meanwhile, and a reference that a soft wrap cuts. A wide character fills its second cell in that text, so a path right after it is cut, as in the chat. A relative path starts from the agent's folder: the app does not know the shell's current folder. Ctrl+click on a web address, or on a link a program marks (OSC 8), opens it in the browser. Only http and https links take the pointer and Ctrl+click, and the pointer over one shows its address. ghostty-web still underlines another link a program marks, under the pointer. Selecting text copies it, a click without a drag copies nothing, and Ctrl+Shift+C copies too (ghostty-web would send it as Ctrl+C). Ctrl+V or Ctrl+Shift+V pastes, always through `term.paste`, which brackets the text for a shell that asks, without the escapes the text may hold.
  - The "+" opens a terminal with the default shell, which its tooltip names. When the daemon lists more than one shell, a ⌄ beside it opens a menu of them (`ui/shell_menu.rs`, `Popover::ShellMenu`): each row shows the shell's name, "default" on the default one, and its program, the file's name for a Windows path and the whole path otherwise.
  - A right-click opens the app's menu (`state/terminal_menu.rs`, `ui/terminal_menu.rs`): on a link, open it or copy it first, then Copy, Paste, Select the screen, Clear the screen (Ctrl+L to the shell), a new terminal, the move to the other dock, and Close. The page sends the link under the pointer, found as ghostty-web finds the one a click opens, and runs Copy and Select the screen. WebKitGTK refuses the page's own clipboard read, so on Linux the app reads the clipboard through GTK for Paste.
  - The page owns the terminals: its script keeps one [ghostty-web](https://github.com/coder/ghostty-web) terminal per shell, in a box of its own, and puts each dock's shown one's box in that dock's screen. Moving the pane keeps the screen; a terminal the page has not shown yet asks `terminal.attach` first. `assets/ghostty-web/` holds the library, which carries its WebAssembly inline.
  - Each terminal has a ghostty-web instance of its own: the library gives a new terminal a closed one's memory without zeroing it, and the closed one's text would show in it. The next instance loads ahead, and a terminal opened before it is ready waits for it, a few milliseconds. After 3 failed loads in a row, the page loads no more until a terminal closes: a WebView2 page holds 124 instances (the spare is one), and a closed terminal's instance frees its place. ghostty-web's `dispose` leaves two of the listeners its `open` puts on the document, which would keep a closed terminal and its memory, so the page removes them too.
  - The Settings dialog's Scrollback sets the history of each new terminal (`Scrollback`, saved under `terminal` in `prefs.json`): 5, 10 (the default), 25 or 50 MB, which keep about 3,000, 6,000, 15,000 and 30,000 lines at 120 columns. ghostty-web's `scrollback` is a count of bytes for Ghostty, though ghostty-web calls it lines, so a wider terminal keeps fewer lines. The page gets the size as it starts, since the pane may open a terminal before the script reads its first message, then each change (`ToPage::Scrollback`). A terminal keeps the size it opened with: ghostty-web cannot change it. Its instance's memory grows by doubling, so a full history takes 16, 32 or 64 MB for 10, 25 or 50 MB.
  - The pane draws the terminal's scrollbar itself (`scrollbarFor` in the page script), at the pane's right edge, in the strip that ghostty-web's fit keeps free of text. ghostty-web paints its own on the canvas, over the last column and under the text's pointer shape, so the page turns that one and its click handler off.
  - The page drops the terminal of a closed tab, since ghostty-web draws each terminal on every frame. A gap in a terminal's output makes the page start that terminal again from `terminal.attach`. Each new connection to a daemon lists its agents' terminals again: a restarted daemon numbers its terminals from 1 again, and the terminal updates sent while the app was away are lost.
  - The kept output waits for the terminal's first fit to the pane: before it, the terminal has ghostty-web's 80 by 24. The page then writes each part of it at the size the PTY had when the shell wrote it (`replay`), and gives the terminal the pane's size again. Those sizes stay in the page; the PTY learns the pane's size, when the last one differs. ghostty-web's answers to the queries the replayed output holds, such as the cursor's place, do not reach the shell either, which would read them as typed, except for a terminal this window just opened (`Screen::fresh`): all its output is recent, and Windows' pseudo console holds a starting shell until it learns where the cursor is. ghostty-web emits at most 256 bytes of answers per write, so the replay sends or drops the rest itself (`takeAnswers`). A size past 1000 columns or 500 rows is skipped: that part goes in at the size before. Before the first fit, the output that waits stops at 512 KB, what the daemon keeps: past it, the page drops it and attaches again at the fit (`lose`). A terminal hidden before its first fit does the same, since the output that waits would show much later as live, and the shell would get answers to queries that stopped waiting. A fresh screen dropped this way leaves the next one fresh. The replay cannot help output that the kept 512 KB cuts in the middle of a full-screen program's work.
  - Output goes from the connection to the page apart from the state (`terminal_feed`), since each change of the state renders the window again.
  - The screen carries `data-records-keys`, so the page script leaves every key typed there to the terminal, Esc included. The pane's shortcut in the terminal still hides it. ghostty-web skips a key its custom key handler returns `true` for, unlike xterm.js.
  - The terminal fits its box once the box's size stays the same for 180 ms: the pane grows as it opens, and a fit at each step would give the shell several sizes.
- **Find** (`state/find.rs`, `ui/find.rs`, `ui/find_folds.rs`): the chat and the file view each have a find. `UiState.chat_finds` and `FileView.find` hold which are open. `Drafts.finds` holds the text and the results, so typing renders neither the chat nor the sidebar.
  - A page script searches the text the page shows and highlights the matches with the CSS Custom Highlight API. Each match is a `StaticRange`, which no change of the page has to update. The app drops a report (`FindReport`) about an older text.
  - The chat's folds put nothing on the page until they open. So the app counts their matches from the transcript (`FoldTexts`), and the script opens a fold when the find reaches it. The counts include Markdown marks, so the page counts again once the fold opens.
    The app gathers the fold texts once per change of the transcript (`Transcript::revision`). That number comes from one count for every transcript, so a transcript a reconnect builds again never takes an older one's number.
  - The chat's scroll script scrolls to a match (`REVEAL_EVENT`), so the entry stays in place as the entries above it take their height.
  - Rows the chat holds but has not drawn are not on the page either. The find bar says how many (`UndrawnRows`), and its "Search them" draws them all.
  - The field takes its text when it mounts (`initial_value`), so a render that lags behind the typing never writes an older query over the keys typed since. A Ctrl+F on a selection, or another agent's find, mounts it again.
  - Text that changes every second carries the `find-skip` class (`SKIP_CLASS`): the find leaves it out, and a change in it starts no search.
- **Markdown preview** (`ViewMode::Preview`, `FilePreview` in `ui/file_view.rs`): a Markdown file gets a preview mode, rendered with the chat's markdown component up to `MAX_RENDERED_LINES` lines.
- **Open in browser** (`OpenInBrowserButton` in `ui/file_view.rs`, `state/page_copy.rs`, `page_files.rs`): an HTML file gets a button that opens it in the browser.
  For an agent on this computer, `browser::open_page` opens the file itself. It resolves the path's links first, and the target must be a page, since on Linux the opener picks the program by the file's name.
  For a server's file, the daemon reads (`agent.read_file`, `Purpose::PageCopy`) at most 32 files that the page's `src` and `href` name by a relative address. As an IDE serves a page from its project, it reads only web files (`WEB_EXTENSIONS`) in the page's folder or the agent's, so a key or a shell script stays on the server. `page_files::open_copy` writes the copy to a new private temporary folder (`yetack-page-*`) and leaves it, since the browser reads it after the app is done. A browser in a snap or a Flatpak has its own `/tmp`, so it cannot read the copy: the user accepted that, since Windows and NixOS can.
- **PDF** (`PdfFile` and `PdfCard` in `ui/file_view.rs`, `pdf_files.rs`): the file view shows a card in its middle, with the PDF's name, its size and a button that opens it outside the app, in the browser on Windows and macOS and in the program `xdg-open` picks for PDFs on Linux.
  The webview shows no PDF: its viewer would parse the file beside the app's pages, and WebKitGTK has none.
  The daemon sends the PDF's bytes with each read, as for an image. For an agent on this computer, `browser::open_pdf` opens the file itself, with the checks of `open_page`. A server's PDF opens as a copy in a new private temporary folder (`yetack-pdf-*`), under its own name when this computer takes it and it ends with `.pdf`, else `document.pdf`.
  A PDF the daemon does not send, as one over 8 MiB, still gets the card for an agent on this computer (`PdfSource::unread`), with the daemon's sentence under the button, since the file itself opens without its bytes. The card says only "not sent by the daemon": the app gets the failure as a sentence, and cannot tell a file too large from another failure.
  On Windows, no page or PDF on a network share opens from the file view (`browser::file_url` checks first, with `path_style::is_network_share`): resolving its path would send the share's host a hash of the user's login, and the button under a failed read can name a share the daemon refused.
- **Git** (`state/git.rs`, `ui/file_git.rs`, `ui/branch_card.rs`): the app shows what the daemon reads of git, and runs no git action. `AgentEntry.git` holds, for the selected agent, the state of its checkouts and, for each file the file view showed, its last committed text, its blame, its history and the commits shown. Each is asked for when something shows it, and read again when the files pane's listings are: after the agent's step (an edit marks only the checkouts' state stale, a Bash call everything, since it may commit), when the agent comes back on screen, and on F5. The old value stays on screen meanwhile. A daemon older than the git requests answers "unknown method", and the app then shows what it showed before.
  - The file view's Diff compares the file with its last commit (`DiffBase::Git`), so it shows every change, whoever made it. Outside a checkout it falls back to the agent's edits (`DiffBase::Edits`). The app makes both diffs itself, from the texts.
  - In Whole file, the hovered line shows who last changed it, from the blame of the last committed text that the app maps onto the file with its diff (`git::line_commits`). The text sits in a `data-text` attribute, so a selection, a copy and the find leave it out. It sits in the line's text, after its last character: a wrapped line gives it no width, so showing it never wraps the line again, and a line wider than the view keeps it at the view's right edge. Resting 400 ms on it shows the commit's card, and the card follows the pointer to another line at once.
  - The history button lists the last commits that changed the file. Picking one shows its change to the file (`FileView.commit`) in place of the mode, until Back or another mode.
  - The files pane and the header's changes chip take git's files and counts over the agent's (`file_tree::changes`): every uncommitted file counts, with git's letter. The files pane's and the tabs' menus show a file's history, open its page on GitHub (`git::github_file_page`), and show it in the system's file manager for an agent on this computer (`file_manager.rs`, which refuses a network share).
  - A link to GitHub (`git::GitHubLinks`) goes only to a page GitHub has. A commit, or a file at HEAD, gets one only when the remote held the commit at the last fetch. A branch gets one only through its upstream on that remote.
  - The header names the branch after the folder. A rest on a sidebar row, or on that name, shows the branch's card (`UiState.branch_card`), and the row's menu has a Git submenu. Both ask for the state of an agent that is not selected.
- **Reading the shown file again** (`state/file_reread.rs`): the file view reads its file again when it comes back on screen, when the selected agent edits it, and on F5. Off screen, an edit triggers no read. The page script takes F5 everywhere, so the webview never reloads the page.
- **State**: `AppState` holds the summaries, the open transcripts, the plan usage and the UI state (selected agent, unread agents, open blocks).
  A pure function `apply(&mut AppState, Event)` updates it, so the state logic is tested without a window.
- **Connection task**: a Dioxus coroutine owns the `Connection`, applies events to the state signal, and sends the requests the components ask for.
- **Components** follow the mockups: sidebar, header, transcript, composer, floating blocks, file view, palette, switcher, dialogs, agent map.
- **Untested Claude Code** (`state/claude_code_version.rs`, `UntestedNote` in `ui/header.rs`): each release sets `TESTED_VERSION` to the Claude Code version it was tested with ([releasing.md](releasing.md)). The header shows a note when the selected agent's `claude_code_version` is newer, compared as Cargo versions (`version_order::compare`). The tested version is the app's own: the update screens offer to bring a daemon of another release to the app's build.
- **Accounts** (`state/accounts.rs`, `ui/accounts.rs`): with two accounts or more, each agent shows its account. Each viewer picks the accounts' colors, and the preferences keep them: the protocol carries none.
  - The app asks `accounts.list` once connected, after `AccountsChanged`, when an accounts or new agent dialog opens, and on "Check again". The daemon ends the sign-in waits that are over before it answers.
  - The new agent dialog starts on the account of the agent it copies (see "New agent dialog"). The folder never changes it. Remove is off for an account any agent uses.
  - An agent that waits for its account's sign-in shows a card whose button sends the message once the account has signed in. It also sends while the agent waits when no check would end the wait (`unknown`, `environment`). A login written before the failed turn does not: Claude Code failed on it, and the message would fail again at once.
- **Sidebar foot**: Accounts, Machines and the Settings gear, and nothing else.
- **Grouping** (`state/grouping.rs`, `ui/grouping.rs`): the sidebar groups the agents by machine, by project, or not at all. By machine, the agents follow the daemon's order. Otherwise the app keeps its own order across machines in the preferences (`agent_order`, keyed by machine name and agent id), and a move sends no `agent.move`. A machine that does not answer says so under its header, or in a strip at the top when no header names it (`machine_status::strip_lines`).
- **Projects** (`state/projects.rs`, `ui/projects_tab.rs`): a project belongs to this desktop and lives in `prefs.json`: a name, a color and rules. A rule is a repository, or a folder, where a trailing `*` holds every path that starts with the rest.
  - An agent belongs to the project it was moved to by hand, if that project still exists. Else it goes to the kept project whose rule holds its folder most precisely: a folder, then its repository, then a folder that ends with `*`, the longest path first, and the earlier project between equal rules. Else it goes to an unsaved project of its repository, which goes away with its last agent, else to "No project".
  - A choice by hand is kept only when the rules would put the agent elsewhere, so a later rule change moves it again. Removing an agent drops its choice, since a later agent may take its id.
  - A project picked in the new agent dialog applies once the app has both the start's answer and the agent's summary, since the rules need the summary to tell whether the pick is a choice to keep.
  - The folded projects stay folded after a restart: `prefs.json` keeps their keys (`projects.folded`), and skips a key it cannot read. The machine panels fold for the run only.
- **Agent order**: grouped by machine, the sidebar shows the daemon's order. A move sends `agent.move`, shows at once, and goes back when the daemon refuses it. A page script follows the drag (`ui/reorder.rs`), and Rust hears of it once, on release.
- **Block order**: the floating blocks follow one order for every agent (`blocks.order` in the preferences). Alt+Up and Alt+Down on a focused head move its block: the page script leaves those keys to the head.
- **Folded blocks** (`state/blocks_view.rs`, `ui/blocks.rs`): the floating blocks show as cards in a 296 px column at the chat's top right, or folded into badges on its right edge, where a click opens one block as a popover. The button at the top of the column or of the rail, and Ctrl+Shift+B, fold and unfold them for every agent (`blocks.form` in the preferences). Without room for the cards, they fold on their own: beside the file view, and while the chat stage is narrower than 760 px, as with the files pane open. The page measures the stage (`CHAT_ROOM_SCRIPT`) and tells the app only when it crosses that width. The button then shows dimmed and does nothing, and the cards come back when the chat widens. Folded, the chat keeps only the badges' 52 px strip (`blocks-folded` on the center), and the quotes' notes leave the strip, as beside the file view. The column and the rail fade in and out, at once on a switch (`use_agent_presence`).
- **MCP reconnect** (`state/mcp_reconnect.rs`): the daemon answers `agent.mcp_reconnect` once Claude Code has tried, without the outcome. So the app then asks `agent.mcp_status` and shows the server's status.
- **MCP sign-in** (`state/mcp_sign_in.rs`, `sign_in_tasks.rs`, `sign_in_listener.rs`, `ui/mcp_sign_in.rs`): a server that needs a sign-in shows a "Sign in" box. The app opens the link of `agent.mcp_sign_in` in the browser, which goes back to `http://localhost:<port>/callback?code=…&state=…` once the user approves.
  - For an agent on this computer, `claude` listens on that port and takes the code itself.
  - For an agent on another machine, or of this computer's daemon reached over SSH, the app listens on `127.0.0.1:<port>` and passes the address on with `agent.mcp_sign_in_callback`. It takes only an address whose `state` is the link's, and reads each connection on its own for at most 5 seconds. When the port is taken, the box asks to paste the address.
  - The app asks `agent.mcp_status` every 2 seconds until the server connects. It gives up after 10 minutes in the browser. A server that does not connect once Claude Code holds the token (at the first status after the daemon's reconnect, or after 30 seconds) shows "Signed in, but the server did not connect", with its status and a Reconnect button. Cancel, a new attempt or the end of the wait stops the listener within a second.
  - One loop (`sign_in_tasks::drive`) serves every sign-in: it reads the state every 250 ms, and changes it only when a page, a listener or a status request is due.
- **Reloads** (`state/reload.rs`): the "/" palette lists `/reload-skills` and `/reload-plugins` among the commands, whether Claude Code lists them or not, and leaves out Claude Code's own entries of those names. They match by their name alone. A pick sends `agent.reload_skills` or `agent.reload_plugins` at once, and drops the "/" word typed for it. A message that is one of them alone sends the request too, in place of a prompt, and the draft keeps its images and quotes. An info note says what the reload found: "Reloaded plugins: 3 plugins, 1 error. New: /notes:search."
  An exited agent lists them turned off, and a pick shows a note that asks to resume the agent. The new agent dialog's palette leaves them out, since the agent has not started.
- **Sidebar rows**: an agent that finishes out of sight gets the unread dot, as does one marked unread, even the selected one.
  An idle or exited row's time changes at most once a minute. Only a small component per row reads the app's one-second `Clock`, so a tick renders those texts and nothing else. The times come from the daemon's clock, and read 0 s rather than below when it runs ahead of the laptop's.
- **Status filter**: chips hide the other statuses' rows and never change the order. The chosen chip stays even at 0, so the last agent leaving it does not take the filter away. A hidden agent still raises its notifications. A move among the shown rows leaves the hidden agents in their places.
- **Machine load**: the app asks `machine.load` (see "Machine load" in the daemon's section) every 3 seconds while connected. It keeps the last answer in a signal of its own (`Cockpit.load`), so a new sample never renders the transcript. A daemon that cannot measure its load, or does not know the request, shows the load as unknown.
- **New agent dialog**: it copies the selected agent, else the last agent started (`AppState::copied_agent`): its machine, folder, account, model, effort and permission mode (`open_new_agent`). So Ctrl+N then Enter starts an agent beside the selected one. It opens on that agent's project too when it was picked by hand, since the folder alone would not give it again. The folder field lists the folders inside the folder it names (`folders.list`), by the path rules of the daemon's machine. Its optional name field becomes the start's `title`, which the daemon also gives the Claude session with a `/rename` sent before the first message, so the session keeps it after the agent closes. That `/rename` is quiet (`AgentView::quiet_prompt_sent`): its answer shows no "Session renamed" notice, since the user renamed nothing. Its placeholder shows the name the daemon gives without one (`default_title` in `cockpit_protocol`, which the daemon calls too): the first message's first line, else the folder's name.
  A "/" word in its first message opens the "/" palette over the field (`state/new_agent_palette.rs`), as in the composer. The agent has not started, so it has no commands yet: the palette lists those of an agent of the dialog's machine and account, one in the dialog's folder first, then one whose commands the app has, then the latest active, and asks for them when the app has none. An agent in the same folder has the very list the new agent will have, so the palette says nothing more. One in another folder may lack this folder's own commands, or have some that do not work here, so the palette's foot names that agent and its folder. With no such agent, the list says so, and a typed command still works once the agent starts. The field sends its text and caret to the app as the composer does (`new_agent_prompt_input`), and while the palette is open, Up, Down, Enter, Tab and Esc go to it: Esc closes it for the rest of its "/" word, and the dialog stays.
- **One highlight in a list**: in the switcher, the Ctrl+O list, the folder completion, the resume list and the "/" palette, the pointer moves the keyboard's highlight, so one row is lit.
  WebKitGTK sends a mousemove when a list scrolls under a still pointer, so only a pointer that moved takes the highlight (`PointerMoves` in `ui/dialogs.rs`). Only the keys scroll the list, so it never moves under the mouse.
- **Motion** (the style sheet's Motion and Layout sections, `ui/presence.rs`): the `--motion-in` and `--motion-out` tokens set every duration, and a reduced-motion setting sets them to 0.
  On Linux the app turns WebKitGTK's accelerated compositing off (`webview_settings.rs`). With it, WebKitGTK 2.54 now and then draws an animation's first frame again after its last, so popovers and dialogs flash.
  A closing element stays for its exit, marked `leaving` and inert, so input goes past it. In the frame that switches agents, the root has the `switching` class, which turns transitions off. An agent's element that opens with an animation (a find bar, the exited agent's bar) uses `use_agent_presence`: opened as its agent comes on screen, it gets the `instant` class, since an animation starts as `switching` goes. A dialog's height follows its content (`ui/dialog_height.rs`).
- **Rendering**: Markdown with `pulldown-cmark`, syntax colors with `syntect` and `two-face`, diffs with `similar`. A page script (`FOLD_SCRIPT` in `ui/transcript.rs`) folds long rows by their lines on screen. A command's output keeps its last lines, since they say how the command ended.
  The agent's work between its messages folds into one line (`ui/work_runs.rs`): two or more rows of thinking and calls in a row become "Ran 4 commands, read 1 file, thought 2 times". While a call runs, the line says its kind as under way: "Running 2 commands, read 1 file". The notice of a background task that finished folds in too, between two pieces of work (a run starts and ends on work), and the line counts it ("2 background tasks finished"): Claude Code gives a real failure the status "failed". The agent's text, a card, a subagent, any other notice (a stopped or failed task, a message from another session), a call that waits for approval and the "New" divider end a run, and a run holds the work of one agent. A run shows its rows while a call of it runs, while it ends a turn that goes on, and while a quote being written holds a passage of it, until a click decides. The jump to the latest counts a new run once. Its line's entry id ends with its first item (`run_entry_id`), and a closed run counts the find's matches of its rows on its chip (`FoldKey::Run`).
  A request Claude Code no longer waits on takes no card. One you allowed shows as a "you allowed" tag on the line of the call it names (`Approval.tool_call`), when the window holds that call on a row of its own. One you denied, one Claude Code withdrew, or one whose call is not held or can share a row (a read, a search, a skill) or is a subagent shows as one line, as a call does, which unfolds to the whole request. A request that waits keeps its card with the whole command, since you approve that text. A request answered while the chat shows its agent keeps its card for the exit animation (`AgentEntry::cards_leaving`, `CARD_EXIT`), which shrinks it away, so the rows around it do not jump. A denied or withdrawn request's line unfolds above the card as it shrinks.
  Thinking and a subagent's line are call-sized rows. A thinking has a hollow dot, since it neither succeeds nor fails.
  In agent text, a file's path or a link to a file opens the file view. A relative path resolves in the agent's folder, and an absolute one can be anywhere, since the daemon decides which files it reads. A link that goes up with `..`, starts with `~` or `//`, or has a scheme other than `http` or `https` shows as text, so nothing in an agent's text makes the system open a file. A web image is not fetched: it shows as a link.
  A call on a file shows the file's name, then its folder, dim (`file_edits::call_path`): relative inside the agent's folder, from `..` up to two folders above it, from the root further. The folder keeps its first name, cut in its middle when long, and its last two, so a file in a sibling worktree takes one line. Hover shows the whole path.
  A local image shows at its own size, at most 200 px high. The app reads its size from the file's header (`ChatImage::size`), so the image has its room before the webview decodes it.
  On Linux, `browser.rs` runs `xdg-open` without `LD_LIBRARY_PATH`: an app started from an agent's session inherits the NixOS `claude` wrapper's alsa-lib, which makes Chromium exit at startup.
- **Switching agents**: in the switch's frame the chat draws only the latest 40 rows, then 40 older ones per frame, up to 120 (`OPEN_ROWS`, with `ShownRows` and `Drawn` in `ui/transcript.rs`). Older rows the app holds wait until the view nears the top, which draws 60 more each time (`MORE_ROWS`), then asks the daemon for older messages. A click that shows an entry in the chat (the Messages block, the agent map) first draws from 40 rows above that entry, since the scroll script scrolls only to a drawn entry. Each render compares every row drawn: with the 2,000 rows a morning of work gave an agent, each chunk took 350 to 900 ms, and the app stayed blocked for seconds after a switch. Dioxus runs effects once the webview has applied a frame, so an effect starts each next chunk.
  The chat builds its rows only when `ChatInputs` change: the selected agent's transcript revision, its history, status and sending messages, and its scroll state. An event of another agent leaves them as they are.
  An agent not looked at keeps its latest 300 items (`KEPT_ITEMS_AWAY`), from the moment it is left. Scrolling up loads the older ones again. An agent left with its chat scrolled up keeps 2,000 (`KEPT_ITEMS_SCROLLED_UP`), so that the chat can go back to the place the user was reading.
  The demo with `YETACK_DEMO_FILL=2000` gives two agents that many more entries, to check how the chat copes with a long session.
  Each entry has `content-visibility: auto`, so the webview lays out only the entries in view. The chat's scroll script reads inside only the entries at the view's edges: reading inside a skipped entry lays it out, which on a chat of 1600 entries makes a scroll frame take 100 ms instead of 7. The script restores an agent's place only once the chat has drawn the rows it opens with and names the agent (`data-agent`). A chat that opens scrolled up draws at once from 120 rows above the last entry seen, or from 40 rows above the first row that was in view when that one is higher, so the row of that place is drawn.
  The chat follows new entries within `BOTTOM_THRESHOLD_PX` of its bottom. For `WHEEL_UP_MS` after the wheel turns up, it follows only from its very bottom: WebKit spreads a notch over a few frames, and a frame still within the threshold would take the chat back down.
- **Performance info** (`perf.rs`): "Performance info" in the slash palette shows a box that times each switch: the answers the switch waits for, the chat's first rows and the rows it opens with, and when they paint. It also notes what holds the window's thread: a stall of the app's own code, or a long frame of the page. Its top lines sum the last second's events and chat builds, and name the agents that hold the most items. A switch slower than half a second to paint stays pinned. Off, its hooks return at once.
- **Older messages**: the scroll script sends `NearTop` once the view comes within `NEAR_TOP_SCREENS` view heights of the chat's top, and again for each new first entry while the view stays there. The chat then draws more of the rows it holds, or once all are drawn, asks for the page before the first item it holds, so older messages are usually there before the user reaches the top.
  The script sends `NearTop` only while the chat has `data-loads-older`: older messages exist, and no page of them is on its way or has failed. In any other state the app refuses the request. For example, a first page whose items the app already held from live events leaves the first entry as it was: a `NearTop` sent while that page loaded would never be sent again. Until they come, grey rows (`OlderSkeleton`) stand above the first message, and the script keeps the entry in view where it is as the page arrives. A page that fails shows its error with Retry (`HistoryState::OlderFailed`). Once nothing older exists, a line marks the start of the conversation, with the time of its first message when that is the user's.
- **Quotes and replies** (`state/quotes.rs`, `ui/quotes.rs`): a quoted passage with a comment waits in the agent's draft (`Draft.quotes`) and goes with the next message as a Markdown quote. A quote names its passage by its transcript entry and its start and end in UTF-16 units, as `Range.toString` counts them. If the entry's text moved, the page script finds the passage again by its text.
  The script marks passages with the CSS Custom Highlight API, which leaves Dioxus's elements unchanged. WebKitGTK before 2.44 lacks that API, so passages show without highlights. An opened note carries `data-takes-escape`, so the page's key script leaves Esc to it, and the Esc that folds the note never stops the agent.
  After a send, the script finds each quote's passage in an earlier entry by its text. Nothing is stored for this: the message's text holds the quotes, so it works for messages loaded from the history too.
- **Message time** (`grouped_times` in `ui/transcript.rs`): the times under sent messages follow the local day (`Today` in `connection.rs`), so they render again at midnight, not at each clock tick.
- **Entries in view** (`InView` in `ui/chat_scroll.rs`): the Messages block marks the message whose answer is in view. The scroll script reports the entries at the view's top and bottom, and whether the chat is at its bottom, in one message, so the two never disagree. `InView` is a signal of its own, so scrolling renders only the Messages block. WebKit gives an entry out of view a placeholder height that its content overflows, so the script takes the lower of the entry's bottom and its wrapper's.
- **Messages block** (`ui/messages.rs`): the user's top-level messages and the waiting prompts, with the one being read marked (`Transcript::message_read`). A message's time (`UserMessage::sent_at`) comes from the daemon's clock, or from Claude Code's line for a message read from a session file.
- **Renaming in the sidebar**: F2 edits the selected agent's title (`agent.rename`). The typed text lives in the drafts, so typing renders neither the transcript nor the sidebar.
- **An exited agent** (`ui/exited_bar.rs`, `state/resume.rs`): a bar above the composer has Resume, so it stays in view however far the chat scrolls. Resume sends `agent.resume` once. When the daemon answers `NotFound`, the agent's folder is gone, and the bar also offers to move the agent (see "Working folder"), unless it is a fork that has not had its first message, which does not move. The refusal belongs to the folder it names: once the agent moves, the bar offers Resume again.
  A session picked in "Resume a session" that an exited agent holds resumes that agent.
- **Working folder** (`state/working_folder.rs`, daemon `hub/folders.rs`): an agent can fork into another folder (`agent.fork`) or move there (`agent.change_folder`).
  Claude Code finds a session only in the project of the folder it starts in (`~/.claude/projects/<the folder's path, each character but an ASCII letter or digit written ->/`). The daemon copies the session file and its side files there in a blocking task, since a session file can reach hundreds of megabytes. Past 2 minutes, the fork or the move fails with a timeout, so a FIFO or a stuck mount in place of the file cannot leave it unanswered. It keeps the originals, since transcripts name some side files by their path. It refuses a project name over 200 characters, since Claude Code shortens those its own way.
  A fork's first start passes `--fork-session` (`resume_as: fork` in `agents.json`). A move is refused while the agent works, and restarts a running agent once `claude` exits and the copy is done. A stop meanwhile leaves the agent stopped in its new folder, and a close answers the move with `NotFound`.
  A copy of the session replaces the file of that session in the target's project, so the daemon refuses a copy over a file another agent needs. A fork that has not had its first message does not move ("send the fork a message before moving it"): its file is the one it copies, and in its source's folder it would replace the source's own. For the same reason, it forks only within its own project ("send the fork a message before forking it into another folder"). A fork into its source's own folder copies nothing: until its first message it starts from the source's live file, so a restart before then picks up the source's later turns. An agent does not move or fork into a project where a fork of its session waits for its first message, or where the copy for such a fork is under way: that would replace the copy the fork starts from. It does not fork into the folder it moves to either, since the move copies the session there once `claude` exits. The app greys out Move for a fork that has not had its first message (`working_folder::blocked`).
- **Closing an agent**: Ctrl+Shift+W sends `agent.close`, and Ctrl+W stays the file view's. The agent leaves the sidebar at once. The app ignores its events until the daemon's `AgentRemoved`, so a late update does not bring it back. For 8 seconds, Undo resumes the session as a new agent, at its place in the sidebar and in the project it was moved to by hand. The daemon gives the new agent the model, effort and permission mode chosen for the closed one.
- **Marking an agent to handle** (`state/to_handle.rs`, `agent.mark`): a mark lasts until you send the agent a message, or until you clear it. The daemon saves it in `agents.json`. It clears a mark that lasts until a message when the agent gets a prompt, so that mark stays until you give the agent the input it waited for.
- **The row menu** (`ui/row_menu.rs`, frame `KeyedMenu` in `ui/menu.rs`): it marks itself `data-own-keys`, so the bridge script leaves its keys to it. The pointer may cross other rows for 200 ms on its way into an open submenu.
- **Details** (`state/details_updates.rs`): an agent's entry holds its details with the seq of the last details event they hold (`details_seq`). A `DetailsChanged` replaces them. A `DetailsUpdated` applies when its seq is past `details_seq`, waits in `early_details` while the answer to `agent.details` is on its way, and is dropped before the app asked. The answer takes the waiting updates after its `last_seq`, unless a whole `DetailsChanged` came first.
  The app drops an agent's events while its close is on its way, so an agent whose close failed asks for its details again.
  The daemon also sends its answers to `agent.context_usage` and `agent.mcp_status` as details events. The app writes such an answer into the details only when no details event reached them since the request left, since that event may be newer.
- **Agent map** (`Dialog::AgentMap`, logic in `state/agent_map.rs`): an outline of an agent's subagents, built from each one's `parent` in the transcript the app holds. "Show its call in the chat" scrolls the chat to the call (`ChatReveal`). The scroll script then keeps the revealed entry at its height, since an entry out of view has a guessed height until it comes into view. The outline takes the focus when the map opens, so Enter never presses a button left focused behind the dialog.
- **Shortcuts** (`keymap.rs`): the Settings dialog changes each command's shortcut, saved under `keys` in `prefs.json`. A new shortcut needs Ctrl or Alt, or is an F key, so it never takes a key typed in a field. Ctrl+Alt with a character is refused, since AltGr types it. The page script takes Ctrl+R and F5 everywhere, so the webview never reloads.

## Security

### Threats

- Other users of the server. When `/proc` is mounted without `hidepid`, they see every command line, and they can connect to any port on 127.0.0.1.
- Processes that run as the user on the server, the agents' commands included. A prompt-injected agent could try to connect to the daemon and approve its own requests.
- Anyone on the network.
- Root on the server can read everything: accepted.

### Measures

- No TCP port anywhere: the daemon listens only on its Unix socket, and the daemon in the app on nothing. The exception is an MCP sign-in for an agent on another machine: the app listens on the laptop's `127.0.0.1:<port>` until the browser comes back with the sign-in's `state`, for 10 minutes at most. Another user of the laptop who sends a made-up address gets an error page, and one who takes the port first gets only a code that is useless alone (below). The VS Code and Cursor remote servers listen on a 127.0.0.1 port that every user of the machine can reach, protected only by a token file.
- The state folder has mode 700, and the socket mode 600. On Linux, connecting to a Unix socket needs write permission on its file, so other users cannot connect.
- The [peer checks](#server-and-peer-checks) turn away another user, and a command an agent runs that simply connects. A process that detaches on purpose (a double fork, `setsid`) gets a new parent and passes. Claude Code's sandbox, set to deny the agents' commands the state folder, is meant to stop it. Yetack does not set that sandbox up, and nobody has checked that it covers a Unix socket.
- `agent.git_status` lists the filters the checkout's config defines, then runs `git status` with each one turned off. A command the agent runs can make the two see different configs every time, as by serving an included config from a FIFO, and git then runs the filter as the daemon. Only an agent that already runs commands can do it, so it matters only under Claude Code's sandbox, as the socket does.
- The SSH key is the only login to a server: the app runs `ssh <host> yetack bridge`, with `ForwardAgent=no` (see [Client](#client-cockpit_client)).
- A prompt goes to `claude` only through stdin (see [Claude Code processes](#claude-code-processes)).
- The daemon never reads, stores or passes a credential on its own. The file view is the exception, as in an IDE: it shows any file the user opens, and sends it only to the user's app.
- An MCP sign-in from the app passes one secret through the daemon: the authorization code in the address the browser goes back to. The code works once, and only with the verifier the agent's `claude` keeps in memory (PKCE). The daemon logs that line without the code, and Claude Code keeps the token in the account's folder.
- A terminal runs a shell as the daemon's user, in the agent's folder: it can do what the agents' commands can. A client names the shell from the daemon's list, and the daemon runs no other program. Its output goes only to clients that passed the peer checks.
- Dictation records only while the talk key is held, after a tap of it, or after a click on the mic, and keeps the audio in memory. Neither the audio nor its text leaves the computer, but as the message the user sends. The speech model downloads only on the user's click, over HTTPS (rustls) from the pinned addresses, and the app uses a file only once its SHA-256 matches.
- Choosing a folder in the app trusts it: `claude` runs without a terminal, so it shows no trust prompt, and a repository's project MCP servers and settings start with the agent. The app asks no confirmation.
- An install from the app writes only under `~/.cache/yetack/`, `~/.local/share/yetack/`, `~/.config/systemd/user/` and `~/.config/yetack/` on the server, plus one crontab line when the user clicks for it. An update replaces only a daemon in `~/.local/share/yetack/bin` or `~/.local/bin`. The server's script checks the binary's checksum and version before it replaces anything. The app never runs `sudo`.

## Errors

- Each library crate has typed errors (`thiserror`), one enum per area, such as `ParseError` in `claude_protocol` or `PeerError` in `cockpit_daemon`. Only the two binaries use `anyhow`, to report.
- A request that fails returns an error response with a code and a message. The connection stays open.
- A `claude` line that does not parse is logged in `raw/` and skipped. It never stops the agent.

## Tests

- `claude_protocol` parses fixture lines in `crates/claude_protocol/tests/fixtures/`, written by hand from the shapes Claude Code sends, with made-up paths, ids and text.
- `cockpit_daemon` keeps its store in a temporary folder. Its end-to-end tests run a fake `claude`, a test binary that replays a script of lines, selected with `YETACK_CLAUDE`. The daemon in the app runs over in-memory streams (`tests/in_app.rs`).
- `cockpit_client` runs against an in-process fake daemon over a socket pair.
- `cockpit_desktop` tests `apply` on event sequences, and the service's stop and start against a made-up system.

## Platforms

Linux runs everything. The workspace also builds and passes its tests on Windows, where the daemon runs inside the desktop app and its agents run Windows' own `claude`. The code marks what needs Unix with `#[cfg(unix)]`, and gives Windows its own version with `#[cfg(windows)]`.

| | Linux | Windows |
| --- | --- | --- |
| daemon | `yetack serve` on a Unix socket, or in the app | in the app (`InAppDaemon`) |
| `claude` | `claude` on `PATH`, in its own process group | `claude.exe`, else npm's `claude.cmd` through `cmd.exe`, in its own Job Object |
| machine load | `/proc` | `GetSystemTimes`, `GlobalMemoryStatusEx`, each agent's job; no load average |
| `yetack` | every command | client commands, with `--ssh`; no `serve` or `bridge` |
| desktop app | WebKitGTK | WebView2 |
| daemon's state folder | `$XDG_STATE_HOME/yetack` | `%LOCALAPPDATA%\yetack` |
| app's config file | `$XDG_CONFIG_HOME/yetack/config.toml` | `%APPDATA%\yetack\config.toml` |
| app's preferences | `$XDG_STATE_HOME/yetack-desktop/prefs.json` | `%LOCALAPPDATA%\yetack-desktop\prefs.json` |
| app's log | stderr | stderr, else `%LOCALAPPDATA%\yetack-desktop\app.log` (a release build has no console), rotated by size |
| notifications | freedesktop service | toasts (see Notifications) |
| paths the app shows | `/work/app` | `C:\work\app`, typed with `\` or `/`; a Linux daemon's paths keep Unix's rules |
| home folder (holds `.claude`) | `HOME` | `USERPROFILE` |

Each path comes from its environment variable, and falls back to its default under the home folder, as on Linux.

On Windows, for now:

- `Transport::Local` fails at once: tokio has no Unix sockets there. The socket server and its peer checks are not built.
- Nothing sets file modes. A file or folder takes the permissions of its folder, and those of `%LOCALAPPDATA%` admit only the user.
- The tests that need a shell, `/proc`, a Unix socket, file modes or symbolic links are left out, the end-to-end tests among them.
  The app's tests with made-up Unix paths, such as `/work/app`, run on Windows too: they stand for a Linux daemon's paths, which the app reads by Unix's rules on either platform.
