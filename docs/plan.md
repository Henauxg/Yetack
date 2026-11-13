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
- Or run the agents inside the app itself, when no daemon answers: they stop when the app closes, and resume the next time the app runs them.
- Use the Claude subscription, through the unmodified `claude` binary signed in with your own account.
- Run agents with several Claude accounts, such as a personal and a work subscription: each account is a Claude Code config folder with its own login (`CLAUDE_CONFIG_DIR`).

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
   The daemon in the app listens on nothing: the app reaches it through memory.
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
| blocked | A `can_use_tool` request or an `AskUserQuestion` call waits for an answer, or the agent's account needs a sign-in |
| idle | After the `result` event |
| exited | The `claude` process ended |

The Python SDK also reads a `system/session_state_changed` event, which may give this status directly.

## Daemon

- Runs as a systemd user service on the server.
  It needs linger on the account (`loginctl enable-linger`), so it keeps running after logout.
- Or runs inside the desktop app, with the same state folder and the same `serve.lock`, so the app and a service never run at once on one folder (see "How the app reaches a daemon").
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
- Requests in v1: `agent.list`, `agent.start` (folder, title), `agent.prompt`, `agent.approve` (allow or deny, optional message), `agent.interrupt`, `agent.stop`, `agent.history` (older events, for scrolling up), `agent.set_model`, `agent.set_effort`, `agent.set_mode`, `agent.context_usage`, `agent.answer` (a question or a plan), `agent.stop_task`, `agent.mcp_status`, `agent.mcp_reconnect`, `agent.mcp_toggle`, `mcp.remove` (runs `claude mcp remove <name> -s <scope>`), `settings.set` (the hidden MCP servers, kept by the daemon), `sessions.list` and `sessions.resume` (past Claude sessions on the server, of every account), `accounts.list`, `accounts.add`, `accounts.change` and `accounts.remove` (the Claude accounts agents run with).
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
- The daemon in the app has no socket, so no peer check applies and the agents' commands have nothing to connect to.
- The parent-chain check stops an agent that simply calls the socket.
  It does not stop code that detaches on purpose (double fork, `setsid`), because such a process gets a new parent.
  The real protection against that is Claude Code's sandbox, configured to deny the agents' commands access to `~/.local/state/agents-cockpit/`: the probe checks whether this works with a Unix socket.
- `ForwardAgent` stays off for the server in the laptop's `~/.ssh/config`, so a process on the server cannot use your laptop's key to log back in through `sshd`.
- Prompts go only through stdin.
- The daemon never reads, stores or passes a credential, an API key or a token.
  An account is a config folder: the daemon sets `CLAUDE_CONFIG_DIR` to it, the user signs in by running `claude auth login` once with that variable, and the login stays in the folder (or the macOS Keychain).
  The daemon removes `CLAUDE_CODE_OAUTH_TOKEN` from every agent's environment, and `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` unless the agent's account takes its credentials from the environment, without reading or logging any of them.
  Claude Code prefers each of these to the account folder's login, so one left in the daemon's environment would run every account on it.
  To tell whether an account is signed in, it checks only that the credentials file exists, and when it was written.
- Choosing a folder in the app trusts it.
  `claude` runs without a terminal, so it shows no trust prompt: a repository's project MCP servers (`.mcp.json`) and its project settings start as soon as an agent opens the folder, with any account, even one that never opened it before.
  The app adds no confirmation, so pick only folders whose code you trust to run.

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
  - One button ends the composer. While an agent works or waits for you, it is Stop while the message is empty, and Send once something is written. Send (Enter) steers the agent: Claude reads the message at its next step. Alt+Enter queues it until the turn ends. For a second after the button sends, it stays an inactive Send, and the focus goes back to the text, so a double click or a key pressed next never stops the agent.
  - The composer shows the model and effort, the prompt cache timer and the permission mode.
  - Changed files open from a compact list, not a full side panel.
  - A read-only file view opens on the right and closes with a cross.
  - No cross-agent approvals inbox.
- Decided after round 2:
  - Usage is one chip in the header (plan and context percentages) that opens a detail card: plan windows, context by category, prompt cache, turns, cost at API prices.
  - Code blocks, inline diffs and the file view get syntax colors, picked from the block's language tag or the file's extension.
  - Scrollbars are thin and dark.
  - The center header stays on one line: the title shortens first, then the usage and files chips shrink to icons.
- Decided after round 3 (mocked in round 4):
  - An agent that finished while you looked elsewhere stays unread: bold title and a white dot until you open it.
  - Desktop notifications when an agent starts waiting for you, and when it finishes, while you work in another window.
  - Questions (`AskUserQuestion`) show one at a time behind tabs, with their options as a list and an "Other" row. 1 to 9 pick an option of the question on screen, Left and Right switch questions, and Enter goes to the next unanswered one, then sends. An answered question folds to one line. A click on the line shows every question, its answer and the options not picked.
  - Plans show as a card with "Yes, accept edits", "Yes, ask before edits" and "No, keep planning".
  - When the file view is closed, a right panel shows three blocks: the task list (`TodoWrite`), what runs in the background (subagents and commands, each with Stop) and the MCP servers (a failed one has Retry).
  - The sidebar row shows the task progress ("3/5").
  - Each tool call shows one line by default: its tool, its description or file, and its result in a word (a line count, `exit 1`, `+3 -1`). A click on the line, or its chevron, shows its input and output; Settings → Chat → Tool calls shows every call in full instead.
  - Subagent tool calls fold under the call that started them.
  - `file:line` references and links to files in agent text open the file view at that line; agent text renders as markdown (headings, lists, tables); bare web addresses are links, and an image of a file shows in the message.
  - "/" opens a palette of commands and skills, filtered as you type: names that start with the text first, then names with a word that starts with it (`/arch` finds `workdocs-archive`), then names that hold it, then descriptions that hold it from the start of a word. A "/" word in the middle of a message, which may be a path, matches only names that start with it.
  - Pasted screenshots show as thumbnails in the composer and in the sent message. A click on one shows it over the whole window.
  - Messages Claude has not read yet show in a list pinned above the composer: "After this turn" for a queued one, which has Send now and Cancel, and "Sent" for one `claude` has. Claude reads a sent one once its running calls end, so its row says "Sent · waits for Bash ↑ · 2 min 36 s", and a click on "Bash ↑" shows the call. While an approval, a question or a plan waits, it says "Sent · waits for your answer ↑", and with nothing running "Sent · read at the next step". A message this window sent shows its images as thumbnails that open them over the window; one sent from another window shows their count, since the daemon sends only that.
  - A running call that has a task (a command, even in the foreground) has a Stop button after its time: Claude gets the call as stopped, goes on, and reads the messages it has.
  - Command output collapses after 5 lines, and a message you sent after 8; a failed command has a red dot and its exit code.
  - Ctrl+N opens the new agent dialog (recent folders, or resume a past session); Ctrl+K opens the agent switcher; Ctrl+H, or a right-click on a sidebar row, marks an agent to handle, until you clear the mark or until you send it a message; Esc closes what is open (a menu, a dialog, an edit), and with nothing left open it stops a busy agent after a dialog, which Settings can turn off; Y and N answer an approval or a plan.
  - No rewind of file changes in v1.
- Decided after round 4 (mocked in round 5), replacing the right panel above:
  - Tasks, the messages sent, background work and MCP servers are floating blocks at the top right of the chat, each folding to one line. They show only when they have content, except MCP servers.
  - The Messages block lists the messages the user sent, oldest first, each with how long ago it was sent, then the prompts Claude has not read yet. It marks the latest message on screen, and a click on a message scrolls the chat to it. An eye in its head shows or hides the last message over the chat; the preferences keep the choice.
  - When the file view is open, the blocks become badges on the right edge of the chat (tasks "3/5", messages sent, running count, MCP); a click opens that block as a popover.
  - A failing MCP server shows only as a red dot on the folded block or badge, never as red text in the chat.
  - Each MCP server can be hidden from the view: it keeps running, but leaves the list and the red dot. A "Show them" link lists hidden servers.
  - Clicking a server opens its detail: status, version, command or URL, scope, error, three icon buttons with tooltips on its title row (Reconnect, "Disable for this agent", "Hide from my view"), its tools with read-only or destructive tags, filters and per-agent call counts, and "Remove from the config" for user, project and local scopes.
  - A background task stops with a small round icon button.
- Decided after v1, and built in milestone 5: the agent map, as in the Claude Code extension.
  - A composer button with a live dot (teal, the Working color, while a subagent runs, and amber while one waits for you) opens a map of the selected agent in a dialog, as Ctrl+M does: the agent at the root, every subagent it started under it, and subagents started by subagents under those.
  - The map is an indented outline, one row per node, with the selected node's details on the right.
  - Each row shows a status dot, the subagent's description, its type, a "background" tag where it applies, and on a second line its current tool call, what it waits for, its error, or "Stopped by you". Columns on the right give its tool calls, duration and token count, updated live while it runs. The root row's tokens are the total of its subagents'.
  - The map shows the current turn's subagents, and those of earlier turns that still run. The other earlier ones fold into one row that opens.
  - Many subagents stay readable three ways: a subtree with no running or failed subagent folds by itself; past eight siblings, the oldest ended ones that did not fail collapse into one row ("4 more done"); and a toggle shows only the running and failed ones, with the ones above them.
  - A subagent waiting for an approval or a question shows amber, as "Needs you" does, and its details show the approval card, answered in place: a click, or Y and N for an approval or a plan.
  - Clicking a node shows its details: the prompt it got, its transcript (the items nested under it), its tool calls, and Stop while it runs. Stop also stops the subagents under it.
  - Up and Down move between rows, Left and Right fold, Enter opens a folded row, Esc closes.
  - The "Running" block stays as the compact live view, with a link to open the map. A click on a subagent there opens the map with that node selected.
  - Data: the tree comes from `parent_tool_use_id` (each subagent's messages carry the id of the tool call that started it); tokens from `task_progress` (its `usage`: total tokens, tool uses, duration) and `task_notification`. The captured lines show that a subagent's call, its task events and its own lines share one tool call id. They do not show whether a foreground subagent gets task events, so the map does not need them: a subagent's calls, time and status come from its own lines and its call's result, and without task events it shows no tokens rather than a guess.
  - Protocol: `AgentDetails` gains a list of subagents (its call's item and its parent's, description, type, foreground or background, status with waiting for you, start and end time, tokens, tool calls, last tool call, error, turn, background task), filled by the daemon.
  - Stop: a subagent with a background task stops with `agent.stop_task`, and so do the running subagents under it. The control protocol has no stop for one foreground subagent without a task: its button stops the agent's turn, and says so.
- Decided with the quotes mockup, and built: reply to a passage of the chat.
  - Selecting text in one entry of the chat shows a Reply button above it; Reply or Ctrl+R opens a field for a comment on the passage. Enter adds the quote, Esc drops it.
  - The passage stays highlighted with a number until the message goes, and the composer lists each quote on one line.
  - While the chat has the strip on its right (no file view), each comment shows in a note beside its passage, below the floating blocks; a note that has no room falls back to its number. Hovering a number shows the passage and the comment. Each note can be hidden to its number, and the Settings dialog (a gear in the sidebar's foot) sets whether new comments start beside the text or hidden.
  - The message quotes each passage as a Markdown quote, followed by its comment, then the typed text.
  - Once sent, a quoted passage keeps a faint underline: hovering it shows "You replied" with a link to the message, and a quote in the message leads back to its passage.
- Decided with the accounts mockup, and built: several Claude accounts in the app.
  - From two accounts, every agent shows its account in the account's color, the default account's too: its first letter on its sidebar row, and a tag with its name after its status in the header.
  - The new agent dialog picks the machine, then the account, at its top, above its two tabs; the machine and the account are the last started agent's by default, and the folder never changes the account. Recent folders show their account, and the resume list shows the chosen account's sessions.
  - "Accounts" in the sidebar's foot lists the accounts, adds one in two steps (its name, color, folder and whether it uses the API key or token from the environment, then its sign-in command), renames, recolors and removes them.
  - The Plan chip and card show the selected agent's account and every account's plan.
  - An agent whose account is not signed in needs you, with a card in the chat that says how to sign in.
  - Not in scope: moving an agent to another account, and a filter by account in the sidebar.
- Decided with the projects mockups, and built: the sidebar groups agents by machine, by project, or not at all.
  - There is no grouping by account, since an account belongs to one machine.
  - Projects belong to the app, on each desktop: a name, a color, and rules, each a repository or a folder. An agent joins the project whose rule holds its folder most precisely: a folder, then its repository, then a folder that ends with `*`.
  - An agent in a repository no project claims gets a temporary project, shown dashed, which goes away with its last agent unless the user keeps it or moves an agent into it.
  - An agent moves to another project from its row's menu, by drag and drop, or from the new agent dialog's Project menu. Kept projects are reordered by dragging their headers.
  - A right-click on a project's header opens its menu: Keep (a temporary project), Rename (the name turns into a field in the header), the six colors (digits 1 to 6), Edit the rules (its card in Settings), Fold, and Delete, which asks for a second pick. A rename, a color or the rules keep a temporary project first.
  - The Settings dialog's Projects tab edits the projects, with a card per project as the Accounts dialog has.
- It starts `ssh <server> /absolute/path/to/agents-cockpit bridge` and speaks the daemon API over the process's stdin and stdout.
  Use an absolute path: a non-interactive SSH command does not load your interactive shell setup, so `~/.cargo/bin` may be missing from `PATH`.
- It reconnects after an SSH drop and sends its last sequence number.
- How the app reaches a daemon:
  - The app's config file, per machine, names its default daemon: a local socket, a host over SSH, or the daemon in the app (`transport = "in_app"`).
  - When the first connection fails at startup, the app shows a "No daemon answers" screen.
    It names the daemon it tried and why it failed, and offers Retry, "Run agents in this app", and "Start the background service here" (on Unix, where the app starts `agents-cockpit serve` as a process that outlives it).
  - A choice holds for this run only, so a laptop that usually reaches a server can run locally for a day while the server is down.
    With "Use this every time" ticked, the choice becomes the config's default.
  - The daemon in the app is the daemon's own code in the app's process, reached through an in-memory stream that carries the same protocol.
    When a background service already holds the state folder's lock, the app connects to its socket instead, and says so.
  - Closing the app stops its agents as a daemon's stop does, so their cut-off turns resume the next time the app runs them.
  - A connection that drops later reconnects, as above.
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
5. Agent map (built): the subagent list in `AgentDetails`, filled by the daemon, then the map dialog and its node details in the app.

### Follow-up pass after the v1 reviews

Data the mockups show but the protocol does not carry yet, and choices approved during the reviews:

- Queued prompts leave the transcript: they live in `AgentDetails.waiting_prompts`, which the app draws pinned above the composer. A prompt leaves the list and joins the transcript where Claude reads it, so each message sits where Claude read it. Done, with steering as the default delivery.
- Tool calls gain start and end times (the "exit 0, 18 s" on tool lines).
- An `Edit` gains the line where it applies, for line numbers in inline diffs.
- Recent folders carry their git branch; past sessions say where they started (a terminal, an editor); pasted images keep their file name.
- `Settings` gains the prompt cache TTL, so the app can change it.
- `cockpit_client` exports the default socket path, so the app and the daemon cannot disagree on it.
- A remote daemon with a non-default state folder: `Transport::Ssh` passes `--state-dir`.
- The folder of `agent.start` may start with `~`: the daemon expands it to the daemon user's home.
- Deny and "No, keep planning" get an optional message field in the app (the daemon already sends it).
- Syntax colors for Nix and TOML files.
- CLI commands for model, mode, effort, history and MCP servers.
- The daemon marks the end of a replay, and the app raises no notification for replayed events, so a reconnect does not bring back hours-old alerts.
- A file edit whose texts were dropped for size (over 1 MiB) says so, so the app shows "too large to show" instead of an empty diff.
- A tool input too large for one frame (a huge `Write`) is cut in its live event as it already is in history, and `settings.set` gets a size limit.

Found by the first end-to-end run of the app against a real daemon:

- A user message Claude Code writes itself carries an `origin` (`{"kind": "task-notification"}` when a background command ends). The transcript shows it as a short note ("Background command stopped: Wait 60 seconds"), not as a bubble of raw `<task-notification>` XML the user seems to have typed.
- A tool line shows its result ("Created, 1 line") only once the tool has run. Before that, while it waits for approval, the line says it waits.
- An interrupted turn shows one muted line. Today it shows the `[Request interrupted by user]` message and a red "The turn stopped after 6 s: Interrupted" under it.
- The background tasks panel shows its spinner and "Running" only while a task runs.
- The new agent dialog keeps the folder field in place when the recent folders arrive, which can take more than a second. With six recent folders, the Start button must stay visible in a 900 px high window.
- Single line breaks in Claude's answers show as line breaks, as on GitHub and claude.ai. Today a list written one item per line, without list markers, renders as one paragraph.

## Background

- herdr ([github.com/herdrdev/herdr](https://github.com/herdrdev/herdr)) runs Claude Code in its terminal interface inside a pseudo-terminal and reads the screen to detect status.
  Its public API returns screen snapshots, and its live stream (`herdr terminal session control`) sends ANSI bytes.
  That fits a terminal UI, not a CSS one, which is why agents-cockpit talks to Claude Code in JSON mode instead.
- agents-cockpit copies herdr's useful ideas: a daemon that outlives the client, an SSH stdin/stdout attach, and a snapshot followed by events with sequence numbers.
