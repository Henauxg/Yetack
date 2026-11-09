//! The chat's scrolling, kept by a page script. At the bottom, the chat follows new entries.
//! Scrolled up, the entry in view stays where it is, whatever changes above or below it.
//!
//! The chat's box lays its column out from the bottom (`column-reverse`), so it keeps its
//! distance from the bottom: an entry added below would push the view up by its height.
//! Browsers that implement CSS scroll anchoring (`overflow-anchor`) correct that; WebKitGTK does
//! not, so the script does it. It tells the app only when the chat reaches or leaves its bottom.

use cockpit_protocol::{AgentId, ItemId};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::actions;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::splitter::FILE_VIEW_ID;

/// The chat's scroll box.
pub const CHAT_SCROLL_ID: &str = "chat-scroll";

/// The button that jumps to the latest entry. The script handles its clicks.
pub const JUMP_BUTTON_ID: &str = "jump-to-latest";

/// The wrapper of each entry, which the script anchors on. It lays nothing out of its own.
pub const ENTRY_CLASS: &str = "transcript-entry";

/// The class that highlights an entry the chat scrolled to, for as long as its animation runs.
const REVEALED_CLASS: &str = "chat-revealed";

/// How close to the bottom, in pixels, the chat still follows new entries.
const BOTTOM_THRESHOLD_PX: u16 = 40;

/// The most new entries the jump button's badge counts; past it, it shows "99+".
const MAX_BADGE_COUNT: usize = 99;

/// Waits for its settings, then keeps the chat's place. Message and field names are serde's names
/// for `Settings`, `ScrollCommand` and `ScrollMessage`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
let box = null;
let column = null;
let agent = null;
// At the bottom, the view follows new entries; scrolled up, it keeps the first entry in view,
// `anchor`, at the same height.
let following = true;
let anchor = null;
let reported = "";
// The scroll position the script set last. Its own scroll leaves the anchor as it was: measured
// again, the anchor would drift by the rounding of each correction.
let corrected = null;
// Each agent's place, kept while another agent shows.
const places = new Map();
const boxTop = () => box.getBoundingClientRect().top;
const gapBelow = () => column.getBoundingClientRect().bottom - box.getBoundingClientRect().bottom;
// An entry's wrapper has no box of its own: its last element shows the entry, after the "New"
// divider when it has one.
const shown = (entry) => entry.lastElementChild;
const firstInView = () => {
  const entries = column.getElementsByClassName(settings.entry_class);
  const top = boxTop();
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    const element = shown(entries[middle]);
    if (element !== null && element.getBoundingClientRect().bottom <= top) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  const entry = entries[low];
  const element = entry === undefined ? null : shown(entry);
  return element === null ? null : { id: entry.id, top: element.getBoundingClientRect().top - top };
};
const report = () => {
  const now = `${agent} ${following}`;
  if (agent === null || now === reported) {
    return;
  }
  reported = now;
  dioxus.send({ type: "at_bottom", agent: Number(agent), at_bottom: following });
};
// After a scroll by the user: where the view now is.
const record = () => {
  if (corrected !== null && box.scrollTop === corrected) {
    corrected = null;
    return;
  }
  corrected = null;
  following = gapBelow() <= settings.bottom_threshold_px;
  anchor = following ? null : firstInView();
  report();
};
// After the content changed: back to the bottom, or the anchor back at its height.
const settle = () => {
  let delta = gapBelow();
  if (!following) {
    const entry = anchor === null ? null : document.getElementById(anchor.id);
    const element = entry === null ? null : shown(entry);
    if (element === null) {
      anchor = firstInView();
      return;
    }
    delta = element.getBoundingClientRect().top - boxTop() - anchor.top;
  }
  if (Math.abs(delta) >= 1) {
    box.scrollTop += delta;
    corrected = box.scrollTop;
  }
};
// Another agent's entries replace the chat's: keep the place of the one that leaves, and go back
// to the place of the one that comes, or to its bottom. A chat that draws only some of its
// agent's entries names no agent, so the place waits for all of them.
const changed = () => {
  if (box === null) {
    return;
  }
  const shownAgent = box.dataset.agent ?? null;
  if (shownAgent !== agent) {
    if (agent !== null) {
      places.set(agent, { following, anchor });
    }
    agent = shownAgent;
    const place = places.get(agent);
    const kept = place !== undefined && !place.following && place.anchor !== null
      && document.getElementById(place.anchor.id) !== null;
    following = !kept;
    anchor = kept ? place.anchor : null;
  }
  settle();
  report();
};
const mutations = new MutationObserver(changed);
const resizes = new ResizeObserver(changed);
// The chat's box comes and goes with the selected agent.
const attach = () => {
  const found = document.getElementById(settings.chat_scroll_id);
  if (found === box) {
    return;
  }
  mutations.disconnect();
  resizes.disconnect();
  box = found;
  column = box === null ? null : box.firstElementChild;
  if (box === null || column === null) {
    box = null;
    return;
  }
  box.addEventListener("scroll", record, { passive: true });
  mutations.observe(box, { childList: true, subtree: true, characterData: true });
  resizes.observe(column);
  resizes.observe(box);
  changed();
};
new MutationObserver(attach).observe(document.body, { childList: true, subtree: true });
attach();
const jump = () => {
  if (box === null) {
    return;
  }
  following = true;
  anchor = null;
  settle();
  report();
};
// Scrolls an entry to the middle of the chat, or its top to the chat's top when it is taller, and
// highlights it for a moment. The view then keeps it, as after a scroll by the user.
const reveal = (entryId) => {
  const entry = document.getElementById(entryId);
  const element = entry === null ? null : shown(entry);
  if (box === null || element === null) {
    return;
  }
  const height = element.getBoundingClientRect().height;
  const above = Math.max(0, (box.clientHeight - height) / 2);
  corrected = null;
  box.scrollTop += element.getBoundingClientRect().top - boxTop() - above;
  record();
  element.classList.remove(settings.revealed_class);
  // Restarts the animation when the same entry is revealed again.
  void element.offsetWidth;
  element.classList.add(settings.revealed_class);
  element.addEventListener("animationend", () => element.classList.remove(settings.revealed_class), { once: true });
};
document.addEventListener("click", (event) => {
  if (event.target instanceof Element && event.target.closest(`#${settings.jump_button_id}`) !== null) {
    jump();
  }
});
// While a dialog or a menu is open, End is theirs.
let keysBlocked = false;
(async () => {
  for (;;) {
    const command = await dioxus.recv();
    if (command.type === "jump") {
      jump();
    } else if (command.type === "reveal") {
      reveal(command.entry_id);
    } else if (command.type === "keys_blocked") {
      keysBlocked = command.blocked;
    }
  }
})();
// End in the file view scrolls the file.
document.addEventListener("keydown", (event) => {
  const target = event.target;
  const typing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement
    || target instanceof HTMLSelectElement || (target instanceof HTMLElement && target.isContentEditable);
  const inFileView = target instanceof Element && target.closest(`#${settings.file_view_id}`) !== null;
  if (event.key !== "End" || typing || inFileView || keysBlocked || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) {
    return;
  }
  event.preventDefault();
  jump();
});
await new Promise(() => {});
"#;

#[derive(Debug, Serialize)]
struct Settings {
    chat_scroll_id: &'static str,
    entry_class: &'static str,
    jump_button_id: &'static str,
    /// End there is the file view's.
    file_view_id: &'static str,
    revealed_class: &'static str,
    bottom_threshold_px: u16,
}

/// What the app tells the script once it runs.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ScrollCommand {
    /// Go to the bottom and follow new entries, as after a send.
    Jump,
    /// Scroll to an entry and highlight it.
    Reveal {
        entry_id: String,
    },
    KeysBlocked {
        blocked: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ScrollMessage {
    AtBottom { agent: u32, at_bottom: bool },
}

/// The id of an entry's wrapper.
pub fn entry_id(agent: AgentId, item: ItemId) -> String {
    format!("entry-{}-{}", agent.get(), item.get())
}

/// The jump to the latest entry, while the chat is scrolled up: an arrow, with the count of the
/// entries that arrived meanwhile. A click, or End, goes down; the script handles both.
#[component]
pub fn JumpToLatest(shown: bool, count: usize) -> Element {
    let badge = match count {
        0 => None,
        1..=MAX_BADGE_COUNT => Some(count.to_string()),
        _ => Some(format!("{MAX_BADGE_COUNT}+")),
    };
    let label = match count {
        0 => "Jump to the latest message".to_owned(),
        1 => "Jump to the latest message: 1 new entry".to_owned(),
        _ => format!("Jump to the latest message: {count} new entries"),
    };
    rsx! {
        button {
            id: JUMP_BUTTON_ID,
            class: if shown { "jump-latest shown" } else { "jump-latest" },
            title: "Jump to the latest message (End)",
            aria_label: label,
            aria_hidden: !shown,
            tabindex: if shown { "0" } else { "-1" },
            Icon { kind: IconKind::ArrowDown, size: 16 }
            if let Some(badge) = badge {
                span { class: "jump-badge", "{badge}" }
            }
        }
    }
}

/// The line above the first entry that arrived while the chat was scrolled up.
#[component]
pub fn NewDivider() -> Element {
    rsx! {
        div { class: "new-divider", role: "separator", "New" }
    }
}

/// Runs the script for the window's life.
pub fn use_chat_scroll(cockpit: Cockpit) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        let settings = Settings {
            chat_scroll_id: CHAT_SCROLL_ID,
            entry_class: ENTRY_CLASS,
            jump_button_id: JUMP_BUTTON_ID,
            file_view_id: FILE_VIEW_ID,
            revealed_class: REVEALED_CLASS,
            bottom_threshold_px: BOTTOM_THRESHOLD_PX,
        };
        if let Err(error) = script.send(settings) {
            tracing::warn!("cannot start the chat's scroll script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<ScrollMessage>().await {
                    Ok(ScrollMessage::AtBottom { agent, at_bottom }) => {
                        let agent = AgentId::new(agent);
                        cockpit.update(|state| actions::chat_at_bottom(state, agent, at_bottom));
                    }
                    Err(error) => {
                        tracing::warn!("the chat's scroll script stopped: {error}");
                        return;
                    }
                }
            }
        });
        script
    });
    let blocked = use_memo(move || {
        let state = cockpit.state.read();
        state.ui.dialog.is_some() || state.ui.popover.is_some()
    });
    use_effect(move || {
        let command = ScrollCommand::KeysBlocked { blocked: blocked() };
        if let Err(error) = script.send(command) {
            tracing::warn!("cannot update the chat's scroll script: {error}");
        }
    });
    let jumps = use_memo(move || cockpit.state.read().ui.chat_jumps);
    use_effect(move || {
        if jumps() == 0 {
            return;
        }
        if let Err(error) = script.send(ScrollCommand::Jump) {
            tracing::warn!("cannot take the chat to its bottom: {error}");
        }
    });
    let reveal = use_memo(move || cockpit.state.read().ui.chat_reveal);
    use_effect(move || {
        let Some(reveal) = reveal() else {
            return;
        };
        let command = ScrollCommand::Reveal {
            entry_id: entry_id(reveal.agent, reveal.item),
        };
        if let Err(error) = script.send(command) {
            tracing::warn!("cannot scroll the chat to an entry: {error}");
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
    fn the_fields_the_script_reads_and_sends_are_pinned() {
        let settings = Settings {
            chat_scroll_id: CHAT_SCROLL_ID,
            entry_class: ENTRY_CLASS,
            jump_button_id: JUMP_BUTTON_ID,
            file_view_id: FILE_VIEW_ID,
            revealed_class: REVEALED_CLASS,
            bottom_threshold_px: BOTTOM_THRESHOLD_PX,
        };
        let message: ScrollMessage =
            serde_json::from_value(json!({ "type": "at_bottom", "agent": 7, "at_bottom": false }))
                .unwrap();

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({
                "chat_scroll_id": "chat-scroll",
                "entry_class": "transcript-entry",
                "jump_button_id": "jump-to-latest",
                "file_view_id": "file-view",
                "revealed_class": "chat-revealed",
                "bottom_threshold_px": 40,
            })
        );
        assert!(matches!(
            message,
            ScrollMessage::AtBottom {
                agent: 7,
                at_bottom: false
            }
        ));
        assert_eq!(
            serde_json::to_value(ScrollCommand::Jump).unwrap(),
            json!({ "type": "jump" })
        );
        assert_eq!(
            serde_json::to_value(ScrollCommand::Reveal {
                entry_id: "entry-1-2".to_owned()
            })
            .unwrap(),
            json!({ "type": "reveal", "entry_id": "entry-1-2" })
        );
        assert_eq!(
            serde_json::to_value(ScrollCommand::KeysBlocked { blocked: true }).unwrap(),
            json!({ "type": "keys_blocked", "blocked": true })
        );
    }
}
