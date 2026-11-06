# Todos

Ideas and fixes noted for later. None of them is planned yet.

- **Pinned messages per agent.** Pin a message of an agent's chat. A "Pins" block in the right column lists that agent's pinned messages, and a click on one scrolls the chat to it.
- **Scheduled jobs.** An agent with a job scheduled through `/loop` or `CronCreate` shows as Idle, with nothing saying it will run again.
  - The daemon keeps each agent's active jobs from the `CronCreate` and `CronDelete` calls it already sees (job id, schedule, repeating or not, prompt), and computes the next run from the schedule rather than from Claude Code's sentence. A job leaves the list when it is deleted, when it expires (Claude Code drops a recurring job after 7 days) and when the `claude` process ends.
  - When a job fires, Claude Code sends no user message: it sends `{"type": "command_lifecycle", "state": "started", "wakeup_source": "schedule_wakeup"}`, then `"state": "completed"`. The daemon ignores that line today, so a scheduled run starts with no prompt above it. It becomes a "Scheduled run" marker in the transcript.
  - The app shows "Idle, next run 00:21" with a clock on the sidebar row and in the header, and a "Scheduled" block on the right that lists each job: its schedule, next run and prompt.
  - These jobs live only inside the `claude` process: stopping the agent, or restarting or updating the daemon, loses them. The app warns before a stop that would lose one, and the daemon's update procedure checks for scheduled jobs, not only for idle agents.
- **Voice control.** Talk to an agent instead of typing: dictate a message into the composer, and possibly answer an approval or pick an agent by voice.
- **Agents from several Claude accounts, in the app.** The daemon and the protocol run agents with several accounts (architecture.md, "Accounts"). The app's screens wait for an approved mockup: the account list with each one's sign-in state and command, adding, renaming and removing an account, the account of a new agent and of a past session, which account an agent uses, and each account's plan usage.
- **Machine load sparklines.** On each sidebar row, a 52 by 18 pixel line of the agent's CPU over the last five minutes: teal while it works, amber past a quarter of the server's cores, grey when idle. The row's second line shows its cores now, and a tooltip its cores over five minutes, its memory and its heaviest command. `machine.load` already answers each agent's five-minute history.
