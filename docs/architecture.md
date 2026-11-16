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

Dependencies point one way:

```text
claude_protocol ◄── cockpit_daemon ──► cockpit_protocol ◄── cockpit_client
                      ▲      ▲                                  ▲      ▲
                      │      └────────────── yetack ────────────┘      │
                      └──────────────── cockpit_desktop ───────────────┘
```

The app hands its in-app daemon the receiving end of the client's in-memory link, so `cockpit_daemon` and `cockpit_client` do not depend on each other outside tests.
`cockpit_protocol` does not depend on `claude_protocol`, so a change in Claude Code's wire format changes only `claude_protocol` and the daemon's conversion, never the app.

## Data model (`cockpit_protocol`)

### Ids

Each id is a newtype ([standards/rust/api_design.md](standards/rust/api_design.md#give-every-persisted-id-its-own-type)).
`Seq` is one global number per event, from the hub.
`ItemId` numbers an agent's transcript items, so an update can target one.
`AccountId::DEFAULT` (0) is the default account.

### Agents

`AgentSummary` holds what the sidebar and the header show of one agent.

- `repository` is the folder's git remote. `Repository::from_remote` writes every form of a remote the same way, so the app can group agents by it.
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

- An `Edit` or `Write` call drops its old and new text when either is over 1 MiB.
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
4. Responses and events share the connection, in any order.

The snapshot holds no transcripts: the app asks for them with `agent.history`, a page at a time.

### Requests

The requests are the methods of one enum, with fixed strings ([standards/rust/api_design.md](standards/rust/api_design.md#declare-a-reusable-string-once)).
[Machine load](#machine-load), [Accounts](#accounts-1) and "Working folder" describe theirs.
A daemon answers an unknown method with ``unknown method `<name>` ``. The app looks for that text to tell an older daemon, so a test pins it.

`agent.close` ends an agent even while its process runs, which `agent.stop` then `agent.remove` cannot do.
Removing the agent closes `claude`'s stdin. `claude` then ends its session file and exits, or is killed after 10 seconds, so `sessions.resume` can bring the session back.

A card sends its answer once. The daemon takes a repeat of an item's answer as done, and refuses a different one, since Claude Code has the first.

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

`agent.read_file` sends at most 4 MiB of text or 8 MiB of image. It adds the agent's edits of the file, since the app may not hold the messages that made them.
Its `git ls-files` search stops at 20 matches or 3 seconds.
`agent.list_dir` and `folders.list` read at most 20,000 entries, send at most 2,000, and run off the hub's task.
`folders.list` sends no file names, and leaves out a name that is not UTF-8, since the protocol carries paths as text.

A history page is limited by bytes as well as by count, since images travel inline.
An item too large for any response comes as a `Notice` under its id, so paging moves on.
Titles and model names go into every snapshot, so the daemon refuses a title over 200 characters and a model name over 100.

### Events

Every event carries its `Seq`.
Slash commands and skills change only when `claude` starts or reloads, so they travel in `CommandsChanged`, apart from the details.

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
- Binary: `claude` from `PATH`, or `YETACK_CLAUDE`. On Windows, relative folders of `PATH` are skipped.
  A `.cmd` or `.bat`, such as an npm `claude.cmd`, runs through the system folder's `cmd.exe` with `/d /v:off /s /c`. An argument holding a quote, a `%`, a line break or a NUL is refused, since `cmd.exe` would read it. The child of `cmd.exe`, the real `claude`, counts as `claude` itself (`Launch::Interpreter`).
  Every program the daemon starts on Windows, and the client's `ssh`, starts with `CREATE_NO_WINDOW`: the app has no console to share.
- On Unix, `claude` leads a process group of its own. On Windows, it starts suspended and joins a Job Object of its own (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, no breakaway) before it runs, so every process it starts is in the job.
  Either way, stopping the agent stops its MCP servers and commands too, and on Windows the job also ends when the app dies. When Windows cannot make the job, `claude` runs and stops alone.
- An agent given no effort runs at the one Claude Code picks, which the daemon asks for with `get_settings`. It does not read the settings files itself, since the user's own `effortLevel` counts only for some models.
- Effort and the session's name have no control request, so the daemon sends `/effort` and `/rename` as prompts.
  Claude Code restates the session's saved name as it starts, but the daemon keeps the agent's own, so a rename made while the agent was stopped stays.
- A prompt goes to stdin as a `user` message, never as an argument: other users can read arguments with `ps`.
  A prompt sent while the agent works is either queued in the daemon until a turn ends with every steered prompt read, or steered: written to stdin at once, for Claude Code to read at the next tool boundary or turn.
  The agent stays working until Claude Code has echoed (`isReplay`) every steered prompt. A slash command always queues, since Claude Code does not echo every one.
  After a stop between turns, a steered prompt whose text the transcript holds counts as read. A prompt still unread then, or when the process ends, becomes a "not sent" notice.
- Approvals, `AskUserQuestion` and `ExitPlanMode` come as `can_use_tool` control requests.
  This control protocol is not documented. The open-source Python Agent SDK implements it, in [`_internal/query.py`](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py) (`_handle_control_request`): it is the reference for its messages.
  Claude Code echoes control responses, so the daemon ignores one whose id it did not send.
- After `--resume`, Claude Code's `total_cost_usd` includes the session's past cost. The daemon reads that cost from the session file's last `cost-state` line, so each turn shows its own cost.
  The agent's cost is the higher of Claude Code's total and its own count: a process killed with the daemon writes no `cost-state`, and a `/clear` starts a session whose cost leaves out the one before.

### Store

Everything lives in `$XDG_STATE_HOME/yetack/`: the socket, `serve.lock`, `agents.json`, `settings.json` (the accounts included), and for each agent its events, its latest details and every line exchanged with `claude`.
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

`sessions.list` lists the newest 100 sessions of all accounts. `sessions.resume` makes an agent that runs with the session's account.
It reads the session file before the agent exists, through the same conversion as `claude`'s stdout (`PastTranscript`).
The newest 2,000 items show, with ids up to 2,001. The agent numbers its own items from 2,002, so the ids never clash.
The past items go in no event: `agent.history` serves them. A restarted daemon reads the same bytes again, since Claude Code only appends to the file.

### Accounts

The daemon keeps the accounts in `settings.json`, outside the `Settings` that `settings.set` replaces, so an app that predates accounts cannot erase them. Such an app speaks protocol version 1, and the daemon refuses it (see [Session](#session)).
An account's folder must be absolute and be no other account's, the default one's included, whatever its case on Windows and macOS. The default folder keeps part of its login outside it (`.claude.json` in the home folder, the Keychain on macOS), so the same folder named in `CLAUDE_CONFIG_DIR` would be another login.
The default account and an account that an agent uses cannot be removed. Ids are never reused.

The user signs in by running `claude auth login`, with `CLAUDE_CONFIG_DIR` set, in a terminal on the daemon's machine. The daemon writes this command for a POSIX shell, or for PowerShell on a Windows daemon, and names the program it runs (module `process::program`).
On Windows, it names plain `claude` only when PowerShell's own lookup finds the same file: PowerShell runs an npm install's `claude.ps1` first, and its execution policy may refuse it.

An account counts as signed in when `.credentials.json` exists in its folder. The daemon never opens the file.
On macOS, the login lives in the Keychain, so a missing file means `unknown`.
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
A background subagent ends with its task, since its call returns at once. The turn's end stops the turn's foreground subagents.
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
  - The check asks GitHub's API for the latest release every 24 hours, over HTTPS. It reads at most 4 MB and sends only the `User-Agent` and `Accept` headers. The tag may hold only letters, digits, `.` and `-`, since the card shows it in commands to copy.
  - The Windows update downloads the release's Windows program, its Linux daemon and `SHA256SUMS`, only from under `https://github.com/Henauxg/yetack/releases/download/`, each at most 200 MB, and checks each file's SHA-256. A file may take 30 minutes at most, and 30 seconds without data end its download. It renames the running program and the daemon beside it to `<name>.old` and moves the new files in (`put_in_place`), since Windows lets a program rename its running file and not write over it. A failed move puts back the files moved so far. When they cannot all go back, the update deletes nothing, since one may be the only copy of the app. A `RunId` tells the runs apart, so a cancelled run's late news changes nothing.
  - The new program starts with `--after-update <pid>`, waits up to 30 seconds for the old app to end, puts back an `.old` file whose place is empty, and deletes the rest. Until the restart, the daemon beside the app is the new release's, so a server's update sends the `.old` one, which matches the running app (`server_daemon::carried`).
- **A panic** (`stop_on_panic`): while the app runs agents itself, the panic hook stops the daemon in the app and exits with status 101. Otherwise the agents' processes would outlive the app, and the next start would resume their sessions a second time.
  Only the first panic stops the daemon. The stop waits at most 1 second for the host's lock, which the panicking thread may hold. A panic on the window's thread waits for the stop, since that thread's unwinding would end the process. A panic on another thread unwinds at once, so its task's drop kills the processes the task holds.
- **Log** (module `logging`): the app logs to stderr, at the level `RUST_LOG` sets (`warn` by default). A release build on Windows has no stderr, so it logs to `app.log` in its state folder, rotated at 10 MiB over 5 files. A new run appends, so the rotation carries on across runs. A failed rotation (another program holds a file) tries again 10 MiB later, so no line is lost.
- **Notifications** (module `notifications`): at most one per agent, when it needs you or finishes. None shows while the window has the focus.
  An agent finishes once its turn has ended and no background task runs. After the last background task ends, the app waits 10 s (`FINISH_HOLD`) before it announces the finish: Claude Code usually starts a turn to read the task's result, and that turn's end is the real finish.
  Levels (`state/notification_levels.rs`, saved in `prefs.json`): one for the app and one per agent, each "All", "Only when it needs you" or "Off". An alert shows only when both let it through. A change of level, or the window getting the focus back, reaches every machine's notifier (`ToDaemon::Notifications`), which closes the notifications no longer let through. Removing an agent drops its level, since a later agent may take its id. The config's `[notifications] enabled = false` sets the app's level until the user picks one in the window.
  On Linux they go through the freedesktop service, so Do Not Disturb holds them back.
  On Windows they are toasts from notify-rust. The app has no Start menu entry, so toasts show PowerShell's name and icon. A click reaches the app only while the toast is on screen, and a toast cannot be withdrawn.
- **Paths** (module `path_style`): the app follows the path rules of the daemon's machine, since the app on Windows may reach a Linux daemon over SSH. `PathStyle` works on text, so the Linux tests check both styles. A path's root tells its style. The new agent dialog's folder field has no agent yet, so it takes Unix over SSH (only Unix runs `yetack bridge`) and the app's own platform otherwise.
- **Preferences**: `prefs.json` in the app's state folder (see Platforms) keeps one viewer's choices. A missing or broken file means the defaults. The demo reads and saves none, since its agents take the ids of real ones.
- **Splitters** (`ui/splitter.rs`) resize the sidebar, the file view and the files pane. A page script follows the drag, and Rust hears of the new size once, on release.
- **List of open files** (`state/file_list.rs`, `ui/file_list.rs`, `Popover::FileList`): a button after the file view's tabs lists the open files, with a filter.
- **Tab menu** (`state/tab_menu.rs`, `ui/tab_menu.rs`, `Popover::TabMenu`): a right-click on a tab, or on its row in the list of open files, opens a menu of actions on the file and the tabs. "Show in the files pane" is off for a file the tree never shows (`file_tree::can_show`).
- **Messages on their way** (`state/sending.rs`): a sent message shows at once, dimmed, since the daemon's echo takes a round trip. The echo replaces it when it has the same text and as many images. A refused message's draft comes back.
  A message to a busy agent can wait in the app for a few seconds (`chat.steer_hold`, off by default), so the user can cancel it: once sent, Claude Code holds it and no request removes it. Held messages leave in the order sent. One still held when the app quits is not sent.
- **Esc and Stop** (`ui/composer.rs`, `state/shortcuts.rs`): one button is Stop while the agent is busy and the message empty, and Send otherwise. For a second after a send it stays an inactive Send, so a double click or the next key never stops the agent.
  Esc first closes what is open: the comment field, a menu or palette, a dialog, the find. Then, on a busy agent, it opens "Stop the agent", or stops at once when `chat.esc_stop` is `at_once`. Esc in another field stops nothing. A character typed into that dialog closes it and goes into the message, and the agent keeps working. Keys with Ctrl, Alt or Meta leave it open, since a shortcut may take them.
- **Files pane** (`state/file_tree.rs`, `ui/files_pane.rs`): it shows the selected agent's folder as a tree. Each agent keeps its own tree (`AgentEntry.tree`), and whether its pane is open (`FilesPanePrefs.agents`, by machine name and agent id, dropped with the agent).
  - "All" lists a folder with `agent.list_dir` when it opens, then the outside folders where the agent changed or read a file. Such a folder shows only those files unless it is a git checkout's top folder, so that a folder such as `/etc` does not bury them.
  - The shown listings are read again when the agent finishes an edit or a Bash call, when it comes back on screen, and on F5.
  - `file_tree::reveal` keeps the row to show in `FileTree.reveal` until it shows, since a folder may wait for its listing.
- **Find** (`state/find.rs`, `ui/find.rs`, `ui/find_folds.rs`): the chat and the file view each have a find. `UiState.chat_finds` and `FileView.find` hold which are open. `Drafts.finds` holds the text and the results, so typing renders neither the chat nor the sidebar.
  - A page script searches the text the page shows and highlights the matches with the CSS Custom Highlight API. Each match is a `StaticRange`, which no change of the page has to update. The app drops a report (`FindReport`) about an older text.
  - The chat's folds put nothing on the page until they open. So the app counts their matches from the transcript (`FoldTexts`), and the script opens a fold when the find reaches it. The counts include Markdown marks, so the page counts again once the fold opens.
    The app gathers the fold texts once per change of the transcript (`Transcript::revision`). That number comes from one count for every transcript, so a transcript a reconnect builds again never takes an older one's number.
  - The chat's scroll script scrolls to a match (`REVEAL_EVENT`), so the entry stays in place as the entries above it take their height.
  - Text that changes every second carries the `find-skip` class (`SKIP_CLASS`): the find leaves it out, and a change in it starts no search.
- **Markdown preview** (`ViewMode::Preview`, `FilePreview` in `ui/file_view.rs`): a Markdown file gets a preview mode, rendered with the chat's markdown component up to `MAX_RENDERED_LINES` lines.
- **Open in browser** (`OpenInBrowserButton` in `ui/file_view.rs`, `state/page_copy.rs`, `page_files.rs`): an HTML file gets a button that opens it in the browser.
  For an agent on this computer, `browser::open_page` opens the file itself. It resolves the path's links first, and the target must be a page, since on Linux the opener picks the program by the file's name.
  For a server's file, the daemon reads (`agent.read_file`, `Purpose::PageCopy`) at most 32 files that the page's `src` and `href` name by a relative address. As an IDE serves a page from its project, it reads only web files (`WEB_EXTENSIONS`) in the page's folder or the agent's, so a key or a shell script stays on the server. `page_files::open_copy` writes the copy to a new private temporary folder (`yetack-page-*`) and leaves it, since the browser reads it after the app is done. A browser in a snap or a Flatpak has its own `/tmp`, so it cannot read the copy: the user accepted that, since Windows and NixOS can.
- **Reading the shown file again** (`state/file_reread.rs`): the file view reads its file again when it comes back on screen, when the selected agent edits it, and on F5. Off screen, an edit triggers no read. The page script takes F5 everywhere, so the webview never reloads the page.
- **State**: `AppState` holds the summaries, the open transcripts, the plan usage and the UI state (selected agent, unread agents, open blocks).
  A pure function `apply(&mut AppState, Event)` updates it, so the state logic is tested without a window.
- **Connection task**: a Dioxus coroutine owns the `Connection`, applies events to the state signal, and sends the requests the components ask for.
- **Components** follow the mockups: sidebar, header, transcript, composer, floating blocks, file view, palette, switcher, dialogs, agent map.
- **Untested Claude Code** (`state/claude_code_version.rs`, `UntestedNote` in `ui/header.rs`): each release sets `TESTED_VERSION` to the Claude Code version it was tested with ([releasing.md](releasing.md)). The header shows a note when the selected agent's `claude_code_version` is newer, compared as Cargo versions (`version_order::compare`). The tested version is the app's own: the update screens offer to bring a daemon of another release to the app's build.
- **Accounts** (`state/accounts.rs`, `ui/accounts.rs`): with two accounts or more, each agent shows its account. Each viewer picks the accounts' colors, and the preferences keep them: the protocol carries none.
  - The app asks `accounts.list` once connected, after `AccountsChanged`, when an accounts or new agent dialog opens, and on "Check again". The daemon ends the sign-in waits that are over before it answers.
  - The new agent dialog starts on the account of the agent it copies (see "New agent dialog"). The folder never changes it. Remove is off for an account any agent uses.
  - An agent that waits for its account's sign-in shows a card whose button sends the message once the account has signed in. It also sends while the agent waits when a check finds a login written before the failed turn, since that login may work again, or when no check would end the wait (`unknown`, `environment`).
- **Sidebar foot**: Accounts, Machines and the Settings gear, and nothing else.
- **Grouping** (`state/grouping.rs`, `ui/grouping.rs`): the sidebar groups the agents by machine, by project, or not at all. By machine, the agents follow the daemon's order. Otherwise the app keeps its own order across machines in the preferences (`agent_order`, keyed by machine name and agent id), and a move sends no `agent.move`. A machine that does not answer says so under its header, or in a strip at the top when no header names it (`machine_status::strip_lines`).
- **Projects** (`state/projects.rs`, `ui/projects_tab.rs`): a project belongs to this desktop and lives in `prefs.json`: a name, a color and rules. A rule is a repository, or a folder, where a trailing `*` holds every path that starts with the rest.
  - An agent belongs to the project it was moved to by hand, if that project still exists. Else it goes to the kept project whose rule holds its folder most precisely: a folder, then its repository, then a folder that ends with `*`, the longest path first, and the earlier project between equal rules. Else it goes to an unsaved project of its repository, which goes away with its last agent, else to "No project".
  - A choice by hand is kept only when the rules would put the agent elsewhere, so a later rule change moves it again. Removing an agent drops its choice, since a later agent may take its id.
  - A project picked in the new agent dialog applies once the app has both the start's answer and the agent's summary, since the rules need the summary to tell whether the pick is a choice to keep.
- **Agent order**: grouped by machine, the sidebar shows the daemon's order. A move sends `agent.move`, shows at once, and goes back when the daemon refuses it. A page script follows the drag (`ui/reorder.rs`), and Rust hears of it once, on release.
- **Block order**: the floating blocks follow one order for every agent (`blocks.order` in the preferences). Alt+Up and Alt+Down on a focused head move its block: the page script leaves those keys to the head.
- **MCP reconnect** (`state/mcp_reconnect.rs`): the daemon answers `agent.mcp_reconnect` once Claude Code has tried, without the outcome. So the app then asks `agent.mcp_status` and shows the server's status.
- **Sidebar rows**: an agent that finishes out of sight gets the unread dot, as does one marked unread, even the selected one.
  An idle or exited row's time changes at most once a minute. Only a small component per row reads the app's one-second `Clock`, so a tick renders those texts and nothing else. The times come from the daemon's clock, and read 0 s rather than below when it runs ahead of the laptop's.
- **Status filter**: chips hide the other statuses' rows and never change the order. The chosen chip stays even at 0, so the last agent leaving it does not take the filter away. A hidden agent still raises its notifications. A move among the shown rows leaves the hidden agents in their places.
- **Machine load**: the app asks `machine.load` (see "Machine load" in the daemon's section) every 3 seconds while connected. It keeps the last answer in a signal of its own (`Cockpit.load`), so a new sample never renders the transcript. A daemon that cannot measure its load, or does not know the request, shows the load as unknown.
- **New agent dialog**: it copies the selected agent, else the last agent started (`AppState::copied_agent`): its machine, folder, account, model, effort and permission mode (`open_new_agent`). So Ctrl+N then Enter starts an agent beside the selected one. It opens on that agent's project too when it was picked by hand, since the folder alone would not give it again. The folder field lists the folders inside the folder it names (`folders.list`), by the path rules of the daemon's machine.
- **One highlight in a list**: in the switcher, the Ctrl+O list, the folder completion, the resume list and the "/" palette, the pointer moves the keyboard's highlight, so one row is lit.
  WebKitGTK sends a mousemove when a list scrolls under a still pointer, so only a pointer that moved takes the highlight (`PointerMoves` in `ui/dialogs.rs`). Only the keys scroll the list, so it never moves under the mouse.
- **Motion** (the style sheet's Motion and Layout sections, `ui/presence.rs`): the `--motion-in` and `--motion-out` tokens set every duration, and a reduced-motion setting sets them to 0.
  On Linux the app turns WebKitGTK's accelerated compositing off (`webview_settings.rs`). With it, WebKitGTK 2.54 now and then draws an animation's first frame again after its last, so popovers and dialogs flash.
  A closing element stays for its exit, marked `leaving` and inert, so input goes past it. In the frame that switches agents, the root has the `switching` class, which turns transitions off. An agent's element that opens with an animation (a find bar, the exited agent's bar) uses `use_agent_presence`: opened as its agent comes on screen, it gets the `instant` class, since an animation starts as `switching` goes. A dialog's height follows its content (`ui/dialog_height.rs`).
- **Rendering**: Markdown with `pulldown-cmark`, syntax colors with `syntect` and `two-face`, diffs with `similar`. A page script (`FOLD_SCRIPT` in `ui/transcript.rs`) folds long rows by their lines on screen. A command's output keeps its last lines, since they say how the command ended.
  In agent text, a file's path or a link to a file opens the file view. A relative path resolves in the agent's folder, and an absolute one can be anywhere, since the daemon decides which files it reads. A link that goes up with `..`, starts with `~` or `//`, or has a scheme other than `http` or `https` shows as text, so nothing in an agent's text makes the system open a file. A web image is not fetched: it shows as a link.
  A local image shows at its own size, at most 200 px high. The app reads its size from the file's header (`ChatImage::size`), so the image has its room before the webview decodes it.
  On Linux, `browser.rs` runs `xdg-open` without `LD_LIBRARY_PATH`: an app started from an agent's session inherits the NixOS `claude` wrapper's alsa-lib, which makes Chromium exit at startup.
- **Switching agents**: in the switch's frame the chat draws only the latest 25 rows, then 40 older ones per frame (`ShownRows` and `Drawn` in `ui/transcript.rs`). For a chat of 150 entries, drawing every row puts the switch on screen after about 180 ms, against about 90 ms for 25 rows. Drawing the older rows in one frame blocks clicks for about 100 ms. Dioxus runs effects once the webview has applied a frame, so an effect starts each next chunk.
  Each entry has `content-visibility: auto`, so the webview lays out only the entries in view. The chat's scroll script reads inside only the entries at the view's edges: reading inside a skipped entry lays it out, which on a chat of 1600 entries makes a scroll frame take 100 ms instead of 7. The script restores an agent's place only once the chat has drawn all its rows and names the agent (`data-agent`).
  The chat follows new entries within `BOTTOM_THRESHOLD_PX` of its bottom. For `WHEEL_UP_MS` after the wheel turns up, it follows only from its very bottom: WebKit spreads a notch over a few frames, and a frame still within the threshold would take the chat back down.
- **Quotes and replies** (`state/quotes.rs`, `ui/quotes.rs`): a quoted passage with a comment waits in the agent's draft (`Draft.quotes`) and goes with the next message as a Markdown quote. A quote names its passage by its transcript entry and its start and end in UTF-16 units, as `Range.toString` counts them. If the entry's text moved, the page script finds the passage again by its text.
  The script marks passages with the CSS Custom Highlight API, which leaves Dioxus's elements unchanged. WebKitGTK before 2.44 lacks that API, so passages show without highlights. An opened note carries `data-takes-escape`, so the page's key script leaves Esc to it, and the Esc that folds the note never stops the agent.
  After a send, the script finds each quote's passage in an earlier entry by its text. Nothing is stored for this: the message's text holds the quotes, so it works for messages loaded from the history too.
- **Message time** (`grouped_times` in `ui/transcript.rs`): the times under sent messages follow the local day (`Today` in `connection.rs`), so they render again at midnight, not at each clock tick.
- **Last instruction** (`ui/last_instruction.rs`): a bar shows the user's message once it has scrolled out above the chat. The scroll script reports the entries at the view's top and bottom, and whether the chat is at its bottom, in one message, so the two never disagree (`InView`, a signal of its own, so scrolling renders only the bar and the Messages block). WebKit gives an entry out of view a placeholder height that its content overflows, so the script takes the lower of the entry's bottom and its wrapper's.
- **Messages block** (`ui/messages.rs`): the user's top-level messages and the waiting prompts, with the one being read marked (`Transcript::message_read`). A message's time (`UserMessage::sent_at`) comes from the daemon's clock, or from Claude Code's line for a message read from a session file.
- **Renaming in the sidebar**: F2 edits the selected agent's title (`agent.rename`). The typed text lives in the drafts, so typing renders neither the transcript nor the sidebar.
- **An exited agent** (`ui/exited_bar.rs`, `state/resume.rs`): a bar above the composer has Resume, so it stays in view however far the chat scrolls. Resume sends `agent.resume` once. When the daemon answers `NotFound`, the agent's folder is gone, and the bar also offers to move the agent (see "Working folder"). The refusal belongs to the folder it names: once the agent moves, the bar offers Resume again.
  A session picked in "Resume a session" that an exited agent holds resumes that agent.
- **Working folder** (`state/working_folder.rs`, daemon `hub/folders.rs`): an agent can fork into another folder (`agent.fork`) or move there (`agent.change_folder`).
  Claude Code finds a session only in the project of the folder it starts in (`~/.claude/projects/<the folder's path, each character but an ASCII letter or digit written ->/`). The daemon copies the session file and its side files there in a blocking task, since a session file can reach hundreds of megabytes. It keeps the originals, since transcripts name some side files by their path. It refuses a project name over 200 characters, since Claude Code shortens those its own way.
  A fork's first start passes `--fork-session` (`resume_as: fork` in `agents.json`). A move is refused while the agent works, and restarts a running agent once `claude` exits and the copy is done.
- **Closing an agent**: Ctrl+Shift+W sends `agent.close`, and Ctrl+W stays the file view's. The agent leaves the sidebar at once. The app ignores its events until the daemon's `AgentRemoved`, so a late update does not bring it back. For 8 seconds, Undo resumes the session as a new agent.
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

- No TCP port anywhere: the daemon listens only on its Unix socket, and the daemon in the app on nothing. The VS Code and Cursor remote servers listen on a 127.0.0.1 port that every user of the machine can reach, protected only by a token file.
- The state folder has mode 700, and the socket mode 600. On Linux, connecting to a Unix socket needs write permission on its file, so other users cannot connect.
- The [peer checks](#server-and-peer-checks) turn away another user, and a command an agent runs that simply connects. A process that detaches on purpose (a double fork, `setsid`) gets a new parent and passes. Claude Code's sandbox, set to deny the agents' commands the state folder, is meant to stop it. Yetack does not set that sandbox up, and nobody has checked that it covers a Unix socket.
- The SSH key is the only login to a server: the app runs `ssh <host> yetack bridge`, with `ForwardAgent=no` (see [Client](#client-cockpit_client)).
- A prompt goes to `claude` only through stdin (see [Claude Code processes](#claude-code-processes)).
- The daemon never reads, stores or passes a credential on its own. The file view is the exception, as in an IDE: it shows any file the user opens, and sends it only to the user's app.
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
