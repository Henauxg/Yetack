# agents-cockpit plan

agents-cockpit runs several Claude Code agents on the server and lets you watch and steer them from a desktop app on your laptop.
The app draws a real interface with HTML and CSS: conversations, tool calls, approval buttons.
Agents keep working when the laptop disconnects.

## Goals

- Run several Claude Code agents on the server, each in the folder you choose.
- Show each agent's status (working, blocked, idle) and its conversation in one desktop app on the laptop.
- Answer approvals, send prompts and stop agents from that app.
- Keep agents running when the laptop sleeps or the SSH connection drops.
- Also connect to a daemon on the same machine as the app, directly over its Unix socket, without SSH.
- Use the Claude subscription, through the unmodified `claude` binary signed in with your own account.

## Out of scope for v1

- A git worktree per agent: you pick the folder when you start an agent.
- Agents on machines other than the server.
- Access from a phone or a browser.

## Decisions

1. Claude Code runs in JSON mode, not in a terminal.
   A CSS interface needs messages, tool calls and approval requests as data.
   A terminal multiplexer such as herdr only sees Claude Code's screen as a grid of characters.
2. A daemon on the server owns the `claude` processes, so they survive the laptop disconnecting.
3. The daemon listens only on a Unix socket in a folder of mode 700.
   No TCP port is open, on the server or on the laptop.
4. The laptop reaches the daemon through `ssh <server> agents-cockpit bridge`.
   Your SSH key is the only login.
5. The front end is a Dioxus desktop app on the laptop (HTML and CSS in a webview).

## Architecture

```text
laptop                               server
agents-cockpit desktop app
  runs: ssh <server> agents-cockpit bridge
        (stdin/stdout over SSH)  →   agents-cockpit bridge
                                       copies bytes to the Unix socket
                                     agents-cockpit daemon
                                       one claude process per agent,
                                       JSON on stdin/stdout
```

One Cargo workspace with three crates:

- `protocol`: serde types for Claude Code's JSON events and for the daemon API.
- `agents-cockpit`: one binary for the server with the subcommands `serve` (the daemon), `bridge`, and a few CLI commands (`list`, `start`, `prompt`) for scripts and tests.
- `desktop`: the Dioxus desktop app for the laptop.

### What the bridge does

SSH can run a command on the server and connect that command's stdin and stdout to the laptop app.
`agents-cockpit bridge` is that command: it connects to `~/.local/state/agents-cockpit/agents-cockpit.sock` and copies bytes in both directions until one side closes.
It does the same job as `socat - UNIX-CONNECT:<socket>`, without needing socat on the server or the socket path in the laptop's config.
Without it, the laptop would need `ssh -L` socket forwarding, which leaves a socket file on the laptop that any laptop process can use.
Later, the bridge can also start the daemon when it is not running.

## Claude Code integration

The Claude Code extension for VS Code and Cursor starts each agent with these flags:

```text
claude --output-format stream-json --verbose --input-format stream-json
       --permission-prompt-tool=stdio --resume=<session-id>
       --setting-sources=user,project,local --permission-mode=auto
       --replay-user-messages --enable-auth-status ...
```

agents-cockpit starts `claude` the same way:

- One long-running process per agent, with JSON in and out, one object per line.
- Prompts go in as `{"type":"user","message":{"role":"user","content":"..."}}` on stdin.
- Claude Code writes events on stdout: `system/init`, `assistant` (text and `tool_use`), `user` (tool results), `result` at the end of a turn, and `stream_event` text deltas with `--include-partial-messages`.
- `--replay-user-messages` echoes each prompt back, so every connected UI sees prompts sent from another UI.
- Approvals use the stdio control protocol: Claude Code writes a `control_request` with subtype `can_use_tool` and waits for a `control_response` on stdin.
  This protocol is not documented.
  The open-source Python Agent SDK implements it in [claude_agent_sdk/_internal/query.py](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py) (`_handle_control_request`), so copy it from there.
  Keep it in one Rust module and pin the Claude Code version in the tests.
- The daemon changes a running agent with control requests (subtypes from the Python SDK's `query.py`): `interrupt` (the Stop button), `set_model`, `set_permission_mode` (`default`, `acceptEdits`, `plan`, `auto`, ...) and `get_context_usage` (context by category).
  Effort has no control request: the daemon sends `/effort <level>` as a prompt, which `-p` mode accepts.
- A restarted agent starts with `--resume <session-id>`.
  With `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`, Claude Code continues a turn that was cut off.
  The daemon sets it only for the agents it resumes as it starts, whose turn its own stop cut off: a turn a user stopped stays stopped when the user resumes the agent or a past session.
- Never pass `--bare`: bare mode ignores the subscription login and needs an API key.
- Never pass a prompt as a command-line argument: every user on the server can read arguments with `ps`.
- Usage comes from two events (field names from the Python SDK's `types.py`):
  - `result`: `total_cost_usd` (an API-equivalent estimate), `num_turns`, `usage`, and `model_usage` with each model's `contextWindow`, which gives the context percentage.
  - `rate_limit_event`: `rate_limit_info` with `rate_limit_type` (`five_hour`, `seven_day`, ...), `utilization` from 0 to 1, and `resets_at`.
    The SDK says Claude Code sends it when the rate limit status changes, so the probe checks how often `utilization` updates.

Docs: [Run Claude Code programmatically](https://code.claude.com/docs/en/headless) and [Legal and compliance](https://code.claude.com/docs/en/legal-and-compliance).
The legal page says its rules do not prevent "an end user from signing in to the unmodified Claude Code binary with their own Claude subscription".

### Agent status

| Status | When |
| --- | --- |
| working | From a prompt until the `result` event |
| blocked | A `can_use_tool` request or an `AskUserQuestion` call waits for an answer |
| idle | After the `result` event |
| exited | The `claude` process ended |

The Python SDK also reads a `system/session_state_changed` event, which may give this status directly.

## Daemon

- Runs as a systemd user service on the server.
  It needs linger on the account (`loginctl enable-linger`), so it keeps running after logout.
- Leaves the umask alone, so `claude` and the agents' commands create files as the user would.
  Each of its own files and folders gets a private mode when it is created.
- Keeps everything in `~/.local/state/agents-cockpit/` (mode 700):
  - `agents-cockpit.sock`: the Unix socket (mode 600).
  - `serve.lock`: held while a daemon runs.
  - `agents.json`: one entry per agent with its Id, folder, title and Claude session Id.
  - `logs/<agent-id>.jsonl`: every event received from `claude` and every message sent to it, appended.
- Starts in this order: take `serve.lock`, delete a socket file a crash left behind, bind, set mode 600.
  The lock tells whether another daemon still uses the socket, so the daemon never deletes a live one.
- On restart, reloads `agents.json`, restarts each agent that was running with `--resume`, and serves the history from the logs.

## Daemon API

- Newline-delimited JSON over the Unix socket, one connection per UI.
- The bridge copies bytes in both directions and parses nothing.
- Requests carry an `id`, and responses echo it.
- After `hello`, the daemon sends a snapshot (agents and their statuses), then live events.
- Every event carries a global sequence number.
  A UI that reconnects sends its last sequence number and receives what it missed, read from the logs.
- Requests in v1: `agent.list`, `agent.start` (folder, title), `agent.prompt`, `agent.approve` (allow or deny, optional message), `agent.interrupt`, `agent.stop`, `agent.history` (older events, for scrolling up), `agent.set_model`, `agent.set_effort`, `agent.set_mode`, `agent.context_usage`, `agent.answer` (a question or a plan), `agent.stop_task`, `agent.mcp_status`, `agent.mcp_reconnect`, `agent.mcp_toggle`, `mcp.remove` (runs `claude mcp remove <name> -s <scope>`), `settings.set` (the hidden MCP servers, kept by the daemon), `sessions.list` and `sessions.resume` (past Claude sessions on the server).
- MCP server details come from the `mcp_status` control request: status (`connected`, `failed`, `needs-auth`, `pending`, `disabled`), server version, error, config, scope, and tools with their `readOnly`, `destructive` and `openWorld` annotations. The daemon counts tool calls per agent from its event log.

## Security

### Threats

- Other users on the server.
  When `/proc` is mounted without `hidepid`, they see every command line.
  They can connect to any port on 127.0.0.1.
- Processes running as you on the server, including the shell commands the agents run.
  A prompt-injected agent could try to connect to the daemon and approve its own requests.
- Anyone on the network.
- Root on the server can read everything: accepted, as today.

### Measures

- No TCP port anywhere.
  For comparison, the VS Code and Cursor remote servers listen on a 127.0.0.1 port that every user of the machine can reach, protected only by a token file.
- Folder mode 700 and socket mode 600.
  On Linux, connecting to a Unix socket needs write permission on the socket file, so other users cannot connect.
- A peer check on every connection:
  - Get the peer's user Id and pid with `SO_PEERCRED`, and reject any user Id other than your own.
  - Walk the peer's parent chain in `/proc` and reject it when the chain reaches the daemon (the agents' commands descend from it, the bridge descends from `sshd`).
- The parent-chain check stops an agent that simply calls the socket.
  It does not stop code that detaches on purpose (double fork, `setsid`), because such a process gets a new parent.
  The real protection against that is Claude Code's sandbox, configured to deny the agents' commands access to `~/.local/state/agents-cockpit/`: the probe checks whether this works with a Unix socket.
- `ForwardAgent` stays off for the server in the laptop's `~/.ssh/config`, so a process on the server cannot use your laptop's key to log back in through `sshd`.
- Prompts go only through stdin.

## Front end

- A Dioxus desktop app on the laptop.
- The laptop runs NixOS, so the webview is WebKitGTK.
  The flake's dev shell provides the Dioxus Linux dependencies: WebKitGTK 4.1, GTK 3, libsoup 3, xdotool, OpenSSL and pkg-config (nixpkgs: `webkitgtk_4_1`, `gtk3`, `libsoup_3`, `xdotool`, `openssl`, `pkg-config`, to confirm when writing the flake).
  If the window stays blank, try `WEBKIT_DISABLE_DMABUF_RENDERER=1`, a common WebKitGTK problem with some GPU drivers.
- The layout was settled over five rounds of mockups (round 1 compared three columns, a chat with a file view, and an approvals inbox).
- Decided after round 1:
  - A black theme with neutral greys, no blue tint.
  - The compact sidebar of proposal C (two-line rows, a badge when an agent waits for you).
  - No app-wide top bar.
  - Usage goes in the header of the center pane, not in the sidebar.
  - Small edits show inline in the chat as compact diffs.
  - Code blocks in agent answers have a copy button.
  - Stop takes the place of Send while an agent works.
  - The composer shows the model and effort, the prompt cache timer and the permission mode.
  - Changed files open from a compact list, not a full side panel.
  - A read-only file view opens on the right and closes with a cross.
  - No cross-agent approvals inbox.
- Decided after round 2:
  - Usage is one chip in the header (plan and context percentages) that opens a detail card: plan windows, context by category, prompt cache, turns, cost at API prices.
  - Code blocks, inline diffs and the file view get syntax colors, picked from the block's language tag or the file's extension.
  - Scrollbars are thin and dark.
  - The center header stays on one line: the title shortens first, then the usage and changes chips shrink to icons.
- Decided after round 3 (mocked in round 4):
  - An agent that finished while you looked elsewhere stays unread: bold title and a white dot until you open it.
  - Desktop notifications when an agent starts waiting for you, and when it finishes while you look elsewhere.
  - Questions (`AskUserQuestion`) show as option buttons plus an "Other" field; plans show as a card with "Yes, accept edits", "Yes, ask before edits" and "No, keep planning".
  - When the file view is closed, a right panel shows three blocks: the task list (`TodoWrite`), what runs in the background (subagents and commands, each with Stop) and the MCP servers (a failed one has Retry).
  - The sidebar row shows the task progress ("3/5").
  - Subagent tool calls fold under the call that started them.
  - `file:line` references in agent text open the file view at that line; agent text renders as markdown (headings, lists, tables).
  - "/" opens a palette of commands and skills, filtered as you type.
  - Pasted screenshots show as thumbnails in the composer and in the sent message.
  - A message sent while the agent works shows as queued, with Cancel.
  - Command output collapses after 5 lines; a failed command has a red dot and its exit code.
  - Ctrl+N opens the new agent dialog (recent folders, or resume a past session); Ctrl+K opens the agent switcher; Esc stops the agent; Y and N answer an approval or a plan.
  - No rewind of file changes in v1.
- Decided after round 4 (mocked in round 5), replacing the right panel above:
  - Tasks, background work and MCP servers are floating blocks at the top right of the chat, each folding to one line. They show only when they have content, except MCP servers.
  - When the file view is open, the blocks become badges on the right edge of the chat (tasks "3/5", running count, MCP); a click opens that block as a popover.
  - A failing MCP server shows only as a red dot on the folded block or badge, never as red text in the chat.
  - Each MCP server can be hidden from the view: it keeps running, but leaves the list and the red dot. A "Show them" link lists hidden servers.
  - Clicking a server opens its detail: status, version, command or URL, scope, error, three icon buttons with tooltips on its title row (Reconnect, "Disable for this agent", "Hide from my view"), its tools with read-only or destructive tags, filters and per-agent call counts, and "Remove from the config" for user, project and local scopes.
  - A background task stops with a small round icon button.
- Planned after v1: the agent map, as in the Claude Code extension.
  - A composer button with a live dot (green while a subagent runs) opens a map of the selected agent: the agent at the root, every subagent it started under it, and subagents started by subagents under those.
  - Each node shows a status dot, the subagent's description, its duration and its token count, updated live while it runs.
  - Clicking a node shows its details: the prompt it got, its transcript (the items nested under it), its tool calls, and Stop while it runs.
  - The "Running" block stays as the compact live view, with a link to open the map.
  - Data: the tree comes from `parent_tool_use_id` (each subagent's messages carry the id of the tool call that started it); status, tokens, tool calls and duration from `task_started`, `task_progress` (its `usage`: total tokens, tool uses, duration) and `task_notification`.
  - Protocol: `AgentDetails` gains a list of subagents (id, parent, description, foreground or background, status, start and end time, tokens, tool calls, last tool), filled by the daemon.
- It starts `ssh <server> /absolute/path/to/agents-cockpit bridge` and speaks the daemon API over the process's stdin and stdout.
  Use an absolute path: a non-interactive SSH command does not load your interactive shell setup, so `~/.cargo/bin` may be missing from `PATH`.
- It reconnects after an SSH drop and sends its last sequence number.
- Views in v1:
  - The agent list with statuses.
  - The conversation, with markdown and code blocks.
  - Tool calls as cards (command, file, result).
  - Approval cards with Allow and Deny.
  - The prompt box and a Stop button.
  - Plan usage (5-hour and 7-day windows), and each agent's context percentage, turns and API-equivalent cost.

Options set aside:

- A web UI in a browser: it needs a port on the laptop, and any website open in that browser can send requests to localhost ports, so it would need origin checks and a token.
- Dioxus native (Blitz): it draws with its own engine instead of a browser, its CSS support is incomplete (as of mid-2026), and it cannot use browser libraries for markdown, code highlighting or diffs.

## Milestones

1. Probe (1 day), in a scratch folder, by hand:
   - Check whether `-p` is needed: the extension passes none.
   - Capture a prompt with a tool call, an approval allowed and one denied, an `AskUserQuestion`, an interrupt, a kill then `--resume`, and `--include-partial-messages`.
   - Check whether `system/session_state_changed` gives the status.
   - Check how often `rate_limit_event` arrives, and whether it carries `utilization` when the status has not changed.
   - Check how an `AskUserQuestion` answer and a plan approval go back to Claude Code.
   - Check that a prompt with an image content block works, and that `system/init` lists the skills and commands for the palette.
   - Check the `task_started` and `task_notification` events and the `stop_task` request for background work.
   - Check whether a foreground subagent (the `Agent` tool) also gets `task_*` events, or only background ones, and whether a subagent started by a subagent nests through `parent_tool_use_id` (for the agent map).
   - Check whether Claude Code's sandbox can deny the agents' commands access to the socket folder.
   - Save the captures as test fixtures, with personal paths and content replaced.
2. Daemon (about 1 week): start, resume and stop agents, approvals, event logs, the API with sequence numbers, the peer check, the bridge, the CLI commands.
3. Desktop app (1 to 2 weeks): the v1 views above.
4. Polish: diff views for file edits, a desktop notification when an agent becomes blocked, keyboard shortcuts.
5. Agent map: the subagent list in `AgentDetails`, filled by the daemon, then the map dialog and its node details in the app.

## Background

- herdr ([github.com/herdrdev/herdr](https://github.com/herdrdev/herdr)) runs Claude Code in its terminal interface inside a pseudo-terminal and reads the screen to detect status.
  Its public API returns screen snapshots, and its live stream (`herdr terminal session control`) sends ANSI bytes.
  That fits a terminal UI, not a CSS one, which is why agents-cockpit talks to Claude Code in JSON mode instead.
- agents-cockpit copies herdr's useful ideas: a daemon that outlives the client, an SSH stdin/stdout attach, and a snapshot followed by events with sequence numbers.
