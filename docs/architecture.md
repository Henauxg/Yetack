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
| `agents_cockpit` | bin `agents-cockpit` | The command line: `serve`, `bridge`, and client commands (`list`, `start`, `prompt`, `answer`, `stop`, `tail`). |
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

### Agents

`AgentSummary` is what the sidebar and the header need:
id, title, folder, status, activity line, pending count, model, effort, permission mode, task progress, context percentage, prompt cache expiry, turns, API-equivalent cost, start time.

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

- `UserMessage` (text, attached images), with `queued` while the agent works;
- `AssistantText` (markdown) and `Thinking` (folded in the app);
- `ToolCall`: tool name, summary, input, status (running, done, failed), output, and for `Edit` and `Write` the file path with the old and new text;
- `Subagent`: a tool call whose own items carry it as `parent` (from `parent_tool_use_id`);
- `Approval`: tool, input (with the diff for an edit), reason, and its answer state;
- `Question`: the `AskUserQuestion` questions and options, and its answer state;
- `Plan`: the `ExitPlanMode` plan text, and its answer state (a decision, with an optional message when Claude should keep planning);
- `TurnEnd`: duration, usage and cost of the turn, or the error that ended it;
- `Notice`: a line such as "Conversation compacted".

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

`agent.start`, `agent.prompt`, `agent.cancel_queued`, `agent.answer` (approval, question or plan), `agent.interrupt`, `agent.stop`, `agent.resume`, `agent.remove`, `agent.history`, `agent.details`, `agent.set_model`, `agent.set_effort`, `agent.set_mode`, `agent.context_usage`, `agent.stop_task`, `agent.mcp_status`, `agent.mcp_reconnect`, `agent.mcp_toggle`, `agent.commands` (slash commands and skills), `agent.read_file` (the file view), `mcp.remove` (with the agent, whose folder decides the project and local scopes), `settings.set`, `sessions.list`, `sessions.resume`.

A history page is limited by bytes as well as by count: images travel inline, and a line over the framing maximum is refused.

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
- A prompt goes to stdin as a `user` message, never as an argument: other users can read arguments with `ps`.
- Claude Code asks for approvals with `can_use_tool` control requests.
  `AskUserQuestion` and `ExitPlanMode` come the same way; the hub shows them as a question or a plan instead of an approval.
  The answer is a `control_response` with `behavior: allow` (and `updatedInput`) or `behavior: deny` (and a message).
- The daemon's own control requests (`interrupt`, `set_model`, `set_permission_mode`, `get_context_usage`, `mcp_status`, `mcp_reconnect`, `mcp_toggle`, `stop_task`) carry ids of its own.
  Claude Code echoes control responses; one whose id the daemon did not send is ignored.
- Effort has no control request: the daemon sends `/effort <level>` as a prompt.

### Store

Everything lives in `$XDG_STATE_HOME/agents-cockpit/` (default `~/.local/state/agents-cockpit/`), mode 700, with `umask 077`:

- `agents-cockpit.sock`: the socket, mode 600;
- `serve.lock`: held while a daemon runs, so a second daemon refuses to start and a leftover socket is safe to delete;
- `agents.json`: one entry per agent (id, title, folder, Claude session id, model, effort, mode), written atomically (write to a temporary file, then rename);
- `settings.json`: the hidden MCP servers and other daemon-side settings;
- `events/<agent-id>.jsonl`: the agent's cockpit events, each with its `Seq`, appended;
- `raw/<agent-id>.jsonl`: every line exchanged with `claude`, for debugging.

On start the daemon reads `agents.json`, finds the last `Seq` in the logs, and resumes each agent that was running with `--resume`.

### Server and peer checks

- The daemon listens only on the Unix socket: no TCP port.
- Each connection passes two checks before any byte is read:
  1. the peer's user id (`SO_PEERCRED`) equals the daemon's;
  2. the peer's parent chain in `/proc` does not reach the daemon's pid, so a command an agent runs cannot drive the daemon.
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
- **State**: `AppState` holds the summaries, the open transcripts, the plan usage and the UI state (selected agent, unread agents, open blocks).
  A pure function `apply(&mut AppState, Event)` updates it, so the state logic is tested without a window.
- **Connection task**: a Dioxus coroutine owns the `Connection`, applies events to the state signal, and sends the requests the components ask for.
- **Components** follow the mockups: sidebar, center header (usage chip, changes chip), transcript, composer (model, mode, cache timer, Send or Stop), floating blocks and badges (tasks, running, MCP servers), file view, command palette, agent switcher, new agent dialog, notifications.
- **Rendering**: markdown with `pulldown-cmark`, syntax colors with `syntect` (Rust, SQL, Markdown, JSON, Bash, Nix), diffs with `similar` (line diff, then word highlights inside changed lines).
- **Shortcuts**: Ctrl+K, Ctrl+N, Esc, Y, N, 1 to 9, as in plan.md.

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
