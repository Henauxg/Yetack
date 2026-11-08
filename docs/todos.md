# Todos

Ideas and fixes noted for later. None of them is planned yet.

- **Pinned messages per agent.** Pin a message of an agent's chat. A "Pins" block in the right column lists that agent's pinned messages, and a click on one scrolls the chat to it.
- **Scheduled jobs.** An agent with a job scheduled through `/loop` or `CronCreate` shows as Idle, with nothing saying it will run again.
  - The daemon keeps each agent's active jobs from the `CronCreate` and `CronDelete` calls it already sees (job id, schedule, repeating or not, prompt), and computes the next run from the schedule rather than from Claude Code's sentence. A job leaves the list when it is deleted, when it expires (Claude Code drops a recurring job after 7 days) and when the `claude` process ends.
  - When a job fires, Claude Code sends no user message: it sends `{"type": "command_lifecycle", "state": "started", "wakeup_source": "schedule_wakeup"}`, then `"state": "completed"`. The daemon ignores that line today, so a scheduled run starts with no prompt above it. It becomes a "Scheduled run" marker in the transcript.
  - The app shows "Idle, next run 00:21" with a clock on the sidebar row and in the header, and a "Scheduled" block on the right that lists each job: its schedule, next run and prompt.
  - These jobs live only inside the `claude` process: stopping the agent, or restarting or updating the daemon, loses them. The app warns before a stop that would lose one, and the daemon's update procedure checks for scheduled jobs, not only for idle agents.
- **Voice control.** Talk to an agent instead of typing. Dictation tools that type into the focused field already work: Windows' Win+H was enough for the user, and Wispr Flow should behave the same. Next steps, if wanted: built-in push-to-talk with a local Whisper model (`whisper-rs`, audio through `cpal`), for machines without such a tool; then a few voice commands (allow, deny, stop, switch to an agent), risky ones confirmed.
- **The agent switch's second frame.** After the click's frame, drawing all of a long chat's rows keeps the webview busy for about 100 ms, and a click meanwhile waits. Add the older rows in chunks, and cache the syntax colors per item (about 25 ms of Rust work).
- **Details updates per subagent call.** Each update carries the whole subagent list (up to about 70 KB), and the event logs keep every one. Send at most one per agent every 250 to 500 ms, or a per-subagent event.
- **Smaller fixes noticed in reviews, not done:**
  - With the typed "/" palette open, Shift+Enter picks a command instead of adding a line.
  - The new agent dialog fills its folder from the first recent folder before the past sessions arrive, so it can open on an older folder.
  - A background subagent's tool call may set an activity line on an idle agent's sidebar row.
  - The Ctrl+O dialog may stay open after its agent is removed.
  - On Windows, an agent without a Job Object (if Windows refused to make one) that runs an npm `claude.cmd` loses only `cmd.exe` when stopped; the real `claude` runs on to the end of its turn.
  - Credential variables other than the three the daemon handles (Bedrock, Vertex, a custom base URL) pass to every account's agents.
  - The daemon sends no event when an account signs in: the app asks again with "Check again".
- **Machine load sparklines.** On each sidebar row, a 52 by 18 pixel line of the agent's CPU over the last five minutes: teal while it works, amber past a quarter of the server's cores, grey when idle. The row's second line shows its cores now, and a tooltip its cores over five minutes, its memory and its heaviest command. `machine.load` already answers each agent's five-minute history.
