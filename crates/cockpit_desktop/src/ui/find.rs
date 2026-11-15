//! Ctrl+F: the chat's find, a bar under the header that pushes the chat down, and the file view's,
//! a box over the top right of the file. A page script searches what the page shows, highlights
//! the matches with the CSS Custom Highlight API, scrolls to the current one, and tells the app
//! how many it found. In the chat, a fold that is closed counts the matches it hides on a chip
//! (`find_folds`), and the script opens the fold when the find reaches it.

use cockpit_protocol::AgentId;
use dioxus::document::Eval;
use dioxus::prelude::*;
use serde::Serialize;

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::Cockpit;
use crate::state::find::{self, FindDraft, FindPane, FindReport};
use crate::state::{AppState, Drafts, HistoryState, actions};
use crate::ui::chat_scroll::{CHAT_SCROLL_ID, ENTRY_CLASS, FOLD_BUTTON_CLASS};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::use_presence;
use crate::ui::splitter::FILE_VIEW_ID;

/// The chat find's field.
const CHAT_FIELD_ID: &str = "chat-find-field";

/// The file view find's field.
const FILE_FIELD_ID: &str = "file-find-field";

/// The file view's find box: the file's text under it does not count as in view.
const FILE_BOX_ID: &str = "file-find-box";

/// The event the script sends the chat's scroll script to bring a match into view.
pub(super) const REVEAL_EVENT: &str = "yetack-find-reveal";

/// Marks what the find leaves out, such as a time that changes every second. The components write
/// it as a literal.
const SKIP_ATTRIBUTE: &str = "data-find-skip";

/// Marks the chip of a fold with matches inside, with their count (`find_folds::FoldChip`).
const FOLDED_ATTRIBUTE: &str = "data-find-folded";

/// Past this many matches, the find stops counting, and the bar shows "10000+".
const MAX_MATCHES: u32 = 10_000;

/// How long the script waits after a change of the page before it searches again, in
/// milliseconds: a working agent changes the chat many times a second.
const RERUN_MS: u32 = 250;

/// A match outside this margin of its pane's edges, in pixels, is out of view.
const VIEW_MARGIN_PX: u16 = 24;

/// Where the find puts a match it scrolls to, as a share of the pane's height from its top.
const REVEAL_AT: f32 = 0.35;

/// How long the script waits for a fold it clicked to open, in milliseconds, before the find
/// steps on without it.
const OPEN_WAIT_MS: u32 = 1_500;

/// Waits for its settings, then follows the finds the app sends. Field names are serde's names
/// for `Settings`, `FindCommand` and `FindReport`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
const highlights = typeof CSS !== "undefined" && "highlights" in CSS && typeof Highlight === "function";
const scratch = document.createRange();
// An offset kept from an earlier search may be past the end of a text that changed since.
const within = (node, offset) => Math.min(offset, node.nodeType === Node.TEXT_NODE ? node.length : node.childNodes.length);
// -1 when the point (b, bOffset) is before (a, aOffset), 0 when they are the same, 1 after.
const order = (a, aOffset, b, bOffset) => {
  scratch.setStart(a, within(a, aOffset));
  scratch.collapse(true);
  return scratch.comparePoint(b, within(b, bOffset));
};
const pointOf = (stop) => stop.range === undefined
  ? { node: stop.chip, offset: 0 }
  : { node: stop.range.startContainer, offset: stop.range.startOffset };
const pointAfter = (element) => {
  scratch.setStartAfter(element);
  return { node: scratch.startContainer, offset: scratch.startOffset };
};
// The first stop at or after `point`, or the stops' count when every stop is before it.
const firstFrom = (stops, point) => {
  let low = 0;
  let high = stops.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    const at = pointOf(stops[middle]);
    if (order(point.node, point.offset, at.node, at.offset) < 0) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return low;
};
// A match's range is static: thousands of live ranges would each follow every change of the page.
// A page that changed since the search may cut it short.
const measure = document.createRange();
const rectOf = (stop) => {
  if (stop.range === undefined) {
    return stop.chip.getBoundingClientRect();
  }
  try {
    measure.setStart(stop.range.startContainer, stop.range.startOffset);
    measure.setEnd(stop.range.endContainer, stop.range.endOffset);
    return measure.getBoundingClientRect();
  } catch {
    return new DOMRect();
  }
};
const elementOf = (stop) => stop.range === undefined ? stop.chip : stop.range.startContainer.parentElement;
const escaped = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
// Scrolls the boxes that cut `rect` sideways, such as a wide code block, until it shows.
const showSideways = (element, rect, last) => {
  for (let box = element; box !== null; box = box.parentElement) {
    if (box.scrollWidth > box.clientWidth + 1 && ["auto", "scroll"].includes(getComputedStyle(box).overflowX)) {
      const edges = box.getBoundingClientRect();
      const margin = settings.view_margin_px;
      if (rect.left < edges.left + margin || rect.right > edges.right - margin) {
        box.scrollLeft += rect.left - edges.left - edges.width / 3;
      }
    }
    if (box === last) {
      return;
    }
  }
};

class Finder {
  constructor(pane) {
    this.pane = pane;
    this.want = { open: false, agent: null, query: "", match_case: false, target: "" };
    this.stops = [];
    this.capped = false;
    this.current = -1;
    // Where the current stop is, so that a search after the page changed finds it again.
    this.point = null;
    // A fold the find clicked open, until its content shows.
    this.opening = null;
    // The chips of folds that did not open when clicked: the find does not click them again.
    this.unopened = new WeakSet();
    this.scrollNext = false;
    this.timer = null;
    this.reported = "";
    this.root = null;
  }

  // The element the find searches and the box that scrolls it, once the page shows what the app
  // asked for.
  where() {
    if (this.pane === "chat") {
      const box = document.getElementById(settings.chat_scroll_id);
      // The chat names its agent once it draws all its rows.
      if (box === null || box.dataset.agent !== String(this.want.agent) || box.firstElementChild === null) {
        return null;
      }
      return { root: box.firstElementChild, scroller: box };
    }
    const body = document.getElementById(settings.file_view_id)?.querySelector(`.${settings.file_body_class}`) ?? null;
    if (body === null || body.querySelector(`.${settings.file_loading_class}`) !== null) {
      return null;
    }
    return { root: body, scroller: body };
  }

  set(want) {
    const query = want.query !== this.want.query || want.match_case !== this.want.match_case;
    const target = want.agent !== this.want.agent || want.target !== this.want.target || want.open !== this.want.open;
    this.want = want;
    if (!want.open || want.query === "") {
      this.clear();
      return;
    }
    if (query || target) {
      this.point = null;
      this.opening = null;
      // A new text goes to its first match from the view. Another agent or file keeps its place.
      this.scrollNext = query;
    }
    this.schedule(0);
  }

  schedule(delay) {
    if (this.timer !== null) {
      if (delay > 0) {
        return;
      }
      clearTimeout(this.timer);
    }
    this.timer = setTimeout(() => this.run(), delay);
  }

  // Whether a change of the page may change what the find finds.
  concerns(record) {
    if (!this.want.open || this.want.query === "") {
      return false;
    }
    if (this.root === null || !this.root.isConnected) {
      return true;
    }
    const node = record.target;
    const element = node instanceof Element ? node : node.parentElement;
    if (element === null || element.closest(`[${settings.skip_attribute}]`) !== null) {
      return false;
    }
    return this.root.contains(node) || node.contains(this.root);
  }

  clear() {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.stops = [];
    this.current = -1;
    this.point = null;
    this.opening = null;
    this.root = null;
    this.paint();
    this.reported = "";
  }

  search(root) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode: (node) => node.parentElement === null || node.parentElement.closest(settings.skip_selector) !== null
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT,
    });
    // The text of the root, a new line between its blocks so that no match runs across two.
    const pieces = [];
    let text = "";
    let block = null;
    for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
      const own = node.parentElement.closest(settings.block_selector);
      if (own !== block) {
        text += "\n";
        block = own;
      }
      pieces.push({ node, start: text.length });
      text += node.data;
    }
    const locate = (at, end) => {
      let low = 0;
      let high = pieces.length - 1;
      while (low < high) {
        const middle = (low + high + 1) >> 1;
        if (pieces[middle].start <= at - (end ? 1 : 0)) {
          low = middle;
        } else {
          high = middle - 1;
        }
      }
      return pieces[low];
    };
    const matches = [];
    this.capped = false;
    const pattern = new RegExp(escaped(this.want.query), this.want.match_case ? "gu" : "giu");
    for (const match of text.matchAll(pattern)) {
      if (matches.length >= settings.max_matches) {
        this.capped = true;
        break;
      }
      const start = locate(match.index, false);
      const end = locate(match.index + match[0].length, true);
      matches.push({
        range: new StaticRange({
          startContainer: start.node,
          startOffset: match.index - start.start,
          endContainer: end.node,
          endOffset: match.index + match[0].length - end.start,
        }),
      });
    }
    // A closed fold's chip counts as its matches, where the fold is.
    const chips = this.pane === "chat" ? [...root.querySelectorAll(`[${settings.folded_attribute}]`)] : [];
    const stops = [];
    let next = 0;
    for (const chip of chips) {
      while (next < matches.length && order(chip, 0, matches[next].range.startContainer, matches[next].range.startOffset) < 0) {
        stops.push(matches[next]);
        next += 1;
      }
      const count = Number(chip.getAttribute(settings.folded_attribute)) || 0;
      for (let index = 0; index < count; index += 1) {
        stops.push({ chip, index });
      }
    }
    stops.push(...matches.slice(next));
    if (stops.length > settings.max_matches) {
      stops.length = settings.max_matches;
      this.capped = true;
    }
    return stops;
  }

  // The top of the pane's view that nothing covers.
  viewTop(scroller) {
    const top = scroller.getBoundingClientRect().top;
    const covers = [...document.querySelectorAll(this.pane === "chat" ? settings.chat_covers : `#${settings.file_box_id}`)];
    const bottoms = covers.map((cover) => cover.getBoundingClientRect().bottom);
    return Math.max(top, ...bottoms) + settings.view_margin_px;
  }

  pick(scroller) {
    const stops = this.stops;
    if (stops.length === 0) {
      return -1;
    }
    if (this.opening !== null) {
      const { chip, line, backward, since } = this.opening;
      if (chip.isConnected && performance.now() - since < settings.open_wait_ms) {
        const waiting = stops.findIndex((stop) => stop.chip === chip);
        if (waiting !== -1) {
          return waiting;
        }
      }
      if (chip.isConnected) {
        this.unopened.add(chip);
      }
      this.opening = null;
      this.scrollNext = true;
      if (line.isConnected) {
        if (backward) {
          const end = firstFrom(stops, pointAfter(line.parentElement));
          return Math.max(end - 1, 0);
        }
        const start = firstFrom(stops, pointAfter(line));
        return start < stops.length ? start : 0;
      }
    }
    if (this.point !== null && this.point.node.isConnected) {
      const at = firstFrom(stops, this.point);
      return at < stops.length ? at : 0;
    }
    // From the view: the first stop whose bottom is below the view's top. When the count stopped
    // before the view, no stop is current, rather than one far above.
    const top = this.viewTop(scroller);
    let low = 0;
    let high = stops.length;
    while (low < high) {
      const middle = (low + high) >> 1;
      if (rectOf(stops[middle]).bottom <= top) {
        low = middle + 1;
      } else {
        high = middle;
      }
    }
    if (low < stops.length) {
      return low;
    }
    return this.capped ? -1 : 0;
  }

  run() {
    this.timer = null;
    if (!this.want.open || this.want.query === "") {
      return;
    }
    const found = this.where();
    if (found === null) {
      return;
    }
    this.root = found.root;
    this.stops = this.search(found.root);
    this.current = this.pick(found.scroller);
    const here = this.stops[this.current];
    this.point = here === undefined ? null : pointOf(here);
    this.paint();
    if (this.scrollNext) {
      this.scrollNext = false;
      this.reveal(found.scroller);
    }
    this.report();
  }

  step(forward) {
    const count = this.stops.length;
    const found = this.where();
    // While a fold opens, its matches are not on the page yet: the step would click it again.
    if (count === 0 || found === null || (this.opening !== null && this.opening.chip.isConnected)) {
      return;
    }
    this.opening = null;
    this.current = this.current < 0
      ? (forward ? 0 : count - 1)
      : (this.current + (forward ? 1 : count - 1)) % count;
    this.point = pointOf(this.stops[this.current]);
    this.paint();
    this.reveal(found.scroller, !forward);
    this.report();
  }

  paint() {
    if (this.pane === "chat") {
      document.querySelectorAll(`[${settings.folded_attribute}][${settings.current_attribute}]`)
        .forEach((chip) => chip.removeAttribute(settings.current_attribute));
    }
    if (!highlights) {
      return;
    }
    const others = `find-${this.pane}`;
    const mark = `find-${this.pane}-current`;
    const here = this.stops[this.current];
    const ranges = this.stops.filter((stop) => stop.range !== undefined && stop !== here).map((stop) => stop.range);
    if (ranges.length > 0) {
      CSS.highlights.set(others, new Highlight(...ranges));
    } else {
      CSS.highlights.delete(others);
    }
    if (here?.range !== undefined) {
      const highlight = new Highlight(here.range);
      highlight.priority = 1;
      CSS.highlights.set(mark, highlight);
    } else {
      CSS.highlights.delete(mark);
    }
    if (here?.chip !== undefined) {
      here.chip.setAttribute(settings.current_attribute, "");
    }
  }

  // Brings the current stop into view: opens the fold that holds it, on its last match when the
  // find goes `backward`, and the folded row that cuts it, then scrolls.
  reveal(scroller, backward = false) {
    const here = this.stops[this.current];
    if (here === undefined) {
      return;
    }
    if (here.chip !== undefined) {
      // The chip sits in the fold's button, or beside it on the fold's line.
      const line = here.chip.closest("button[aria-expanded]") ?? here.chip.parentElement;
      const control = line.matches('[aria-expanded="false"]') ? line : line.querySelector(':scope > [aria-expanded="false"]');
      if (control !== null && this.opening === null && !this.unopened.has(here.chip)) {
        this.opening = { chip: here.chip, line, backward, since: performance.now() };
        control.click();
        // A fold that never opens lets the find go on.
        setTimeout(() => this.schedule(0), settings.open_wait_ms);
      }
      this.scrollTo(scroller, here);
      return;
    }
    const clip = elementOf(here).closest(settings.clip_selector);
    const cut = clip === null ? null : clip.querySelector(`.${settings.clip_class}, .${settings.message_clip_class}`);
    if (cut !== null) {
      const rect = rectOf(here);
      const edges = cut.getBoundingClientRect();
      if (rect.top < edges.top || rect.bottom > edges.bottom) {
        clip.querySelector(`.${settings.fold_button_class}`)?.click();
        // The row grows over its transition: the match takes its place at its end.
        cut.addEventListener("transitionend", () => this.scrollTo(scroller, here), { once: true });
      }
    }
    this.scrollTo(scroller, here);
  }

  scrollTo(scroller, stop) {
    if (stop.range !== undefined && !stop.range.startContainer.isConnected) {
      return;
    }
    const rect = rectOf(stop);
    const top = this.viewTop(scroller);
    const bottom = scroller.getBoundingClientRect().bottom - settings.view_margin_px;
    if (rect.top < top || rect.bottom > bottom) {
      const delta = rect.top - (top + (bottom - top) * settings.reveal_at);
      const entry = this.pane === "chat" ? elementOf(stop).closest(`.${settings.entry_class}`) : null;
      if (entry !== null) {
        // The chat's scroll script keeps the entry in place as the entries above it take their
        // real height.
        document.dispatchEvent(new CustomEvent(settings.reveal_event, { detail: { entry_id: entry.id, delta } }));
      } else {
        scroller.scrollTop += delta;
      }
    }
    showSideways(elementOf(stop), rectOf(stop), scroller);
  }

  report() {
    const message = {
      pane: this.pane,
      agent: this.want.agent,
      query: this.want.query,
      match_case: this.want.match_case,
      current: this.current < 0 ? null : this.current + 1,
      total: this.stops.length,
      folded: this.stops.filter((stop) => stop.chip !== undefined).length,
      capped: this.capped,
    };
    const key = JSON.stringify(message);
    if (key !== this.reported) {
      this.reported = key;
      dioxus.send(message);
    }
  }
}

const finders = { chat: new Finder("chat"), file: new Finder("file") };
new MutationObserver((records) => {
  for (const finder of Object.values(finders)) {
    if (records.some((record) => finder.concerns(record))) {
      finder.schedule(settings.rerun_ms);
    }
  }
}).observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["class", "data-agent"] });
// Enter in a find's field and F3 anywhere go to the next match, with Shift to the one before.
document.addEventListener("keydown", (event) => {
  if (event.defaultPrevented || event.isComposing || event.ctrlKey || event.altKey || event.metaKey) {
    return;
  }
  const target = event.target;
  if (event.key === "Enter" && target instanceof HTMLInputElement) {
    const finder = target.id === settings.chat_field_id ? finders.chat : target.id === settings.file_field_id ? finders.file : null;
    if (finder !== null) {
      event.preventDefault();
      finder.step(!event.shiftKey);
    }
    return;
  }
  if (event.key === "F3") {
    const inFile = target instanceof Element && target.closest(`#${settings.file_view_id}`) !== null;
    const { chat, file } = finders;
    const finder = inFile && file.want.open ? file : chat.want.open ? chat : file.want.open ? file : null;
    if (finder !== null) {
      event.preventDefault();
      finder.step(!event.shiftKey);
    }
  }
}, true);
for (;;) {
  const command = await dioxus.recv();
  const finder = finders[command.pane];
  if (command.type === "find") {
    finder.set(command);
  } else if (command.type === "step") {
    finder.step(command.forward);
  }
}
"#;

#[derive(Debug, Serialize)]
struct Settings {
    chat_scroll_id: &'static str,
    entry_class: &'static str,
    file_view_id: &'static str,
    file_body_class: &'static str,
    /// The placeholder lines of a file that loads, which holds no text yet.
    file_loading_class: &'static str,
    file_box_id: &'static str,
    chat_field_id: &'static str,
    file_field_id: &'static str,
    /// What the find leaves out: its own chips, the fold buttons, the line numbers and notes.
    skip_selector: String,
    skip_attribute: &'static str,
    /// The elements whose text a match never runs across.
    block_selector: &'static str,
    folded_attribute: &'static str,
    /// What covers the top of the chat: the last instruction's bar.
    chat_covers: &'static str,
    /// A row that a fold cuts, and the part of it that cuts.
    clip_selector: &'static str,
    clip_class: &'static str,
    message_clip_class: &'static str,
    fold_button_class: &'static str,
    /// Marks the chip of the current match while its fold has not opened.
    current_attribute: &'static str,
    reveal_event: &'static str,
    max_matches: u32,
    rerun_ms: u32,
    view_margin_px: u16,
    reveal_at: f32,
    open_wait_ms: u32,
}

/// What the app tells the script.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FindCommand {
    /// What a find looks for, and where.
    Find {
        pane: FindPane,
        open: bool,
        agent: Option<AgentId>,
        query: String,
        match_case: bool,
        /// The file the file view shows: another one starts from its view.
        target: String,
    },
    Step {
        pane: FindPane,
        forward: bool,
    },
}

impl FindCommand {
    fn of(state: &AppState, drafts: &Drafts, pane: FindPane) -> Self {
        let agent = state.ui.selected;
        let open = agent.is_some_and(|agent| find::is_open(state, pane, agent));
        let draft = agent
            .filter(|_| open)
            .and_then(|agent| drafts.finds.get(pane, agent));
        let target = match pane {
            FindPane::Chat => String::new(),
            FindPane::File => agent
                .and_then(|agent| state.ui.file_view(agent))
                .map(|view| view.path.display().to_string())
                .unwrap_or_default(),
        };
        Self::Find {
            pane,
            open,
            agent,
            query: draft.map(|draft| draft.query.clone()).unwrap_or_default(),
            match_case: draft.is_some_and(|draft| draft.match_case),
            target,
        }
    }
}

/// The running script, for the bars' buttons, and the last request for the focus a field took.
#[derive(Clone, Copy)]
struct FindScript {
    script: Eval,
    focus_taken: CopyValue<u64>,
}

impl FindScript {
    fn step(self, pane: FindPane, forward: bool) {
        if let Err(error) = self.script.send(FindCommand::Step { pane, forward }) {
            tracing::warn!("cannot step the find: {error}");
        }
    }
}

/// Runs the find's script for the window's life, and keeps it in step with the finds.
pub(super) fn use_find(cockpit: Cockpit) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        let settings = Settings {
            chat_scroll_id: CHAT_SCROLL_ID,
            entry_class: ENTRY_CLASS,
            file_view_id: FILE_VIEW_ID,
            file_body_class: "file-body",
            file_loading_class: "file-loading",
            file_box_id: FILE_BOX_ID,
            chat_field_id: CHAT_FIELD_ID,
            file_field_id: FILE_FIELD_ID,
            skip_selector: format!(
                "[{SKIP_ATTRIBUTE}], [{FOLDED_ATTRIBUTE}], [aria-hidden=\"true\"], .{FOLD_BUTTON_CLASS}, .chevron, .line-gutter, .file-note, .edit-title, .lines-not-shown"
            ),
            skip_attribute: SKIP_ATTRIBUTE,
            block_selector: "p, li, pre, td, th, h1, h2, h3, h4, h5, h6, blockquote, div, button",
            folded_attribute: FOLDED_ATTRIBUTE,
            chat_covers: ".last-instruction:not(.leaving) .last-instruction-message",
            clip_selector: ".io-row[data-tall]:not(.unfolded), .message-fold[data-tall]:not(.unfolded)",
            clip_class: "io-clip",
            message_clip_class: "message-clip",
            fold_button_class: FOLD_BUTTON_CLASS,
            current_attribute: "data-find-current",
            reveal_event: REVEAL_EVENT,
            max_matches: MAX_MATCHES,
            rerun_ms: RERUN_MS,
            view_margin_px: VIEW_MARGIN_PX,
            reveal_at: REVEAL_AT,
            open_wait_ms: OPEN_WAIT_MS,
        };
        if let Err(error) = script.send(settings) {
            tracing::warn!("cannot start the find's script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<FindReport>().await {
                    Ok(report) => cockpit.with_drafts(|_, drafts| drafts.finds.report(&report)),
                    Err(error) => {
                        tracing::warn!("the find's script stopped: {error}");
                        return;
                    }
                }
            }
        });
        script
    });
    let focus_taken = use_hook(|| CopyValue::new(0_u64));
    use_context_provider(|| FindScript {
        script,
        focus_taken,
    });
    for pane in [FindPane::Chat, FindPane::File] {
        let command =
            use_memo(move || FindCommand::of(&cockpit.state.read(), &cockpit.drafts.read(), pane));
        use_effect(move || {
            if let Err(error) = script.send(command()) {
                tracing::warn!("cannot update the find: {error}");
            }
        });
    }
}

/// The chat's find, under the header. Each agent keeps its own: a switch shows the other agent's
/// at once, and only the agent that closes its find sees it leave.
#[component]
pub(super) fn ChatFindBar() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let selected = state.ui.selected;
    let open = selected.filter(|agent| state.ui.chat_finds.contains(agent));
    let history = open
        .and_then(|agent| state.agents.get(&agent))
        .map(|entry| entry.transcript.history().clone());
    drop(state);
    let shown = use_presence(open);
    let Some(shown) = shown.filter(|shown| !shown.leaving || Some(shown.value) == selected) else {
        return rsx! {};
    };
    let agent = shown.value;
    let found = cockpit
        .drafts
        .read()
        .finds
        .get(FindPane::Chat, agent)
        .and_then(|draft| draft.found);
    let folded = found.map_or(0, |found| found.folded);
    rsx! {
        div {
            key: "{shown.opening}",
            class: if shown.leaving { "find-strip leaving" } else { "find-strip" },
            inert: shown.leaving.then_some(true),
            role: "search",
            aria_label: "Find in the chat",
            FindControls { pane: FindPane::Chat, agent }
            div { class: "find-notes",
                if folded > 0 {
                    span { class: "find-note", title: "Their calls open as the find reaches them",
                        span { class: "find-note-count", "{folded}" }
                        " of them in folded calls"
                    }
                }
                match history {
                    Some(HistoryState::Loaded { has_older: true }) => rsx! {
                        span { class: "find-note", "Older messages are not loaded." }
                        button {
                            class: "link-button find-load",
                            onclick: move |_| cockpit.change(|state| actions::load_older(state, agent)),
                            "Load them"
                        }
                    },
                    Some(HistoryState::LoadingOlder) => rsx! {
                        span { class: "find-note", role: "status",
                            span { class: "spinner", aria_hidden: true }
                            "Loading older messages"
                        }
                    },
                    _ => rsx! {},
                }
            }
            CloseButton { pane: FindPane::Chat, agent }
        }
    }
}

/// The file view's find, over the top right of the file.
#[component]
pub(super) fn FileFindBox(agent: AgentId, open: bool) -> Element {
    let shown = use_presence(open.then_some(agent));
    let Some(shown) = shown else {
        return rsx! {};
    };
    rsx! {
        div {
            key: "{shown.opening}",
            id: (!shown.leaving).then_some(FILE_BOX_ID),
            class: if shown.leaving { "find-box leaving" } else { "find-box" },
            inert: shown.leaving.then_some(true),
            role: "search",
            aria_label: "Find in the file",
            FindControls { pane: FindPane::File, agent }
            CloseButton { pane: FindPane::File, agent }
        }
    }
}

/// The field, the count, the case toggle and the arrows.
#[component]
fn FindControls(pane: FindPane, agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let finder: FindScript = use_context();
    let field_id = field_id(pane);
    let drafts = cockpit.drafts.read();
    let draft = drafts.finds.get(pane, agent).cloned().unwrap_or_default();
    drop(drafts);
    // Each request for the focus goes to one field, once: a field drawn again later, as after a
    // switch to another agent, leaves the focus where it is.
    use_effect(move || {
        let request = cockpit.state.read().ui.find_focus;
        let mut taken = finder.focus_taken;
        if request.pane == pane && request.serial != *taken.peek() {
            taken.set(request.serial);
            bridge::select_field(field_id);
        }
    });
    let count = count_text(&draft);
    let none = draft.found.is_some_and(|found| found.total == 0) && !draft.query.is_empty();
    let match_case = draft.match_case;
    let placeholder = match pane {
        FindPane::Chat => "Find in the chat",
        FindPane::File => "Find in the file",
    };
    rsx! {
        label { class: if none { "find-field none" } else { "find-field" },
            Icon { kind: IconKind::Search, size: 13 }
            input {
                id: field_id,
                r#type: "text",
                placeholder,
                aria_label: placeholder,
                spellcheck: false,
                autocomplete: "off",
                value: "{draft.query}",
                oninput: move |event| {
                    let query = event.value();
                    cockpit.with_drafts(|_, drafts| drafts.finds.get_mut(pane, agent).query = query);
                },
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        event.stop_propagation();
                        close(cockpit, pane, agent);
                    }
                },
            }
        }
        span { class: if none { "find-count none" } else { "find-count" }, aria_live: "polite", "{count}" }
        button {
            class: "find-button find-case",
            aria_pressed: match_case,
            aria_label: "Match case",
            title: "Match case",
            onclick: move |_| {
                cockpit.with_drafts(|_, drafts| {
                    let draft = drafts.finds.get_mut(pane, agent);
                    draft.match_case = !draft.match_case;
                });
            },
            "Aa"
        }
        span { class: "find-separator", aria_hidden: true }
        button {
            class: "find-button",
            aria_label: "Previous match",
            title: "Previous match (Shift+Enter)",
            onclick: move |_| finder.step(pane, false),
            Icon { kind: IconKind::ChevronUp, size: 14 }
        }
        button {
            class: "find-button",
            aria_label: "Next match",
            title: "Next match (Enter)",
            onclick: move |_| finder.step(pane, true),
            Icon { kind: IconKind::ChevronDown, size: 14 }
        }
    }
}

#[component]
fn CloseButton(pane: FindPane, agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "find-button find-close",
            aria_label: "Close the find",
            title: "Close (Esc)",
            onclick: move |_| close(cockpit, pane, agent),
            Icon { kind: IconKind::Close, size: 14 }
        }
    }
}

/// Closes the find, and gives the focus back to where the pane takes keys.
fn close(cockpit: Cockpit, pane: FindPane, agent: AgentId) {
    cockpit.update(|state| {
        find::close(state, pane, agent);
    });
    match pane {
        FindPane::Chat => bridge::focus_field(COMPOSER_ID),
        FindPane::File => bridge::focus_element(FILE_VIEW_ID.to_owned()),
    }
}

fn field_id(pane: FindPane) -> &'static str {
    match pane {
        FindPane::Chat => CHAT_FIELD_ID,
        FindPane::File => FILE_FIELD_ID,
    }
}

/// "3 of 28", "10000+" past the limit, or "No results". Nothing before the text or the page's
/// first count.
fn count_text(draft: &FindDraft) -> String {
    let Some(found) = draft.found.filter(|_| !draft.query.is_empty()) else {
        return String::new();
    };
    let more = if found.capped { "+" } else { "" };
    match found.current {
        _ if found.total == 0 => "No results".to_owned(),
        Some(current) => format!("{current} of {}{more}", found.total),
        None => format!("{}{more}", found.total),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::find::Found;

    #[test]
    fn the_count_says_where_the_find_is() {
        let draft = |query: &str, found: Option<Found>| FindDraft {
            query: query.to_owned(),
            match_case: false,
            found,
        };
        let found = Found {
            current: Some(3),
            total: 28,
            folded: 0,
            capped: false,
        };

        assert_eq!(count_text(&draft("column", Some(found))), "3 of 28");
        assert_eq!(
            count_text(&draft(
                "e",
                Some(Found {
                    capped: true,
                    total: 1_000,
                    ..found
                })
            )),
            "3 of 1000+"
        );
        assert_eq!(
            count_text(&draft(
                "columnz",
                Some(Found {
                    current: None,
                    total: 0,
                    ..found
                })
            )),
            "No results"
        );
        assert_eq!(count_text(&draft("", Some(found))), "");
        assert_eq!(count_text(&draft("column", None)), "");
    }

    /// The script reads these fields by name: a rename in Rust would make it read `undefined`.
    #[test]
    fn the_fields_the_script_reads_are_pinned() {
        let command = FindCommand::Find {
            pane: FindPane::File,
            open: true,
            agent: Some(AgentId::new(3)),
            query: "column".to_owned(),
            match_case: true,
            target: "src/lexer.rs".to_owned(),
        };
        let step = FindCommand::Step {
            pane: FindPane::Chat,
            forward: false,
        };
        let report = r#"{"pane":"chat","agent":3,"query":"col","match_case":false,"current":2,"total":9,"folded":4,"capped":false}"#;

        assert_eq!(
            serde_json::to_value(command).unwrap(),
            json!({
                "type": "find",
                "pane": "file",
                "open": true,
                "agent": 3,
                "query": "column",
                "match_case": true,
                "target": "src/lexer.rs",
            })
        );
        assert_eq!(
            serde_json::to_value(step).unwrap(),
            json!({ "type": "step", "pane": "chat", "forward": false })
        );
        let report: FindReport = serde_json::from_str(report).unwrap();
        assert_eq!(report.found.current, Some(2));
        assert_eq!(report.found.folded, 4);
    }
}
