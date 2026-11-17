//! The chat's scrolling, kept by a page script. At the bottom, the chat follows new entries.
//! Scrolled up, the entry in view stays where it is, whatever changes above or below it.
//!
//! The chat's box lays its column out from the bottom (`column-reverse`), so it keeps its
//! distance from the bottom: an entry added below would push the view up by its height.
//! Browsers that implement CSS scroll anchoring (`overflow-anchor`) correct that; WebKitGTK does
//! not, so the script does it. It tells the app only when the chat reaches or leaves its bottom,
//! and when another entry comes to the top of the view.
//!
//! Within [`NEAR_TOP_SCREENS`] of the top, while the chat has [`LOADS_OLDER_ATTRIBUTE`], it asks
//! for older messages, once per first entry: the chat draws more of the rows it holds, or loads
//! the page before them. Either changes the first entry, so the next ask follows if the view is
//! still near the top.

use cockpit_protocol::{AgentId, ItemId};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bridge::RECORDS_KEYS_ATTRIBUTE;
use crate::connection::Cockpit;
use crate::state::{RevealPlace, actions};
use crate::ui::find::REVEAL_EVENT;
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

/// Marks the chat while it can show older messages: rows it holds but has not drawn, or older
/// messages the daemon has, with none on its way. Only then does the script ask for them. The
/// chat's view writes it out, since an attribute name in `rsx!` takes no constant.
pub const LOADS_OLDER_ATTRIBUTE: &str = "data-loads-older";

/// How close to the bottom, in pixels, the chat still follows new entries.
const BOTTOM_THRESHOLD_PX: u16 = 40;

/// How near the top, in heights of the view, the chat asks for older messages, so that they usually
/// come before the user reaches the top.
const NEAR_TOP_SCREENS: u8 = 2;

/// The room, in pixels, above an entry the chat scrolls to.
const REVEAL_TOP_PX: u16 = 12;

/// How long after a wheel turned up the chat does not follow new entries, even at its bottom.
/// A smooth scroll spreads a notch over a few frames, and the first ones can leave the view within
/// a pixel of the bottom. A change of the content there would scroll back down and stop the notch.
/// Linux turns smooth scrolling off, but WebView2 on Windows keeps it.
const WHEEL_UP_MS: u16 = 600;

/// The largest change of the distance to the bottom, in pixels, that the browser makes alone and
/// the script does not count as a scroll. WebKitGTK scales the page by the screen's DPI, and at
/// 100 or 106 DPI a row that grows at the bottom moves the scroll by 1 px.
const ROUNDING_PX: f32 = 1.5;

/// The most new entries the jump button's badge counts; past it, it shows "99+".
const MAX_BADGE_COUNT: usize = 99;

/// Waits for its settings, then keeps the chat's place. Message and field names are serde's names
/// for `Settings`, `ScrollCommand` and `ScrollMessage`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
let box = null;
let column = null;
let agent = null;
// At the bottom, the view follows new entries; scrolled up, it keeps the first entry whose top is
// in view, `anchor`, at the same height.
let following = true;
let anchor = null;
let reported = "";
let reportedView = "";
// The agent and first entry the chat last asked older messages for.
let reportedNearTop = "";
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
let wheeledUpAt = -Infinity;
// The distance to the bottom at the last scroll `record` saw, moved by the corrections since.
let gapSeen = 0;
// The last distance to the bottom the script saw. Another one at a change of the content is a
// scroll whose event has not come yet.
let gapKnown = 0;
// A user scroll that `settle` saw before its event came.
let scrollPending = false;
// Takes the distance to the bottom as it is, without a scroll by the user.
const knowGap = () => {
  scrollPending = false;
  gapSeen = gapBelow();
  gapKnown = gapSeen;
};
const boxTop = () => box.getBoundingClientRect().top;
const gapBelow = () => column.getBoundingClientRect().bottom - box.getBoundingClientRect().bottom;
// An entry's wrapper has padding past the entry: its last element shows the entry, after the
// "New" divider when it has one.
const shown = (entry) => entry.lastElementChild;
// WebKit skips the layout of an entry out of view and gives it a placeholder height, which its
// content overflows, over the entries below: the wrapper ends where the entry ends on screen.
const bottomOf = (entry, element) =>
  Math.min(element.getBoundingClientRect().bottom, entry.getBoundingClientRect().bottom);
// The searches below find where the view's edge `y` falls among the entries in two steps. The
// page lays out an entry's wrapper even when it skips the entry's content, but reading the
// position of anything inside a skipped entry lays that content out: in a chat of 1600 entries,
// a search over the shown elements makes a scroll frame last about 100 ms. So the search runs
// over the wrappers, and only the entries at the edge, laid out already, are read inside.
// The first entry whose wrapper starts at or below `y`. Wrappers come in order, and each entry
// starts no higher than its wrapper, so every entry from it on starts at or below `y`.
const firstStartingBelow = (entries, y) => {
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if (entries[middle].getBoundingClientRect().top < y) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return low;
};
// Moves `index` back over the entries before it that `stays` keeps: the entries at the edge.
const backWhile = (entries, index, stays) => {
  let at = index;
  while (at > 0 && stays(entries[at - 1])) {
    at -= 1;
  }
  return at;
};
// The first entry whose bottom is lower than `below` pixels under the view's top.
const firstInView = (below = 0) => {
  const entries = column.getElementsByClassName(settings.entry_class);
  const top = boxTop();
  const limit = top + below;
  const low = backWhile(entries, firstStartingBelow(entries, limit), (entry) => {
    const element = shown(entry);
    return element === null || bottomOf(entry, element) > limit;
  });
  const entry = entries[low];
  const element = entry === undefined ? null : shown(entry);
  return element === null ? null : { id: entry.id, top: element.getBoundingClientRect().top - top };
};
// The entry the view keeps in place: the first one whose top is below the view's top, even past
// its bottom when one row fills the view. The chat keeps its distance from the bottom, so a row
// that changes height moves only the rows above it. A row cut by the view's top folds as it comes
// into view, and must not count.
const anchorInView = () => {
  const entries = column.getElementsByClassName(settings.entry_class);
  const top = boxTop();
  const low = backWhile(entries, firstStartingBelow(entries, top), (entry) => {
    const element = shown(entry);
    return element === null || element.getBoundingClientRect().top >= top;
  });
  const entry = entries[low];
  const element = entry === undefined ? null : shown(entry);
  return element === null ? firstInView() : { id: entry.id, top: element.getBoundingClientRect().top - top };
};
// The last entry whose top is above the view's bottom.
const lastInView = () => {
  const entries = column.getElementsByClassName(settings.entry_class);
  const bottom = box.getBoundingClientRect().bottom;
  const low = backWhile(entries, firstStartingBelow(entries, bottom), (entry) => {
    const element = shown(entry);
    return element === null || element.getBoundingClientRect().top >= bottom;
  });
  return low === 0 ? null : entries[low - 1];
};
// An entry's id ends with its item's: see `entry_id`.
const itemOf = (entry) => entry === null ? null : Number(entry.id.slice(entry.id.lastIndexOf("-") + 1));
const report = () => {
  if (agent === null || box === null) {
    return;
  }
  const now = `${agent} ${following}`;
  if (now !== reported) {
    reported = now;
    dioxus.send({ type: "at_bottom", agent: Number(agent), at_bottom: following });
  }
  // An entry with no more than `reveal_top_px` pixels in view at the top does not count.
  const top = itemOf(firstInView(settings.reveal_top_px));
  const bottom = itemOf(lastInView());
  const view = `${agent} ${following} ${top} ${bottom}`;
  if (view !== reportedView) {
    reportedView = view;
    dioxus.send({ type: "in_view", agent: Number(agent), at_bottom: following, top, bottom });
  }
  // Without the mark, as while a page is on its way, the app refuses the ask: it waits for the mark.
  if (!box.hasAttribute(settings.loads_older_attribute)
    || boxTop() - column.getBoundingClientRect().top >= settings.near_top_screens * box.clientHeight) {
    reportedNearTop = "";
    return;
  }
  const first = itemOf(column.querySelector(`.${settings.entry_class}`));
  const nearTop = `${agent} ${first}`;
  if (nearTop !== reportedNearTop) {
    reportedNearTop = nearTop;
    dioxus.send({ type: "near_top", agent: Number(agent) });
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
  const wheelingUp = performance.now() - wheeledUpAt < settings.wheel_up_ms;
  const gap = gapBelow();
  // A scroll event that moved nothing comes from the browser, such as a clamp to a shorter chat.
  const moved = scrollPending || Math.abs(gap - gapKnown) > settings.rounding_px;
  scrollPending = false;
  // A scroll up leaves the bottom, however short: an arrow key or the scrollbar sends no wheel.
  const leaving = gap > gapSeen + settings.rounding_px;
  const nearing = gap < gapSeen - settings.rounding_px;
  gapSeen = gap;
  gapKnown = gap;
  following = !leaving && (nearing || !wheelingUp) && gap <= settings.bottom_threshold_px;
  anchor = following ? null : anchorInView();
  if (agent === null && moved) {
    scrolledWhileDrawing = true;
  }
  report();
};
// After the content changed: back to the bottom, or the pinned button or the anchor back at its
// height.
const settle = () => {
  // The user scrolled, and its event has not come yet: the correction keeps that scroll. `record`
  // decides from `gapSeen` once the event comes.
  const gap = gapBelow();
  const scrolled = gap - gapKnown;
  if (Math.abs(scrolled) > settings.rounding_px) {
    scrollPending = true;
    pinned = null;
    if (following && scrolled > 0) {
      following = false;
      anchor = anchorInView();
      gapKnown = gap;
      return;
    }
    // A scroll up moves the anchor down by as much.
    if (anchor !== null) {
      anchor = { id: anchor.id, top: anchor.top + scrolled };
    }
  }
  let delta = gap;
  if (pinned !== null && !pinned.element.isConnected) {
    pinned = null;
  }
  if (!following && pinned !== null) {
    delta = pinned.element.getBoundingClientRect().top - boxTop() - pinned.top;
  } else if (!following) {
    const entry = anchor === null ? null : document.getElementById(anchor.id);
    const element = entry === null ? null : shown(entry);
    if (element === null) {
      anchor = anchorInView();
      gapKnown = gap;
      return;
    }
    delta = element.getBoundingClientRect().top - boxTop() - anchor.top;
  }
  if (Math.abs(delta) >= 1) {
    box.scrollTop += delta;
    // The user's scroll event comes with this one, as a single event: `record` must see it.
    corrected = scrollPending ? null : box.scrollTop;
  }
  if (!following && pinned !== null) {
    anchor = anchorInView();
  }
  const after = gapBelow();
  gapSeen += after - gap;
  gapKnown = after;
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
    knowGap();
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
// A wheel turned up over `target` scrolls a box inside the chat, such as a folded message, until
// that box reaches its top.
const scrollsInside = (target) => {
  let element = target;
  while (element instanceof Element && element !== box) {
    if (element.scrollTop > 0 && ["auto", "scroll"].includes(getComputedStyle(element).overflowY)) {
      return true;
    }
    element = element.parentElement;
  }
  return false;
};
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
  box.addEventListener("wheel", (event) => {
    if (event.deltaY < 0 && !scrollsInside(event.target)) {
      wheeledUpAt = performance.now();
      // Before the first scroll of the notch, a change of the content would take the chat back down.
      if (following && box.scrollHeight > box.clientHeight) {
        following = false;
        anchor = anchorInView();
      }
    } else if (event.deltaY > 0) {
      // A wheel turned down can take the chat back to its bottom, where it follows again.
      wheeledUpAt = -Infinity;
      // At the bottom already, the wheel scrolls nothing, and no scroll event comes.
      if (!following && gapBelow() <= 1) {
        following = true;
        pinned = null;
        anchor = null;
        report();
      }
    }
  }, { passive: true });
  mutations.observe(box, {
    childList: true,
    subtree: true,
    characterData: true,
    attributeFilter: [settings.loads_older_attribute],
  });
  resizes.observe(column);
  resizes.observe(box);
  knowGap();
  changed();
};
new MutationObserver(attach).observe(document.body, { childList: true, subtree: true });
attach();
const jump = () => {
  if (box === null) {
    return;
  }
  knowGap();
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
  knowGap();
  pinned = null;
  following = gapBelow() <= settings.bottom_threshold_px;
  anchor = following ? null : { id: entry.id, top: element.getBoundingClientRect().top - boxTop() };
  // An entry that comes into view overlaps the others until the next frame lays it out.
  requestAnimationFrame(() => requestAnimationFrame(report));
  element.classList.remove(settings.revealed_class);
  // Restarts the animation when the same entry is revealed again.
  void element.offsetWidth;
  element.classList.add(settings.revealed_class);
  element.addEventListener("animationend", () => element.classList.remove(settings.revealed_class), { once: true });
};
// The find scrolls the chat by `delta` to bring a match into view. The match's entry is the anchor,
// as for a reveal.
document.addEventListener(settings.find_reveal_event, (event) => {
  const entry = document.getElementById(event.detail.entry_id);
  const element = entry === null ? null : shown(entry);
  if (box === null || element === null) {
    return;
  }
  box.scrollTop += event.detail.delta;
  corrected = box.scrollTop;
  knowGap();
  pinned = null;
  following = gapBelow() <= settings.bottom_threshold_px;
  anchor = following ? null : { id: entry.id, top: element.getBoundingClientRect().top - boxTop() };
  requestAnimationFrame(() => requestAnimationFrame(report));
});
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
    loads_older_attribute: &'static str,
    /// The event by which the find scrolls the chat to a match.
    find_reveal_event: &'static str,
    bottom_threshold_px: u16,
    /// The room an entry the chat scrolls to keeps above it.
    reveal_top_px: u16,
    near_top_screens: u8,
    wheel_up_ms: u16,
    rounding_px: f32,
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
        at_bottom: bool,
        top: Option<u32>,
        bottom: Option<u32>,
    },
    /// The view came near the top: older messages are wanted.
    NearTop {
        agent: u32,
    },
}

/// The first and the last entry in the chat's view, as the script last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InView {
    pub agent: AgentId,
    /// As `ScrollMessage::AtBottom` says, but in the same message as the entries, so the two never
    /// disagree for a frame.
    pub at_bottom: bool,
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
pub fn use_chat_scroll(
    cockpit: Cockpit,
    mut in_view: Signal<Option<InView>>,
    near_top: Callback<AgentId>,
) {
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
            loads_older_attribute: LOADS_OLDER_ATTRIBUTE,
            find_reveal_event: REVEAL_EVENT,
            bottom_threshold_px: BOTTOM_THRESHOLD_PX,
            reveal_top_px: REVEAL_TOP_PX,
            near_top_screens: NEAR_TOP_SCREENS,
            wheel_up_ms: WHEEL_UP_MS,
            rounding_px: ROUNDING_PX,
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
                    Ok(ScrollMessage::InView {
                        agent,
                        at_bottom,
                        top,
                        bottom,
                    }) => {
                        in_view.set(top.zip(bottom).map(|(top, bottom)| InView {
                            agent: AgentId::new(agent),
                            at_bottom,
                            top: ItemId::new(top),
                            bottom: ItemId::new(bottom),
                        }));
                    }
                    Ok(ScrollMessage::NearTop { agent }) => {
                        near_top.call(AgentId::new(agent));
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
            loads_older_attribute: LOADS_OLDER_ATTRIBUTE,
            find_reveal_event: REVEAL_EVENT,
            bottom_threshold_px: BOTTOM_THRESHOLD_PX,
            reveal_top_px: REVEAL_TOP_PX,
            near_top_screens: NEAR_TOP_SCREENS,
            wheel_up_ms: WHEEL_UP_MS,
            rounding_px: ROUNDING_PX,
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
                "loads_older_attribute": "data-loads-older",
                "find_reveal_event": "yetack-find-reveal",
                "bottom_threshold_px": 40,
                "reveal_top_px": 12,
                "near_top_screens": 2,
                "wheel_up_ms": 600,
                "rounding_px": 1.5,
            })
        );
        assert!(matches!(
            message,
            ScrollMessage::AtBottom {
                agent: 7,
                at_bottom: false
            }
        ));
        let near_top: ScrollMessage =
            serde_json::from_value(json!({ "type": "near_top", "agent": 7 })).unwrap();
        assert!(matches!(near_top, ScrollMessage::NearTop { agent: 7 }));
        assert_eq!(entry_id(AgentId::new(3), ItemId::new(12)), "entry-3-12");
        let in_view = |top: Value, bottom: Value| -> ScrollMessage {
            serde_json::from_value(json!({
                "type": "in_view",
                "agent": 7,
                "at_bottom": false,
                "top": top,
                "bottom": bottom,
            }))
            .unwrap()
        };
        assert!(matches!(
            in_view(json!(12), json!(15)),
            ScrollMessage::InView {
                agent: 7,
                at_bottom: false,
                top: Some(12),
                bottom: Some(15)
            }
        ));
        assert!(matches!(
            in_view(Value::Null, Value::Null),
            ScrollMessage::InView {
                agent: 7,
                at_bottom: false,
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
