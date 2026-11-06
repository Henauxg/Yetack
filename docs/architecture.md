# Architecture

This document says how the code is split and why.
[plan.md](plan.md) says what the app does and the decisions behind it.
The Rust code follows [standards/rust/](standards/rust/README.md).

## Overview

```text
desktop app or CLI                       daemon (agents-cockpit serve)
  cockpit_client                           cockpit_daemon
    transport:                               server: Unix socket, peer checks
      Local: the Unix socket       ──────►   hub: agents, sequence numbers, broadcast
      Ssh: ssh <host> agents-cockpit bridge  store: state folder, event logs
           (stdin/stdout, copied to the socket)
                                             claude process per agent:
                                               claude_protocol (JSON mode, control protocol)
```

There are two ways to reach a daemon:

- **Remote**: the app runs `ssh <host> agents-cockpit bridge`.
  The bridge copies bytes between its stdin/stdout and the daemon's socket on that host.
- **Local**: the daemon runs on the same machine as the app.
  The app connects to the daemon's Unix socket directly, with no SSH and no bridge.

Both carry the same protocol, so everything above the transport is identical.

## Crates

All crates live in `crates/` and are application code ([standards/rust/README.md](standards/rust/README.md#library-code-and-application-code)): nothing outside this repository calls them.

| crate | kind | owns |
| --- | --- | --- |
| `claude_protocol` | lib | Claude Code's JSON mode: the lines `claude` reads and writes, and the control protocol. Types and parsing only, no IO. |
| `cockpit_protocol` | lib | The daemon API: ids, the transcript model, requests, responses, events, snapshots, and the line framing. |
| `cockpit_client` | lib | A connection to a daemon over a transport (local socket or SSH bridge): requests, responses, events, reconnection. |
| `cockpit_daemon` | lib | The daemon: claude processes, the hub, the store, the socket server and its peer checks. |
| `agents_cockpit` | bin `agents-cockpit` | The command line: `serve`, `bridge`, and client commands (`list`, `start`, `prompt`, `answer`, `stop`, `close`, `rename`, `move`, `tail`). |
| `cockpit_desktop` | bin `agents-cockpit-desktop` | The Dioxus desktop app. |

Dependencies point one way:

```text
claude_protocol ◄── cockpit_daemon ──► cockpit_protocol ◄── cockpit_client ◄── cockpit_desktop
                          ▲                                       ▲
                          └──────────── agents_cockpit ───────────┘
```

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

### Agents

`AgentSummary` is what the sidebar and the header need:
id, title, folder, status, activity line, pending count, model, effort, permission mode, task progress, context percentage, prompt cache expiry, turns, API-equivalent cost, start time, last activity time, running turn's start time, position in the list.

The two times let the app count the time on a sidebar row by itself, so the daemon sends no event to move a clock:

- `turn_started_at` is set when a prompt goes to `claude`, or at the first line of a turn Claude Code starts without one (a resumed cut-off turn). It is `None` between turns.
- `last_activity_at` moves when the agent starts, a prompt goes to `claude`, the agent begins to wait for you (the first of its pending requests), a turn ends, or its process ends.
  Lines within a turn and streamed text do not move it, and neither does a slash command that runs no turn, such as the `/effort` the menu sends.
- Each of these moments already changes the summary's status, pending count or turn count, so the times add no `AgentUpdated` event.
- After a daemon restart, both come from the last summary in the event log. A turn the restart cut off has no turn start any more, and its last activity is the later of its start and the logged last activity.
  A log written before these fields existed reads `None` for both, and the row then shows no time, as with an older daemon. An older app ignores the two fields.

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
| `NeedsYou` | an approval, a question or a plan waits for an answer |
| `Idle` | after `result`, nothing pending |
| `Exited` | the `claude` process ended |

### Transcript

An agent's conversation is a list of `TranscriptItem`s, each with an `ItemId` and an optional `parent` (the subagent it nests under).
The daemon numbers items in transcript order, so the app can merge a history page with live appends by id.
The kinds of item:

- `UserMessage` (text, attached images), with `queued` while the agent works; a slash command shows as typed, such as `/review fix`, not as Claude Code's `command-name` wrapper;
- `AssistantText` (markdown) and `Thinking` (folded in the app);
- `ToolCall`: tool name, summary, input, status (running, done, failed), output, and for `Edit` and `Write` the file path with the old and new text (both left empty when either is over 1 MiB);
- `Subagent`: a tool call whose own items carry it as `parent` (from `parent_tool_use_id`);
- `Approval`: tool, input (with the diff for an edit), reason, its answer state, and the item of its tool call (from the request's `tool_use_id`, which Claude Code sends after the call's `tool_use`);
- `Question`: the `AskUserQuestion` questions and options, and its answer state;
- `Plan`: the `ExitPlanMode` plan text, and its answer state (a decision, with an optional message when Claude should keep planning);
- `TurnEnd`: duration, usage and cost of the turn, and whether the user stopped it (`stopped`, from the `result` line's `terminal_reason`), or the error that ended it;
- `Notice`: a line such as "Conversation compacted" or "Conversation cleared" (a `conversation_reset` line, after which Claude Code's total cost counts from 0 again), a local command's output such as `/effort`'s, or a message Claude Code writes itself (a user line whose `origin` is not `human`), such as its summary of a background task that ended.
  Context Claude Code adds for Claude (a user line marked `isSynthetic`) becomes a notice whose `detail` holds its text, which the app shows folded: "Skill instructions: /loop" after a `Skill` call that loaded it, "Conversation summary" after a compaction, else "Context added by Claude Code".

An answer state is `Waiting`, `Answered`, or `Cancelled` (Claude Code withdraws a pending request when the turn is interrupted).

`AgentDetails` holds what the side blocks show: the task list (from `TodoWrite`), background tasks (`task_started`, `task_notification`), changed files, and the last context usage.

### Plan usage

`PlanUsage` holds the latest `rate_limit_event` per window (`five_hour`, `seven_day`, ...): utilization, reset time, status.
It belongs to the account, not to an agent.

## Protocol (`cockpit_protocol`)

### Framing

One JSON object per line, UTF-8, `\n` at the end.
A reader refuses a line longer than a fixed maximum (16 MiB) instead of buffering without bound.

### Session

1. The client sends `Hello { protocol_version, last_seq }`.
2. The daemon answers `Welcome { protocol_version, catchup }`, or `Refused { reason }` when the versions differ.
3. `catchup` is `Replay` when `last_seq` was given and the logs still hold every event after it: the daemon then sends those events, then live events.
   Otherwise it is `Snapshot(snapshot)`: the client replaces its state with it, and live events follow.
   A welcome never carries both, so no event is applied twice.
4. The client sends `Request { id, request }`; the daemon answers `Response { id, result }`.
   Responses and events share the connection, in any order.

The snapshot holds every `AgentSummary`, the plan usage, the settings, and the last `Seq`.
Transcripts are not in the snapshot: the app asks for an agent's transcript with `agent.history` when it opens the agent, a page at a time.

### Requests

`agent.start`, `agent.prompt`, `agent.cancel_queued`, `agent.answer` (approval, question or plan), `agent.interrupt`, `agent.stop`, `agent.resume`, `agent.remove` (an exited agent), `agent.close` (any agent), `agent.history`, `agent.details`, `agent.set_model`, `agent.set_effort`, `agent.set_mode`, `agent.rename`, `agent.move` (the agent goes just above `before`, or to the bottom when `before` is `null`), `agent.context_usage`, `agent.stop_task`, `agent.mcp_status`, `agent.mcp_reconnect`, `agent.mcp_toggle`, `agent.commands` (slash commands and skills), `agent.read_file` (the file view), `agent.list_dir` and `agent.find_files` (the "Open file" dialog), `agent.stop_process` (one of the agent's commands), `mcp.remove` (with the agent, whose folder decides the project and local scopes), `settings.set`, `sessions.list`, `sessions.resume`, `folders.list` (the new agent dialog's folder field), `machine.load` (see [Machine load](#machine-load)).

`agent.close` ends an agent whatever its process does, in one request.
`agent.stop` then `agent.remove` would not do it: `remove` refuses an agent whose process still runs, and the process exits some time after `stop` closes its stdin.
The daemon interrupts a working agent's turn, takes the agent out at once (`AgentRemoved`, its logs deleted), and the process handle leaving with it closes `claude`'s stdin.
`claude` ends its session file and exits, or is killed after 10 seconds, so the session can come back with `sessions.resume`.

The three file requests reach the same folders, once symlinks are resolved and with `..` refused: the agent's folder, and each git checkout that holds a file the agent read or changed (the folder with the `.git` entry nearest above it, unless that is `/` or holds the home folder).
`agent.read_file` also reads a file the agent touched that no such checkout holds, and that file only.
The daemon and the app run as the same user, and the agent's commands already read what that user can, so these limits keep the views to the agent's work rather than hide anything from the user.

- `agent.read_file` sends a text file of at most 4 MiB. A relative path that names no file in the agent's folder is looked up: the most recent file the agent read or changed whose path ends with it, then a `git ls-files` search of the folders above, which stops at 20 matches or 3 seconds. Several matches come back as `FileMatches`, for the user to pick.
- `agent.list_dir` answers a folder's entries, folders first, each with its kind (folder, file or other), a file's size, and whether it is a symbolic link. It reads at most 20,000 entries and sends at most 2,000, and says when it cut the list. It runs off the hub's task.
- `agent.find_files` answers the files whose name holds a text, whatever its case, from the same bounded search.

`folders.list` answers the folders inside a folder, for the new agent dialog, as a `Directory` that holds folders alone.
It reaches any folder the daemon's user can read, since an agent can start in any of them, and it sends no file names.
It refuses a relative path, and one that starts with `~`, as `agent.start` does.
It keeps the bounds of `agent.list_dir`: it reads at most 20,000 entries, files included, sends at most 2,000 folders, and says when it cut the list.
It leaves out a folder whose name is not UTF-8: the protocol carries paths as text, so no request could reach that folder.
A symbolic link to a folder counts as a folder, and a hidden folder lists as any other.
It runs off the hub's task.

A history page is limited by bytes as well as by count: images travel inline, and a line over the framing maximum is refused.
An item too large for any response comes as a `Notice` that says so, under the item's id, so paging moves on.

Titles and model names go into every snapshot, so the daemon refuses a title over 200 characters and a model name over 100 with `invalid_request`.
It cuts a past session's title at 200 characters.

The method names are an enum with fixed strings, pinned by a test ([standards/rust/api_design.md](standards/rust/api_design.md#declare-a-reusable-string-once)).

### Events

`AgentAdded`, `AgentUpdated(AgentSummary)`, `AgentRemoved`, `TranscriptAppended { agent, item }`, `TranscriptChanged { agent, item }`, `DetailsChanged { agent, details }`, `PlanUsageChanged`, `SettingsChanged`.
Each carries its `Seq`.

## Daemon (`cockpit_daemon`)

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

- Command: `claude --output-format stream-json --input-format stream-json --verbose --permission-prompt-tool stdio --replay-user-messages`, plus `--resume <session>`, `--model`, `--effort`, `--permission-mode` when set.
  It runs in the agent's folder.
  The binary is `claude` from `PATH`, or `AGENTS_COCKPIT_CLAUDE` for tests.
- First message: the `initialize` control request.
  Its answer lists the slash commands and skills.
  The daemon then asks for the MCP status and the context usage, which Claude Code answers before the first prompt, so a new or resumed agent shows them at once.
- An agent given no effort runs at the one Claude Code applies by itself. The daemon asks for it with `get_settings` once `initialize` is answered, and again after a model change, and the summary shows its `applied.effort` with `effort_is_default`.
  Claude Code resolves it: the local, then the project settings file of the folder it runs in, else the model's default. The user's own `effortLevel` counts only for some models, so the daemon does not read the files itself.
- A prompt goes to stdin as a `user` message, never as an argument: other users can read arguments with `ps`.
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
- `serve.lock`: held while a daemon runs, so a second daemon refuses to start and a leftover socket is safe to delete;
- `agents.json`: one entry per agent (id, title, folder, Claude session id, model, effort, mode, position in the list, and for an agent made from a past session the part of the session file it shows), written atomically (write to a temporary file, then rename);
- `settings.json`: the hidden MCP servers and other daemon-side settings;
- `events/<agent-id>.jsonl`: the agent's cockpit events, each with its `Seq`, appended;
- `raw/<agent-id>.jsonl`: every line exchanged with `claude`, for debugging.

The daemon leaves the umask alone, since `claude` and the agents' commands inherit it.
It creates each folder with mode 700 and each file with mode 600.
The socket gets mode 600 right after the bind; until then, the 700 folder keeps other users away from it.

On start the daemon reads `agents.json`, finds the last `Seq` in the logs, and resumes each agent that was running with `--resume`.

### Past sessions

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

### Machine load

`machine.load` answers the latest sample and the samples of the last five minutes, for the server and for each agent: CPU in cores in use, memory, the load average, and each agent's three heaviest commands.

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

`agent.stop_process` sends SIGTERM to one of the agent's processes.
The daemon opens a pidfd on the process, checks at that moment that its parents in `/proc` reach the agent's `claude`, then signals through the pidfd.
A process outside the agent is refused with `not_found`, and `claude` itself with `invalid_request`: `agent.stop` stops it.
When the process ends and its pid goes to another one during the check, the signal goes to the ended process through the pidfd, and fails.

### Server and peer checks

- The daemon listens only on the Unix socket: no TCP port.
- Each connection passes two checks before any byte is read:
  1. the peer's user id (`SO_PEERCRED`) equals the daemon's;
  2. the peer's parent chain in `/proc` does not reach the daemon's pid, so a command an agent runs cannot drive the daemon.
     The walk stops, and accepts, at a process of another user or one that `/proc` hides (`hidepid=1` or `2`): the daemon's descendants all run as its user, and `/proc` always shows a user's own processes.
- `agents-cockpit bridge` connects to the socket like any client; it descends from `sshd`, not from the daemon.

## Client (`cockpit_client`)

- `Transport::Local { socket }` connects to the socket.
- `Transport::Ssh { host, remote_command }` runs `ssh -T -x -o BatchMode=yes -o ForwardAgent=no -o ClearAllForwardings=yes -- <host> '<remote_command>' bridge`.
  `BatchMode` makes a missing key fail at once instead of waiting for a password the app cannot show.
  `ForwardAgent=no`, `-x` and `ClearAllForwardings=yes` open no agent, X11 or port forwarding to the laptop, even when the user's ssh config asks for one.
  `--` keeps ssh from reading the host or the remote command as an option, and the quotes keep the host's shell from reading anything in the remote command.
- `Connection` sends requests and matches responses by `RequestId`, and yields events.
  It reads the daemon at all times, so a response never waits behind events the app has not read.
  Events wait for the app in a queue of 4096: when an event finds it full, the waiting events are dropped and `next_event` returns `CloseError::EventsLost`.
- `Session` reconnects with backoff after a drop, sending the last `Seq` it saw.
  After lost events, it reconnects without a last `Seq`, so the daemon answers with a snapshot.

## Desktop app (`cockpit_desktop`)

- **Config**: `~/.config/agents-cockpit/config.toml` lists the daemons to reach, each local or over SSH.
  v1 connects to one.
- **Preferences**: `$XDG_STATE_HOME/agents-cockpit-desktop/prefs.json` (default `~/.local/state/agents-cockpit-desktop/`), on the machine the app runs on, keeps one viewer's choices: the file view's width and its wrap toggle, and the sidebar's status filter.
  A missing or broken file means the defaults.
- **State**: `AppState` holds the summaries, the open transcripts, the plan usage and the UI state (selected agent, unread agents, open blocks).
  A pure function `apply(&mut AppState, Event)` updates it, so the state logic is tested without a window.
- **Connection task**: a Dioxus coroutine owns the `Connection`, applies events to the state signal, and sends the requests the components ask for.
- **Components** follow the mockups: sidebar, center header (the title, which its pencil, a double-click or Rename in the "/" menu turns into a field that renames the agent; usage chip, load chip, changes chip), transcript, composer (model, mode, cache timer, Send or Stop), floating blocks and badges (tasks, running, MCP servers), file view, command palette, agent switcher, new agent dialog, notifications.
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
- **New agent dialog**: the folder field works as the "Open file" dialog's path field.
  Once it is typed in, the folders of the folder its text names (`folders.list`) take the recent folders' place, filtered by the last segment.
  Tab or Enter completes the highlighted folder, Backspace on an empty segment goes up, and Esc brings the recent folders back.
  A text that names no absolute folder lists nothing.
- **Rendering**: markdown with `pulldown-cmark`, syntax colors with `syntect` (Rust, SQL, Markdown, JSON, Bash, Nix), diffs with `similar` (line diff, then word highlights inside changed lines).
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
- **Shortcuts**: Ctrl+K, Ctrl+N, Esc (closes what is open, never stops the agent), Y, N, 1 to 9, as in plan.md, Alt+Up and Alt+Down to move the selected agent in the sidebar, F2 to rename it, and Ctrl+Shift+W to close it.

## Errors

- Each library crate has typed errors (`thiserror`), one enum per area: `ParseError` in `claude_protocol`, `FrameError` in `cockpit_protocol`, `ConnectError` and `RequestError` in `cockpit_client`, `StoreError`, `ProcessError` and `PeerError` in `cockpit_daemon`.
- `anyhow` only in the two binaries, to report.
- A request that fails returns an error response with a code and a message; the connection stays open.
- A `claude` line that does not parse is logged in `raw/` and skipped; it never stops the agent.

## Tests

- `claude_protocol`: parsing of fixture lines in `crates/claude_protocol/tests/fixtures/`.
  Fixtures are written by hand from the shapes Claude Code sends, with made-up paths, ids and text.
- `cockpit_protocol`: round trips of every message, and the method names pinned.
- `cockpit_daemon`: the transcript conversion on fixture sequences; the store in a temporary folder; the server end to end with a fake `claude` (a small test binary that replays a script of lines, selected with `AGENTS_COCKPIT_CLAUDE`).
- `cockpit_client`: against an in-process fake daemon over a socket pair.
- `cockpit_desktop`: `apply` on event sequences; rendering helpers (markdown, diffs) on sample input.

## Development

- `nix develop` gives the toolchain: stable Rust, nightly rustfmt (the standard's `rustfmt.toml` needs it), clippy, and the WebKitGTK libraries for the desktop app.
- Format: `cargo fmt` (nightly rustfmt from the dev shell, reading the root `rustfmt.toml`).
- Lint: `cargo clippy -q --workspace --all-targets`; the workspace lints in `Cargo.toml` and `CLIPPY_CONF_DIR` apply the standard.
- Test: `cargo test -q --workspace`.
