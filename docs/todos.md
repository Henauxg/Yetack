# Todos

Ideas and fixes noted for later. None of them is planned yet.

- **Scheduled jobs.** An agent with a job scheduled through `/loop` or `CronCreate` shows as Idle, with nothing saying it will run again.
  - The daemon keeps each agent's active jobs from the `CronCreate` and `CronDelete` calls it already sees (job id, schedule, repeating or not, prompt), and computes the next run from the schedule rather than from Claude Code's sentence. A job leaves the list when it is deleted, when it expires (Claude Code drops a recurring job after 7 days) and when the `claude` process ends.
  - When a job fires, Claude Code sends no user message: it sends `{"type": "command_lifecycle", "state": "started", "wakeup_source": "schedule_wakeup"}`, then `"state": "completed"`. The daemon ignores that line today, so a scheduled run starts with no prompt above it. It becomes a "Scheduled run" marker in the transcript.
  - The app shows "Idle, next run 00:21" with a clock on the sidebar row and in the header, and a "Scheduled" block on the right that lists each job: its schedule, next run and prompt.
  - These jobs live only inside the `claude` process: stopping the agent, or restarting or updating the daemon, loses them. The daemon's update already refuses while an agent may hold a job. The app should also warn before a stop or a close that would lose one.
- **Voice control.** Talk to an agent instead of typing. Dictation tools that type into the focused field already work: Windows' Win+H was enough for the user, and Wispr Flow should behave the same. Next steps, if wanted: built-in push-to-talk with a local Whisper model (`whisper-rs`, audio through `cpal`), for machines without such a tool; then a few voice commands (allow, deny, stop, switch to an agent), risky ones confirmed.
- **Dioxus from crates.io again.** The app takes Dioxus 0.7.10 from the fork `Henauxg/dioxus-fork`, branch `desktop-without-tls-0.7.10`, whose `dioxus-desktop` builds its websockets without tungstenite's `native-tls`: that feature linked OpenSSL into the app though both websockets are plain TCP. The branch `desktop-without-tls` holds the same change on Dioxus's `main`, for a pull request, not opened yet. Once Dioxus has the change, take `dioxus` from crates.io again and drop the flake's `outputHashes` entry.
- **Git blame in the file view.** Show who last changed each line of the open file, in which commit and when, as editors do. The daemon runs `git blame` on the agent's machine. Lines not committed yet show as such.
- **Notes on an agent.** A short Markdown note per agent session, which the user writes and edits by hand: what the agent is for, what to check next. Open question: whether the agent can read the note too, or write to it.
- **Smaller fixes, not done:**
  - `settings.set` takes a value of any size. Give it a limit.
  - Undo after closing an agent brings it back under a new id, so it loses its files pane (open or closed) and its notification level. Carry both through the undo (`ui.closed`, `Reopening`).
  - The notification levels of a removed server's agents stay in `prefs.json` for good. Drop them in `machines::forget`, as its files panes are dropped.
  - With no agent selected on its machine, the new agent dialog fills its folder from the first recent folder before the past sessions arrive, so it can open on an older folder.
  - A background subagent's tool call may set an activity line on an idle agent's sidebar row.
  - The Ctrl+O dialog may stay open after its agent is removed.
  - On Windows, an agent without a Job Object (if Windows refused to make one) that runs an npm `claude.cmd` loses only `cmd.exe` when stopped; the real `claude` runs on to the end of its turn.
- **No shared ssh connection through a jump host.** The app's ssh passes `-o ControlPath=none`, so it never joins or hosts a connection shared with the user's own sessions. A `ProxyJump` hop runs a second ssh that does not get the app's `-o` options: through a jump host with `ControlMaster` on, that hop can still share a connection, and runs without `BatchMode`, `ClearAllForwardings` and the keepalives. Only `-F` reaches the hop. Fix when a user needs it: for a host whose config has a `ProxyJump` (`ssh -G <host>` tells), the app writes a private config file (mode 600) that starts with `ControlPath none`, then includes the user's config and the system's (`/etc/ssh/ssh_config`, or `%PROGRAMDATA%\ssh\ssh_config` on Windows, since `-F` skips both), and passes it with `-F`. Every other host keeps plain ssh.
- **Cloud sessions in the app.** Sessions on claude.ai/code cannot show in the app: there is no public API, and the terms forbid handling the user's claude.ai credentials. The supported route is `claude --teleport` into a local session, then "Resume a session". The user said not now.

## Not checked yet

Things nobody has run or seen yet.

- **Claude Code's sandbox and the daemon's socket.** A command an agent runs can pass the peer check by detaching on purpose (architecture.md, "Security"). Check whether Claude Code's sandbox can deny the agents' commands the state folder, its Unix socket included.
- **Several machines with a real server, from Windows.** Add a server in the Machines dialog, start an agent there, stop the server, and watch "Retry now". The user tests with WSL as the server, reached over SSH on localhost. The daemon there needs `claude` on the `PATH` of the terminal that runs `serve`: Claude Desktop does not provide it.
- **The find in WebKitGTK.** The find was checked on Windows only, in the demo. Its highlights need the CSS Custom Highlight API, which WebKitGTK has from 2.44 on.
- **An interrupted foreground subagent** may show "failed" rather than "Stopped by you". It depends on what Claude Code writes, and no capture shows it.
- **Windows.** The user has only started agents in the app there. Toasts (whether their threads end when Windows sends no answer), Job Objects on a real Windows, and `CREATE_NO_WINDOW` were checked by CI tests or not at all.
- **macOS** was never run. Its sign-in state is `unknown` (Keychain), and its leftover check records nothing (no `/proc`).
