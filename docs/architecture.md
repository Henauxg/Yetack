# Architecture

This document says how the code is split and why.
[plan.md](plan.md) says what the app does and the decisions behind it.
The Rust code follows [standards/rust/](standards/rust/README.md).

## Overview

```text
desktop app or CLI                       daemon (agents-cockpit serve, or in the app)
  cockpit_client                           cockpit_daemon
    transport:                               server: Unix socket, peer checks
      Local: the Unix socket       ──────►   hub: agents, sequence numbers, broadcast
      Ssh: ssh <host> agents-cockpit bridge  store: state folder, event logs
           (stdin/stdout, copied to the socket)
      InApp: an in-memory stream             claude process per agent:
           (the daemon in the app's process)   claude_protocol (JSON mode, control protocol)
```

There are three ways to reach a daemon:

- **Remote**: the app runs `ssh <host> agents-cockpit bridge`.
  The bridge copies bytes between its stdin/stdout and the daemon's socket on that host.
- **Local**: the daemon runs on the same machine as the app.
  The app connects to the daemon's Unix socket directly, with no SSH and no bridge.
- **In the app**: the daemon runs inside the desktop app's process (see [The daemon in the app](#the-daemon-in-the-app)).
  Each connection is an in-memory stream, and no socket listens.

All three carry the same protocol and the same session (hello, catch-up, requests, events), so everything above the transport is identical.

## Crates

All crates live in `crates/` and are application code ([standards/rust/README.md](standards/rust/README.md#library-code-and-application-code)): nothing outside this repository calls them.

| crate | kind | owns |
| --- | --- | --- |
| `claude_protocol` | lib | Claude Code's JSON mode: the lines `claude` reads and writes, and the control protocol. Types and parsing only, no IO. |
| `cockpit_protocol` | lib | The daemon API: ids, the transcript model, requests, responses, events, snapshots, and the line framing. |
| `cockpit_client` | lib | A connection to a daemon over a transport (local socket, SSH bridge, or in-memory stream): requests, responses, events, reconnection. |
| `cockpit_daemon` | lib | The daemon: claude processes, the hub, the store, the socket server and its peer checks, and `InAppDaemon`, the daemon inside the app. |
| `agents_cockpit` | bin `agents-cockpit` | The command line: `serve`, `bridge`, and client commands (`list`, `start`, `prompt`, `answer`, `stop`, `close`, `rename`, `move`, `tail`, `account`). |
| `cockpit_desktop` | bin `agents-cockpit-desktop` | The Dioxus desktop app. |

Dependencies point one way:

```text
claude_protocol ◄── cockpit_daemon ──► cockpit_protocol ◄── cockpit_client
                      ▲      ▲                                  ▲      ▲
                      │      └────────── agents_cockpit ────────┘      │
                      └──────────────── cockpit_desktop ───────────────┘
```

The app depends on the daemon to run it in its own process. Outside the daemon's tests, `cockpit_daemon` and `cockpit_client` do not depend on each other: the app hands the daemon the receiving end of the client's in-memory link.

- `cockpit_protocol` does not depend on `claude_protocol`.
  The daemon turns Claude Code's events into the cockpit's own transcript model.
  Claude Code's wire format changes often; when it does, only `claude_protocol` and the daemon's conversion change, never the app.
- `cockpit_desktop` never sees a Claude Code line.

## Data model (`cockpit_protocol`)

### Ids

Each id is a newtype ([standards/rust/api_design.md](standards/rust/api_design.md#give-every-persisted-id-its-own-type)).

| type | wraps | comes from |
| --- | --- | --- |
| `AgentId` | `u32` | the daemon, at `agent.start`; persisted in `agents.json` |
| `Seq` | `u64` | the hub; one global sequence number per event, persisted in the event logs |
| `RequestId` | `u32` | the client, for each request on its connection |
| `ClaudeSessionId` | `String` | Claude Code's `session_id`, used with `--resume` |
| `PendingId` | `String` | the `request_id` of a `can_use_tool` control request from Claude Code |
| `ItemId` | `u32` | the daemon, per transcript item of an agent, so an update can target an item |
| `ProcessId` | `u32` | `/proc` on the daemon's machine, for an agent's commands in `machine.load` and `agent.stop_process` |
| `AccountId` | `u32` | the daemon, at `accounts.add`; persisted in `settings.json` and `agents.json`. `AccountId::DEFAULT` (0) is the default account |

### Agents

`AgentSummary` is what the sidebar and the header need:
id, title, folder, status, activity line, pending count, model, effort, permission mode, task progress, context percentage, prompt cache expiry, turns, API-equivalent cost, start time, last activity time, running turn's start time, position in the list, account, whether it waits for its account's sign-in (`sign_in_wait`), how many of its background tasks still run (`background_running`), and its folder's git repository (`repository`).

`repository` is the remote of the folder's repository, which the app groups agents by when it groups them by project.
The daemon reads it from the folder's git files when the agent starts, is restored, or changes folder (`cockpit_daemon` `repository.rs`): the `origin` remote, else the first remote, through a worktree's or a submodule's `.git` file too.
`cockpit_protocol::Repository::from_remote` writes every form of a remote the same way, so that `git@github.com:acme/app.git` and `https://github.com/acme/app` are both `github.com/acme/app`. A repository travels and is saved as that text.
A folder outside git, or a summary from an older daemon, has no repository.
A background task (a subagent or a command started in the background) can outlive the turn that started it, so an `Idle` agent can still have work running: the daemon sends a new summary when that count changes. A summary without the count reads 0.

The two times let the app count the time on a sidebar row by itself, so the daemon sends no event to move a clock:

- `turn_started_at` is set when a prompt goes to `claude`, or at the first line of a turn Claude Code starts without one (a resumed cut-off turn). It is `None` between turns.
- `last_activity_at` moves when the agent starts, the user resumes it, a prompt goes to `claude`, the agent begins to wait for you (the first of its pending requests), a turn ends, or its process ends.
  Lines within a turn and streamed text do not move it, and neither does a slash command that runs no turn, such as the `/effort` the menu sends.
- Each of these moments already changes the summary's status, pending count or turn count, so the times add no `AgentUpdated` event.
- After a daemon restart, both come from the last summary in the event log. A turn the restart cut off has no turn start any more, and its last activity is the later of its start and the logged last activity.
  A log written before these fields existed reads `None` for both, and the row then shows no time, as with an older daemon. An older app ignores the two fields.

The transcript's "Working, 4 min" line and the agent map's row for the agent count the turn from `turn_started_at` as well, so a turn's time survives a reconnect.
With an older daemon, they count from when the app saw the turn start, and show no time for a turn already running when the app connected.

The list's order belongs to the user, and the daemon keeps it, so every client and `agents-cockpit list` show the same one:

- The list shows the highest `position` first, and the newer agent first between equal positions.
- A new agent goes on top, and so does an agent made from a past session.
  Only `agent.move` moves an agent: a status change, a turn or a rename does not.
- Positions leave room between neighbours.
  A move gives the moved agent a position between its new neighbours', so one `AgentUpdated` event goes out.
  When they leave no room, every agent gets a new position, 1024 apart, and each agent whose position changed gets an event.
  For example, with C at 3072, B at 2048 and A at 1024, moving A just above B gives A 2560.
- `cockpit_protocol` holds the order and the move (`list_order`, `move_agent`, `place_on_top`), so the app reorders at once with the same numbers the daemon will send.
- An `agents.json` saved before positions existed reads 0 for every agent, so the agents list newest first, as new agents do, until one moves.

`AgentStatus` is derived by the daemon, never by the app:

| status | when |
| --- | --- |
| `Working` | from a prompt until the turn's `result` |
| `NeedsYou` | an approval, a question or a plan waits for an answer, or the agent's account needs a sign-in (`sign_in_wait` is `waiting`) |
| `Idle` | after `result`, nothing pending |
| `Exited` | the `claude` process ended |

### Transcript

An agent's conversation is a list of `TranscriptItem`s, each with an `ItemId` and an optional `parent` (the subagent it nests under).
The daemon numbers items in transcript order, so the app can merge a history page with live appends by id.
The kinds of item:

- `UserMessage` (text, attached images); a slash command shows as typed, such as `/review fix`, not as Claude Code's `command-name` wrapper;
- `AssistantText` (markdown) and `Thinking` (folded in the app);
- `ToolCall`: tool name, summary, input, status (running, done, failed), output, and for `Edit` and `Write` the file path with the old and new text (both left empty when either is over 1 MiB);
- `Subagent`: a tool call whose own items carry it as `parent` (from `parent_tool_use_id`);
- `Approval`: tool, input (with the diff for an edit), reason, its answer state, and the item of its tool call (from the request's `tool_use_id`, which Claude Code sends after the call's `tool_use`);
- `Question`: the `AskUserQuestion` questions and options, and its answer state;
- `Plan`: the `ExitPlanMode` plan text, and its answer state (a decision, with an optional message when Claude should keep planning);
- `TurnEnd`: duration, usage and cost of the turn, its `outcome`, and for a failed turn the error that ended it. The outcome is `completed`, `stopped` (the user stopped it, from the `result` line's `terminal_reason`), `failed`, or `not_signed_in` (the error is a missing login);
- `Notice`: a line such as "Conversation compacted" or "Conversation cleared" (a `conversation_reset` line, after which Claude Code's total cost counts from 0 again), a local command's output such as `/effort`'s, or a message Claude Code writes itself (a user line whose `origin` is not `human`), such as its summary of a background task that ended.
  Context Claude Code adds for Claude (a user line marked `isSynthetic`) becomes a notice whose `detail` holds its text, which the app shows folded: "Skill instructions: /loop" after a `Skill` call that loaded it, "Conversation summary" after a compaction, else "Context added by Claude Code".

An answer state is `Waiting`, `Answered`, or `Cancelled` (Claude Code withdraws a pending request when the turn is interrupted).

`AgentDetails` holds what the side blocks and the agent map show: the task list (from `TodoWrite`), background tasks (`task_started`, `task_notification`), the subagents, changed files, and the last context usage.
A background task names its `call`, the tool call that started it. Claude Code gives a task to a command that runs in the foreground too (`is_backgrounded: false`), so `agent.stop_task` stops one running command: Claude gets the call as stopped (exit code 137) and goes on with its turn.
It also holds `waiting_prompts`: the prompts Claude has not read yet, each `queued` (the daemon holds it) or `sent` (`claude` has it), with its text and image count.
Claude Code reads a sent prompt at its next step, so only once the step's running calls end. Its row says what it waits for (`AgentEntry::message_hold`): an approval, a question or a plan that waits for the user, or else the step's running calls (`Transcript::running_calls`), named by the one that started first. That call's row has a Stop button when it has a task.

### Subagents

`AgentDetails.subagents` lists every subagent the agent started, at any depth, in the order they started (details without the list read it as empty). A `Subagent` has:

- `item`, the transcript item of the `Agent` call that started it, which its own items name as their `parent`, and `parent`, the item of the subagent that started it (`None` when the agent did);
- its description, its type (`Explore`, `general-purpose`), and `run`: `foreground`, or `background` when its call returns at once;
- `status`: `running`, `needs_you` (one of its approvals, questions or plans waits), `done`, `failed` or `stopped`, with `error` for a failed one;
- start and end times, `tokens` (`None` until Claude Code reports its usage), `tool_calls`, and `last_tool`, its latest call (`tool`, `summary`, and the call's `item`, which names the subagent it waits for when that call started one);
- `turn`, the agent's turn it started in, counted as `AgentSummary.turns` counts them, and `task`, its background task for `agent.stop_task`.

### Plan usage

`PlanUsage` holds the latest `rate_limit_event` per window (`five_hour`, `seven_day`, ...): utilization, reset time, status.
It belongs to an account, not to an agent: the daemon keeps one per account, from the events of that account's agents.

### Accounts

An `Account` is a Claude Code config folder the daemon runs agents with: its id, a name the user chooses, the folder (absolute, on the daemon's machine), and `credentials`: `sign_in` (the folder's login) or `environment` (the daemon's own `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`).
The folder holds the account's login, user settings, `.claude.json` (the account, personal MCP servers, per-project trust), `projects/` (session transcripts), skills, agents, commands, plugins and global `CLAUDE.md`.
A repository's own `.claude/`, `.mcp.json` and `CLAUDE.md` stay shared between accounts.

- The default account (`AccountId::DEFAULT`) is the login `claude` has with the daemon's own environment: `CLAUDE_CONFIG_DIR` when the daemon has it, else `.claude` in the home folder (`%USERPROFILE%\.claude` on Windows).
  Every daemon has it, and it cannot be removed. Its name starts as "Default", and the user can change it.
- Every agent has one account, chosen at `agent.start` or `sessions.resume` (the default one when the request names none), and keeps it for good: across stops, resumes and daemon restarts.
- A message from before accounts existed reads as the default account: an agent's summary, a past session, `agents.json`, a start or resume request (serde defaults).
- `settings.json` and the event logs of older daemons hold `use_environment_api_key` and `default_uses_environment_api_key`, `bool`s, in place of `credentials` and `default_credentials`. They still read, as `environment` for `true`. The word `environment_api_key`, which older daemons write for `environment`, still reads too.

## Protocol (`cockpit_protocol`)

### Framing

One JSON object per line, UTF-8, `\n` at the end.
A reader refuses a line longer than a fixed maximum (16 MiB) instead of buffering without bound.

### Session

1. The client sends `Hello { protocol_version, last_seq }`.
   The version is 9. An app and a daemon of different versions refuse each other.
2. The daemon answers `Welcome { protocol_version, daemon_version, daemon_commit, catchup }`, or `Refused { reason }` when the versions differ.
   `daemon_version` is the daemon's release, its Cargo version ("0.1.0"). The Machines dialog shows it on each machine's row, and the About section at the end of the Settings dialog's General tab shows the app's own version.
   `daemon_commit` is the full git commit of the program that runs the daemon (`BuildCommit`), or `null` when its build did not know it. Two builds of one release can differ, so the commit tells the app whether a daemon runs its own code.
   `cockpit_daemon`'s `build.rs` sets it when the crate builds, from `AGENTS_COCKPIT_SOURCE_COMMIT` or git's `HEAD` (see "Updating a machine's daemon" under [Desktop app](#desktop-app-cockpit_desktop)). `agents-cockpit` and the app each build the daemon from their own tree, so it is their commit too.
   The app keeps it in `MachineState.daemon_commit`, beside `daemon_version`.
3. `catchup` is `Replay` when `last_seq` was given and the logs still hold every event after it: the daemon then sends those events, with each agent's latest details in place of the older ones, then live events.
   Otherwise it is `Snapshot(snapshot)`: the client replaces its state with it, and live events follow.
   A welcome never carries both, so no event is applied twice.
4. The client sends `Request { id, request }`; the daemon answers `Response { id, result }`.
   Responses and events share the connection, in any order.

The snapshot holds every `AgentSummary`, the default account's plan usage (`plan_usage`), the settings, the last `Seq`, the accounts, and the other accounts' plan usage (`other_plan_usage`).
Transcripts are not in the snapshot: the app asks for an agent's transcript with `agent.history` when it opens the agent, a page at a time.

### Requests

`agent.start`, `agent.prompt` (with a delivery: `steer`, the default, or `queue`), `agent.cancel_queued`, `agent.send_queued` (a queued prompt goes to `claude` now), `agent.answer` (approval, question or plan), `agent.interrupt`, `agent.stop`, `agent.resume`, `agent.remove` (an exited agent), `agent.close` (any agent), `agent.history`, `agent.details`, `agent.set_model`, `agent.set_effort`, `agent.set_mode`, `agent.rename`, `agent.mark` (`mark`: `to_handle`, which a prompt clears, `to_handle_kept`, which stays until cleared, or `unmarked`), `agent.fork` and `agent.change_folder` (see "Working folder"), `agent.move` (the agent goes just above `before`, or to the bottom when `before` is `null`), `agent.context_usage`, `agent.stop_task`, `agent.mcp_status`, `agent.mcp_reconnect`, `agent.mcp_toggle` (`action`: `enable` or `disable`), `agent.commands` (slash commands and skills), `agent.read_file` (the file view), `agent.list_dir` and `agent.find_files` (the "Open file" dialog and the files pane), `agent.stop_process` (one of the agent's commands), `mcp.remove` (with the agent, whose folder decides the project and local scopes), `settings.set`, `sessions.list`, `sessions.resume`, `folders.list` (the new agent dialog's folder field), `machine.load` (see [Machine load](#machine-load)), `accounts.list`, `accounts.add`, `accounts.change`, `accounts.remove` (see [Accounts](#accounts-1)).

`agent.close` ends an agent whatever its process does, in one request.
`agent.stop` then `agent.remove` would not do it: `remove` refuses an agent whose process still runs, and the process exits some time after `stop` closes its stdin.
The daemon interrupts a working agent's turn, takes the agent out at once (`AgentRemoved`, its logs deleted), and the process handle leaving with it closes `claude`'s stdin.
`claude` ends its session file and exits, or is killed after 10 seconds, so the session can come back with `sessions.resume`.

The three file requests reach the same folders, once symlinks are resolved and with `..` refused: the agent's folder, and each git checkout that holds a file the agent read or changed (the folder with the `.git` entry nearest above it, unless that is `/` or holds the home folder).
`agent.read_file` also reads a file the agent touched that no such checkout holds, and that file only.
The daemon and the app run as the same user, and the agent's commands already read what that user can, so these limits keep the views to the agent's work rather than hide anything from the user.

Each request checks the file or folder once it is open, so a folder of its path swapped for a symbolic link after the checks changes nothing: on Linux through `/proc/self/fd`, on Windows through `GetFinalPathNameByHandleW`.
On Windows, a folder's entries come from `GetFileInformationByHandleEx` on the open folder, and the checks read paths as text (module `windows_path`):

- The verbatim prefix that `canonicalize` adds, `\\?\C:\` or `\\?\UNC\host\share\`, counts as absent. The daemon keeps canonical paths without it, so they show as a user writes them.
- Names compare whatever their case, as Windows does, with one upper case character for each: `ß` stays `ß`. Drive letters and shares must match.
- A device path (`\\.\pipe\...`), a device name such as `NUL`, `COM1` or `AUX:` in any folder, and a network share that none of the agent's allowed folders and files is on, are refused before any file system call, so that a check never makes Windows connect to another host. A symbolic link in an allowed folder that leads to a share is still followed when the path is resolved, then refused: the agent can reach that host itself.
  A link that leads to a device name is refused once resolved, before the file opens. A search leaves out a path that `git ls-files` lists outside its checkout, which an index file the agent wrote can hold.
- `folders.list` takes a share, since the user types it.

- `agent.read_file` sends a text file of at most 4 MiB, with the agent's finished edits of it over the whole session (subagents' included, the newest up to 4 MiB of JSON), or, for a file whose extension names an image type Claude reads (`.png`, `.jpg`, `.gif`, `.webp`), the image of at most 8 MiB as `Image`, in base64. The file view diffs from these edits: the app holds only the latest pages of the messages, which may not reach back to them. A relative path that names no file in the agent's folder is looked up: the most recent file the agent read or changed whose path ends with it, then a `git ls-files` search of the folders above, which stops at 20 matches or 3 seconds. Several matches come back as `FileMatches`, for the user to pick.
- `agent.list_dir` answers a folder's entries, folders first, each with its kind (folder, file or other), a file's size, and whether it is a symbolic link (`link`: `direct` or `symbolic_link`). It reads at most 20,000 entries and sends at most 2,000, and says when it cut the list (`completeness`: `complete` or `truncated`). Each entry says whether its git checkout ignores it (`ignore`: `kept` or `ignored`), from one `git check-ignore` run in the folder, bounded to a second. The `.git` entry is always `ignored`. Without git, outside a checkout or past the second, every other entry is `kept`. It runs off the hub's task.
- `agent.find_files` answers the files whose name holds a text, whatever its case, from the same bounded search.

`folders.list` answers the folders inside a folder, for the new agent dialog, as a `Directory` that holds folders alone.
It reaches any folder the daemon's user can read, since an agent can start in any of them, and it sends no file names.
It refuses a relative path, and one that starts with `~`, as `agent.start` does.
It keeps the bounds of `agent.list_dir`: it reads at most 20,000 entries, files included, sends at most 2,000 folders, and says when it cut the list.
It leaves out a folder whose name is not UTF-8: the protocol carries paths as text, so no request could reach that folder.
A symbolic link to a folder counts as a folder, and a hidden folder lists as any other.
It runs off the hub's task.

A history page says whether older items exist (`older_items`: `exist` or `none`).
It is limited by bytes as well as by count: images travel inline, and a line over the framing maximum is refused.
An item too large for any response comes as a `Notice` that says so, under the item's id, so paging moves on.

Titles and model names go into every snapshot, so the daemon refuses a title over 200 characters and a model name over 100 with `invalid_request`.
It cuts a past session's title at 200 characters.

The method names are an enum with fixed strings, pinned by a test ([standards/rust/api_design.md](standards/rust/api_design.md#declare-a-reusable-string-once)).
A daemon answers a method it does not know with `invalid_request` and a message that starts with ``unknown method `<name>` ``.
`Method::unknown_message` writes that text for the daemon, and the app finds it in an error to tell an older daemon.
Older daemons send it as it is, so a test pins it.

### Events

`AgentAdded`, `AgentUpdated(AgentSummary)`, `AgentRemoved`, `TranscriptAppended { agent, item }`, `TranscriptChanged { agent, item }`, `DetailsChanged { agent, details }`, `CommandsChanged { agent, commands }` (the slash commands and skills, which change only when `claude` starts or reloads, so they travel apart from the details), `PlanUsageChanged` (the default account's), `SettingsChanged`, `AccountPlanUsageChanged { account, usage }` (another account's), `AccountsChanged { accounts }`.
Each carries its `Seq`.

`DetailsChanged` carries the whole details, so it goes out only when something they hold changes: a subagent's text or a streamed delta sends none, its tool calls and its task events do. During a burst, a change waits until 200 ms passed since the last one, and a later change replaces it. A change of the waiting prompts goes out at once, since the app matches them against the transcript events.

## Daemon (`cockpit_daemon`)

### Entry points

The daemon starts the same way behind a socket and inside the app (module `daemon`):

1. `Daemon::prepare` creates the state folder, takes `serve.lock` and writes its process into it (see [Store](#store)), and reads `agents.json`, the settings and the event logs back.
2. `PreparedDaemon::start` starts the hub, which resumes the agents that were running.

`serve` (`agents-cockpit serve`) binds the socket between the two steps, so a failed bind starts no agent, then runs the server with its peer checks until its shutdown future completes.
The binary owns the rest of what a separate process needs: the SIGTERM and Ctrl+C handlers, the log output, and the Tokio runtime, whose end kills the agents' processes.

### The daemon in the app

`InAppDaemon` runs the daemon inside another program, the desktop app:

- It runs on a Tokio runtime of its own, which owns every task of the daemon, the agents' processes included.
- It uses the background service's state folder and takes the same `serve.lock`, so the two never serve one folder at once, and each resumes the agents the other left running.
  When the lock is held, `InAppDaemon::start` fails with `StoreError::AlreadyRunning`, and the app connects to that daemon's socket instead.
- Each client is a `tokio::io::duplex` stream whose daemon end comes through a channel (`InAppLink` on the client's side).
  The connection task is the socket's, generic over the stream, so a client gets the same hello, catch-up, requests and events.
- No socket listens, so the peer check does not apply. Only the app holds the channel's sending end, and an agent's command, the threat the peer check is for (plan.md, "Security"), has no endpoint to connect to.
- `InAppDaemon::shutdown` stops it as a service's stop does: it stops the hub, then shuts the runtime down, which drops the process tasks and kills each `claude`'s process group, or on Windows its Job Object.
  The agents stay marked as running, so the next daemon resumes them, and a turn the stop cut off carries on.
  When the app dies instead, the next daemon first stops the `claude` processes left running (see [Store](#store)).
  It waits at most the given time for tasks that block, then releases the lock.
  It blocks, so the app calls it from a thread outside any runtime.

The in-app daemon's own code needs nothing Unix-only beyond what the hub, the store and the claude processes already use.

### Tasks and ownership

The daemon is a set of tokio tasks that talk over channels; no state is shared behind a mutex.

- **Hub** (one task) owns every agent's state, assigns sequence numbers, writes the event logs through the store, and sends each event to every connected client.
- **Claude process** (one task per running agent) owns the child process.
  It writes prompts and control messages to its stdin, reads its stdout line by line, parses each line with `claude_protocol`, and sends the parsed message to the hub.
- **Server** (one task) accepts connections and checks each peer.
- **Connection** (one task per client) reads requests, sends them to the hub, and writes responses and events.
  A client that reads too slowly is disconnected rather than buffered without bound; it reconnects with its last `Seq`.

The hub turns Claude Code messages into transcript items and status changes (module `transcript`): this conversion is the only place that knows both formats.

### Claude Code processes

- Environment: the daemon's own, except for what the agent's account changes (module `accounts`):
  - `CLAUDE_CONFIG_DIR` is set to the account's folder, except for the default account, whose `claude` gets the daemon's own variable, set or not.
  - `CLAUDE_CODE_OAUTH_TOKEN` is always removed: it would take the place of any folder's login.
  - `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` are removed, unless the account's `credentials` are `environment`: then both pass as the daemon's environment has them. Claude Code prefers either one to the folder's login, so one of them in the daemon's environment would otherwise run every account on it.
  - The route switches (`ROUTE_ENV` in `accounts/route.rs`: `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`, `ANTHROPIC_BASE_URL`, the providers' own addresses and keys) are removed on the same rule: in the daemon's environment they would send every account's agents to that provider. An account picks a provider in its folder's `settings.json` instead, whose `env` block Claude Code applies itself. The providers' credentials, such as `AWS_PROFILE`, stay: Claude Code ignores them without a switch, and the agents' commands may need them.
  The daemon never reads or logs these values. It only checks whether `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set, for `accounts.list`.
  For `accounts.list`, the daemon also reads the switches of each account's `settings.json`, and of its own environment for an account whose `credentials` are `environment`, into the account's `route`: `anthropic`, `bedrock`, `vertex`, `foundry` or `custom_server`. It parses the `env` block keeping only the switches: the other values are skipped unread, and a custom server's address is never kept. The app shows the route in place of the sign-in state: such an account needs no Claude login, and has no plan limits.
  `claude mcp remove` gets the same environment, so it edits the agent's account's user scope.
- Command: `claude --output-format stream-json --input-format stream-json --verbose --permission-prompt-tool stdio --replay-user-messages`, plus `--resume <session>`, `--model`, `--effort`, `--permission-mode` when set.
  It runs in the agent's folder.
  Without a terminal, `claude` asks no trust question: it trusts the folder, and a repository's project MCP servers (`.mcp.json`) start with the agent, for any account.
  Choosing a folder in the app trusts it.
  The binary is `claude` from `PATH`, or `AGENTS_COCKPIT_CLAUDE` for tests.
  On Windows, the default is the native installer's `claude.exe` on `PATH`, then in `.local\bin` in the home folder, then an npm install's `claude.cmd` on `PATH`; relative folders of `PATH` are skipped.
  A `.cmd` or `.bat` runs through the system folder's `cmd.exe` with `/d /v:off /s /c`, each argument in double quotes with a trailing backslash doubled; an argument holding a quote, a `%`, a line break or a NUL is refused, since `cmd.exe` would read it.
  The agent's root process is then `cmd.exe`, and the real `claude` (`node.exe`) is its child. The process handle records this (`Launch::Interpreter`), and the root's direct children in the agent's job count as `claude` itself: `agent.stop_process` refuses them as it refuses the root, and the machine load leaves them out of the heaviest commands. A child of the root is one whose parent id is the root's: every process of the job started after the root, so that id names the root.
  On Windows, every program the daemon starts, `claude`, `claude mcp remove` and `git`, and the client's `ssh`, starts with `CREATE_NO_WINDOW`: the app has no console to share.
- On Unix, `claude` leads a process group of its own, which the daemon kills to stop the agent's MCP servers and commands with it.
  On Windows, `claude` starts suspended, joins a Job Object of its own, then runs, so every process it starts is in the job from the start.
  The job has `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and no breakaway: stopping the agent terminates the job, and Windows ends it when its last handle closes, as when the app dies.
  The threads resume through a Toolhelp snapshot, since tokio gives no handle to the main thread.
  When Windows cannot make the job, `claude` runs without one, is stopped alone, and the log says so.
- First message: the `initialize` control request.
  Its answer lists the slash commands and skills.
  The daemon then asks for the MCP status and the context usage, which Claude Code answers before the first prompt, so a new or resumed agent shows them at once.
- An agent given no effort runs at the one Claude Code applies by itself. The daemon asks for it with `get_settings` once `initialize` is answered, and again after a model change, and the summary shows its `applied.effort` with `effort_source` set to `claude_code_default`.
  Claude Code resolves it: the local, then the project settings file of the folder it runs in, else the model's default. The user's own `effortLevel` counts only for some models, so the daemon does not read the files itself.
- A prompt goes to stdin as a `user` message, never as an argument: other users can read arguments with `ps`.
- A prompt for an idle agent starts a turn and joins the transcript at once. While the agent works, `agent.prompt` delivers it one of two ways:
  - `steer` writes it to stdin at once. Claude Code reads it at the next tool boundary, in the same turn, or, when the turn ends first, starts the next turn with it; an interrupted turn does the same. Either way it echoes the prompt (`isReplay`) where Claude reads it, in order. Prompts that arrive together, as just before a turn ends, can share one echo, their texts joined by a line break. The prompt joins the transcript at its echo, which takes the sent prompt with the same text, else the sent prompts it joins, in any order, else the oldest. The agent stays working until every sent prompt is read. A stop between turns, answered with no prompt still queued (`still_queued`), ends that wait: a sent prompt whose lines a message of the transcript holds was read, and any other becomes a "Message not sent" notice.
  - `queue` holds it in the daemon until a turn ends with no sent prompt left unread, or a stop releases the sent prompts; it then starts the next turn. It can be cancelled, or sent now with `agent.send_queued`.
  - A slash command always queues: Claude Code does not echo every one, so the daemon could not tell when it was read.
  - When the process ends, each prompt Claude did not read becomes a "Message not sent" notice that unfolds to its text.
- Claude Code asks for approvals with `can_use_tool` control requests.
  `AskUserQuestion` and `ExitPlanMode` come the same way; the hub shows them as a question or a plan instead of an approval.
  The answer is a `control_response` with `behavior: allow` (and `updatedInput`) or `behavior: deny` (and a message).
- The daemon's own control requests (`interrupt`, `set_model`, `set_permission_mode`, `get_context_usage`, `get_settings`, `mcp_status`, `mcp_reconnect`, `mcp_toggle`, `stop_task`) carry ids of its own.
  Claude Code echoes control responses; one whose id the daemon did not send is ignored.
- Effort has no control request: the daemon sends `/effort <level>` as a prompt.
  A `/effort <level>` or `/model <model>` typed in the composer is recorded as the menus record it, once it goes to `claude`, so a restart keeps it. A level the cockpit does not know changes nothing.
- Naming a session has none either: `agent.rename` sends `/rename <title>`, so Claude Code's resume list and `sessions.list` show the name. Claude Code then writes `system/session_title_changed`, which also renames the agent after a `/rename` typed in the composer.
  As it starts, before it answers `initialize`, Claude Code restates the session's saved name the same way: the daemon leaves the agent's name, so a rename made while the agent was stopped stays.
  When that saved name is not the agent's, the daemon sends `/rename <title>` once Claude Code has answered `initialize`, so the session takes the agent's name.
- After `--resume`, Claude Code's `total_cost_usd` counts from the cost it restores: the `totalCostUSD` of the session file's last `cost-state` line, which Claude Code writes when its process exits.
  The daemon reads that line in a task as the process starts, so the first turn's end shows that turn's cost alone.
  The agent's cost is Claude Code's total, the session's past included, or what the agent counted when that is more: a process killed with the daemon writes no `cost-state`, and a `/clear` starts a session whose cost leaves out the one before.
  The agent's total can then read higher than Claude Code's own `/cost`.

### Store

Everything lives in `$XDG_STATE_HOME/agents-cockpit/` (default `~/.local/state/agents-cockpit/`), mode 700:

- `agents-cockpit.sock`: the socket, mode 600;
- `serve.lock`: held while a daemon runs, the background service or the app's own, so a second daemon refuses to start and a leftover socket is safe to delete. It names the daemon that holds it (see [The holder of `serve.lock`](#the-holder-of-servelock));
- `agents.json`: one entry per agent (id, title, folder, account, Claude session id, model, effort, mode, position in the list, for an agent made from a past session the part of the session file it shows, and while its `claude` runs, that process's id, start time and, on Linux, boot), written atomically (write to a temporary file, then rename);
- `settings.json`: the hidden MCP servers, the accounts (the default account's name and credentials, the added accounts, the next account id), and other daemon-side settings;
- `events/<agent-id>.jsonl`: the agent's cockpit events, each with its `Seq`, appended, but its details;
- `details/<agent-id>.json`: the frame of the agent's latest `DetailsChanged`, replaced at each change. Each change replaces the last one, and logging them all would grow a busy agent's log by tens of kilobytes per change. A log from an older daemon gives its details to this file at start, once;
- `raw/<agent-id>.jsonl`: every line exchanged with `claude`, for debugging.

The daemon leaves the umask alone, since `claude` and the agents' commands inherit it.
It creates each folder with mode 700 and each file with mode 600.
The socket gets mode 600 right after the bind; until then, the 700 folder keeps other users away from it.

#### The holder of `serve.lock`

Once it holds `serve.lock`, the daemon writes its process into the file (module `lock_holder`).
It empties the file as it lets the lock go, so the file never names a process that no longer holds the lock.
The file holds one line of JSON, with the fields of a process in `agents.json`: `{"pid":4242,"started":987654,"boot":"6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b"}`.
A system that cannot tell a start time writes the pid alone, `{"pid":4242}`.
A daemon older than the record writes nothing, so its lock's file is empty, or holds what a killed daemon left.

On Unix, the app finds and stops the daemon of a state folder with three functions of `cockpit_daemon`:

1. `lock_holder(state_dir)` tries the lock. When it gets it, it lets it go at once and answers `None`. Otherwise it reads the file, and fails with `LockHolderError::Unnamed` when the file names no process.
2. `LockHolder::runs` checks that a process of this boot runs with the recorded pid and start time, as the check of the leftover `claude` processes does. A record without a start time never passes.
3. `stop_lock_holder(state_dir)` reads the holder and refuses one that is the calling process: the daemon runs in the app. It opens a pidfd on the recorded pid, checks the process, then sends SIGTERM through the pidfd, so a process that gets the pid after the check gets no signal. It then looks at the lock every 100 ms, for up to 10 seconds, until the lock is free. It blocks.
   It fails with `StopLockHolderError`: `NoDaemon`, `ThisProcess`, `Unverified` (the process ended, or may be a later one with its pid), `Signal`, `StillRunning`, or the read's own error.

The update of this computer's background service stops its daemon this way (see "Updating a machine's daemon" under [Desktop app](#desktop-app-cockpit_desktop)).
`serve` stops on SIGTERM as on a service's stop: it leaves its agents marked as running, so the next daemon resumes them.
The read takes a free lock for a moment, so a daemon that starts at that moment refuses to start.
On Windows, the lock keeps every other process from reading the file, so only Unix reads it.

On start the daemon reads `agents.json`, finds the last `Seq` in the logs, and resumes each agent that was running with `--resume`.

The event logs of an older daemon hold a `bool` in place of some enums, and still read:

- a turn end's `stopped` and `not_signed_in` give its `outcome`. Without them, a turn end with an error reads as `failed`, and one without as `completed`;
- a user message's `queued` or `delivery` is left out, since waiting prompts live in the details, and a question's `multi_select` gives its `selection`;
- an account's `use_environment_api_key` gives its `credentials`;
- a summary's `waits_for_sign_in` gives its `sign_in_wait`, and its `effort_is_default` its `effort_source`. Without them, the summary reads `not_waiting` and `chosen`.

A daemon that dies without stopping its agents (a forced kill, a crash, no memory left) leaves each `claude` running: its stdin closed, so it exits by itself, but once its turn ends, with its builds still running.
A daemon that resumed the session meanwhile would run two `claude` on one session.
So before any agent resumes, `Daemon::prepare`, shared by `serve` and the daemon in the app, stops each recorded process that still runs with the recorded start time (the kernel's clock ticks after boot from `/proc/<pid>/stat` on Linux, the creation time from `GetProcessTimes` on Windows), and logs it.
On Unix, it sends SIGTERM to the process's group, waits up to 5 seconds for `claude` to end, then sends SIGKILL to the group.
On Windows, a `claude` in a job died with the daemon, so only one that ran without a job can be left. `TerminateProcess` ends it and every process under it: a snapshot of the machine's processes (Toolhelp) gives each process's parent, and the walk down keeps a process only when it started after the parent it names, since a parent that ended gives its id away, and when the process, once opened, still names that parent (`NtQueryInformationProcess`), since a process that ended after the snapshot also gives its id away. For an npm `claude.cmd`, this stops `cmd.exe` and the real `claude` under it.
A process with the recorded id and another start time took the id since, and is left alone.
On Linux, the start time counts clock ticks from the boot, so a process of a later boot can have the recorded id and start time: the record also holds the boot (`/proc/sys/kernel/random/boot_id`), and a record from another boot is left alone. A record saved before the boot existed, or a boot that cannot be read, leaves the start time alone to decide.
On Windows, the start time is a date, in 100 ns units since 1601, so a process of a later boot cannot have the recorded one: the record holds no boot. A boot time from `GetTickCount64` against the clock would move with every clock change, so it could only be compared within a margin, and would add nothing to the start time.
The record is written when `claude` starts and cleared when it exits; a file without it reads as no process.
A Unix without `/proc`, such as macOS, records nothing.

### Past sessions

`sessions.list` reads the `projects/` folder of every account, and lists the newest 100 sessions of all of them, each with its account.
`sessions.resume` names the account whose folder holds the session (the default one when it names none), and the agent made from it runs with that account.

`sessions.resume` reads the session file in a task, before the agent exists, so the first `agent.history` already gets the past.

- Its lines go through the same conversion as `claude`'s stdout (`PastTranscript`), so origins, notices and tool calls come out the same.
  The file's other lines (titles, file history snapshots, API requests) read as lines of an unknown type, which show nothing.
  It leaves out the lines of subagents (`isSidechain`), which it does not tie to their call.
  The user lines marked `isMeta` or `isCompactSummary` are the ones `claude`'s stdout marks `isSynthetic`: it shows a skill's instructions and the summary after a compaction as folded notices, and leaves out the rest, unless another source than the user wrote them.
- The newest 2,000 items show, numbered to end at id 2,001, after a notice when older ones are left out.
  The agent numbers its own items from 2,002, so the two never share an id.
- The past items go in no event: clients read them with `agent.history`.
  `agents.json` records the session and the bytes read, and a restarted daemon reads those bytes again.
  Claude Code only appends to a session file, so the same bytes give the same items, and a large session does not add megabytes to the event log.

### Accounts

The daemon keeps the accounts in `settings.json`, outside the `Settings` that `settings.set` replaces, so an app that predates accounts cannot erase them.

- `accounts.list` answers every account, the default one first, with whether it is signed in, the command that signs in to it, and whether the daemon's environment has `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` (`environment_credentials`: `set` or `unset`; older daemons write `environment_api_key`, about the key alone, which still reads).
- `accounts.add` takes a name and, optionally, a folder and `credentials` (`sign_in` when missing).
  The name has each run of whitespace made one space and is trimmed.
  It holds no control character, has at most 60 characters, and is no other account's whatever its case.
  A given folder must be absolute, hold no `..` and no control character, and be no other account's, the default one's included, whatever its case on Windows and macOS: the default folder without `CLAUDE_CONFIG_DIR` keeps `.claude.json` in the home folder, and on macOS its Keychain entry, so the same folder named in the variable would be another login.
  Without a folder, the daemon proposes `.claude-<name>` in the home folder, the name in lower case with each run of characters other than ASCII letters, digits, `_` and `.` made a `-`, without a `-` or `.` at either end, and `account` when nothing is left (`Client Work` gives `.claude-client-work`, `..` gives `.claude-account`).
  A number is added when another account has that folder (`.claude-client-work-2`).
  Without an absolute home folder, the daemon proposes none, and the request must name the folder.
  The daemon does not create the folder: `claude` does when the user signs in.
- `accounts.change` renames an account or changes its `credentials`, for the default account too. The folder does not change. An agent gets the new key choice at its next start.
- `accounts.remove` refuses the default account (`invalid_request`) and an account any agent uses, running or not (`account_in_use`). It leaves the folder as it is.
- Ids are never reused, so a removed account's id never names another one.

Signing in: the user runs the sign-in command once in a terminal on the daemon's machine.
It runs `claude auth login`, which signs in with a Claude subscription, or with an Anthropic Console account when `--console` is added, then exits.
The command is for a POSIX shell, `CLAUDE_CONFIG_DIR='<folder>' claude auth login` (a `'` in the folder written `'\''`), or on a Windows daemon for PowerShell, `$env:CLAUDE_CONFIG_DIR = '<folder>'; claude auth login; Remove-Item Env:CLAUDE_CONFIG_DIR` (each `'`, and each of PowerShell's typographic single quotes, doubled). The last part takes the variable out of the PowerShell session again.
The default account's command is `claude auth login` alone, unless the daemon's environment has `CLAUDE_CONFIG_DIR`.

The command names the program the daemon runs (module `process::program`), quoted the same way:

- A program `AGENTS_COCKPIT_CLAUDE` names is named by its path: `'<path>' auth login`, or in PowerShell `& '<path>' auth login`.
- On Unix, the default program is plain `claude`, which the daemon and a shell both find on `PATH`.
- On Windows, the default program is plain `claude` only when PowerShell's own lookup finds the same file: in each folder of `PATH` in turn, `claude.ps1`, then `claude` with each extension of `PATHEXT`. Otherwise the command names its path. This covers a `claude.exe` that only the installer's `.local\bin` holds, one behind another `claude` on `PATH`, and an npm `claude.cmd`: npm puts a `claude.ps1` beside it, which PowerShell runs first and its execution policy may refuse.
- The notice in the transcript, `accounts.list` and the app's sign-in card all show this one command. The daemon looks for the program when it writes the command, as it does when it starts an agent.

Whether an account is signed in (`SignIn`): the daemon checks only that `.credentials.json` exists in the folder, and never opens it.

- Linux and Windows: the file there means `signed_in`, else `not_signed_in`.
- macOS: Claude Code keeps the login in the Keychain, under a name tied to the folder, so a missing file means `unknown`.
- A folder signed in with a Console account, or with an `apiKeyHelper`, may keep its key outside that file: the check can say `not_signed_in` although `claude` works.

An account can use an API key or token in two ways, neither of which passes one through the cockpit:

1. `credentials` set to `environment`: the account's agents get the daemon's own `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`, set where the daemon starts (the systemd unit, or the environment the app starts in).
2. The folder itself: signing in to it with an Anthropic Console account (API billing), or an `apiKeyHelper` in the folder's `settings.json`, needs nothing from the daemon.

An agent of an account without a valid login starts as any other: `claude` answers `initialize`, and `system/init` says nothing about the login.
Its first prompt fails: Claude Code writes an assistant line from `<synthetic>` whose `error` is `authentication_failed` (a word of the Agent SDK's types), with a text such as "Not logged in · Please run /login", then a `result` with `is_error`.
The daemon leaves out that text, which names a command the cockpit has no place for, and adds a notice once per turn: "The account Work is not signed in, or its login expired. Run this command in a terminal on the daemon's machine to sign in, then send your message again: `<command>`".
For an account that takes its credentials from the environment, while the daemon has a key or token, the notice says they were refused instead.
The turn ends with the error "The account is not signed in", and its `TurnEnd` has the outcome `not_signed_in`.

The agent then needs you: its summary's `sign_in_wait` is `waiting`, and its status is `NeedsYou` with no pending request, so the app raises a notification and the "Needs you" filter shows it.
It waits until one of these:

- A message goes to `claude`: its turn starts, and it fails again if the account still has no login.
- An `accounts.list`, or the hub's own check every 3 seconds, finds the account's credentials file written after the failed turn ended, even when the agent was stopped or resumed since. After a daemon restart, the log does not say when the turn failed: the file must be newer than the agent's last activity.
  The hub reads the file's time with the sign-in states, then ends the waits before it answers, so the app's "Check again" sees the agent idle.
  The hub's check (`SIGN_IN_CHECK`, in a blocking task, one at a time) also compares each account's sign-in state with the last check's, and sends `AccountsChanged` when one changed, or when an account was not there at the last check. The clients then ask `accounts.list` again, so a sign-in shows without "Check again". The daemon's first check sends nothing: a client lists the accounts when it connects.
  A login in the macOS Keychain writes no file there: only a message ends the wait, and the app's "Send the message again" stays on for an account whose state is `unknown`.
  An account whose `credentials` are `environment` has no file to find either: once its key or token was refused, only a message ends the wait, so the button stays on for such an account too.

`--enable-auth-status`, which the editor extensions pass, adds `auth_status` lines, which report a login in progress (`isAuthenticating`, the output of a helper such as a cloud provider's refresh). A signed-in run gives one with `isAuthenticating: false` and no output. They do not tell a missing login, so the daemon does not pass the flag.

A repository the account has not trusted yet: Claude Code turns trust verification off when it runs without a terminal, as in JSON mode, and asks nothing.
With a new config folder, a repository's `.mcp.json` server connects at once, and `.claude.json` records no trust for the folder.
Choosing the folder in the app is what trusts it, for every account: the app asks nothing more (plan.md, "Security").

An app that predates accounts speaks protocol version 1, and the daemon refuses it (see [Session](#session)).

### Subagents for the agent map

The daemon fills `AgentDetails.subagents` from the lines `claude` writes (module `transcript::subagents`):

- An `Agent` (or `Task`) call starts a subagent, in the turn after the summary's `turns`. Its input gives the description, the type and `run_in_background`.
- The lines whose `parent_tool_use_id` names a subagent's call are its own: each of its tool calls counts, and becomes its `last_tool`. A subagent started by a subagent names that one as its parent.
- `task_started` links a subagent to its task through `tool_use_id`. `task_progress` and `task_notification` then give its tokens, and its tool calls when they count more.
- A foreground subagent ends with its call's result, or with its task's end. A background subagent's call returns at once: it ends with its task's end, from `task_notification`, `task_updated`, or the `<task-notification>` message Claude Code writes, which names the call. A subagent that ends stops the foreground subagents under it, which cannot outlive it.
- The turn's `result` ends the foreground subagents that still run: stopped after an interrupt, failed after an error, done otherwise. A foreground subagent under a background one is not the turn's: it runs on, and ends with its call's result or with that background subagent. An exited process, or a restarted daemon, stops them all.
- A subagent waits for you while an approval, question or plan of its own lines waits.
- The list keeps every running subagent and the latest ended ones, 200 in all.

The captured lines link a call, its task events and its own lines by one tool call id, but do not show whether a foreground subagent gets task events. Without them, a subagent still shows its calls, its time and its status, and shows no tokens.

Stopping: a subagent with a task stops with `agent.stop_task`, which the app sends for it and for each running subagent under it that has a task. The control protocol the daemon uses has no request that stops one foreground subagent without a task. Only `agent.interrupt` stops it, with the agent's turn and every foreground subagent in that turn.

### Machine load

`machine.load` answers the latest sample and the samples of the last five minutes, for the server and for each agent: CPU in cores in use, memory, the load average (`None` on Windows, which has none), and each agent's three heaviest commands.

- A task samples every 3 seconds, and only while clients ask: the first `machine.load` starts it, and it ends 30 seconds after the last one. Its history then starts afresh with the next request.
  The last 100 samples stay in memory: the event logs never see them.
- The server's figures come from `/proc/stat` (CPU), `/proc/meminfo` (used memory is the total minus `MemAvailable`) and `/proc/loadavg`.
- An agent's processes are its `claude` and every process under it.
  The daemon walks down from `claude` through the kernel's children lists, one per thread (`/proc/<pid>/task/<tid>/children`), and reads each process's `stat`.
  It reads no other process of the machine, except on a kernel without those lists, where it reads every process's parent instead and logs a warning.
- An agent's CPU counts each process's own time and the time of the children it reaped (`cutime` and `cstime`), so a command that starts and ends between two samples still counts.
  For example, over 3 seconds at 100 ticks a second, `cargo` grows from 40 to 340 ticks, 250 of them from a `rustc` it reaped that had 150 ticks at the previous sample: the agent used 150 ticks, 0.5 cores.
  A process that leaves the tree and still runs, as a server started in the background does when its shell exits, gives nothing back, and counts no more.
  A new `claude` for the agent starts its counts afresh.
- "Other processes" is the server's use minus the agents', never below zero.
  Memory is resident memory summed over the processes, so a page that several of them share counts once for each.
- The daemon reads the command line of each agent's three heaviest processes only.
- The `/proc` files stay open from one sample to the next and are read again at offset 0, which costs about a tenth of an open.
  A file opened under `/proc/<pid>` stays bound to that process, so a pid that another process takes is never read in its place.
  One reading walks at most 4,096 processes, and the daemon keeps at most 4,096 files open, and no more than a quarter of its open-file limit.
- The cost, measured on a 64-core server running 3,700 processes, in CPU time per sample: 80 µs for the server's figures, 63 µs of them for `/proc/stat`; 300 µs for one busy agent of 21 processes and 146 threads; 1.5 ms for 11 agents of 117 processes and 793 threads in all, or for a made-up tree of 321 processes.
  Most of an agent's cost is one read per thread: `claude` alone runs about 80.
  The full scan of the fallback costs 45 ms there.
  The test `sampling_cost` measures it: `cargo test --release -p cockpit_daemon sampling_cost -- --ignored --nocapture`.
- On a machine without `/proc`, `machine.load` answers `unsupported`.

On Windows, the same task samples other counters (modules `job_reading` and `job_sample`):

- The server's CPU comes from `GetSystemTimes`, its cores from `GetActiveProcessorCount`, its memory from `GlobalMemoryStatusEx` (used is the total minus the available).
- An agent's processes are those of its Job Object. Its CPU is the growth of the job's accounting (`QueryInformationJobObject`), which counts the processes that ended too. Its memory is the working sets of the processes in the job, summed.
- Each process's CPU comes from `GetProcessTimes`, and its start time tells it from a later process with its id. The heaviest three show their command line (`NtQueryInformationProcess`) or their program's name.
- An agent whose `claude` runs without a job counts `claude` alone. One reading opens at most 4,096 processes.
- The heaviest commands leave out `claude`: the root process, and for an npm `claude.cmd`, the root's children (each process's parent comes from `NtQueryInformationProcess`).

`agent.stop_process` sends SIGTERM to one of the agent's processes.
The daemon opens a pidfd on the process, checks at that moment that its parents in `/proc` reach the agent's `claude`, then signals through the pidfd.
A process outside the agent is refused with `not_found`, and `claude` itself with `invalid_request`: `agent.stop` stops it.
When the process ends and its pid goes to another one during the check, the signal goes to the ended process through the pidfd, and fails.
On Windows, the daemon opens the process, checks with `IsProcessInJob` that it is in the agent's job, then ends it with `TerminateProcess` through the same handle, which keeps its id from going to another process meanwhile.
A child of an interpreter root, the real `claude` of an npm `claude.cmd`, is refused with `invalid_request` as the root is.

### Server and peer checks

- The daemon listens only on the Unix socket: no TCP port.
- Each connection passes two checks before any byte is read:
  1. the peer's user id (`SO_PEERCRED`) equals the daemon's;
  2. the peer's parent chain in `/proc` does not reach the daemon's pid, so a command an agent runs cannot drive the daemon.
     The walk stops, and accepts, at a process of another user or one that `/proc` hides (`hidepid=1` or `2`): the daemon's descendants all run as its user, and `/proc` always shows a user's own processes.
- `agents-cockpit bridge` connects to the socket like any client; it descends from `sshd`, not from the daemon.
- The daemon in the app listens on no socket, so it has no peer to check (see [The daemon in the app](#the-daemon-in-the-app)).

## Client (`cockpit_client`)

- `Transport::Local { socket }` connects to the socket.
- `Transport::Ssh { host, remote_command }` runs `ssh -T -x -o BatchMode=yes -o ForwardAgent=no -o ClearAllForwardings=yes -o ControlPath=none -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -- <host> '<remote_command>' bridge`.
  The keepalives end ssh after a minute of silence, so a dead link shows as a lost connection.
  `BatchMode` makes a missing key fail at once instead of waiting for a password the app cannot show.
  `ForwardAgent=no`, `-x` and `ClearAllForwardings=yes` open no agent, X11 or port forwarding to the laptop, even when the user's ssh config asks for one.
  `ControlPath=none` keeps the app out of ssh connection sharing, even when the user's config sets `ControlMaster`. The app never joins the user's shared connection and never hosts one, so neither cuts the other off, and the user's port forwards never run in the app's ssh process. `ControlMaster=no` would not do: ssh still joins an existing master unless `ControlPath` is `none`. A `ProxyJump` host in the user's config is reached by a second ssh, which these options do not reach: that hop can still share a connection.
  `--` keeps ssh from reading the host or the remote command as an option, and the quotes keep the host's shell from reading anything in the remote command.
- `Transport::InApp(InAppLink)` reaches a daemon in the same process.
  Each connection opens a `tokio::io::duplex` stream and sends the daemon its other end through the link's channel. A link whose daemon has stopped fails with `ConnectError::InAppStopped`.
- `Connection` sends requests and matches responses by `RequestId`, and yields events.
  It reads the daemon at all times, so a response never waits behind events the app has not read.
  Events wait for the app in a queue of 4096: when an event finds it full, the waiting events are dropped and `next_event` returns `CloseError::EventsLost`.
- `Session` reconnects with backoff after a drop, sending the last `Seq` it saw.
  After lost events, it reconnects without a last `Seq`, so the daemon answers with a snapshot.

## Desktop app (`cockpit_desktop`)

- **Config**: `~/.config/agents-cockpit/config.toml`, per machine, names the daemon to reach (module `config`):

  ```toml
  [daemon]
  transport = "ssh"            # or "local", or "in_app"
  host = "build-box"
  remote_command = "/opt/agents-cockpit/bin/agents-cockpit"
  ```

  - `transport = "local"` takes an optional `socket` (default: the daemon's default socket), and `start_service = true` to start the service when the socket does not answer. The service is the app's own program, run again with `--state-dir <folder> serve`, so any build of the app can start it.
  - `transport = "in_app"` runs the daemon in the app.
  - A missing file means the local daemon at its default socket.
  - A `config.toml` that is a symbolic link, as home-manager or a dotfiles repository makes, stays one: the app writes the link's target.
  - Each `[[machines]]` table names another machine the app reaches at the same time, over SSH: `name` (what the sidebar shows), `host` and `remote_command`.
  - An SSH machine, `[daemon]` or `[[machines]]`, takes an optional `source_dir`: the git checkout the daemon's update builds from, `~/agents-cockpit` when unset (see below).
- **Several machines** (module `machine`): `[daemon]` is machine 0, this computer, and each `[[machines]]` table the next number.
  The interface calls machine 0 "Local", with a laptop, and each server by its name, with a server (`MachineName` in `state/machines.rs`). A `[daemon]` over SSH shows its host instead of "Local". A server cannot take the name "Local".
  Each daemon numbers its own agents, so the app gives each agent an id of its own: the machine in the high bits, the daemon's id in the low 20.
  Machine 0's agents keep the daemon's ids.
  Each machine has its own connection task, which translates the ids of what it sends and receives (`map_agents` in `cockpit_protocol`).
  A router hands each request to the task of the machine its agent is on. A request about no agent, such as the sessions list, names its machine (`Outgoing.to`).
  `AppState.machines` holds what the app knows of each daemon (`MachineState`: label, connection, plan usage, settings, accounts, sessions), and a snapshot from one machine replaces that machine's agents alone.
  Account ids collide across machines, so every account function takes the machine, and the Accounts dialog picks one.
  The header's plan usage, the hidden MCP servers, the undo of a close and the machine load follow the selected agent's machine.
  A machine over SSH that does not answer is tried again every 10 seconds, and its requests fail meanwhile. "Retry now" on its panel ends the wait.
  What the app says of each machine comes from one function (`machine_status::status`): a tone (green, grey, amber or red), the Machines dialog's chip, and the line under its panel's head with its links. An update to show (`daemon_update::update_line`) comes before how the machine answers: connecting, not connected ("Retry now"), or, for Local, no daemon yet or a daemon that does not answer ("Choose how agents run here", which opens the "No daemon answers" screen as a dialog).
  The Machines dialog (`state/machines.rs`, `ui/machines.rs`) has a row per machine, Local first, its parts in columns that line up: the machine's icon, its name with where its daemon runs and its agents' count (and when the app tries it again, or that its agents stop with the app), its status chip, the release its daemon runs while connected, one button of one width (the first link of its panel's line, such as "Retry now" or "Update"), and on a server's row a ⋯ menu. The menu answers the keys as the row menu does: "Edit the server" (E), "Copy the SSH host" (C) and "Remove from the app" (R). "Add a server", under the rows, opens the form for a new server, and Cancel or Esc closes it. Esc closes the menu first, then a form, then the dialog.
  The dialog adds, edits and removes servers. The connection task starts or ends their tasks and writes the list back (`save_machines`, which keeps the rest of the file as `save_daemon` does). Removing a server drops its agents from view and leaves them running there.
  Edit opens a server's form in its row, filled from the config each `MachineState::server` holds: the SSH host, the name, the path and the source checkout of its updates. It is off while the server's update runs. A save is checked as an add is. The connection task ends the server's task and starts one with the new config (`Servers::restart`), and the agents come back with its snapshot. A new name takes the preferences kept under the old one (`Prefs::rename_machine`): account colors, notification levels, project moves, files panes and places in the sidebar. What a removed server left under the new name goes. The failed update and the older-version card take the new config for their next run.
  Only machine 0 has the "No daemon answers" screen. With servers configured, it shows as a dialog from Local's panel, so the servers stay usable.
- **Reaching the daemon** (module `reach`): the connection task turns the config's daemon into a transport.
  For `in_app`, it starts `InAppDaemon` with the default state folder (`InAppHost` keeps it), or connects to the socket with a note when another daemon holds the lock.
  For `start_service`, it starts the service unless its socket answers.
- **"No daemon answers"**: when the first connection to the daemon fails, the session stops and the window shows this screen in place of its panes (or as a dialog when servers are configured).
  It names the daemon the app tried and the error, and offers Retry (the daemon it tried), "Run agents in this app", and "Start the background service here" (on Unix only).
  A choice holds for this run only, so the next start tries the config's daemon again.
  With "Use this every time" ticked, the app writes the choice into the config first (`save_daemon`, which keeps the file's other settings and comments): `in_app`, or `local` with `start_service`.
  When the config is a symbolic link, `save_daemon` writes its target through a temporary file beside it, which takes the target's mode, and the link stays.
  A read-only config, such as a link into the Nix store, is not written: the screen shows why, and the choice still holds for this run.
  The screen's state is pure (`state/no_daemon.rs`): which choice is under way, the tick, and which daemon a choice runs and saves.
  A connection that drops after it connected reconnects as before, with no screen.
- **Updating a machine's daemon** (`state/update_offer.rs`, `state/update_plan.rs`, `state/daemon_update.rs`, `update_script.rs`, `service_update.rs`, `ui/daemon_update.rs`): the app treats every machine the same way, this computer included. `MachineState.version_gap` says where the machine's daemon stands against the app (`GapKind`):
  - `OlderProtocol`: a first connection was refused for its protocol, and the daemon is older. `ConnectError::daemon_protocol` gives the daemon's protocol, from `VersionMismatch` or from the text of an older daemon's `Refused`. It counts where the app can update the daemon: over SSH, and on Unix this computer's background service or a daemon at the app's socket. The app cannot use the machine meanwhile. Such a daemon gets the "runs an older version" card in place of the "No daemon answers" screen, with "Update <machine>" first. Any other daemon of an older protocol keeps that screen.
  - `NewerProtocol`: refused, and newer. The "runs a newer version" card offers no update.
  - `OtherBuild`: connected, and the daemon's release is older than the app's, or the releases are equal and both commits are known and differ. The machine stays in use, and its line says "Update available.".
  - `NewerRelease`: connected, and the daemon's release is newer. The card offers no update, and suggests updating the app.

  `connected` sets the gap from the release and the commit of `Welcome`, and `attempt_failed` from a refusal. Releases compare as Cargo versions (`release_order`). The app's own is `cockpit_daemon::RELEASE_VERSION`, the constant the daemon sends, so a daemon built from the app's tree compares equal. Equal releases with a commit either side does not know offer nothing. `update_offer::offer` gathers what the screens show: the kind, the plan, the daemon's build while connected, the app's, and the details.
  Each machine's line (`update_line`) says it in the sidebar and the Machines dialog, with an `Update` link (`Details` when the app has no plan) that opens the screens as a dialog (`Dialog::DaemonUpdate`), this computer's too. A machine the app cannot use also gets `Retry now`, and the other machines keep working.
  - Plans (`UpdatePlan`): `Server` runs the script below, for a daemon reached over SSH, when the app knows its commit. `Service`, for this computer's background service or a daemon at the socket of a state folder (Unix), stops the daemon that holds the folder's lock and starts the service with the app's own program. The daemon in the app needs no update: it runs the app's code. The connection task sets `MachineState.update_plan` from the daemon it reaches: `plan_for_this`, from the config and the place reached, which covers the socket daemon the daemon in the app gave way to, and `plan_for_machine`. The script's steps are "Get version", "Build it on the server", "Restart the daemon" and "Reconnect"; the service's are "Stop version X", "Start version Y" and "Reconnect".
  - Busy agents (`busy_agents`): a restart ends the turns of the agents that work or wait for an answer, as the app last saw them. An agent that waits only for its account's sign-in runs no turn. When the user asks to update a connected machine that has some, `Dialog::ConfirmUpdate` names them and offers "Update when they are idle", "Update now" and "Cancel". A machine the app does not reach shows no agents: its update starts at once, and a server's script checks them itself.
    "Update when they are idle" sets `DaemonUpdate::Waiting`, and the machine's line says it waits. After each update from the machine, its connection task calls `start_when_idle`, which starts the update once none of its agents is busy, while the app is connected. "Update now" runs with `BusyAgents::Interrupt`, which passes `--even-if-busy` to the script: it skips the refusal for busy agents and for agents that may hold a scheduled job, and nothing else. The script's refusal (exit 3) offers "Update now" too.
  - This computer's service (`service_update.rs`): `restart` holds the order on every platform, and `ServiceControl` the system's calls, which the tests replace. It stops the daemon with `cockpit_daemon::stop_lock_holder` (see [The holder of `serve.lock`](#the-holder-of-servelock)) from `spawn_blocking`, starts the service with `service::ensure`, and the app then reconnects. Nothing cancels it. A stop that fails says why, with what the user stops by hand (`HandStop`): the process, when the daemon did not stop or took no signal; or the `agents-cockpit serve` of the state folder, once, for a daemon too old to record its process, and when the record matches no running process. A start that fails shows the error and the lines the service wrote to `serve.log` as it failed. Once the daemon stopped, the next connection reaches a new daemon: one of another build means that another program restarted the old one, such as a systemd unit that restarts it whenever it stops, and the failure says so (`OtherBuildAnswers`).
  - While the app reaches this computer alone, its update screens take the window's place while an update runs or failed and the app cannot use the daemon (`takes_the_window`). A wait for idle agents, or a daemon that still answers, leaves the window in use.
  - `cockpit_daemon`'s `build.rs` writes the commit the tree was built from into `AGENTS_COCKPIT_COMMIT`, from `AGENTS_COCKPIT_SOURCE_COMMIT` when the environment sets it (the Nix build sets it to the flake's revision, empty for a tree with uncommitted changes, and the release build to the tree's commit) and else from `git rev-parse HEAD`. The two variables have different names because `cargo run` puts `AGENTS_COCKPIT_COMMIT` in the program's environment, so the agents the app starts would build with the app's commit. It tells cargo to rerun when `HEAD` or the ref it points to moves, in a linked worktree too. `cockpit_daemon::build_commit` reads it: the daemon sends it in `Welcome`, and the app takes it as its own commit (`update_script::app_commit`), since the app builds the daemon from its own tree. So each new commit rebuilds `cockpit_daemon` and both programs. Without a commit the older-version card offers no update and says so.
  - A server's update runs beside the connection, in a task of its own: the embedded `contrib/update-daemon.sh` goes on the stdin of `ssh <host> sh -s -- --commit <sha> --binary <remote_command> [--source-dir <dir>] [--even-if-busy]`, with the same ssh options as the bridge. It uses plain SSH because an old daemon refuses the app's protocol. Its output streams into the screen's "Build output", and the steps follow its lines (`update-daemon: building`, `restarting the daemon`, `the daemon runs`).
  - The update's state (`MachineState.update`) lives per machine and outlives the connection's attempts, which an old daemon keeps refusing while the build runs: the screen shows it until the first connection after success, which the app tries at once. A connection to a daemon of the app's build ends any update. Otherwise a server's update goes on until its script ends, since the app may have reached the old daemon again; a script that ends once the app reached the new daemon needs no retry.
  - The script's exit status picks the failure screen: 3 is the agents' refusal, with their list and how to stop one; 1 says the machine still runs its old version, and that the update was cancelled when "Cancel the update" reached the server; 4 says the daemon may not have come back; 5 says the new version did not start and the machine runs its old version again, with the script's message; 6 says another update is already running on the machine, which the running screen invites by letting the user close the app. Any other status, such as ssh losing the connection, says the app cannot tell how it went. The sidebar says "Was not updated." for 1, 3 and 5, "Update failed during the restart." for 4, "An update is already running." for 6, and "Update status unknown." otherwise. "Update again" reruns, and "Back" returns to the older-version card. A daemon that is now newer than the app ends the failure's screen: "Update again" would move it backwards.
  - "Cancel the update" runs the same script with `--cancel` over a second ssh, until the restart step starts. The running script stops before it installs. Its build runs on.
  - When no daemon answers after the restart (or the restart command fails) and a previous binary exists, the script puts `<binary>.previous` back and restarts the daemon the same way, under the same ignored signals. It exits with 5 when a daemon runs that binary again, and with 4 when none comes back. A restart that leaves another binary running also exits with 4: a rollback would not change it. A retry reads the agents with `<binary>.previous` when `<binary>` cannot, since a failed restart can leave the old daemon behind a new binary.
  - The update's lock is `lock.pid` in the script's folder: the owner's pid and start time (field 22 of `/proc/<pid>/stat`, which tells a reused pid apart), written to a temporary file and moved into place, so no lock shows without its owner. A dead owner's lock counts as free, so a reboot or a SIGKILL blocks nothing. `flock` on `lock.pid.guard` makes reading the lock and replacing it one step, so exactly one of several updates takes it. A live owner's lock refuses with 6.
  - Over SSH with no terminal, a client that goes away sends the remote processes no signal: it only closes their pipes. So the script ignores SIGHUP and SIGPIPE from its start, and cargo writes through `tee` to `build.log`, so a lost connection cannot end the build or the install.
- **The background service**: the app runs its own program again, as `<program> --state-dir <folder> serve`.
  It runs in a process group of its own, so Ctrl+C in the terminal that started the app does not reach it, and it outlives the app.
  Its stderr goes to `serve.log` in the app's state folder.
  The app waits up to 20 seconds for its socket, and shows the screen again with the reason when the program is missing or the service stops at once (the log's last line).
  An update of this computer's daemon stops the service and starts it again with the app's program (see "Updating a machine's daemon").
- **Closing the window**: the event loop ends the process right after its last event, without dropping anything, so the app stops the daemon in the app in that event (`LoopDestroyed`), waiting at most 5 seconds after the agents are killed.
- **A panic** (`stop_on_panic`): the app's panic hook reports the panic through the default hook and the log, then, while the app runs agents itself, stops the daemon in the app as the window's close does and exits with status 101.
  Otherwise the agents' processes, in process groups of their own on Unix, would outlive the app, and the next start would resume their sessions a second time.
  Only the first panic stops the daemon, and a panic in the stop ends the app at once.
  The stop waits at most 1 second for the host's lock, which the panicking thread may hold.
  A panic on the window's thread waits for the stop, since that thread's unwinding would end the process. A panic on another thread unwinds at once, so that its task's drop kills the processes the task holds.
- **Log** (module `logging`): the app logs to stderr, at the level `RUST_LOG` sets (`warn` by default).
  A release build on Windows is a GUI program, which opens no console window, and one started from the desktop has no stderr: its log goes to `app.log` in the app's state folder.
  The file rotates by size: once `app.log` would pass 10 MiB, it becomes `app.1.log`, `app.1.log` becomes `app.2.log`, and so on, and the oldest of the 5 files goes (50 MiB at most). A line is never split, and a new run appends to `app.log`, so the rotation carries on across runs. A rotation that fails, as when another program holds a file, leaves the log in the file it has and is tried again 10 MiB later, so no line is lost.
  An older version's `app.previous.log` becomes `app.1.log`, which is the run before the current `app.log`, unless `app.1.log` exists already: then it is deleted. When that file can be neither moved nor deleted, the log still starts, and its first line says why.
- **Notifications** (module `notifications`): at most one per agent, for an agent that needs you or finished, and a click on one brings the window up and opens its agent.
  An agent finishes once its turn has ended and none of its background tasks still runs (`background_running`).
  Until then, its sidebar row and the header say "Idle, N running in the background", with the status dot's ring in the working color.
  When the last background task of an idle agent ends, the app waits 10 s (`FINISH_HOLD` in `state/apply.rs`) before it announces the finish, and drops it if the agent works again meanwhile: Claude Code usually starts a turn to read the task's result, and that turn's end is the finish.
  On Linux they go through the freedesktop notification service, and a "needs you" one closes once the agent no longer needs you.
  On Windows they are toasts, from notify-rust's Windows backend.
  Windows shows toasts only for an app the Start menu registers, which the app is not, so the toasts carry PowerShell's name and icon.
  A click reaches the app only while the toast is on screen: once the toast is in the notification center, a click goes to PowerShell.
  notify-rust cannot withdraw a toast, so a "needs you" toast stays until it times out.
  Which ones show is this desktop's choice (`state/notification_levels.rs`), saved in `prefs.json`: a level for the app, from the bell in the sidebar's head or the Settings dialog, and one per agent, from its row's menu, keyed by the daemon machine's name and the daemon's agent id. A level is "All", "Only when it needs you" or "Off", and an alert shows only when both levels let its kind through. An agent whose level is not "All" shows a bell on its row (crossed out and grey for off, with a grey "?" for "needs you" only) and a tag in the header that opens its choices. A change of level reaches every machine's notifier (`ToDaemon::NotificationLevels`), which closes the notifications the levels no longer let through. The daemon's removal of an agent drops its level, since a later agent may take its id.
  The config file's `[notifications] enabled = false` sets the app's level to "Off" until a choice in the window, which then wins. The preferences never save the config's level. The demo raises none, and reads and saves no preferences: its agents take the ids of real ones.
- **Paths** (module `path_style`): the app follows the path rules of the daemon's machine, not its own, since the app on Windows may reach a Linux daemon over SSH.
  `PathStyle` holds the rules, on text, so the Linux tests check both.
  On Unix, `/` separates names and case matters.
  On Windows, `\` and `/` separate names, `C:\` and `\\server\share\` are roots with no folder above, names compare whatever their case, and paths show with `\`.
  A path tells its machine by its root: an agent's folder starts with a drive or `\\` on Windows, and with `/` on Unix.
  The new agent dialog's folder field has no agent: it takes the daemon's machine, Unix over SSH (only Unix runs `agents-cockpit bridge`) and the app's own platform otherwise.
  Ctrl+O, the folder field, the files pane, the file view's tabs and short paths, markdown file links and `path:line` references, the header, the switcher and the recent folders follow these rules.
- **Preferences**: `$XDG_STATE_HOME/agents-cockpit-desktop/prefs.json` (default `~/.local/state/agents-cockpit-desktop/`), on the machine the app runs on, keeps one viewer's choices: the file view's width and its wrap toggle, the sidebar's status filter and width, each agent's files pane, open or closed, the pane for a new agent and the pane's width, the accounts' colors, how the sidebar groups the agents, the app's order of the agents and the projects, where new comments on quotes show, how long a message sent to a working agent can be cancelled, whether Esc asks before it stops an agent, and the changed keyboard shortcuts (the Settings dialog, `state/settings.rs`, opened by the gear in the sidebar's foot).
  A missing or broken file means the defaults.
- **Splitters** (`ui/splitter.rs`): a line on the sidebar's right edge, and one on the left edge of the file view and of the files pane, resize them, by a drag or the Left and Right keys, and a double-click gives back the default width. The sidebar keeps a width in pixels, from 200 to 480 and at most 40% of the window. The files pane keeps one from 180 to 520. The file view keeps a share of the window. One page script follows the drag, and Rust hears of the new size once, on release.
- **List of open files** (`state/file_list.rs`, `ui/file_list.rs`): a button at the end of the file view's tabs shows how many files are open and opens their list (`Popover::FileList`), every file at once, with the shown one checked. A filter field narrows it on the path the row shows, without the folders above the agent's folder. Up, Down and Enter show a file, and Ctrl+W or a row's × closes one while the list stays open. The pointer moves the keyboard's highlight, as in the palette. A right-click on a row opens the tab menu for its file, over the list. The list closes with the file view.
- **Tab menu** (`state/tab_menu.rs`, `ui/tab_menu.rs`): a right-click on a file view tab, or on its row in the list of open files, opens a menu at the pointer (`Popover::TabMenu`, the tab in `FileView.menu`, and its `TabMenuOrigin`): close the tab, the others, the tabs on its left or its right, or all of them; copy its path or its path from the agent's folder; mention it in the message; or show it in the files pane. A close that takes the shown file shows the right-clicked tab instead, unless that tab goes too.
  - It answers the keys as the row menu does (`ui/menu.rs`, `state/menu_keys.rs`): W, O, L, R, A, C, E, M and F pick its rows.
  - "Show in the files pane" is greyed out for a file the tree never shows, by the test the tree's rows follow (`file_tree::can_show`): a file in the agent's folder, a file the agent changed or read, or a file in a folder outside the agent's that holds such a file, unless the daemon refuses to list that folder.
  - From the list, its groups are the rows the list shows: "Close the files above" (U) and "Close the files below" (D) take the rows its filter leaves. A close or a copy keeps the list open, as do Esc and a click beside the menu, and the focus goes back to its filter.
- **Messages on their way** (`state/sending.rs`): a sent message shows at once, dimmed, without waiting for the daemon's echo, a round trip that is long to a server. It shows where the daemon most likely puts it: at the end of the chat for an agent between turns, as a "Sending" row above the composer while the agent's turn runs or messages wait. The echo replaces it: a message added to the chat, or one more waiting message, with the same text and as many images. A refused message leaves, and its draft comes back. A reconnect's snapshot drops them with the agent's state.
  A message steered to a busy agent can wait in the app before it leaves, for the time Settings sets ("Time to cancel a message sent while the agent works", `chat.steer_hold`: off by default, or 3, 5 or 10 s). Its row says "Sending in 3 s", counting down, with Send now and Cancel. Cancel puts its text back in the composer, after the text there. Once sent it cannot be taken back: Claude Code holds it, and no request removes it. A message sent while one is held waits behind it, so they reach the agent in the order sent, and Send now sends the held ones before it too. A task for the window's life sends each held message when its hold ends, whichever agent shows. While the agent's machine does not answer, a held message waits, and the reconnect's snapshot keeps it. A refused one comes back to the composer after the text typed meanwhile. A message still held when the app quits is not sent.
- **Esc and Stop** (`ui/composer.rs`, `state/shortcuts.rs`): one button ends the composer. While the agent works or waits for you, it is Stop while the message is empty and Send once something is written. For a second after it sends, it stays an inactive Send, and the focus goes back to the text. A double click or the next key then never stops the agent. Esc closes what is open first: the comment field, then a menu or palette, then a dialog. With nothing left open and the agent busy, Esc on the page or in the composer opens "Stop the agent". Esc in another field, such as a search, stops nothing. The dialog takes the focus itself, and a plain Enter stops. A character typed into it, Space included, closes it and keeps the agent working: the character goes into the message, and the focus goes back there. Keys with Ctrl, Alt or Meta do not close it, since a shortcut may take them. Esc and Keep working put the focus back in the message too. The dialog closes once the turn ends, a reconnect included. Its box "Stop without asking next time" sets `chat.esc_stop` to `at_once`, and Settings sets it back ("Ask before Esc stops an agent"). With `at_once`, Esc stops the turn at once. On an idle agent, Esc does nothing. The file view is a pane, not something Esc closes.
- **Files pane** (`state/file_tree.rs`, `ui/files_pane.rs`): Ctrl+B or the header's files chip opens a pane at the window's right edge, after the file view, with the selected agent's folder as a tree. Each agent keeps its pane open or closed (`FilesPanePrefs.agents`, keyed by the daemon machine's name and the daemon's agent id, as the notification levels are), and an agent never toggled takes the Settings dialog's choice for a new agent, closed until changed. The daemon's removal of an agent drops its pane. A switch to an agent whose pane differs shows it at once: as the file view, the pane slides only when its own agent opens or closes it. One splitter width serves every agent. Each agent keeps its own tree (`AgentEntry.tree`): open folders, listings, filter, highlight and search.
  - "All" lists a folder with `agent.list_dir` when it opens, then, under "Also worked in", the folders outside the agent's that hold a file it changed or read. Such a folder outside every git checkout, which the daemon refuses to list, shows the files the agent touched in it. Its folders start open too. "Changed" and "Read" show only those files, under folders that start open and close on a click (the tree keeps the closed ones apart from the opened ones of "All", in `closed`), and replace the old changes list: the chip opens the pane on "Changed" when the agent changed a file. In these two, and in a folder outside every checkout, a folder that holds one folder and no file shares its row ("crates/lexer/src"), which closes by the path of its last folder. The search field runs `agent.find_files` once typing pauses. "Close every folder" closes the folders of every view. A click acts on the row clicked, since a folder can show twice under "All": inside a folder outside every checkout and at the top level.
  - A long name loses its middle, not its end: the row keeps its last 12 characters whole ("settings-pa…component.ts").
  - A row shows the lines the agent changed in a file, a dot on a file it read, and an amber dot on a folder that holds a change. Rows the checkout ignores, and everything under them, are dimmed.
  - The listings shown are read again, keeping the old entries on screen, when the agent finishes an edit or a Bash call, when the agent comes back on screen, on F5 and on Refresh.
  - Up and Down move the one highlight, which the pointer moves too. Right opens a folder or goes into it, Left closes it or goes to the folder above, Enter opens a file in the file view, and Alt+Enter puts its path in the message. A right-click opens a menu (`Popover::TreeMenu`): open (O), open on its diff (D), mention in the message (M), copy the path (C) or the relative path (E). It answers the keys as the row menu does.
  - "Show the file view's file", and "Show in the files pane" in the tab menu, open the file's folders and highlight its row (`file_tree::reveal`). The tree keeps the row in `FileTree.reveal` until it shows, as a folder may wait for its listing, and the pane then scrolls it into view (`RevealScroll`). The arrows scroll the pane to the row they highlight.
- **Markdown preview** (`ViewMode::Preview`, `FilePreview` in `ui/file_view.rs`): a `.md` or `.markdown` file gets a third view mode, which renders it with the chat's markdown component, up to `MAX_RENDERED_LINES` lines. Such a file opens in it unless the agent changed it. A changed one opens on its diff, or in the preview when the file shown before was in it. A link to a line opens the whole file. Relative links start from the file's folder, and open the file view by the same rule as the chat's links. Task list items show a read-only checkbox, in the chat too.
- **Reading the shown file again** (`state/file_reread.rs`): the file view reads its shown file once on opening, then again when it comes back on screen (its agent selected again, the window back in front), when the selected agent finishes an Edit or Write of it, and on Reload or F5 for changes the transcript does not show. Off screen, an edit triggers no read: coming back reads it. The old text stays on screen until the new one arrives, and the view keeps its mode and scroll. The foot says when the text was read ("Read at 14:32:07, 3 min ago"), or "Updated just now" for 10 s after a read that changed it, while the lines that changed glow and fade. A read again that fails, but for a missing file, keeps the old text and adds an error note. The page script takes F5 everywhere, so the webview never reloads the page.
- **State**: `AppState` holds the summaries, the open transcripts, the plan usage and the UI state (selected agent, unread agents, open blocks).
  A pure function `apply(&mut AppState, Event)` updates it, so the state logic is tested without a window.
- **Connection task**: a Dioxus coroutine owns the `Connection`, applies events to the state signal, and sends the requests the components ask for.
- **Components** follow the mockups: sidebar, center header (the title, which a click on it, its pencil or Rename in the "/" menu turns into a field that renames the agent; usage chip, load chip, files chip), transcript, composer (model, mode, cache timer, agent map button, Send or Stop), floating blocks and badges (tasks, messages, running, MCP servers), file view, command palette, agent switcher, new agent dialog, agent map, notifications.
- **Accounts** (`state/accounts.rs`, `ui/accounts.rs`): with two accounts or more, each agent shows its account in the account's color: as its first letter after the status dot of its sidebar row, the name on hover, and as a tag with the name after the status in the header. Past sessions and recent folders show theirs too. With one account, nothing names it.
  - The protocol carries no color: each viewer picks them, and the preferences keep them by the daemon's machine and the account's id. An account without a choice takes the color its id gives. The six colors avoid the status colors.
  - The app asks `accounts.list` once connected, after `AccountsChanged` (which the daemon also sends when an account signs in or out), when the new agent or the accounts dialog opens, and on "Check again". The answer gives each account's sign-in state and command. The daemon ends the sign-in waits that are over before it answers.
  - The new agent dialog's account dropdown sits at its top, after the machine and above the "Start in a folder" and "Resume a session" tabs, which both use them (`NewAgentTarget`). It lists the chosen machine's accounts, and shows with two accounts or more. It starts on the account of the last agent started on that machine, else the default account, and only a pick in its menu changes it: the folder, typed or picked, never does. The menu shows each account's plan usage (the 5-hour figure amber from 80 %), "no plan limits" for an account that uses the environment's API key or token, or "Not signed in", then "Manage accounts".
  - "Accounts", in the sidebar's foot, opens a dialog with a card per account: its color, name, folder, sign-in state and number of agents. A card opens to rename the account, change its color or its environment choice ("Use the API key or token from my environment"), copy its sign-in command, or remove it. Remove is off for the default account and for an account any agent uses.
  - "Manage accounts" in the new agent dialog's menu opens the same dialog, which keeps the new agent dialog's form (`from_new_agent`), as "Add an account" opened from it does. Closing it (Close, Esc, a click outside it, Ctrl+N, or Esc in "Add an account") goes back to the new agent dialog with its folder, message, account, model and mode as they were: the typed text stays in the drafts. Ctrl+K and Ctrl+M leave it open, as they leave the new agent dialog. Opened from the sidebar's foot or the plan card, it closes to nothing. An answer to the form's folder listing or start that comes meanwhile goes to the form it keeps.
  - "Add an account" takes the name, the color, the folder and the environment choice. The folder shows the daemon's proposal, `.claude-<name>` beside the default account's `.claude`, until it is typed in, and goes as none until then. Once the daemon added the account, the dialog shows its sign-in command with Copy, and whether it is signed in.
  - The Plan chip shows the selected agent's account: its dot and its 5-hour figure, "API key", or an amber "?" when it is not signed in. The plan card lists every account, this agent's first.
  - The resume list shows the sessions of the dialog's account, every session when the machine has one account (`listed_sessions`).
  - An agent that waits for its account's sign-in shows a card in the chat in place of the turn's notice and end: the sign-in command, the account's state with "Check again", and "Send the message again", which sends the turn's message once the account signed in. While no check would end the wait, the button sends while the agent still waits: when the daemon cannot tell the account's sign-in (`unknown`, a login in the macOS Keychain), and when the account's credentials come from the environment (`environment`). Either way, it sends the message once. Enter presses it while it is on and no text field has the focus (the page's script passes Enter on only then, `send_again` in its key mode). The composer and Stop treat the agent as idle.
    The agent's sidebar row says "Work is not signed in" while it waits, in amber, then "Signed in: send the message again" until the message goes.
- **Sidebar foot**: Accounts, Machines and the Settings gear, and nothing else. Each machine's panel says how its machine answers, Local's too.
- **Grouping** (`state/grouping.rs`, `ui/grouping.rs`): the line above the sidebar's list ("Grouped by machine") and the Settings dialog's General tab group the agents by machine, by project, or not at all, and `prefs.json` keeps the choice (`sidebar.group_by`).
  - By machine, each machine has a panel with a header, Local alone included, and the agents follow the daemon's order. Under the header, a line says what the machine says while it does not answer or has an update to show, with its links, and folds open and shut by its height. A machine that answers no request has a red edge.
  - By project or without groups, no panel names the machines: a strip at the top of the list shows each machine with something to say, Local first, a line each with its name and its panel's line and links ("gpu-box Not connected. Retry now"). The strip, and each of its lines, fold open and shut. It is gone while every machine answers (`machine_status::strip_lines`).
  - By project, each project has a header, with its color, its agents' count, and a Keep button while it is temporary. Without groups, the list has no header.
  - By project, a project whose agents run on several machines lists them in one list, or under a line per machine (Local first), as the menu's choice under Project and the Settings dialog say (`sidebar.project_split`). A drop on the rows of a machine listed above puts the row at the top of its own machine's rows, and a drop on those of a machine listed below, or at the group's end, at their bottom.
  - By project or without groups, the app keeps one order across every machine in the preferences (`agent_order`, each agent by its daemon machine's name and its daemon's agent id), a new agent comes first, and a move changes only that order: no `agent.move` goes out.
  - With several machines, the sidebar names the machine of an agent on another machine than this computer once, where nothing above says it. A project whose agents all run on one such machine names it on its header. Otherwise, without groups or in a project of several machines in one list, the row names it, in one chip with the account's letter before the title.
  - The machine's name shows as a tag, with the whole name on hover: its first letters, three or more, as many as tell it apart from the other machines' names ("devb" and "devo" for "devbox" and "devops").
  - A header folds its rows, and a folded header still counts the agents that wait for you or are marked to handle.
- **Projects** (`state/projects.rs`, `ui/projects_tab.rs`): a project is this desktop's, kept in `prefs.json` (`projects`): a name, a color from the accounts' colors, and rules. A rule is a repository, or a folder, where a trailing `*` holds every path that starts with the rest.
  - An agent belongs to the project it was moved to by hand, if that project still exists, else to the kept project whose rule holds its folder most precisely (a folder, then its repository, then a folder that ends with `*`, the longest path first, and the first project in the list between equal rules), else to a temporary project of its repository, else to "No project".
  - A temporary project is never saved: it goes away with its last agent. It becomes a kept one when the user keeps it, from its header or its card, or moves an agent into it.
  - An agent moves to a project from its row's menu (Project, then a project or "New project…", which opens the new project's card), by a drop in another project's group (a zero-height marker ends each group, so a drop can land at the end of a folded one too), or from the Project field of the new agent dialog, beside the machine and the account. That field starts on Automatic, which names the project the folder's rules or its agents' repository give. While it is on Automatic, each recent folder names the project an agent started there joins. "No project" picked leaves the folder field and lists every recent folder: it starts the agent where you say, without grouping it. Another picked project lists only its recent folders: the folders whose rules or repository give it, and the folders where one of its agents runs, such as an agent moved to it by hand. It puts its latest one in the folder field unless the field already names one of its folders. The picked project applies once the app has both the start's answer, with the agent's id, and the agent's summary, whichever comes first: the rules need the summary to tell whether the pick is a choice to keep.
  - A choice by hand is kept only when the rules would put the agent elsewhere, so that a later rule change moves it again. The daemon's removal of an agent drops its choice, since a later agent may take its id.
  - A kept project's header drags, with a second copy of the rows' page script, to change the order of the kept projects. The temporary ones follow, by name, then "No project".
  - The Settings dialog's Projects tab has a card per project, as the Accounts dialog has per account: a kept project's card renames it, recolors it, adds and removes its rules, and deletes it. A temporary project's card keeps it. Its agents then go where the rules put them.
- **Agent order**: grouped by machine, the sidebar lists the agents in the daemon's order, and the first of that order is selected when the selection goes.
  A row moves by drag and drop, or Alt+Up and Alt+Down on the selected agent; both send `agent.move`.
  The sidebar shows the new order at once, with the positions the daemon then sends, and puts the agents back when the daemon refuses the move.
  A page script follows the drag (`ui/reorder.rs`), as the splitter's does: a lifted copy of the row follows the pointer, a line shows where the row will land, and Rust hears of it once, on release.
  A press that moves less than 4 pixels stays a click.
- **Block order**: the floating blocks, and their badges while the file view is open, follow one order for every agent, which the preferences keep (`blocks.order`, by the blocks' words). It starts with Tasks, Running and MCP servers, then Messages. A block moves by dragging its head, with the same page script as the sidebar's rows. A press on the head that moves less than 4 pixels stays a click, which folds the block. Alt+Up and Alt+Down on a focused head move the block too: the page script leaves them to the head instead of moving the selected agent, and the head keeps the focus as its block moves.
- **MCP reconnect** (`state/mcp_reconnect.rs`): the reconnect button on a server's page shows a spinner, and the status chip "Reconnecting…", until the daemon answers `agent.mcp_reconnect`, once Claude Code tried. The answer says nothing of the outcome, so the app then asks for the servers' status (`agent.mcp_status`) and says what the server's says, under its name: "Reconnected." for a few seconds, or "Not connected" with its status and error, or why the reconnect was refused. An outcome leaves once the server's status says otherwise, when the page closes or another agent shows, and with its agent. A reconnect still running stays.
- **Sidebar rows**: one line each, 31 px high: the status dot, the account's letter, the title, the activity dimmed, the task progress ("3/5"), a time, the pending count and the unread dot. An agent that finishes out of sight gets the dot, and so does one marked unread from its row's menu, even the selected one: only selecting it again reads it. Beside an activity, the title takes at most 60 % of the line, and the activity the rest, cut with an ellipsis and whole on hover. An idle agent with nothing running in the background has no activity: its hollow dot already says it is idle. That dot is a thin, dark grey ring, so that quiet agents stay quiet in the list, and an exited agent's ring is dashed.
  The time is how long the turn has run for a working agent, how long it has waited for an agent that needs you, and how long ago it last did something for an idle or exited one, in one unit: "12 s", "4 min", "2 h", "3 d". An idle or exited row counts no seconds: it says "now" for the first minute, so its time changes at most once a minute.
  The app's one-second clock (`Clock`) ticks it. Only a small component per row reads the clock, so a tick renders those texts and nothing else.
  The times come from the daemon's clock: when it runs ahead of the laptop's, a time reads 0 s rather than below.
- **Status filter**: chips at the top of the sidebar (All, Needs you, Working, Idle, Exited, each with its count) hide the other agents' rows and never change the order.
  A status's chip shows only while an agent has that status, and the chosen chip stays even at 0, so the last agent leaving it does not take the filter away.
  All shows beside the other chips, and the filter hides when All would be its only chip (no agents, with All chosen).
  An agent the filter hides still raises its notifications.
  When the filter hides the selected agent, the center keeps showing it and the sidebar says so, with a link that shows all agents.
  Alt+Up, Alt+Down and a drop move an agent among the rows the sidebar shows, and the hidden agents keep their places.
  The Ctrl+K switcher lists every agent, the ones that wait for you first: it finds an agent, and Ctrl+K then Enter opens the one that needs you.
- **Machine load**: the app asks for `machine.load` every 3 seconds while connected, and keeps the last answer only, in a signal of its own (`Cockpit.load`): a new sample renders the Load chip and its card, never the transcript.
  The chip shows the share of the server's cores in use, amber from 75 % and red from 95 %.
  Its card shows the server's CPU, memory and load average, each agent's cores and memory with their share of the server's cores as a bar (amber past a quarter), the idle agents folded into one line, what the other processes use, and the selected agent's heaviest commands, each with a stop button.
  A click on an agent opens it and keeps the card open.
  A daemon that cannot measure its load, or does not know the request, shows "Load unknown".
- **New agent dialog**: it opens on the machine of the last agent started (this computer when that machine does not answer) and on that machine's last used folder, with the model, effort and permission mode of the last agent started, else Opus and Auto (`open_new_agent`). So Ctrl+N then Enter starts an agent where the last one started. It opens on the last agent's project too when that project was picked by hand, since its folder alone would not give it again. Picking another machine puts the picked project's latest folder on that machine in the folder field, else that machine's last folder, unless you typed a folder.
  When the "Start in a folder" tab shows (on Ctrl+N, the + button, going back from Accounts, or a click on the tab), the folder field takes the focus while it is empty, and the first message takes it once the folder is filled (`start_focus`). The "Resume a session" tab gives the focus to its search field. The caret goes to the end of the text the field already holds.
  The folder field works as the "Open file" dialog's path field.
  Once it is typed in, the folders of the folder its text names (`folders.list`) take the recent folders' place, filtered by the last segment.
  Tab or Enter completes the highlighted folder, Backspace on an empty segment goes up, and Esc brings the recent folders back. With no folder highlighted, Enter starts the agent.
  In the first message, Enter starts the agent and Shift+Enter adds a line, as in the composer. Ctrl+Enter starts it from both fields.
  A text that names no absolute folder, by the rules of the daemon's machine (see Paths), lists nothing.
- **One highlight in a list**: in the Ctrl+K switcher, the Ctrl+O list, the folder completion, the resume list and the "/" palette, the pointer moves the keyboard's highlight, so one row is lit, with no CSS hover and no fade.
  WebKitGTK also sends a mousemove when a list scrolls under a still pointer, so only a pointer that moved takes the highlight (`PointerMoves` in `ui/dialogs.rs`).
  In the Ctrl+O list, the pointer picks its row as a key does, so a cut listing highlights it too.
  The keys scroll the list to the row they highlight. The pointer's row is in view already, so the palette does not scroll to it, and the list never moves under the mouse.
- **Motion** (the style sheet's Motion and Layout sections, `ui/presence.rs`): tokens set every duration and easing. What appears or grows takes `--motion-in` with ease-out, what leaves or shrinks `--motion-out` with ease-in, and a reduced-motion setting sets them to 0. On Linux the app turns WebKitGTK's accelerated compositing off (`webview_settings.rs`): with it, WebKitGTK 2.54 draws an animation's first frame once more after its last, now and then, and the popovers and dialogs flash as they open and close.
  An element that closes stays on screen for its exit, marked `leaving` and inert, so input already goes past it.
  The file view opens and closes by its width, and the MCP card widens for a server's detail. The chat's text, the composer, the last instruction bar and the jump button slide aside with them, with the same timing.
  A switch to another agent draws its layout at once: in the frame that shows the other agent, the root has the `switching` class, which turns those transitions off. A file view that comes or goes with its agent does not slide.
  A dialog's height follows its content with an animation (`ui/dialog_height.rs`): a page script watches each dialog's parts, and the children of a part that scrolls, and animates the dialog from its last height to its new one with the same tokens. A change during the animation starts from the height on screen, so the box never jumps. A new window size sets the height at once.
- **Rendering**: markdown with `pulldown-cmark`, syntax colors with `syntect` and the extra syntaxes of `two-face` (bat's set: Protobuf, TypeScript, TOML, Dockerfile, ...), diffs with `similar` (line diff, then word highlights inside changed lines).
  A command's IN and OUT rows, and a message you sent, fold by their lines on screen: a page script (`FOLD_SCRIPT` in `ui/transcript.rs`) marks `data-tall` the ones with more lines than their `--fold-lines` (3 for IN, 5 for OUT, 8 for a message) and writes how many lines "Show N lines more" hides. A command's OUT row keeps its last lines instead, under "Show N earlier lines", since they say how the command ended, unless the output is JSON, whose last lines are closing brackets: the script starts them where a line of the output starts. An IN or OUT row's lines are its height divided by its line height. A message counts its lines of text, since the gaps between its paragraphs and the padding of its code blocks are no lines; the script cuts it where its 8th line ends. The script measures them again when the width changes, so a narrower window can fold a message that fitted.
  An OUT row shows a run of lines that differ at most in their quoted parts once, with `…` in the quoted parts that change and a "× N" button that shows each line (`render/tool_output.rs`). The CRLF warnings of a `git commit` on Windows become one line.
  A `Read` shows no output: its line says which lines it read ("lines 40–121"), and opens the file view. A failed one says its error there. A `Grep` or `Glob` line says what it found ("4 files", "12 lines", "no matches").
  Some rows of the main conversation stand for several items (`ui/tool_rows.rs`). A `Skill` call takes the notice of its instructions after it, which the protocol titles with `skill_instructions_title`: one line shows the skill and its arguments, and opens on the instructions. Two reads or searches in a row or more, under the same parent and with none waiting for approval, fold into one "Explored" line ("2 files read, 1 search"), which opens on one line per call; a search's line opens on what it found. The jump button counts such a row once. Such a row's entry is named after its first item.
  The script also sets `--full-height`, and a fold or an unfold moves the box's `max-height` between `--fold-height` and `--full-height`: `--motion-in` with ease-out to unfold, `--motion-out` with ease-in to fold. The fade at the cut fades in as the row folds and out as it unfolds.
  A folded message scrolls with the wheel at its folded height, without "Show more", and the wheel goes on to the chat at its ends. Its fade sticks to the bottom of what shows, and goes once the script marks the message scrolled to its end (`data-end`).
  A height measured again applies at once, so an unfolded row shows all its text while the window resizes.
  "Show less" stays under the pointer: the chat's scroll script keeps the button that folds its row at its height until you scroll, so the folded row stays in view.
  In agent text, a `path:line` reference, a Markdown link to a file and a code span that holds only a file's path open the file view. Such a code span names a file in a folder with an extension (`src/lexer.rs`), a file with a common extension (`Cargo.toml`), or a known name without one (`Makefile`, `.env`), so that `self.items`, `console.log` or `cargo test` stay code. A path whose first folder holds a dot, as in `github.com/a/b.rs`, is a web address and stays code. A relative path resolves in the agent's folder, and an absolute one can be anywhere, since the daemon decides which files it reads. A link that goes up with `..`, starts with `~` or `//`, or has another scheme than `http` or `https` shows as its text, so nothing in an agent's text makes the system open a file. A bare `http://` or `https://` address becomes a link, without the sentence's last punctuation.
  A click on a web link opens it through `browser.rs`, and Dioxus's own opener never sees it. On Linux the app runs `xdg-open` without `LD_LIBRARY_PATH`: an app started from an agent's session inherits the NixOS `claude` wrapper's alsa-lib there, which makes Chromium exit at startup. Elsewhere the `webbrowser` crate opens it. A failure shows as an error note.
  A Markdown image of a PNG, JPEG, GIF or WebP file shows in the message, 200 px high so the text below stays in place while it loads, and a click opens it over the whole window, or in the file view while it is not loaded (`state/chat_images.rs`, which keeps the images on screen and each agent's last 16 others). Each message reads its own copy of a file: an agent that writes a new picture over the same path, as a screenshot, shows the new one in its later messages, and an earlier message keeps the copy it read while it is kept. A web image is not fetched: it shows as a link.
  The full view (`Dialog::ImageView`) closes on Esc, its close button or a click beside the image, and has a button that opens the image in the file view. A click on the image in the file view opens it too, unless the image is zoomed. A click on an image attached to a message you sent opens it as well. Such an image has no file: the view calls it "Attached image" and has no file view button. In the full view and the file view, Ctrl+wheel zooms around the pointer up to 8 times, a drag moves the zoomed image, and a double-click or Ctrl+0 fits it again (`ui/zoom.rs`).
- **Switching agents**: a click on a sidebar row, or an agent picked in the Ctrl+K switcher, moves the selection, the header and the composer in the next frame.
  In that frame the chat draws only the agent's latest 25 rows. Each following frame adds 40 older ones, until all are drawn (`ShownRows` and `Drawn` in `ui/transcript.rs`). An item's id marks how far up the rows go, so a row the working agent adds below moves nothing.
  Dioxus runs effects once the webview has applied a frame's changes, so an effect starts each next chunk.
  Drawing a whole conversation is what made the switch slow: for 150 entries, the syntax colors, the page's new elements and their layout put the switch on screen about 180 ms after the click, against about 90 ms for 25 rows or a short chat (release build, the demo under Xvfb).
  Drawing all the older rows in one frame kept the webview from answering a click for about 100 ms on a long chat. In chunks, the demo's chat of 140 entries adds at most 1,166 elements in a frame, against 3,534 before, for the same total, and its longest frame fell from about 92 ms to about 35 ms (release build, the demo under Xvfb, median of 8 switches; the machine was busy, so take the times as rough).
  The syntax colors of a code block or a diff are kept per text and language (`HighlightCache` in `render/syntax.rs`, up to 4 MB of source text, the oldest dropped first): a switch back to that chat runs the grammar on no text, against 77 before.
  When the chat opens on a place scrolled up, it shows the skeleton in that frame instead, as the latest rows may not hold that place, and draws all its rows in the next one. A scroll while the chunks come keeps the rows drawn, and holds: the scroll script, which sees no agent named until the last chunk, then keeps that scroll in place of the agent's place from before.
  Each entry has `content-visibility: auto`, so the webview lays out and paints only the entries in view. On a long real chat, this cut the webview's work per switch from about 790 ms to about 240 ms (release build, under Xvfb). The scroll script keeps the view in place while an entry that comes into view grows from its estimated height to its real one.
  The chat's scroll script keeps each agent's place, and only a chat that draws all its agent's rows names that agent (`data-agent`), so the script goes back to the place once every row is there.
  Within `BOTTOM_THRESHOLD_PX` of its bottom, the chat follows new entries. For `WHEEL_UP_MS` after a wheel turned up, it follows only from its very bottom, and the wheel event itself stops the following: WebKit spreads a notch over a few frames, and a frame within the threshold, or a change of the content before the first frame, took the chat back down. A wheel that scrolls a box inside the chat, such as a folded message, leaves the following as it is.
  On the first click on an agent, the chat shows the skeleton until the daemon sends the first page, then draws it in the same way.
- **Quotes and replies** (`state/quotes.rs`, `ui/quotes.rs`): a passage of the chat with a comment on it waits in the agent's draft (`Draft.quotes`, in the chat's order) and goes with its next message, as a Markdown quote followed by the comment, then the typed text. Quoted code goes in a fence inside the quote. A send takes a comment still being written, and a refused send gives the quotes back with the draft.
  - A quote names its passage by the transcript entry that shows it and the start and end of the passage in that entry's text, in UTF-16 units, as `Range.toString` counts them. When the entry's text moved, the page script finds the passage again by its text, nearest to the old place.
  - The page script (`SCRIPT` in `ui/quotes.rs`) watches the selection: one inside a single entry of the shown agent shows the Reply button above it, and Reply or Ctrl+R sends the passage to the app, which opens the comment field (`Drafts.quote_editor`). Enter in the field adds the quote or saves the comment. Esc, or its × button, closes it: Esc closes the field of the agent on screen before anything else. Each opening of the field gets a number of its own, which keys the component, so a new field never shows the text of the one before.
  - The script draws the passages with the CSS Custom Highlight API, which marks text without changing the elements Dioxus rendered: `quote-pending` for the quotes waiting, `quote-sent` for the passages a sent message quotes, `quote-flash` for a passage the chat scrolled to. WebKitGTK before 2.44 has no such API: the numbers and notes still show, without highlights.
  - The components draw the numbers, the notes, the comment field and the card of each number, hidden, and the script places them over the chat (`quote-layer`) after each scroll, resize or change of the page. A number sits past the end of its passage's last line. A line that a folded row hides, or that a folded message scrolls out of its box, counts as out of view, for the Reply button too. A note goes in the strip on the chat's right, below the floating blocks and the note above it, beside its passage; a note whose passage is out of view leaves, and one with no room left shows as its number alone. The comment field shows as a note there, or as a card under the passage in a narrow chat, for a hidden comment, or when the strip has no room. A number whose note does not show shows the passage and the comment on hover or focus.
  - With the file view open, the strip is gone: the components draw no notes and the composer rows no eye button.
  - After a send, the script finds each quote of a message you sent in an earlier entry by its text, whitespace aside, and underlines it. Hovering the passage shows "You replied", the first block after the quote, and "Show your message". A click on the quote in the message, or Enter on it, scrolls to the passage and lights it. Nothing is stored for this: the message's text holds the quotes, so it works for messages loaded from the history too.
- **Message time**: a faint time under each message the user sent ("13:38" today, "Yesterday 17:05", "8 Nov 09:12" this year), brighter while the pointer is over the message, with the whole moment as its tooltip. Several messages in a row sent at the same minute show it once, under the last (`grouped_times` in `ui/transcript.rs`). The bubbles follow the local day (`Today` in `connection.rs`), not the clock's ticks: they render again at midnight, and "13:38" becomes "Yesterday 13:38".
- **Last instruction** (`ui/last_instruction.rs`): once the user's message has scrolled out above the chat, a bar over the chat's top shows it with how long ago it was sent, so the user knows which instruction the output in view answers. A click scrolls the chat until the message is at the top of the view. The eye in the Messages block hides the bar (`prefs.chat.last_message`).
  The chat's scroll script tells the app which entries are at the top and the bottom of the view, each time another one comes there (`InView`, a signal of its own, so scrolling renders only the bar and the Messages block). The same message says whether the chat is at its bottom, so the two never disagree. The top entry is the first with more than `REVEAL_TOP_PX` of it in view. WebKit gives an entry out of view a placeholder height, which its content overflows, so the script takes the lower of the entry's bottom and its wrapper's. After a reveal, the script reports once the entries that came into view are laid out. The bar shows the latest top-level user message before the top entry (`Transcript::message_before`), and nothing when the entry is a user message itself.
- **Messages block** (`ui/messages.rs`): the user's top-level messages (`Transcript::sent_messages`), then the details' waiting prompts. It marks the message being read (`Transcript::message_read`): at the chat's bottom the latest message at or before the bottom entry, and scrolled up the one the top entry answers, which is where a click on a row brings its message. The rows' text is parsed again only when a message comes or another agent shows.
  In a short window the open cards share the column of blocks before it has to scroll. Each card's list (Tasks, Running, MCP servers, Messages, marked `data-kept-rows`) shrinks down to its first two rows and scrolls inside, and the rest of its card keeps its size. The rows' heights depend on the width and on wrapping, so a page script (`KEPT_ROWS_SCRIPT` in `ui/blocks.rs`) measures them and sets the list's and the card's least heights. The column scrolls only when even those do not fit. The Messages list's first rows are its latest, since it stacks from the bottom.
  A message's time is `UserMessage::sent_at`: the daemon's clock for a prompt sent through it, or the `timestamp` of Claude Code's line for a message read from a session file. An older daemon sends none, and the row shows no time.
  It shows nothing when that message sits in older messages the app has not loaded.
- **Renaming in the sidebar**: F2, or a double-click on a row's title, turns the selected agent's row title into a field, as the header's pencil does for the header's title.
  Enter or leaving the field saves with `agent.rename`, and Esc cancels.
  One title field shows at a time: F2 in the header's field moves the edit to the row with the text typed so far, and F2 in the row's field moves it back.
  The typed text lives in the drafts, as the composer's does, so typing a title renders neither the transcript nor the sidebar.
  An agent the filter hides has no row, so F2 opens the header's field.
- **Working folder** (`state/working_folder.rs`, daemon `hub/folders.rs`): a row's menu has a "Working folder" submenu, which shows the folder `claude` runs in, with "Fork into another folder…" and "Move to another folder…". They act on that folder and on Claude Code's copy of the session there, not on how the sidebar sorts agents. Each opens a dialog with the new agent dialog's folder field, on the folder that holds the agent's (a sibling worktree is one pick away).
  Claude Code finds a session only in the project of the folder it starts in: `~/.claude/projects/<the folder's path, each character but an ASCII letter or digit written ->/<session id>.jsonl`, with a folder of side files (subagents, saved tool outputs) named after the session. The daemon copies both to the new folder's project in a blocking task, since a session file can reach hundreds of megabytes, and leaves the originals: transcripts name some side files by their path. A path whose project name would pass 200 characters is refused, as Claude Code shortens those its own way. `sessions.list` lists a copied session once, its newest file, and `sessions.resume` resumes from that file.
  `agent.fork` starts a new agent on top of the list, with the original's title plus " (fork)", model, effort, mode and account, and its transcript so far. Its first start passes `--fork-session` (`resume_as: fork` in `agents.json`), and once `claude` reports its new session id, it resumes that one.
  `agent.change_folder` keeps the agent and its session: it is refused while the agent works. A running agent stops, and once `claude` exits and the copy is done, it starts again in the new folder; an exited agent stays exited there. It does not resume in between. A failed copy keeps the old folder, and starts the agent again there if it ran.
- **Closing an agent**: the close button on a sidebar row (shown on hover and on the selected row), "Close agent" in the "/" menu, or Ctrl+Shift+W sends `agent.close`.
  Ctrl+W stays the file view's: it closes the shown tab.
- **Marking an agent to handle**: a right-click on a sidebar row opens a menu at the pointer (`ui/row_menu.rs`), and Ctrl+H gives the selected agent its next mark. Both send `agent.mark` (`state/to_handle.rs`).
  A mark is of two kinds: until you send the agent a message, or kept until you clear it. The Settings dialog's General tab picks the kind the first press gives (`sidebar.first_mark`, until a message by default). The next press gives the other kind, and the one after clears the mark. The menu's row says which of these it does.
- **The row menu** (`ui/row_menu.rs`): "Mark to handle" (or the next mark, or "Done handling"), "Mark as unread" (or "Mark as read"), "Rename", the "Notifications" and "Working folder" submenus, and "Close". It takes the focus as it opens, and one highlight runs through it and its open submenu, moved by the pointer and the arrows: Right or Enter opens a submenu, Left closes it. While it is open, H, U, R and W pick the menu's rows, and 1 to 3, F and M the open submenu's. The menu marks itself `data-own-keys`, so the bridge script leaves those keys to it. Esc closes it and gives the focus back to the element it took it from, which the bridge script notes as the menu takes the focus. The files pane's menu, the tab menu and the ⋯ menu of a server in the Machines dialog share this frame (`KeyedMenu` in `ui/menu.rs`). The composer's model and mode menus take its compact spacing (`compact-menu`), each mode with what it does under it, smaller, and keep their own keys: Esc closes them. Once a submenu is open, the pointer may cross other rows for 200 ms on its way into it before the submenu follows the row under the pointer.
  The daemon saves the mark in `agents.json`, and clears a mark until the next message when the agent gets a prompt, so that mark stays until you give the agent the input it waited for, whatever its status.
  A marked row has a violet flag beside its time, filled for a kept mark, and a violet bar on its left edge, a folded machine header counts its marked agents, and the "To handle" filter chip lists them.
  An agent that works, waits for you, or holds a message you have not sent asks first, in a dialog that says what would be lost; Ctrl+Shift+W again, or Enter, confirms.
  The agent leaves the sidebar at once, with its drafts and file view, and the selection goes to the next row, or to the one above for the last.
  Its events until the daemon's `AgentRemoved` are not applied, so a late update does not bring it back; a refused close puts it back.
  A note offers Undo for 8 seconds: `sessions.resume` brings the session back as a new agent, which the app then moves above the agent that was below it.
- **Agent map**: Ctrl+M, the composer's map button, or the Running block's link opens a dialog (`Dialog::AgentMap`) on the selected agent; a click on a subagent in the Running block opens it on that subagent.
  The outline's logic is pure and tested (`state/agent_map.rs`): it builds the tree from each subagent's `parent` (one whose parent left the list shows at the top), splits the current turn from the earlier ones, folds a subtree with no running or failed subagent, folds the oldest ended siblings past eight into one row, applies the filter, and moves the selection with the keys.
  The view keeps only what the user changed: the folds that differ from the default, the opened levels and earlier turns, the filter, the selected row and tab. Opening the map on a subagent unfolds what hides it.
  The details of a subagent come from the transcript the app holds: the prompt from its call's input, its items from the ones nested under its call. When its call is older than the loaded messages, the details say so and offer to load them.
  Once the app holds the call, "Show its call in the chat" at the bottom of the details closes the map and scrolls the chat to the entry that holds the call, which it highlights for a moment.
  That entry is the call's own, or, for a subagent started by a subagent, the entry of the top-level call it nests under.
  The chat's scroll script scrolls and highlights (`ChatReveal` in the UI state). The map's entry goes to the middle of the view. The last instruction's message goes to the top, 12 px under it, as high as the chat can scroll.
  The script then keeps the revealed entry itself at its height. The entries out of view are laid out at a 60 px guess (`content-visibility: auto`) and take their real height once they come into view: kept at its height, the first entry in view would let the revealed one move.
  While the map is open, the page's script passes Up, Down, Left, Right and Enter to it, and Y and N when the selected subagent's approval or plan waits. The outline takes the focus when the map opens, so Enter never presses a button left focused behind the dialog.
- **Shortcuts**: Ctrl+K, Ctrl+N, Ctrl+M (the agent map), Ctrl+B (the files pane), Esc (closes what is open, and with nothing left open stops a busy agent, see "Esc and Stop" above), Y, N, 1 to 9, as in plan.md, Enter outside text fields for the sign-in card's "Send the message again", Alt+Up and Alt+Down to move the selected agent in the sidebar, F2 to rename it, Ctrl+H to give it its next mark to handle, Ctrl+Shift+W to close it, and Ctrl+R to reply to the chat's selection (the page script takes Ctrl+R everywhere, so the webview never reloads).
  These are the defaults. The Settings dialog's Keyboard tab (`ui/settings.rs`, `state/settings.rs`) changes the shortcut of each command (`keymap.rs`: the switcher, a new agent, open a file, the files pane, the agent map, rename, mark, close, move up and down, read the file again), and `prefs.json` keeps the changes under `keys`. It lists the others with a lock: Esc, Enter, Shift+Enter, Alt+Enter, "/", Y and N, 1 to 9, Ctrl+W in the file view and Ctrl+R on a selection keep their keys, since their meaning comes from where they are pressed.
  A new shortcut needs Ctrl or Alt, or is one of F1 to F12, so that it never takes a key typed in a field; one the app, a text field or the system already gives a meaning is refused: Ctrl+Z, Ctrl+C, Ctrl+W, Ctrl+R, Ctrl or Alt with Enter, Ctrl with a key that moves or deletes in a field, Ctrl with a zoom key, Alt+Left and Alt+Right, Alt+F4, Esc, Tab, and Ctrl+Alt with a character, which AltGr types, and one another command has offers a swap. A file whose shortcut does not read, or that gives one chord to two commands, keeps the defaults for those commands, and its other preferences still read.
  The page script gets the commands' chords with its key mode, each with where it takes it (`Reach`): everywhere, outside fields and dialogs but from the title fields (rename), or outside fields other than the message (move). It still takes F5 everywhere, whatever runs it, so the webview never reloads, and it leaves the keys of the element that records a shortcut (`data-records-keys`) to that element. The tooltips and notes that name a command's keys read the keymap.
  On a sidebar row or a machine's or project's header, Up and Down select the agent above or below among the rows on screen and move the focus with it, Left folds the group and moves the focus to its header, and Right shows its rows again (`state/sidebar_keys.rs`).

## Errors

- Each library crate has typed errors (`thiserror`), one enum per area: `ParseError` in `claude_protocol`, `FrameError` in `cockpit_protocol`, `ConnectError` and `RequestError` in `cockpit_client`, `StoreError`, `ProcessError` and `PeerError` in `cockpit_daemon`.
- `anyhow` only in the two binaries, to report.
- A request that fails returns an error response with a code and a message; the connection stays open.
- A `claude` line that does not parse is logged in `raw/` and skipped; it never stops the agent.

## Tests

- `claude_protocol`: parsing of fixture lines in `crates/claude_protocol/tests/fixtures/`.
  Fixtures are written by hand from the shapes Claude Code sends, with made-up paths, ids and text.
- `cockpit_protocol`: round trips of every message, and the method names pinned.
- `cockpit_daemon`: the transcript conversion on fixture sequences; the accounts (names, the default one's name, folders, the sign-in commands of both shells with the program plain or by its path, how a terminal names the program, the environment of a `claude`, the wait for a sign-in); the store in a temporary folder; the server end to end with a fake `claude` (a small test binary that replays a script of lines, selected with `AGENTS_COCKPIT_CLAUDE`); the daemon in the app over in-memory streams, closed mid-turn and started again, and the lock it shares with the service (`tests/in_app.rs`).
- `cockpit_client`: against an in-process fake daemon over a socket pair; the daemon's protocol read from a refusal; the ssh command of a script.
- `cockpit_desktop`: `apply` on event sequences; the daemon's update (the protocol read and compared, the releases and commits compared, each machine's plan, the steps from the script's lines, the exit statuses and the sidebar's line for each, the state over refused attempts, connections and per machine, a cancel the user asked for, the busy agents' confirmation and the wait for idle agents, the service's stop and start in order with a made-up system and their failures, the embedded script's lines, options and line ends); the accounts' state (the new agent's account, the labels, the plan chip, the resume filter, the two dialogs and the way back to the new agent dialog, the sign-in card); rendering helpers (markdown, diffs) on sample input; the "No daemon answers" choices and the config they write, through a symbolic link too; the path rules of both platforms.

## Platforms

Linux runs everything. The workspace also builds and passes its tests on Windows, where the daemon runs inside the desktop app and its agents run Windows' own `claude`.
The code marks what needs Unix with `#[cfg(unix)]` and gives Windows its own version with `#[cfg(windows)]`.

| | Linux | Windows |
| --- | --- | --- |
| daemon | `agents-cockpit serve`, on a Unix socket, or inside the desktop app | inside the desktop app (`InAppDaemon`) |
| `claude` | `claude` on `PATH`, in a process group of its own | `claude.exe`, else npm's `claude.cmd` through `cmd.exe`, in a Job Object of its own |
| machine load | `/proc` | `GetSystemTimes`, `GlobalMemoryStatusEx` and each agent's job; no load average |
| `agents-cockpit` | every command | the client commands, with `--ssh`; no `serve` and no `bridge` |
| desktop app | WebKitGTK | WebView2 |
| daemon's state folder | `$XDG_STATE_HOME/agents-cockpit` | `%LOCALAPPDATA%\agents-cockpit` |
| app's config file | `$XDG_CONFIG_HOME/agents-cockpit/config.toml` | `%APPDATA%\agents-cockpit\config.toml` |
| app's preferences | `$XDG_STATE_HOME/agents-cockpit-desktop/prefs.json` | `%LOCALAPPDATA%\agents-cockpit-desktop\prefs.json` |
| app's log | stderr | stderr, or `%LOCALAPPDATA%\agents-cockpit-desktop\app.log` when the app has none (a release build is a GUI program, with no console window), rotated by size into `app.1.log` to `app.4.log` |
| notifications | the freedesktop notification service | toasts, under PowerShell's name (see the desktop app's Notifications) |
| paths the app shows | `/work/app` | `C:\work\app`, typed with `\` or `/`; a Linux daemon's paths keep Unix's rules |
| home folder (Claude Code's folder is `.claude` in it) | `HOME` | `USERPROFILE` |

Each path comes from its environment variable, and falls back to its default under the home folder, as on Linux.

On Windows, for now:

- `Transport::Local` fails at once: tokio has no Unix sockets there. The socket server and its peer checks are not built.
- Nothing sets file modes. A file or folder takes the permissions of the folder it is in, and those of `%LOCALAPPDATA%` admit only the user.
- The tests that need a shell, `/proc`, a Unix socket, file modes or symbolic links are left out, among them the end-to-end tests and the command line's tests.
  Tests of their own cover the Job Objects, the stop of a process, the machine load, the file requests' paths, and `cmd.exe`'s quoting. The path rules (module `windows_path`) and the Windows sample's arithmetic are plain functions, tested on Linux too.
  The app's tests with made-up Unix paths, such as `/work/app`, run on Windows too: they stand for a Linux daemon's paths, which the app reads by Unix's rules on either platform.

## Development

- `nix develop` gives the toolchain: stable Rust, nightly rustfmt (the standard's `rustfmt.toml` needs it), clippy, and the WebKitGTK libraries for the desktop app.
- Format: `cargo fmt` (nightly rustfmt from the dev shell, reading the root `rustfmt.toml`).
- Lint: `cargo clippy -q --workspace --all-targets`; the workspace lints in `Cargo.toml` and `CLIPPY_CONF_DIR` apply the standard.
- Test: `cargo test -q --workspace`.
- CI (`.github/workflows/ci.yml`) runs plain cargo, without Nix: it checks, lints and tests the workspace on Linux and on Windows, and keeps the Windows desktop app's release build as an artifact.
