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
id, title, folder, status, activity line, pending count, model, effort, permission mode, task progress, context percentage, prompt cache expiry, turns, API-equivalent cost, start time, last activity time, running turn's start time, position in the list, account, whether it waits for its account's sign-in (`sign_in_wait`), and how many of its background tasks still run (`background_running`).
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
It also holds `waiting_prompts`: the prompts Claude has not read yet, each `queued` (the daemon holds it) or `sent` (`claude` has it), with its text and image count.

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
   The version is 4. An app and a daemon of different versions refuse each other.
2. The daemon answers `Welcome { protocol_version, catchup }`, or `Refused { reason }` when the versions differ.
3. `catchup` is `Replay` when `last_seq` was given and the logs still hold every event after it: the daemon then sends those events, then live events.
   Otherwise it is `Snapshot(snapshot)`: the client replaces its state with it, and live events follow.
   A welcome never carries both, so no event is applied twice.
4. The client sends `Request { id, request }`; the daemon answers `Response { id, result }`.
   Responses and events share the connection, in any order.

The snapshot holds every `AgentSummary`, the default account's plan usage (`plan_usage`), the settings, the last `Seq`, the accounts, and the other accounts' plan usage (`other_plan_usage`).
Transcripts are not in the snapshot: the app asks for an agent's transcript with `agent.history` when it opens the agent, a page at a time.

### Requests

`agent.start`, `agent.prompt` (with a delivery: `steer`, the default, or `queue`), `agent.cancel_queued`, `agent.send_queued` (a queued prompt goes to `claude` now), `agent.answer` (approval, question or plan), `agent.interrupt`, `agent.stop`, `agent.resume`, `agent.remove` (an exited agent), `agent.close` (any agent), `agent.history`, `agent.details`, `agent.set_model`, `agent.set_effort`, `agent.set_mode`, `agent.rename`, `agent.move` (the agent goes just above `before`, or to the bottom when `before` is `null`), `agent.context_usage`, `agent.stop_task`, `agent.mcp_status`, `agent.mcp_reconnect`, `agent.mcp_toggle` (`action`: `enable` or `disable`), `agent.commands` (slash commands and skills), `agent.read_file` (the file view), `agent.list_dir` and `agent.find_files` (the "Open file" dialog), `agent.stop_process` (one of the agent's commands), `mcp.remove` (with the agent, whose folder decides the project and local scopes), `settings.set`, `sessions.list`, `sessions.resume`, `folders.list` (the new agent dialog's folder field), `machine.load` (see [Machine load](#machine-load)), `accounts.list`, `accounts.add`, `accounts.change`, `accounts.remove` (see [Accounts](#accounts-1)).

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

- `agent.read_file` sends a text file of at most 4 MiB, or, for a file whose extension names an image type Claude reads (`.png`, `.jpg`, `.gif`, `.webp`), the image of at most 8 MiB as `Image`, in base64. A relative path that names no file in the agent's folder is looked up: the most recent file the agent read or changed whose path ends with it, then a `git ls-files` search of the folders above, which stops at 20 matches or 3 seconds. Several matches come back as `FileMatches`, for the user to pick.
- `agent.list_dir` answers a folder's entries, folders first, each with its kind (folder, file or other), a file's size, and whether it is a symbolic link (`link`: `direct` or `symbolic_link`). It reads at most 20,000 entries and sends at most 2,000, and says when it cut the list (`completeness`: `complete` or `truncated`). It runs off the hub's task.
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

`AgentAdded`, `AgentUpdated(AgentSummary)`, `AgentRemoved`, `TranscriptAppended { agent, item }`, `TranscriptChanged { agent, item }`, `DetailsChanged { agent, details }`, `PlanUsageChanged` (the default account's), `SettingsChanged`, `AccountPlanUsageChanged { account, usage }` (another account's), `AccountsChanged { accounts }`.
Each carries its `Seq`.

`DetailsChanged` carries the whole details, so it goes out only when something they hold changes: a subagent's text or a streamed delta sends none, its tool calls and its task events do.

## Daemon (`cockpit_daemon`)

### Entry points

The daemon starts the same way behind a socket and inside the app (module `daemon`):

1. `Daemon::prepare` creates the state folder, takes `serve.lock`, and reads `agents.json`, the settings and the event logs back.
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
  The daemon never reads or logs these values. It only checks whether `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set, for `accounts.list`.
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
  - `steer` writes it to stdin at once. Claude Code reads it at the next tool boundary, in the same turn, or, when the turn ends first, starts the next turn with it; an interrupted turn does the same. Either way it echoes the prompt (`isReplay`) where Claude reads it, and each prompt gets its own echo, in order. The prompt joins the transcript at its echo, which takes the sent prompt with the same text, else the oldest. The agent stays working until every sent prompt is read.
  - `queue` holds it in the daemon until a turn ends with no sent prompt left unread; it then starts the next turn. It can be cancelled, or sent now with `agent.send_queued`.
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
- `serve.lock`: held while a daemon runs, the background service or the app's own, so a second daemon refuses to start and a leftover socket is safe to delete;
- `agents.json`: one entry per agent (id, title, folder, account, Claude session id, model, effort, mode, position in the list, for an agent made from a past session the part of the session file it shows, and while its `claude` runs, that process's id, start time and, on Linux, boot), written atomically (write to a temporary file, then rename);
- `settings.json`: the hidden MCP servers, the accounts (the default account's name and credentials, the added accounts, the next account id), and other daemon-side settings;
- `events/<agent-id>.jsonl`: the agent's cockpit events, each with its `Seq`, appended;
- `raw/<agent-id>.jsonl`: every line exchanged with `claude`, for debugging.

The daemon leaves the umask alone, since `claude` and the agents' commands inherit it.
It creates each folder with mode 700 and each file with mode 600.
The socket gets mode 600 right after the bind; until then, the 700 folder keeps other users away from it.

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
- An `accounts.list` finds the account's credentials file written after the failed turn ended, even when the agent was stopped or resumed since. After a daemon restart, the log does not say when the turn failed: the file must be newer than the agent's last activity.
  The hub reads the file's time with the sign-in states, then ends the waits before it answers, so the app's "Check again" sees the agent idle.
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
- `Transport::Ssh { host, remote_command }` runs `ssh -T -x -o BatchMode=yes -o ForwardAgent=no -o ClearAllForwardings=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -- <host> '<remote_command>' bridge`.
  The keepalives end ssh after a minute of silence, so a dead link shows as a lost connection.
  `BatchMode` makes a missing key fail at once instead of waiting for a password the app cannot show.
  `ForwardAgent=no`, `-x` and `ClearAllForwardings=yes` open no agent, X11 or port forwarding to the laptop, even when the user's ssh config asks for one.
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
  Each daemon numbers its own agents, so the app gives each agent an id of its own: the machine in the high bits, the daemon's id in the low 20.
  Machine 0's agents keep the daemon's ids.
  Each machine has its own connection task, which translates the ids of what it sends and receives (`map_agents` in `cockpit_protocol`).
  A router hands each request to the task of the machine its agent is on. A request about no agent, such as the sessions list, names its machine (`Outgoing.to`).
  `AppState.machines` holds what the app knows of each daemon (`MachineState`: label, connection, plan usage, settings, accounts, sessions), and a snapshot from one machine replaces that machine's agents alone.
  Account ids collide across machines, so every account function takes the machine, and the Accounts dialog picks one.
  The header's plan usage, the hidden MCP servers, the undo of a close and the machine load follow the selected agent's machine.
  A machine over SSH that does not answer is tried again every 10 seconds, and its requests fail meanwhile. "Retry now" on its panel ends the wait.
  The Machines dialog (`state/machines.rs`) adds and removes servers. The connection task starts or ends their tasks and writes the list back (`save_machines`, which keeps the rest of the file as `save_daemon` does). Removing a server drops its agents from view and leaves them running there.
  Only machine 0 has the "No daemon answers" screen. With servers configured, it shows as a dialog from this computer's panel, so the servers stay usable.
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
- **Updating a server's daemon** (`state/daemon_update.rs`, `update_script.rs`, `ui/daemon_update.rs`): when a first connection is refused for its protocol, `ConnectError::daemon_protocol` gives the daemon's version, from `VersionMismatch` or from the text of an older daemon's `Refused`.
  An older daemon reached over SSH gets the "runs an older version" card in place of the "No daemon answers" screen, with "Update <machine>" first. Any other daemon of an older protocol keeps that screen. A newer daemon gets the "runs a newer version" card, with no update.
  A `[[machines]]` server shows "Runs an older version." on its sidebar line, with `Update` and `Retry now` links: `Update` (`Details` when the app knows no commit) opens the same screens as a dialog (`Dialog::DaemonUpdate`), and the other machines keep working.
  - `build.rs` writes the commit the app was built from into `AGENTS_COCKPIT_COMMIT`, from the environment when it is set (the Nix build sets it to the flake's revision, empty for a tree with uncommitted changes) and else from `git rev-parse HEAD`. It tells cargo to rerun when `HEAD` or the ref it points to moves, in a linked worktree too. Without a commit the older-version card offers no update and says so.
  - The update runs beside the connection, in a task of its own: the embedded `contrib/update-daemon.sh` goes on the stdin of `ssh <host> sh -s -- --commit <sha> --binary <remote_command> [--source-dir <dir>]`, with the same ssh options as the bridge. It uses plain SSH because an old daemon refuses the app's protocol. Its output streams into the screen's "Build output", and the steps follow its lines (`update-daemon: building`, `restarting the daemon`, `the daemon runs`).
  - The update's state (`MachineState.update`) lives per machine and outlives the connection's attempts, which an old daemon keeps refusing while the build runs: the screen shows it until the first connection after success, which the app tries at once.
  - The script's exit status picks the failure screen: 3 is the agents' refusal, with their list and how to stop one; 1 says the machine still runs its old version, and that the update was cancelled when "Cancel the update" reached the server; 4 says the daemon may not have come back; 5 says the new version did not start and the machine runs its old version again, with the script's message; 6 says another update is already running on the machine, which the running screen invites by letting the user close the app. Any other status, such as ssh losing the connection, says the app cannot tell how it went. The sidebar says "Was not updated." for 1, 3 and 5, "Update failed during the restart." for 4, "An update is already running." for 6, and "Update status unknown." otherwise. "Update again" reruns, and "Back" returns to the older-version card. A daemon that is now newer than the app ends the failure's screen: "Update again" would move it backwards.
  - "Cancel the update" runs the same script with `--cancel` over a second ssh, until the restart step starts. The running script stops before it installs. Its build runs on.
  - When no daemon answers after the restart (or the restart command fails) and a previous binary exists, the script puts `<binary>.previous` back and restarts the daemon the same way, under the same ignored signals. It exits with 5 when a daemon runs that binary again, and with 4 when none comes back. A restart that leaves another binary running also exits with 4: a rollback would not change it. A retry reads the agents with `<binary>.previous` when `<binary>` cannot, since a failed restart can leave the old daemon behind a new binary.
  - The update's lock is `lock.pid` in the script's folder: the owner's pid and start time (field 22 of `/proc/<pid>/stat`, which tells a reused pid apart), written to a temporary file and moved into place, so no lock shows without its owner. A dead owner's lock counts as free, so a reboot or a SIGKILL blocks nothing. `flock` on `lock.pid.guard` makes reading the lock and replacing it one step, so exactly one of several updates takes it. A live owner's lock refuses with 6.
  - Over SSH with no terminal, a client that goes away sends the remote processes no signal: it only closes their pipes. So the script ignores SIGHUP and SIGPIPE from its start, and cargo writes through `tee` to `build.log`, so a lost connection cannot end the build or the install.
- **The background service**: the app runs `agents-cockpit --state-dir <folder> serve`, found in the app's own folder, then on `PATH`.
  It runs in a process group of its own, so Ctrl+C in the terminal that started the app does not reach it, and it outlives the app.
  Its stderr goes to `serve.log` in the app's state folder.
  The app waits up to 20 seconds for its socket, and shows the screen again with the reason when the program is missing or the service stops at once (the log's last line).
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
  On Linux they go through the freedesktop notification service, and a "needs you" one closes once the agent no longer needs you.
  On Windows they are toasts, from notify-rust's Windows backend.
  Windows shows toasts only for an app the Start menu registers, which the app is not, so the toasts carry PowerShell's name and icon.
  A click reaches the app only while the toast is on screen: once the toast is in the notification center, a click goes to PowerShell.
  notify-rust cannot withdraw a toast, so a "needs you" toast stays until it times out.
- **Paths** (module `path_style`): the app follows the path rules of the daemon's machine, not its own, since the app on Windows may reach a Linux daemon over SSH.
  `PathStyle` holds the rules, on text, so the Linux tests check both.
  On Unix, `/` separates names and case matters.
  On Windows, `\` and `/` separate names, `C:\` and `\\server\share\` are roots with no folder above, names compare whatever their case, and paths show with `\`.
  A path tells its machine by its root: an agent's folder starts with a drive or `\\` on Windows, and with `/` on Unix.
  The new agent dialog's folder field has no agent: it takes the daemon's machine, Unix over SSH (only Unix runs `agents-cockpit bridge`) and the app's own platform otherwise.
  Ctrl+O, the folder field, the changes list, the file view's tabs and short paths, markdown file links and `path:line` references, the header, the switcher and the recent folders follow these rules.
- **Preferences**: `$XDG_STATE_HOME/agents-cockpit-desktop/prefs.json` (default `~/.local/state/agents-cockpit-desktop/`), on the machine the app runs on, keeps one viewer's choices: the file view's width and its wrap toggle, the sidebar's status filter, and the accounts' colors.
  A missing or broken file means the defaults.
- **State**: `AppState` holds the summaries, the open transcripts, the plan usage and the UI state (selected agent, unread agents, open blocks).
  A pure function `apply(&mut AppState, Event)` updates it, so the state logic is tested without a window.
- **Connection task**: a Dioxus coroutine owns the `Connection`, applies events to the state signal, and sends the requests the components ask for.
- **Components** follow the mockups: sidebar, center header (the title, which a click on it, its pencil or Rename in the "/" menu turns into a field that renames the agent; usage chip, load chip, changes chip), transcript, composer (model, mode, cache timer, agent map button, Send or Stop), floating blocks and badges (tasks, running, MCP servers), file view, command palette, agent switcher, new agent dialog, agent map, notifications.
- **Accounts** (`state/accounts.rs`, `ui/accounts.rs`): with two accounts or more, each agent shows its account as a tag, its name in its color: at the start of its sidebar row's second line, and after the status in the header. Past sessions and recent folders show theirs too. With one account, nothing names it.
  - The protocol carries no color: each viewer picks them, and the preferences keep them by the daemon's machine and the account's id. An account without a choice takes the color its id gives. The six colors avoid the status colors.
  - The app asks `accounts.list` once connected, after `AccountsChanged`, when the new agent or the accounts dialog opens, and on "Check again". The answer gives each account's sign-in state and command. The daemon ends the sign-in waits that are over before it answers.
  - The new agent dialog's account button comes first in the prompt's row. It follows the folder: the account of the last agent or past session there, else the default account. A pick in its menu holds until a recent folder is picked. The menu shows each account's plan usage (the 5-hour figure amber from 80 %), "no plan limits" for an account that uses the environment's API key or token, or "Not signed in", then "Manage accounts".
  - "Accounts", in the sidebar's foot, opens a dialog with a card per account: its color, name, folder, sign-in state and number of agents. A card opens to rename the account, change its color or its environment choice ("Use the API key or token from my environment"), copy its sign-in command, or remove it. Remove is off for the default account and for an account any agent uses.
  - "Manage accounts" in the new agent dialog's menu opens the same dialog, which keeps the new agent dialog's form (`from_new_agent`), as "Add an account" opened from it does. Closing it (Close, Esc, a click outside it, Ctrl+N, or Esc in "Add an account") goes back to the new agent dialog with its folder, message, account, model and mode as they were: the typed text stays in the drafts. Ctrl+K and Ctrl+M leave it open, as they leave the new agent dialog. Opened from the sidebar's foot or the plan card, it closes to nothing. An answer to the form's folder listing or start that comes meanwhile goes to the form it keeps.
  - "Add an account" takes the name, the color, the folder and the environment choice. The folder shows the daemon's proposal, `.claude-<name>` beside the default account's `.claude`, until it is typed in, and goes as none until then. Once the daemon added the account, the dialog shows its sign-in command with Copy, and whether it is signed in.
  - The Plan chip shows the selected agent's account: its dot and its 5-hour figure, "API key", or an amber "?" when it is not signed in. The plan card lists every account, this agent's first.
  - The resume list shows each session's account, with a filter by account beside the search field.
  - An agent that waits for its account's sign-in shows a card in the chat in place of the turn's notice and end: the sign-in command, the account's state with "Check again", and "Send the message again", which sends the turn's message once the account signed in. While no check would end the wait, the button sends while the agent still waits: when the daemon cannot tell the account's sign-in (`unknown`, a login in the macOS Keychain), and when the account's credentials come from the environment (`environment`). Either way, it sends the message once. Enter presses it while it is on and no text field has the focus (the page's script passes Enter on only then, `send_again` in its key mode). The composer and Stop treat the agent as idle.
    The agent's sidebar row says "Work is not signed in" while it waits, in amber, then "Signed in: send the message again" until the message goes.
- **Agent order**: the sidebar lists the agents in the daemon's order, and the first of that order is selected when the selection goes.
  A row moves by drag and drop, or Alt+Up and Alt+Down on the selected agent; both send `agent.move`.
  The sidebar shows the new order at once, with the positions the daemon then sends, and puts the agents back when the daemon refuses the move.
  A page script follows the drag (`ui/reorder.rs`), as the splitter's does: a lifted copy of the row follows the pointer, a line shows where the row will land, and Rust hears of it once, on release.
  A press that moves less than 4 pixels stays a click.
- **Sidebar rows**: the first line holds the status dot, the title, the task progress ("3/5"), a time, the pending count and the unread dot. The second line holds the activity.
  The time is how long the turn has run for a working agent, how long it has waited for an agent that needs you, and how long ago it last did something for an idle or exited one, in one unit: "12 s", "4 min", "2 h", "3 d".
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
- **New agent dialog**: it opens with the model, effort and permission mode of the last agent started, else Opus and Auto (`open_new_agent`).
  When the "Start in a folder" tab shows (on Ctrl+N, the + button, going back from Accounts, or a click on the tab), the folder field takes the focus while it is empty, and the first message takes it once the folder is filled (`start_focus`). The "Resume a session" tab gives the focus to its search field. The caret goes to the end of the text the field already holds.
  The folder field works as the "Open file" dialog's path field.
  Once it is typed in, the folders of the folder its text names (`folders.list`) take the recent folders' place, filtered by the last segment.
  Tab or Enter completes the highlighted folder, Backspace on an empty segment goes up, and Esc brings the recent folders back.
  A text that names no absolute folder, by the rules of the daemon's machine (see Paths), lists nothing.
- **One highlight in a list**: in the Ctrl+K switcher, the Ctrl+O list, the folder completion, the resume list and the "/" palette, the pointer moves the keyboard's highlight, so one row is lit, with no CSS hover and no fade.
  WebKitGTK also sends a mousemove when a list scrolls under a still pointer, so only a pointer that moved takes the highlight (`PointerMoves` in `ui/dialogs.rs`).
  In the Ctrl+O list, the pointer picks its row as a key does, so a cut listing highlights it too.
  The keys scroll the list to the row they highlight. The pointer's row is in view already, so the palette does not scroll to it, and the list never moves under the mouse.
- **Rendering**: markdown with `pulldown-cmark`, syntax colors with `syntect` (Rust, SQL, Markdown, JSON, Bash, Nix), diffs with `similar` (line diff, then word highlights inside changed lines).
- **Switching agents**: a click on a sidebar row, or an agent picked in the Ctrl+K switcher, moves the selection, the header and the composer in the next frame.
  In that frame the chat draws only the agent's latest 25 rows, and it draws all of them in the frame after (`ShownRows` in `ui/transcript.rs`).
  Dioxus runs effects once the webview has applied a frame's changes, so an effect starts the second frame.
  Drawing a whole conversation is what made the switch slow: for 150 entries, the syntax colors, the page's new elements and their layout put the switch on screen about 180 ms after the click, against about 90 ms for 25 rows or a short chat (release build, the demo under Xvfb).
  When the chat goes back to a place scrolled up, it shows the skeleton in that frame instead, as the latest rows may not hold that place.
  The chat's scroll script keeps each agent's place, and only a chat that draws all its agent's rows names that agent (`data-agent`), so the script goes back to the place once every row is there.
  On the first click on an agent, the chat shows the skeleton until the daemon sends the first page, then draws it in the same two frames.
- **Last instruction** (`ui/last_instruction.rs`): once the user's message has scrolled out above the chat, a bar over the chat's top shows it, so the user knows which instruction the output in view answers. A click scrolls the chat back to the message.
  The chat's scroll script tells the app which entry is at the top of the view, each time another one comes there (`TopEntry`, a signal of its own, so scrolling renders only the bar). The bar shows the latest top-level user message before that entry (`Transcript::message_before`), and nothing when the entry is a user message itself.
  It shows nothing when that message sits in older messages the app has not loaded.
- **Renaming in the sidebar**: F2, or a double-click on a row's title, turns the selected agent's row title into a field, as the header's pencil does for the header's title.
  Enter or leaving the field saves with `agent.rename`, and Esc cancels.
  One title field shows at a time: F2 in the header's field moves the edit to the row with the text typed so far, and F2 in the row's field moves it back.
  The typed text lives in the drafts, as the composer's does, so typing a title renders neither the transcript nor the sidebar.
  An agent the filter hides has no row, so F2 opens the header's field.
- **Closing an agent**: the close button on a sidebar row (shown on hover and on the selected row), "Close agent" in the "/" menu, or Ctrl+Shift+W sends `agent.close`.
  Ctrl+W stays the file view's: it closes the shown tab.
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
  The chat's scroll script scrolls and highlights (`ChatReveal` in the UI state), and keeps the new place as after a scroll by the user.
  While the map is open, the page's script passes Up, Down, Left, Right and Enter to it, and Y and N when the selected subagent's approval or plan waits. The outline takes the focus when the map opens, so Enter never presses a button left focused behind the dialog.
- **Shortcuts**: Ctrl+K, Ctrl+N, Ctrl+M (the agent map), Esc (closes what is open, never stops the agent), Y, N, 1 to 9, as in plan.md, Enter outside text fields for the sign-in card's "Send the message again", Alt+Up and Alt+Down to move the selected agent in the sidebar, F2 to rename it, and Ctrl+Shift+W to close it.

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
- `cockpit_desktop`: `apply` on event sequences; the daemon's update (the protocol read and compared, the steps from the script's lines, the exit statuses and the sidebar's line for each, the state over refused attempts and per machine, a cancel the user asked for, the embedded script's lines and line ends); the accounts' state (the new agent's account, the labels, the plan chip, the resume filter, the two dialogs and the way back to the new agent dialog, the sign-in card); rendering helpers (markdown, diffs) on sample input; the "No daemon answers" choices and the config they write, through a symbolic link too; the path rules of both platforms.

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
