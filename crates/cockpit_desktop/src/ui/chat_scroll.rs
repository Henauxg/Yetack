//! The chat's scrolling, kept by a page script. At the bottom, the chat follows new entries.
//! Scrolled up, the entry in view stays where it is, whatever changes above or below it.
//!
//! The chat's box lays its column out from the bottom (`column-reverse`), so it keeps its
//! distance from the bottom: an entry added below would push the view up by its height.
//! Browsers that implement CSS scroll anchoring (`overflow-anchor`) correct that; WebKitGTK does
//! not, so the script does it. It tells the app only when the chat reaches or leaves its bottom,
//! and when another entry comes to the top of the view.

use cockpit_protocol::{AgentId, ItemId};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bridge::RECORDS_KEYS_ATTRIBUTE;
use crate::connection::Cockpit;
use crate::state::{RevealPlace, actions};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::splitter::FILE_VIEW_ID;

/// The chat's scroll box.
pub const CHAT_SCROLL_ID: &str = "chat-scroll";

/// The button that jumps to the latest entry. The script handles its clicks.
pub const JUMP_BUTTON_ID: &str = "jump-to-latest";

/// The wrapper of each entry, which the script anchors on. Its padding reaches past the entry.
pub const ENTRY_CLASS: &str = "transcript-entry";

/// A row's fold button. The script keeps one the user clicks to fold its row where it is, so the
/// row stays in view.
pub const FOLD_BUTTON_CLASS: &str = "fold-lines";

/// The class that highlights an entry the chat scrolled to, for as long as its animation runs.
pub const REVEALED_CLASS: &str = "chat-revealed";

/// How close to the bottom, in pixels, the chat still follows new entries.
const BOTTOM_THRESHOLD_PX: u16 = 40;

/// The room, in pixels, above an entry the chat scrolls to.
const REVEAL_TOP_PX: u16 = 12;

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
let reportedView = "";
// The scroll position the script set last. Its own scroll leaves the anchor as it was: measured
// again, the anchor would drift by the rounding of each correction.
let corrected = null;
// The fold button the user clicked to fold its row, and its height in the view, kept until the
// user scrolls. The anchor alone keeps the entry's top in place, so a row that folds far below
// it would leave the view.
let pinned = null;
// Each agent's place, kept while another agent shows.
const places = new Map();
// The user scrolled while the chat drew its rows, naming no agent yet: once it names its agent,
// that scroll holds, in place of the agent's place from before.
let scrolledWhileDrawing = false;
const boxTop = () => box.getBoundingClientRect().top;
const gapBelow = () => column.getBoundingClientRect().bottom - box.getBoundingClientRect().bottom;
// An entry's wrapper has padding past the entry: its last element shows the entry, after the
// "New" divider when it has one.
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
// The last entry whose top is above the view's bottom.
const lastInView = () => {
  const entries = column.getElementsByClassName(settings.entry_class);
  const bottom = box.getBoundingClientRect().bottom;
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    const element = shown(entries[middle]);
    if (element !== null && element.getBoundingClientRect().top < bottom) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return low === 0 ? null : entries[low - 1];
};
// An entry's id ends with its item's: see `entry_id`.
const itemOf = (entry) => entry === null ? null : Number(entry.id.slice(entry.id.lastIndexOf("-") + 1));
const report = () => {
  if (agent === null) {
    return;
  }
  const now = `${agent} ${following}`;
  if (now !== reported) {
    reported = now;
    dioxus.send({ type: "at_bottom", agent: Number(agent), at_bottom: following });
  }
  const top = itemOf(firstInView());
  const bottom = itemOf(lastInView());
  const view = `${agent} ${top} ${bottom}`;
  if (view !== reportedView) {
    reportedView = view;
    dioxus.send({ type: "in_view", agent: Number(agent), top, bottom });
  }
};
// After a scroll by the user: where the view now is.
const record = () => {
  if (corrected !== null && box.scrollTop === corrected) {
    corrected = null;
    return;
  }
  corrected = null;
  pinned = null;
  following = gapBelow() <= settings.bottom_threshold_px;
  anchor = following ? null : firstInView();
  if (agent === null) {
    scrolledWhileDrawing = true;
  }
  report();
};
// After the content changed: back to the bottom, or the pinned button or the anchor back at its
// height.
const settle = () => {
  let delta = gapBelow();
  if (pinned !== null && !pinned.element.isConnected) {
    pinned = null;
  }
  if (!following && pinned !== null) {
    delta = pinned.element.getBoundingClientRect().top - boxTop() - pinned.top;
  } else if (!following) {
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
  if (!following && pinned !== null) {
    anchor = firstInView();
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
    const scrolledMeanwhile = agent === null && scrolledWhileDrawing;
    scrolledWhileDrawing = false;
    if (agent !== null) {
      places.set(agent, { following, anchor });
    }
    agent = shownAgent;
    if (!scrolledMeanwhile) {
      pinned = null;
      const place = places.get(agent);
      const kept = place !== undefined && !place.following && place.anchor !== null
        && document.getElementById(place.anchor.id) !== null;
      following = !kept;
      anchor = kept ? place.anchor : null;
    }
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
  pinned = null;
  anchor = null;
  settle();
  report();
};
// Scrolls an entry to `place` (see `RevealPlace`) and highlights it for a moment. The entry is the
// anchor: the entries that come into view take their real height after the scroll, and the
// first one in view would move the entry by as much.
const reveal = (entryId, place) => {
  const entry = document.getElementById(entryId);
  const element = entry === null ? null : shown(entry);
  if (box === null || element === null) {
    return;
  }
  const height = element.getBoundingClientRect().height;
  const above = place === "top"
    ? settings.reveal_top_px
    : Math.max(settings.reveal_top_px, (box.clientHeight - height) / 2);
  box.scrollTop += element.getBoundingClientRect().top - boxTop() - above;
  corrected = box.scrollTop;
  pinned = null;
  following = gapBelow() <= settings.bottom_threshold_px;
  anchor = following ? null : { id: entry.id, top: element.getBoundingClientRect().top - boxTop() };
  report();
  element.classList.remove(settings.revealed_class);
  // Restarts the animation when the same entry is revealed again.
  void element.offsetWidth;
  element.classList.add(settings.revealed_class);
  element.addEventListener("animationend", () => element.classList.remove(settings.revealed_class), { once: true });
};
document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element)) {
    return;
  }
  if (event.target.closest(`#${settings.jump_button_id}`) !== null) {
    jump();
    return;
  }
  // A row that unfolds grows below its anchor, where the user reads on.
  const fold = event.target.closest(`#${settings.chat_scroll_id} .${settings.fold_button_class}`);
  if (fold !== null && box !== null && !following) {
    pinned = fold.getAttribute("aria-expanded") === "true"
      ? { element: fold, top: fold.getBoundingClientRect().top - boxTop() }
      : null;
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
      reveal(command.entry_id, command.place);
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
  const recording = target instanceof Element && target.closest(`[${settings.records_keys_attribute}]`) !== null;
  if (event.key !== "End" || typing || inFileView || recording || keysBlocked || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) {
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
    fold_button_class: &'static str,
    jump_button_id: &'static str,
    /// End there is the file view's.
    file_view_id: &'static str,
    /// Marks the element that records a shortcut, whose keys the script leaves alone.
    records_keys_attribute: &'static str,
    revealed_class: &'static str,
    bottom_threshold_px: u16,
    /// The room an entry the chat scrolls to keeps above it.
    reveal_top_px: u16,
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
        place: RevealPlace,
    },
    KeysBlocked {
        blocked: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ScrollMessage {
    AtBottom {
        agent: u32,
        at_bottom: bool,
    },
    /// The first and the last entry in view, `None` when no entry is.
    InView {
        agent: u32,
        top: Option<u32>,
        bottom: Option<u32>,
    },
}

/// The first and the last entry in the chat's view, as the script last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InView {
    pub agent: AgentId,
    pub top: ItemId,
    pub bottom: ItemId,
}

/// The id of an entry's wrapper. The script reads the item back from its end.
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

/// Runs the script for the window's life, and keeps `in_view` on the entries in view. `in_view` is
/// a signal of its own: it changes while the user scrolls, and only the views that read it render
/// again.
pub fn use_chat_scroll(cockpit: Cockpit, mut in_view: Signal<Option<InView>>) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        let settings = Settings {
            chat_scroll_id: CHAT_SCROLL_ID,
            entry_class: ENTRY_CLASS,
            fold_button_class: FOLD_BUTTON_CLASS,
            jump_button_id: JUMP_BUTTON_ID,
            file_view_id: FILE_VIEW_ID,
            records_keys_attribute: RECORDS_KEYS_ATTRIBUTE,
            revealed_class: REVEALED_CLASS,
            bottom_threshold_px: BOTTOM_THRESHOLD_PX,
            reveal_top_px: REVEAL_TOP_PX,
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
                    Ok(ScrollMessage::InView { agent, top, bottom }) => {
                        in_view.set(top.zip(bottom).map(|(top, bottom)| InView {
                            agent: AgentId::new(agent),
                            top: ItemId::new(top),
                            bottom: ItemId::new(bottom),
                        }));
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
            place: reveal.place,
        };
        if let Err(error) = script.send(command) {
            tracing::warn!("cannot scroll the chat to an entry: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// The script reads and writes these fields by name: a rename in Rust would make it read
    /// `undefined`, with no error.
    #[test]
    fn the_fields_the_script_reads_and_sends_are_pinned() {
        let settings = Settings {
            chat_scroll_id: CHAT_SCROLL_ID,
            entry_class: ENTRY_CLASS,
            fold_button_class: FOLD_BUTTON_CLASS,
            jump_button_id: JUMP_BUTTON_ID,
            file_view_id: FILE_VIEW_ID,
            records_keys_attribute: RECORDS_KEYS_ATTRIBUTE,
            revealed_class: REVEALED_CLASS,
            bottom_threshold_px: BOTTOM_THRESHOLD_PX,
            reveal_top_px: REVEAL_TOP_PX,
        };
        let message: ScrollMessage =
            serde_json::from_value(json!({ "type": "at_bottom", "agent": 7, "at_bottom": false }))
                .unwrap();

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({
                "chat_scroll_id": "chat-scroll",
                "entry_class": "transcript-entry",
                "fold_button_class": "fold-lines",
                "jump_button_id": "jump-to-latest",
                "file_view_id": "file-view",
                "records_keys_attribute": "data-records-keys",
                "revealed_class": "chat-revealed",
                "bottom_threshold_px": 40,
                "reveal_top_px": 12,
            })
        );
        assert!(matches!(
            message,
            ScrollMessage::AtBottom {
                agent: 7,
                at_bottom: false
            }
        ));
        assert_eq!(entry_id(AgentId::new(3), ItemId::new(12)), "entry-3-12");
        let in_view = |top: Value, bottom: Value| -> ScrollMessage {
            serde_json::from_value(
                json!({ "type": "in_view", "agent": 7, "top": top, "bottom": bottom }),
            )
            .unwrap()
        };
        assert!(matches!(
            in_view(json!(12), json!(15)),
            ScrollMessage::InView {
                agent: 7,
                top: Some(12),
                bottom: Some(15)
            }
        ));
        assert!(matches!(
            in_view(Value::Null, Value::Null),
            ScrollMessage::InView {
                agent: 7,
                top: None,
                bottom: None
            }
        ));
        assert_eq!(
            serde_json::to_value(ScrollCommand::Jump).unwrap(),
            json!({ "type": "jump" })
        );
        assert_eq!(
            serde_json::to_value(ScrollCommand::Reveal {
                entry_id: "entry-1-2".to_owned(),
                place: RevealPlace::Top,
            })
            .unwrap(),
            json!({ "type": "reveal", "entry_id": "entry-1-2", "place": "top" })
        );
        assert_eq!(
            serde_json::to_value(ScrollCommand::KeysBlocked { blocked: true }).unwrap(),
            json!({ "type": "keys_blocked", "blocked": true })
        );
    }
}
