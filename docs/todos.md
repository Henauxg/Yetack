# Todos

Ideas and fixes noted for later. None of them is planned yet.

- **Pinned messages per agent.** Pin a message of an agent's chat. A "Pins" block in the right column lists that agent's pinned messages, and a click on one scrolls the chat to it.
- **Timing and progress on sidebar rows.** As the mockups showed, each row shows next to the agent's title how long ago it last did something (or how long its turn has run), and its task list's progress ("3/5", which today sits on the row's second line). The daemon must first send the time of the agent's last activity: `AgentSummary` has no such field yet (see the follow-up pass in `plan.md`).
- **Scheduled jobs.** An agent with a job scheduled through `/loop` or `CronCreate` shows as Idle, with nothing saying it will run again.
  - The daemon keeps each agent's active jobs from the `CronCreate` and `CronDelete` calls it already sees (job id, schedule, repeating or not, prompt), and computes the next run from the schedule rather than from Claude Code's sentence. A job leaves the list when it is deleted, when it expires (Claude Code drops a recurring job after 7 days) and when the `claude` process ends.
  - When a job fires, Claude Code sends no user message: it sends `{"type": "command_lifecycle", "state": "started", "wakeup_source": "schedule_wakeup"}`, then `"state": "completed"`. The daemon ignores that line today, so a scheduled run starts with no prompt above it. It becomes a "Scheduled run" marker in the transcript.
  - The app shows "Idle, next run 00:21" with a clock on the sidebar row and in the header, and a "Scheduled" block on the right that lists each job: its schedule, next run and prompt.
  - These jobs live only inside the `claude` process: stopping the agent, or restarting or updating the daemon, loses them. The app warns before a stop that would lose one, and the daemon's update procedure checks for scheduled jobs, not only for idle agents.
- **Voice control.** Talk to an agent instead of typing: dictate a message into the composer, and possibly answer an approval or pick an agent by voice.
- **Agents from several Claude accounts.** Run agents signed in to different Claude accounts from one app, for example a personal and a work subscription. Each account has its own Claude Code config folder (its own login and settings), and the app shows which account an agent uses.
- **Machine load per agent.** Show how much of the server each agent uses: CPU and memory of its `claude` process and of the commands it runs (builds, tests, servers), summed over its process group, which the daemon already keeps per agent. A small figure on each sidebar row or in the header, with the server's total load beside it, so a heavy agent stands out when the machine is slow.
