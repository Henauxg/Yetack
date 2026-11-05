# Todos

Ideas and fixes noted for later. None of them is planned yet.

- **Pinned messages per agent.** Pin a message of an agent's chat. A "Pins" block in the right column lists that agent's pinned messages, and a click on one scrolls the chat to it.
- **Close sessions.** Close an agent's session from the app.
- **An emoji per session.** Choose an emoji that prefixes the agent's title, in the sidebar and the header.
- **Timing and progress on sidebar rows.** As the mockups showed, each row shows next to the agent's title how long ago it last did something (or how long its turn has run), and its task list's progress ("3/5", which today sits on the row's second line). The daemon must first send the time of the agent's last activity: `AgentSummary` has no such field yet (see the follow-up pass in `plan.md`).
