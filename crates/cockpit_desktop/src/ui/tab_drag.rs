//! Dragging a tab along its row, or a terminal's tab to the other dock.
//!
//! The page's script follows the pointer: a lifted copy of the tab moves with it, and a line
//! between two tabs shows where the tab will land. While a tab is dragged, a closed dock shows an
//! area to drop it on, which opens the dock. Rust hears of it once, on release. A pinned tab
//! lands among the pinned tabs, and another tab after them. A press that moves less than
//! `THRESHOLD_PX` stays a click, which shows the tab.

use cockpit_protocol::AgentId;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::docks::{Dock, TabKey};
use crate::state::{actions, terminals};
use crate::ui::terminal_pane::TERMINAL_NAME_FIELD_ID;

/// The attribute a row of tabs carries, which names its dock.
pub const DOCK_ATTRIBUTE: &str = "data-dock";

/// The attribute a tab that can be dragged carries: its key, as [`TabKey::to_page`] writes it.
pub const TAB_ATTRIBUTE: &str = "data-tab";

/// On a pinned tab.
pub const PINNED_ATTRIBUTE: &str = "data-pinned";

/// On the area a closed dock shows while a tab is dragged. It carries the dock's attribute too.
pub const ZONE_ATTRIBUTE: &str = "data-dock-zone";

/// How far the pointer moves before a press becomes a drag.
const THRESHOLD_PX: u32 = 4;

/// Near a row's left or right edge, the row scrolls while a tab is dragged.
const EDGE_PX: u32 = 28;
const SCROLL_STEP_PX: u32 = 8;

/// How far above or below a row the pointer may be and still drop on it.
const ROW_REACH_PX: u32 = 24;

/// Waits for its settings, then follows drags. Field names are serde's names for `Settings` and
/// `Dropped`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
const tabOf = (target) => target instanceof Element ? target.closest(`[${settings.tab_attribute}]`) : null;
const rowOf = (tab) => tab.closest(`[${settings.dock_attribute}]`);
const keyOf = (tab) => tab.getAttribute(settings.tab_attribute);
const pinned = (tab) => tab.hasAttribute(settings.pinned_attribute);
let press = null;
let drag = null;
let frame = 0;
let released = null;
let skipClick = false;
// The row or the closed dock's drop area the pointer is over, or null.
const rowAt = (x, y) => Array.from(document.querySelectorAll(`[${settings.dock_attribute}]`)).find((row) => {
  const box = row.getBoundingClientRect();
  return box.width > 0 && x >= box.left && x <= box.right
    && y >= box.top - settings.row_reach_px && y <= box.bottom + settings.row_reach_px;
}) ?? null;
const follow = () => {
  frame = 0;
  if (drag === null) {
    return;
  }
  drag.ghost.style.left = `${Math.round(drag.x - drag.grabX)}px`;
  drag.ghost.style.top = `${Math.round(drag.y - drag.grabY)}px`;
  const row = rowAt(drag.x, drag.y);
  drag.target = null;
  for (const zone of document.querySelectorAll(`[${settings.zone_attribute}]`)) {
    zone.classList.toggle(settings.zone_target_class, zone === row);
  }
  if (row === null) {
    drag.line.style.display = "none";
    return;
  }
  if (row.hasAttribute(settings.zone_attribute)) {
    drag.line.style.display = "none";
    drag.target = { dock: row.getAttribute(settings.dock_attribute), before: null };
    return;
  }
  const box = row.getBoundingClientRect();
  let scroll = 0;
  if (drag.x < box.left + settings.edge_px) {
    scroll = -settings.scroll_step_px;
  } else if (drag.x > box.right - settings.edge_px) {
    scroll = settings.scroll_step_px;
  }
  const scrolled = row.scrollLeft;
  row.scrollLeft += scroll;
  const all = Array.from(row.querySelectorAll(`[${settings.tab_attribute}]`));
  const others = all.filter((tab) => tab !== drag.tab);
  // A pinned tab lands among the pinned ones, another tab after them.
  const group = others.filter((tab) => pinned(tab) === drag.pinned);
  let before = group.find((tab) => {
    const rect = tab.getBoundingClientRect();
    return drag.x < rect.left + rect.width / 2;
  }) ?? null;
  const last = group[group.length - 1];
  // The end of the pinned tabs is the place before the first other tab.
  const after = drag.pinned ? (others.find((tab) => !pinned(tab)) ?? null) : null;
  const edge = before !== null ? before.getBoundingClientRect().left
    : last !== undefined ? last.getBoundingClientRect().right
    : after !== null ? after.getBoundingClientRect().left
    : box.left;
  if (before === null) {
    before = after;
  }
  const home = all.includes(drag.tab);
  const next = home ? (all[all.indexOf(drag.tab) + 1] ?? null) : undefined;
  const stays = before === next;
  drag.target = stays ? null : { dock: row.getAttribute(settings.dock_attribute), before: before === null ? null : keyOf(before) };
  drag.line.style.display = stays ? "none" : "block";
  drag.line.style.left = `${Math.round(edge - 1)}px`;
  drag.line.style.top = `${Math.round(box.top + 4)}px`;
  drag.line.style.height = `${Math.round(box.height - 8)}px`;
  if (row.scrollLeft !== scrolled) {
    frame = requestAnimationFrame(follow);
  }
};
const start = (event) => {
  const { tab, x, y } = press;
  press = null;
  if (!tab.isConnected) {
    return;
  }
  tab.setPointerCapture(event.pointerId);
  const rect = tab.getBoundingClientRect();
  const ghost = tab.cloneNode(true);
  ghost.removeAttribute("id");
  ghost.querySelectorAll("[id]").forEach((element) => element.removeAttribute("id"));
  ghost.setAttribute("aria-hidden", "true");
  ghost.classList.add(settings.ghost_class);
  ghost.style.width = `${Math.round(rect.width)}px`;
  ghost.style.height = `${Math.round(rect.height)}px`;
  const line = document.createElement("div");
  line.className = settings.drop_line_class;
  document.body.append(line, ghost);
  drag = {
    pointer: event.pointerId,
    tab,
    pinned: pinned(tab),
    line,
    ghost,
    grabX: x - rect.left,
    grabY: y - rect.top,
    x: event.clientX,
    y: event.clientY,
    target: null,
  };
  tab.setAttribute(settings.dragged_attribute, "");
  document.documentElement.classList.add(settings.dragging_class);
  follow();
};
const stop = () => {
  cancelAnimationFrame(frame);
  frame = 0;
  for (const zone of document.querySelectorAll(`[${settings.zone_attribute}]`)) {
    zone.classList.remove(settings.zone_target_class);
  }
  drag.tab.removeAttribute(settings.dragged_attribute);
  drag.line.remove();
  drag.ghost.remove();
  document.documentElement.classList.remove(settings.dragging_class);
  released = drag.pointer;
  drag = null;
};
document.addEventListener("pointerdown", (event) => {
  const tab = tabOf(event.target);
  // A press on the tab's close or pin button stays a press on that button, and one in its name
  // field selects text.
  const onButton = event.target instanceof Element && event.target.closest(settings.button_selector) !== null;
  if (tab === null || onButton || event.button !== 0 || drag !== null) {
    return;
  }
  press = { pointer: event.pointerId, tab, x: event.clientX, y: event.clientY };
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
  drag.x = event.clientX;
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
      drag.x = event.clientX;
      drag.y = event.clientY;
      follow();
    }
    const { tab, target } = drag;
    const agent = Number(rowOf(tab).getAttribute(settings.agent_attribute));
    stop();
    if (dropped && target !== null) {
      dioxus.send({
        agent,
        tab: keyOf(tab),
        dock: target.dock,
        before: target.before,
      });
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
// Esc puts the tab back. It reaches the page's other scripts only when no tab is dragged.
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
    dock_attribute: &'static str,
    /// On a row of tabs: the agent its tabs belong to.
    agent_attribute: &'static str,
    tab_attribute: &'static str,
    pinned_attribute: &'static str,
    zone_attribute: &'static str,
    /// On the drop area under the pointer.
    zone_target_class: &'static str,
    /// The tab's buttons and field, which a drag does not start from.
    button_selector: String,
    /// On the dragged tab, which stays in its place, dimmed.
    dragged_attribute: &'static str,
    /// On the lifted copy of the tab that follows the pointer.
    ghost_class: &'static str,
    /// On the page's root while a tab is dragged.
    dragging_class: &'static str,
    drop_line_class: &'static str,
    threshold_px: u32,
    edge_px: u32,
    scroll_step_px: u32,
    row_reach_px: u32,
}

impl Settings {
    fn new() -> Self {
        Self {
            dock_attribute: DOCK_ATTRIBUTE,
            agent_attribute: "data-agent",
            tab_attribute: TAB_ATTRIBUTE,
            pinned_attribute: PINNED_ATTRIBUTE,
            zone_attribute: ZONE_ATTRIBUTE,
            zone_target_class: "drop-target",
            button_selector: format!(".file-tab-close, #{TERMINAL_NAME_FIELD_ID}"),
            dragged_attribute: "data-dragged",
            ghost_class: "tab-ghost",
            dragging_class: "dragging-tab",
            drop_line_class: "tab-drop-line",
            threshold_px: THRESHOLD_PX,
            edge_px: EDGE_PX,
            scroll_step_px: SCROLL_STEP_PX,
            row_reach_px: ROW_REACH_PX,
        }
    }
}

/// A tab was dropped in the row of `dock`, just before the tab `before`, or at the end when it
/// is `None`. Tabs are named by their keys on the page.
#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Dropped {
    agent: u32,
    tab: String,
    dock: Dock,
    before: Option<String>,
}

/// Runs the drag's script for the window's life. It finds the tabs by their attribute, so it
/// keeps working as tabs come and go.
pub fn use_tab_drag(cockpit: Cockpit) {
    use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(Settings::new()) {
            tracing::warn!("cannot start the tabs' drag script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<Dropped>().await {
                    Ok(dropped) => drop_tab(cockpit, &dropped),
                    Err(error) => {
                        tracing::warn!("the tabs' drag script stopped: {error}");
                        return;
                    }
                }
            }
        });
    });
}

fn drop_tab(cockpit: Cockpit, dropped: &Dropped) {
    let Some(tab) = TabKey::from_page(&dropped.tab) else {
        return;
    };
    let before = dropped.before.as_deref().and_then(TabKey::from_page);
    let agent = AgentId::new(dropped.agent);
    cockpit.update(|state| {
        // Another agent shows now: the tab is not its own.
        if state.ui.selected != Some(agent) {
            return;
        }
        match &tab {
            TabKey::Terminal(terminal) => {
                terminals::move_terminal(state, agent, *terminal, dropped.dock, before.as_ref());
            }
            TabKey::File(path) => {
                actions::move_file(state, agent, path, dropped.dock, before.as_ref());
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
            serde_json::to_value(Settings::new()).unwrap(),
            json!({
                "dock_attribute": "data-dock",
                "agent_attribute": "data-agent",
                "tab_attribute": "data-tab",
                "pinned_attribute": "data-pinned",
                "zone_attribute": "data-dock-zone",
                "zone_target_class": "drop-target",
                "button_selector": ".file-tab-close, #terminal-name-field",
                "dragged_attribute": "data-dragged",
                "ghost_class": "tab-ghost",
                "dragging_class": "dragging-tab",
                "drop_line_class": "tab-drop-line",
                "threshold_px": 4,
                "edge_px": 28,
                "scroll_step_px": 8,
                "row_reach_px": 24,
            })
        );
        let dropped: Dropped =
            serde_json::from_str(r#"{"agent":3,"tab":"file:a.rs","dock":"side","before":null}"#)
                .unwrap();
        assert_eq!(
            dropped,
            Dropped {
                agent: 3,
                tab: "file:a.rs".to_owned(),
                dock: Dock::Side,
                before: None,
            }
        );
    }
}
