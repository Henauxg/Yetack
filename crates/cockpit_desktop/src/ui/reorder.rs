//! Dragging a sidebar row to another place in the list.
//!
//! The page's script follows the pointer: a lifted copy of the row moves with it, and a line
//! between two rows shows where the row will land. Rust hears of it once, on release, with the row the dragged one
//! goes above: a render per mouse move would make the drag lag. A press that moves less than
//! `THRESHOLD_PX` stays a click, which selects the agent.

use cockpit_protocol::AgentId;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::actions;

/// The sidebar's list of rows.
pub const LIST_ID: &str = "agent-list";

/// A row's id is this prefix and the agent's id.
pub const ROW_ID_PREFIX: &str = "agent-row-";

/// The class every row carries, which the script finds rows by.
pub const ROW_CLASS: &str = "agent-row";

/// How far the pointer moves before a press becomes a drag.
const THRESHOLD_PX: u32 = 4;

/// Near the list's top or bottom edge, the list scrolls while a row is dragged.
const EDGE_PX: u32 = 28;
const SCROLL_STEP_PX: u32 = 8;

/// Waits for its settings, then follows drags. Field names are serde's names for `Settings` and
/// `Dropped`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
const list = () => document.getElementById(settings.list_id);
const rowOf = (target) => target instanceof Element ? target.closest("." + settings.row_class) : null;
const agentOf = (row) => Number(row.id.slice(settings.row_id_prefix.length));
const rows = () => {
  const element = list();
  return element === null ? [] : Array.from(element.querySelectorAll("." + settings.row_class));
};
// A press on a row: it becomes a drag once the pointer moves far enough.
let press = null;
let drag = null;
let frame = 0;
// The pointer of the last drag: its release would click the row and select its agent.
let released = null;
let skipClick = false;
const follow = () => {
  frame = 0;
  if (drag === null) {
    return;
  }
  const box = drag.list.getBoundingClientRect();
  let scroll = 0;
  if (drag.y < box.top + settings.edge_px) {
    scroll = -settings.scroll_step_px;
  } else if (drag.y > box.bottom - settings.edge_px) {
    scroll = settings.scroll_step_px;
  }
  const scrolled = drag.list.scrollTop;
  drag.list.scrollTop += scroll;
  drag.ghost.style.top = `${Math.round(drag.ghostTop + drag.y - drag.startY)}px`;
  const all = rows();
  const others = all.filter((row) => row !== drag.row);
  let before = null;
  for (const row of others) {
    const rect = row.getBoundingClientRect();
    if (drag.y < rect.top + rect.height / 2) {
      before = row;
      break;
    }
  }
  const last = others[others.length - 1];
  const edge = before !== null ? before.getBoundingClientRect().top
    : last !== undefined ? last.getBoundingClientRect().bottom : box.top;
  // Where the row already is: no line, and nothing to send.
  const next = all[all.indexOf(drag.row) + 1] ?? null;
  drag.stays = before === next;
  drag.before = before === null ? null : agentOf(before);
  drag.line.style.display = drag.stays ? "none" : "block";
  drag.line.style.top = `${Math.round(edge - 1)}px`;
  drag.line.style.left = `${Math.round(box.left + 4)}px`;
  drag.line.style.width = `${Math.round(box.width - 8)}px`;
  if (drag.list.scrollTop !== scrolled) {
    frame = requestAnimationFrame(follow);
  }
};
const start = (event) => {
  const element = list();
  const { row, y: startY } = press;
  press = null;
  if (element === null || !row.isConnected) {
    return;
  }
  row.setPointerCapture(event.pointerId);
  const rect = row.getBoundingClientRect();
  // A copy of the row follows the pointer, outside the list, which would cut it.
  const ghost = row.cloneNode(true);
  ghost.removeAttribute("id");
  ghost.setAttribute("aria-hidden", "true");
  ghost.classList.add(settings.ghost_class);
  ghost.style.left = `${Math.round(rect.left)}px`;
  ghost.style.width = `${Math.round(rect.width)}px`;
  const line = document.createElement("div");
  line.className = settings.drop_line_class;
  document.body.append(line, ghost);
  drag = {
    pointer: event.pointerId,
    row,
    list: element,
    line,
    ghost,
    ghostTop: rect.top,
    startY,
    y: event.clientY,
    before: null,
    stays: true,
  };
  row.setAttribute(settings.dragged_attribute, "");
  document.documentElement.classList.add(settings.dragging_class);
  follow();
};
const stop = () => {
  cancelAnimationFrame(frame);
  frame = 0;
  drag.row.removeAttribute(settings.dragged_attribute);
  drag.line.remove();
  drag.ghost.remove();
  document.documentElement.classList.remove(settings.dragging_class);
  released = drag.pointer;
  drag = null;
};
document.addEventListener("pointerdown", (event) => {
  const row = rowOf(event.target);
  const editing = event.target instanceof Element && event.target.closest("input, textarea") !== null;
  if (row === null || editing || event.button !== 0 || drag !== null) {
    return;
  }
  press = { pointer: event.pointerId, row, x: event.clientX, y: event.clientY };
});
document.addEventListener("pointermove", (event) => {
  if (press !== null && event.pointerId === press.pointer) {
    if (Math.hypot(event.clientX - press.x, event.clientY - press.y) < settings.threshold_px) {
      return;
    }
    start(event);
  }
  if (drag === null || event.pointerId !== drag.pointer) {
    return;
  }
  drag.y = event.clientY;
  if (frame === 0) {
    frame = requestAnimationFrame(follow);
  }
});
const end = (event, dropped) => {
  if (press !== null && event.pointerId === press.pointer) {
    press = null;
  }
  if (drag !== null && event.pointerId === drag.pointer) {
    if (dropped) {
      drag.y = event.clientY;
      follow();
    }
    const { row, before, stays } = drag;
    stop();
    if (dropped && !stays) {
      dioxus.send({ agent: agentOf(row), before });
    }
  }
  if (released === event.pointerId) {
    released = null;
    skipClick = true;
    setTimeout(() => { skipClick = false; }, 0);
  }
};
document.addEventListener("pointerup", (event) => end(event, true));
document.addEventListener("pointercancel", (event) => end(event, false));
window.addEventListener("click", (event) => {
  if (skipClick) {
    skipClick = false;
    event.stopPropagation();
    event.preventDefault();
  }
}, true);
// Esc puts the row back. It reaches the page's other scripts only when no row is dragged.
window.addEventListener("keydown", (event) => {
  if (drag !== null && event.key === "Escape") {
    event.stopPropagation();
    event.preventDefault();
    stop();
  }
}, true);
await new Promise(() => {});
"#;

#[derive(Debug, Serialize)]
struct Settings {
    list_id: &'static str,
    row_class: &'static str,
    row_id_prefix: &'static str,
    /// On the dragged row, which stays in its place, dimmed.
    dragged_attribute: &'static str,
    /// On the lifted copy of the row that follows the pointer.
    ghost_class: &'static str,
    /// On the page's root while a row is dragged.
    dragging_class: &'static str,
    drop_line_class: &'static str,
    threshold_px: u32,
    edge_px: u32,
    scroll_step_px: u32,
}

impl Settings {
    fn new() -> Self {
        Self {
            list_id: LIST_ID,
            row_class: ROW_CLASS,
            row_id_prefix: ROW_ID_PREFIX,
            dragged_attribute: "data-dragged",
            ghost_class: "drag-ghost",
            dragging_class: "reordering",
            drop_line_class: "drop-line",
            threshold_px: THRESHOLD_PX,
            edge_px: EDGE_PX,
            scroll_step_px: SCROLL_STEP_PX,
        }
    }
}

/// A row was dropped just above the row of `before`, or below the last row when it is `None`.
#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Dropped {
    agent: AgentId,
    before: Option<AgentId>,
}

/// The id of the sidebar row that shows `agent`.
pub fn row_id(agent: AgentId) -> String {
    format!("{ROW_ID_PREFIX}{}", agent.get())
}

/// Runs the drag's script for the window's life. It finds the rows by their class, so it keeps
/// working as agents come and go.
pub fn use_reorder(cockpit: Cockpit) {
    use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(Settings::new()) {
            tracing::warn!("cannot start the sidebar's drag script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<Dropped>().await {
                    Ok(Dropped { agent, before }) => {
                        cockpit.change(|state| actions::drop_agent(state, agent, before));
                    }
                    Err(error) => {
                        tracing::warn!("the sidebar's drag script stopped: {error}");
                        return;
                    }
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The script reads and writes these fields by name: a rename in Rust would make it read
    /// `undefined`, with no error.
    #[test]
    fn the_fields_the_script_reads_and_writes_are_pinned() {
        assert_eq!(
            serde_json::to_value(Settings::new()).unwrap(),
            json!({
                "list_id": "agent-list",
                "row_class": "agent-row",
                "row_id_prefix": "agent-row-",
                "dragged_attribute": "data-dragged",
                "ghost_class": "drag-ghost",
                "dragging_class": "reordering",
                "drop_line_class": "drop-line",
                "threshold_px": 4,
                "edge_px": 28,
                "scroll_step_px": 8,
            })
        );
        let above: Dropped = serde_json::from_str(r#"{"agent":3,"before":5}"#).unwrap();
        let last: Dropped = serde_json::from_str(r#"{"agent":3,"before":null}"#).unwrap();
        assert_eq!(
            above,
            Dropped {
                agent: AgentId::new(3),
                before: Some(AgentId::new(5)),
            }
        );
        assert_eq!(last.before, None);
        assert_eq!(row_id(AgentId::new(12)), "agent-row-12");
    }
}
