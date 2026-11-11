//! Dragging a sidebar row, or a floating block by its head, to another place in its list.
//!
//! The page's script follows the pointer: a lifted copy of the row moves with it, and a line
//! between two rows shows where the row will land. Rust hears of it once, on release, with the
//! row the dragged one goes above: a render per mouse move would make the drag lag. A press that
//! moves less than `THRESHOLD_PX` stays a click, which selects the agent or folds the block.

use cockpit_protocol::AgentId;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::state::{Block, actions};
use crate::ui::blocks::{BLOCK_CARD_CLASS, BLOCK_HEAD_CLASS, BLOCKS_LIST_ID};

/// The sidebar's list of rows.
pub const LIST_ID: &str = "agent-list";

/// A row's id is this prefix and the agent's id.
pub const ROW_ID_PREFIX: &str = "agent-row-";

/// The class every row carries, which the script finds rows by.
pub const ROW_CLASS: &str = "agent-row";

/// A block card's id is this prefix and its place among the blocks' variants.
pub const BLOCK_ID_PREFIX: &str = "block-";

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
const itemOf = (row) => Number(row.id.slice(settings.row_id_prefix.length));
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
  drag.before = before === null ? null : itemOf(before);
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
  // No id may show twice while the copy is on the page.
  ghost.removeAttribute("id");
  ghost.querySelectorAll("[id]").forEach((element) => element.removeAttribute("id"));
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
  // With a handle, only a press on it starts a drag.
  const handled = settings.handle_class === null
    || (event.target instanceof Element && event.target.closest("." + settings.handle_class) !== null);
  if (row === null || editing || !handled || event.button !== 0 || drag !== null) {
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
      dioxus.send({ item: itemOf(row), before });
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
    /// The part of a row a drag starts from, or `None` for the whole row.
    handle_class: Option<&'static str>,
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
    /// The sidebar's agent rows, dragged from anywhere on them.
    fn sidebar() -> Self {
        Self::of(LIST_ID, ROW_CLASS, ROW_ID_PREFIX, None)
    }

    /// The floating blocks, dragged by their heads: their bodies hold buttons and text.
    fn blocks() -> Self {
        Self::of(
            BLOCKS_LIST_ID,
            BLOCK_CARD_CLASS,
            BLOCK_ID_PREFIX,
            Some(BLOCK_HEAD_CLASS),
        )
    }

    fn of(
        list_id: &'static str,
        row_class: &'static str,
        row_id_prefix: &'static str,
        handle_class: Option<&'static str>,
    ) -> Self {
        Self {
            list_id,
            row_class,
            row_id_prefix,
            handle_class,
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
/// Each names a row by the number that ends its id.
#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Dropped {
    item: u32,
    before: Option<u32>,
}

/// The id of the sidebar row that shows `agent`.
pub fn row_id(agent: AgentId) -> String {
    format!("{ROW_ID_PREFIX}{}", agent.get())
}

/// The id of `block`'s card: its place among the blocks' variants.
pub fn block_id(block: Block) -> String {
    let index = Block::VARIANTS
        .iter()
        .position(|variant| *variant == block)
        .unwrap_or_default();
    format!("{BLOCK_ID_PREFIX}{index}")
}

fn block_of(index: u32) -> Option<Block> {
    Block::VARIANTS.get(usize::try_from(index).ok()?).copied()
}

/// Runs the drag's script for the window's life, once for the sidebar and once for the floating
/// blocks. It finds the rows by their class, so it keeps working as rows come and go.
pub fn use_reorder(cockpit: Cockpit) {
    use_hook(|| {
        follow_drops(Settings::sidebar(), "sidebar", move |dropped| {
            let agent = AgentId::new(dropped.item);
            let before = dropped.before.map(AgentId::new);
            cockpit.change(|state| actions::drop_agent(state, agent, before));
        });
        follow_drops(Settings::blocks(), "blocks", move |dropped| {
            let Some(block) = block_of(dropped.item) else {
                return;
            };
            let before = dropped.before.and_then(block_of);
            cockpit.update(|state| actions::drop_block(state, block, before));
        });
    });
}

/// Starts a copy of the script with `settings`, and hands its drops to `dropped`.
fn follow_drops(settings: Settings, list: &'static str, dropped: impl Fn(Dropped) + 'static) {
    let mut script = document::eval(SCRIPT);
    if let Err(error) = script.send(settings) {
        tracing::warn!("cannot start the {list}' drag script: {error}");
    }
    spawn(async move {
        loop {
            match script.recv::<Dropped>().await {
                Ok(drop) => dropped(drop),
                Err(error) => {
                    tracing::warn!("the {list}' drag script stopped: {error}");
                    return;
                }
            }
        }
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
            serde_json::to_value(Settings::sidebar()).unwrap(),
            json!({
                "list_id": "agent-list",
                "row_class": "agent-row",
                "row_id_prefix": "agent-row-",
                "handle_class": null,
                "dragged_attribute": "data-dragged",
                "ghost_class": "drag-ghost",
                "dragging_class": "reordering",
                "drop_line_class": "drop-line",
                "threshold_px": 4,
                "edge_px": 28,
                "scroll_step_px": 8,
            })
        );
        let blocks = serde_json::to_value(Settings::blocks()).unwrap();
        assert_eq!(blocks["list_id"], "floating-blocks");
        assert_eq!(blocks["row_class"], "block-card");
        assert_eq!(blocks["row_id_prefix"], "block-");
        assert_eq!(blocks["handle_class"], "block-head");
        let above: Dropped = serde_json::from_str(r#"{"item":3,"before":5}"#).unwrap();
        let last: Dropped = serde_json::from_str(r#"{"item":3,"before":null}"#).unwrap();
        assert_eq!(
            above,
            Dropped {
                item: 3,
                before: Some(5),
            }
        );
        assert_eq!(last.before, None);
        assert_eq!(row_id(AgentId::new(12)), "agent-row-12");
    }

    #[test]
    fn a_blocks_card_id_names_it_back() {
        for block in Block::VARIANTS {
            let id = block_id(*block);
            let index = id
                .strip_prefix(BLOCK_ID_PREFIX)
                .and_then(|index| index.parse().ok());
            assert_eq!(index.and_then(block_of), Some(*block), "{id}");
        }
        assert_eq!(block_of(99), None);
    }
}
