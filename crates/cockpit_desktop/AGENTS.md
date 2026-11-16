# cockpit_desktop

The desktop app: components in `src/ui/`, pure state in `src/state/`, one style sheet in `src/style.css`.
These rules add to the root [AGENTS.md](../../AGENTS.md).

## Interaction and motion

Nothing on screen jumps.
Every control answers the pointer and the keys, and every change of size, place or presence animates.
Check each new or changed element against these rules before you call the work done.

### Feedback

- Hover changes a control's color, background or border, over `--motion-hover`.
- Press scales a control to `--press-scale` (a button, a chip) or darkens it to `--pressed` (a row).
- Keyboard focus shows a ring in `--focus-ring`. A row in a scrolling list draws it inside (`outline-offset: -2px`), since the list would cut a ring drawn outside.
- A list the keys move through has one highlight, which the pointer moves too: no CSS hover of its own there (see "One highlight in a list" in docs/architecture.md).

### Size, place and presence

- What appears or grows takes `--motion-in` with `--ease-out`. What leaves or shrinks takes `--motion-out` with `--ease-in`.
- What an element pushes aside moves with it, with the same duration and easing, so both arrive together.
  For example, the MCP card widens from 272 to 380 px for a server's detail, and in the same 140 ms the chat's right padding, the composer, the last instruction bar and the jump button move left by as much.
- Put the transition on the property that changes (`width`, `padding-right`, `right`), in both states: the state an element enters sets the timing of the way in.
- Dioxus removes an element at once. An element that closes goes through `ui/presence.rs` (`Presence` or `use_presence`), which keeps it on screen, marked `leaving` and inert, for its exit animation.
- A switch to another agent draws that agent's layout at once. The root has the `switching` class in the frame that shows the other agent, and its rule turns the layout transitions off: a new layout transition adds its element there.
  An element of one agent that opens with an animation, such as the chat's find, uses `use_agent_presence` (`ui/presence.rs`) instead: its `instant` class turns the animation off while the element stays open. The `switching` class lasts one frame, and an animation starts as it goes.

### Tokens

- Write no duration or easing in a rule: take a token from the style sheet's Motion section, or add one there.
- The reduced-motion block sets the tokens to 0, so a rule that uses them follows it. A looping animation needs its own line in that block, as the spinner's `--motion-spin` and the skeleton's `animation: none` have.
- Animate `transform` and `opacity` where they can do the job. A layout property (`width`, `padding`) lays out the chat again on every frame of its animation.

### Checking it on screen

A 140 ms animation is over before a screenshot is taken.
To see one, build with `--motion-in` and `--motion-out` set to 1500ms, capture a frame halfway, then put the values back before you commit.
An exit still lasts `EXIT` in `ui/presence.rs` (120 ms), whatever the tokens say.
