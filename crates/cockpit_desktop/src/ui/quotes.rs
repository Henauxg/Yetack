//! Quotes of the chat with your comments: the Reply button over a selection, the comment field,
//! each quote's number beside its passage, the notes in the strip on the chat's right, and the
//! quotes listed in the composer. Once sent, a quoted passage keeps an underline that tells what
//! you replied.
//!
//! The components draw these elements and the page's script (`SCRIPT`) places them: it measures
//! the passages, which only the page knows, and draws their highlights with the CSS Custom
//! Highlight API, which leaves the chat's own elements as Dioxus rendered them.

use cockpit_protocol::AgentId;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use strum::{IntoStaticStr, VariantArray};

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::Cockpit;
use crate::prefs::CommentPlacement;
use crate::state::quotes::{self, EditorTarget, Passage, Quote, QuoteAnchor, QuoteEditor, QuoteId};
use crate::state::{AppState, Drafts, terminals};
use crate::ui::blocks::{BLOCK_CARD_CLASS, BLOCKS_CLASS};
use crate::ui::chat_scroll::{CHAT_SCROLL_ID, ENTRY_CLASS, REVEALED_CLASS};
use crate::ui::dialog_height;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::markdown::MARKDOWN_CLASS;
use crate::ui::presence::{LEAVING, Shown, use_presence};
use crate::ui::transcript::USER_BUBBLE_CLASS;
use crate::ui::voice::{CommentMic, CommentWords};
use crate::voice_desk::VoiceDesk;

/// The field where a comment is written.
pub const COMMENT_FIELD_ID: &str = "quote-comment";
/// Covers the chat area. The script places its children.
const LAYER_ID: &str = "quote-layer";
const REPLY_BUTTON_ID: &str = "quote-reply";
/// The lines from a passage to its note, which the script draws.
const LINKS_ID: &str = "quote-links";
/// Holds the "You replied" card, which the script makes.
const REPLIED_HOST_ID: &str = "quote-replied";
/// With Ctrl, opens the comment field on the chat's selection.
const REPLY_KEY: &str = "r";
/// The title of what scrolls to a quote's passage.
const SHOW_PASSAGE: &str = "Show the passage";
/// The `data-quote` of the comment field on a passage just selected, in place of a quote's id.
const NEW_FIELD: &str = "new";
const PIN_GROUP_CLASS: &str = "quote-pin-group";
const NOTE_CLASS: &str = "quote-note";
/// The line under the notes that counts the ones with no room, with their numbers.
const MORE_ID: &str = "quote-more";
const MORE_ITEM_CLASS: &str = "quote-more-item";
const MORE_COUNT_CLASS: &str = "quote-more-count";
/// A note on one line that shows in full.
const OPEN_NOTE: &str = "[data-open], :focus-within";
const FIELD_CLASS: &str = "quote-editor";
/// A comment's text that opens the field on a click, with the caret where the click fell.
const COMMENT_TEXT_CLASS: &str = "quote-comment";
/// Marks the comment field while a dictation runs into it: Enter there stops the dictation (the
/// page's key script sends it) and adds no quote.
const DICTATING_ATTRIBUTE: &str = "data-dictating";
/// The comment field's last row, the key hints or a dictation's words, whose height follows them.
const FIELD_FOOT_ID: &str = "quote-field-foot";

/// How the comment field shows: as a note in the strip, or as a card under its passage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum FieldForm {
    Note,
    Card,
}

/// The comment field, with the number of the quote it edits.
#[derive(Debug, Clone, PartialEq)]
struct FieldView {
    editor: QuoteEditor,
    number: Option<usize>,
    form: FieldForm,
}

/// Waits for its settings, then places the quotes' elements and draws their highlights. Message
/// and field names are serde's names for `Settings`, `QuoteCommand` and `QuoteMessage`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
const send = (message) => dioxus.send(message);
// WebKitGTK before 2.44 has no highlights: the numbers and notes still show.
const highlights = typeof CSS !== "undefined" && "highlights" in CSS && typeof Highlight === "function";
const drawn = new Map();
// Ranges are live: one drawn stays right while the page changes, so a highlight is set again only
// for other ranges.
const rangeIds = new WeakMap();
let lastRangeId = 0;
const rangeId = (range) => {
  if (!rangeIds.has(range)) {
    lastRangeId += 1;
    rangeIds.set(range, lastRangeId);
  }
  return rangeIds.get(range);
};
const highlight = (name, ranges) => {
  if (!highlights) {
    return;
  }
  const key = ranges.map(rangeId).join(" ");
  if (drawn.get(name) === key) {
    return;
  }
  drawn.set(name, key);
  if (ranges.length === 0) {
    CSS.highlights.delete(name);
  } else {
    CSS.highlights.set(name, new Highlight(...ranges));
  }
};
let layout = { agent: null, quotes: [], editor: null };
// Where a click on a comment's text fell, for the caret of the field it opens.
let caretClick = null;
const chat = () => document.getElementById(settings.chat_scroll_id);
const layer = () => document.getElementById(settings.layer_id);
// An entry's wrapper ends with the element that shows its row (see the chat's scroll script).
const shownOf = (entry) => entry.lastElementChild;
const entryOf = (node) => {
  const element = node instanceof Element ? node : node.parentElement;
  return element === null ? null : element.closest(`#${settings.chat_scroll_id} .${settings.entry_class}`);
};
// An entry's id is `entry-<agent>-<item>` (see `entry_id`).
const idParts = (entry) => entry.id.split("-").map(Number);
// Offsets count UTF-16 units of text, as `Range.toString` does.
const offsetIn = (root, node, offset) => {
  const range = document.createRange();
  range.setStart(root, 0);
  range.setEnd(node, offset);
  return range.toString().length;
};
const rangeIn = (root, start, end) => {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  let seen = 0;
  let started = false;
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const length = node.data.length;
    if (!started && start <= seen + length) {
      range.setStart(node, start - seen);
      started = true;
    }
    if (started && end <= seen + length) {
      range.setEnd(node, end - seen);
      return range;
    }
    seen += length;
  }
  return null;
};
// The passage at its offsets, or, when the entry's text moved, its occurrence nearest to them.
const resolve = (anchor, raw) => {
  const entry = document.getElementById(`entry-${layout.agent}-${anchor.item}`);
  const root = entry === null ? null : shownOf(entry);
  if (root === null) {
    return null;
  }
  const range = rangeIn(root, anchor.start, anchor.end);
  if (range !== null && range.toString() === raw) {
    return range;
  }
  const text = root.textContent;
  let best = -1;
  for (let at = text.indexOf(raw); at !== -1; at = text.indexOf(raw, at + 1)) {
    if (best === -1 || Math.abs(at - anchor.start) < Math.abs(best - anchor.start)) {
      best = at;
    }
  }
  return best === -1 ? null : rangeIn(root, best, best + raw.length);
};
const ranges = new Map();
const rangeOf = (key, anchor, raw) => {
  const kept = ranges.get(key);
  if (kept !== undefined && kept.range.startContainer.isConnected && kept.range.toString() === raw) {
    return kept.range;
  }
  const range = resolve(anchor, raw);
  if (range === null) {
    ranges.delete(key);
  } else {
    ranges.set(key, { range });
  }
  return range;
};
// The range's lines that show: a folded row, or a folded message scrolled down, hides some.
const lineRects = (range) => {
  let rects = Array.from(range.getClientRects()).filter((rect) => rect.width > 0 && rect.height > 0);
  const box = chat();
  const common = range.commonAncestorContainer;
  for (let element = common instanceof Element ? common : common.parentElement; element !== null && element !== box; element = element.parentElement) {
    if (element.scrollHeight > element.clientHeight && getComputedStyle(element).overflowY !== "visible") {
      const edges = element.getBoundingClientRect();
      rects = rects.filter((rect) => rect.bottom > edges.top && rect.top < edges.bottom);
    }
  }
  return rects;
};
const codeSelector = "pre, code, .code-block, .code-lines, .io-text, .diff-box";
// The chat's selection, when it lies in one entry of the shown agent.
const selected = () => {
  const selection = document.getSelection();
  if (selection === null || selection.isCollapsed || selection.rangeCount === 0 || layout.agent === null) {
    return null;
  }
  const range = selection.getRangeAt(0);
  const entry = entryOf(range.startContainer);
  if (entry === null || entryOf(range.endContainer) !== entry) {
    return null;
  }
  const root = shownOf(entry);
  const [, agent, item] = idParts(entry);
  if (root === null || agent !== layout.agent || !root.contains(range.startContainer) || !root.contains(range.endContainer)) {
    return null;
  }
  const text = selection.toString();
  const raw = range.toString();
  if (text.trim() === "" || raw === "") {
    return null;
  }
  const start = offsetIn(root, range.startContainer, range.startOffset);
  const common = range.commonAncestorContainer;
  const container = common instanceof Element ? common : common.parentElement;
  const code = container !== null && container.closest(codeSelector) !== null;
  return { range, passage: { anchor: { item, start, end: start + raw.length }, raw, text, code } };
};
const reply = () => {
  const found = selected();
  if (found === null) {
    return;
  }
  send({ type: "reply", agent: layout.agent, passage: found.passage });
  document.getSelection()?.removeAllRanges();
  schedule();
};
// Places by `left` and `top`: a popover's entry animation moves its `transform`. A note keeps the
// strip's right edge from the style sheet, so it takes no `left`.
const at = (element, x, y) => {
  element.style.left = x === null ? "" : `${Math.round(x)}px`;
  element.style.top = `${Math.round(y)}px`;
  element.dataset.placed = "";
};
const away = (element) => {
  delete element.dataset.placed;
};
const placeReply = (area, view) => {
  const button = document.getElementById(settings.reply_button_id);
  if (button === null) {
    return;
  }
  const found = selected();
  const rects = found === null ? [] : lineRects(found.range);
  if (rects.length === 0) {
    away(button);
    return;
  }
  const first = rects[0];
  const last = rects[rects.length - 1];
  const width = button.offsetWidth;
  const height = button.offsetHeight;
  const x = Math.min(Math.max(first.right - width, area.left + 8), area.right - width - 8);
  const above = first.top - height - 6;
  const y = above >= view.top + 4 ? above : last.bottom + 6;
  at(button, x - area.left, y - area.top);
};
// The links from passages to their notes.
const drawLinks = (paths) => {
  const svg = document.getElementById(settings.links_id);
  if (svg === null) {
    return;
  }
  while (svg.childElementCount > paths.length) {
    svg.lastElementChild.remove();
  }
  while (svg.childElementCount < paths.length) {
    svg.appendChild(document.createElementNS("http://www.w3.org/2000/svg", "path"));
  }
  paths.forEach((d, index) => {
    const path = svg.children[index];
    if (path.getAttribute("d") !== d) {
      path.setAttribute("d", d);
    }
  });
};
const place = () => {
  const area = layer()?.getBoundingClientRect();
  const box = chat();
  if (area === undefined || box === null) {
    highlight("quote-pending", []);
    return;
  }
  const editor = document.querySelector(`#${settings.layer_id} .${settings.field_class}:not(.${settings.leaving_class})`);
  watch(box, editor);
  const view = box.getBoundingClientRect();
  const column = box.firstElementChild;
  const textRight = column === null ? view.right : column.getBoundingClientRect().right - parseFloat(getComputedStyle(column).paddingRight);
  scanSent(box);
  placeReply(area, view);
  // The quotes and the passage being commented, in the chat's order.
  const items = layout.quotes.map((quote) => ({ key: `${layout.agent}:${quote.id}`, id: String(quote.id), anchor: quote.anchor, raw: quote.raw }));
  if (layout.editor !== null) {
    const id = layout.editor.quote === null ? settings.new_field : String(layout.editor.quote);
    if (id === settings.new_field) {
      items.push({ key: `${layout.agent}:new:${layout.editor.anchor.item}:${layout.editor.anchor.start}`, id, anchor: layout.editor.anchor, raw: layout.editor.raw });
    }
  }
  items.sort((a, b) => a.anchor.item - b.anchor.item || a.anchor.start - b.anchor.start);
  // A range holds its nodes: one kept for a quote that went would keep a removed chat alive.
  const current = new Set(items.map((item) => item.key));
  for (const key of ranges.keys()) {
    if (!current.has(key)) {
      ranges.delete(key);
    }
  }
  const found = [];
  for (const item of items) {
    const range = rangeOf(item.key, item.anchor, item.raw);
    const rects = range === null ? [] : lineRects(range);
    found.push({ ...item, range, rects });
  }
  highlight("quote-pending", found.filter((item) => item.range !== null).map((item) => item.range));
  // Numbers sit at the end of the passage's last line, past the text's right edge.
  const pinX = textRight - area.left + 2;
  let lastPinY = -Infinity;
  for (const item of found) {
    const group = document.querySelector(`#${settings.layer_id} .${settings.pin_group_class}[data-quote="${item.id}"]`);
    if (group === null) {
      continue;
    }
    group.toggleAttribute("data-editing", editor !== null && editor.dataset.quote === item.id);
    const last = item.rects[item.rects.length - 1];
    const middle = last === undefined ? 0 : last.top + last.height / 2;
    if (last === undefined || middle < view.top + 9 || middle > view.bottom - 9) {
      away(group);
      continue;
    }
    let y = middle - 9 - area.top;
    // Two passages that end on one line: the second number sits below the first.
    if (y < lastPinY + 20) {
      y = lastPinY + 20;
    }
    lastPinY = y;
    at(group, pinX, y);
    group.toggleAttribute("data-up", last.top > view.top + view.height / 2);
  }
  // Notes go in the strip, below the cards, in the chat's order, each below the one above. When
  // they do not all fit, every note takes one line; the ones with no room even then show their
  // numbers in the "+N" line under the others.
  const cards = document.querySelectorAll(`.${settings.blocks_class} .${settings.block_card_class}`);
  const floor = cards.length > 0 ? cards[cards.length - 1].getBoundingClientRect().bottom + 10 : view.top + 8;
  const bottom = area.bottom - 8;
  const strip = layer();
  const more = document.getElementById(settings.more_id);
  const shown = [];
  for (const item of found) {
    const group = document.querySelector(`#${settings.layer_id} .${settings.pin_group_class}[data-quote="${item.id}"]`);
    const own = document.querySelector(`#${settings.layer_id} .${settings.note_class}[data-quote="${item.id}"]`);
    const editing = editor !== null && editor.dataset.quote === item.id && editor.dataset.form === settings.note_form;
    const note = editing ? editor : own;
    group?.removeAttribute("data-note-visible");
    if (note === null) {
      continue;
    }
    if (!editing && note.dataset.placement !== settings.beside) {
      away(note);
      continue;
    }
    const first = item.rects[0];
    const last = item.rects[item.rects.length - 1];
    // The field keeps its form, in the strip or as a card, while its passage stays where it was
    // when the form was chosen: growing or shrinking never moves it from one to the other.
    const passageTop = first === undefined ? "" : String(Math.round(first.top));
    const kept = editing && editor.dataset.formAt === passageTop;
    if (kept && editor.dataset.fallback !== undefined) {
      continue;
    }
    // The field is measured in the strip's width. As a card it is wider, so its text takes fewer
    // lines: measured there, it would fit, move back, grow, and fall back again on every round.
    if (editing) {
      editor.dataset.formAt = passageTop;
      delete editor.dataset.fallback;
    }
    if (first === undefined || !(last.bottom > view.top && first.top < view.bottom)) {
      if (editing) {
        editor.dataset.fallback = "";
      } else {
        away(note);
      }
      continue;
    }
    shown.push({ id: item.id, group, note, editing, kept, first, last });
  }
  // The tops of the notes, each below the one above, and which fit above `limit`. The notes keep
  // the chat's order: once one has no room, none after it shows. A field with no room shows as a
  // card instead, and leaves its place to the notes after it. A field kept in the strip stays
  // there, and moves up as far as the notes above it allow.
  const stack = (heights, limit) => {
    const tops = [];
    const fits = [];
    let below = -Infinity;
    let blocked = false;
    for (const [index, entry] of shown.entries()) {
      let top = Math.max(entry.first.top - 10, floor, below + 8);
      let fit = !blocked && top + heights[index] <= limit;
      if (entry.kept && !fit) {
        top = Math.max(floor, below + 8, Math.min(top, limit - heights[index]));
        fit = true;
      }
      tops.push(top);
      fits.push(fit);
      if (fit) {
        below = top + heights[index];
      } else if (!entry.editing) {
        blocked = true;
      }
    }
    return { tops, fits, below };
  };
  const noteLeft = (placed) => shown.some((entry, index) => !entry.editing && !placed.fits[index]);
  // Full size first. Notes on one line are measured by copies out of the strip, which keeps its
  // form: leaving it would restart the animation of an opened note on every round.
  const fullHeights = () => {
    if (!strip.hasAttribute("data-compact")) {
      return shown.map((entry) => entry.note.offsetHeight);
    }
    const host = document.createElement("div");
    host.className = settings.measure_class;
    const copies = shown.map((entry) => {
      if (entry.editing) {
        return null;
      }
      const copy = entry.note.cloneNode(true);
      delete copy.dataset.open;
      host.appendChild(copy);
      return copy;
    });
    strip.appendChild(host);
    const heights = shown.map((entry, index) => copies[index]?.offsetHeight ?? entry.note.offsetHeight);
    host.remove();
    return heights;
  };
  const full = fullHeights();
  const compact = noteLeft(stack(full, bottom));
  if (compact) {
    strip.setAttribute("data-compact", "");
  } else {
    strip.removeAttribute("data-compact");
    shown.forEach((entry) => foldNote(entry.note));
  }
  // An opened line shows over the lines below, which keep the place of a line.
  const lineHeight = shown.find((entry) => !entry.editing && !entry.note.matches(settings.open_note))?.note.offsetHeight ?? 30;
  const heights = shown.map((entry, index) => {
    if (!compact) {
      return full[index];
    }
    return !entry.editing && entry.note.matches(settings.open_note) ? lineHeight : entry.note.offsetHeight;
  });
  let placed = stack(heights, bottom);
  const hidden = new Set();
  // The "+N" line takes the last place, as tall as its numbers make it, which more notes going
  // there can make taller: a few rounds settle it.
  if (compact && noteLeft(placed) && more !== null) {
    let reserved = 0;
    for (let round = 0; round < 4; round++) {
      hidden.clear();
      shown.forEach((entry, index) => {
        if (!entry.editing && !placed.fits[index]) {
          hidden.add(entry.id);
        }
      });
      for (const item of more.querySelectorAll(`.${settings.more_item_class}`)) {
        item.toggleAttribute("data-shown", hidden.has(item.dataset.quote));
      }
      const height = more.offsetHeight;
      if (height <= reserved) {
        break;
      }
      reserved = height;
      placed = stack(heights, bottom - reserved - 8);
    }
  }
  const paths = [];
  let below = -Infinity;
  for (const [index, entry] of shown.entries()) {
    const { note, editing, group, last } = entry;
    if (!placed.fits[index]) {
      if (editing) {
        editor.dataset.fallback = "";
      } else {
        away(note);
        foldNote(note);
      }
      continue;
    }
    const slot = placed.tops[index];
    const top = compact && !editing && note.matches(settings.open_note)
      ? Math.max(Math.min(slot, bottom - note.offsetHeight), floor)
      : slot;
    below = slot + heights[index];
    at(note, null, top - area.top);
    const left = note.getBoundingClientRect().left;
    group?.setAttribute("data-note-visible", "");
    const x1 = pinX + 20;
    const y1 = last.top + last.height / 2 - area.top;
    const x2 = left - area.left;
    const middle = x2 - 12;
    const y2 = slot - area.top + (compact && !editing ? lineHeight / 2 : 17);
    if (x2 > x1) {
      paths.push(`M${x1} ${y1} H${middle} V${y2} H${x2}`);
    }
  }
  if (more !== null) {
    for (const item of more.querySelectorAll(`.${settings.more_item_class}`)) {
      item.toggleAttribute("data-shown", hidden.has(item.dataset.quote));
    }
    const count = more.querySelector(`.${settings.more_count_class}`);
    if (count !== null) {
      count.textContent = `+${hidden.size} more`;
    }
    if (hidden.size > 0) {
      at(more, null, Math.max(below + 8, floor) - area.top);
    } else {
      away(more);
    }
  }
  drawLinks(paths);
  // The comment field as a card: in a narrow chat, for a hidden comment, or when the strip has no
  // room. A quote's field takes its number's card place, a new quote's goes under its passage.
  if (editor !== null && (editor.dataset.form !== settings.note_form || editor.dataset.fallback !== undefined)) {
    const item = found.find((candidate) => candidate.id === editor.dataset.quote);
    const first = item?.rects[0];
    const last = item?.rects[item.rects.length - 1];
    const width = editor.offsetWidth;
    const height = editor.offsetHeight;
    let x;
    let y;
    if (editor.dataset.quote !== settings.new_field && last !== undefined) {
      // Where the number is, placed or not: the field stays in its column while the chat scrolls,
      // and on the side it opened on, so it never jumps.
      const numberTop = last.top + last.height / 2 - 9;
      if (editor.dataset.sideOf !== editor.dataset.quote) {
        editor.dataset.sideOf = editor.dataset.quote;
        editor.dataset.side = last.top > view.top + view.height / 2 ? "up" : "down";
      }
      x = Math.min(Math.max(textRight + 24 - width, area.left + 8), area.right - width - 8);
      y = editor.dataset.side === "up" ? numberTop - 8 - height : numberTop + 26;
    } else {
      x = first === undefined ? view.left + 20 : first.left;
      x = Math.min(Math.max(x, area.left + 8), textRight - width);
      y = last === undefined ? view.bottom - height - 8 : last.bottom + 8;
      // The side is chosen where the passage is, and kept while only the field grows or shrinks:
      // a field under its passage that grows past the view would jump above it.
      const passageTop = first === undefined ? "" : String(Math.round(first.top));
      if (editor.dataset.passageAt !== passageTop) {
        editor.dataset.passageAt = passageTop;
        editor.dataset.passageSide = y + height > view.bottom - 8 && first !== undefined ? "up" : "down";
      }
      if (editor.dataset.passageSide === "up" && first !== undefined) {
        y = first.top - height - 8;
      }
    }
    y = Math.min(Math.max(y, view.top + 8), view.bottom - height - 8);
    at(editor, x - area.left, y - area.top);
  }
  // A hidden field takes no focus: it takes it once placed, the first time.
  if (editor !== null && editor.dataset.placed !== undefined && editor.dataset.focused === undefined) {
    editor.dataset.focused = "";
    const field = editor.querySelector("textarea");
    if (field !== null) {
      const clicked = caretClick !== null && caretClick.quote === editor.dataset.quote ? caretClick.offset : field.value.length;
      const caret = Math.min(clicked, field.value.length);
      field.focus();
      field.setSelectionRange(caret, caret);
    }
    caretClick = null;
  }
};
// After a send: each quote of a message you sent underlines its passage in an earlier entry.
const sentFound = new WeakMap();
let sent = [];
const normalize = (text) => text.replace(/\s+/g, " ").trim();
const findNormalized = (root, wanted) => {
  if (!normalize(root.textContent).includes(wanted)) {
    return null;
  }
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let text = "";
  const nodes = [];
  const offsets = [];
  let space = true;
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const data = node.data;
    for (let index = 0; index < data.length; index += 1) {
      const blank = /\s/.test(data[index]);
      if (blank && space) {
        continue;
      }
      text += blank ? " " : data[index];
      nodes.push(node);
      offsets.push(index);
      space = blank;
    }
  }
  const start = text.indexOf(wanted);
  if (start === -1) {
    return null;
  }
  const end = start + wanted.length - 1;
  const range = document.createRange();
  range.setStart(nodes[start], offsets[start]);
  range.setEnd(nodes[end], offsets[end] + 1);
  return range;
};
// The text after a quote, up to the next quote: the comment on it, at most its first block.
const commentAfter = (quote) => {
  const next = quote.nextElementSibling;
  return next === null || next.tagName === "BLOCKQUOTE" ? "" : normalize(next.textContent);
};
const unlink = (quote) => {
  for (const name of ["data-quote-link", "tabindex", "role", "title"]) {
    quote.removeAttribute(name);
  }
};
let sentDirty = true;
const scanSent = (box) => {
  if (!sentDirty) {
    return;
  }
  sentDirty = false;
  const entries = Array.from(box.getElementsByClassName(settings.entry_class));
  const found = [];
  entries.forEach((entry, index) => {
    for (const quote of entry.querySelectorAll(`.${settings.user_bubble_class} .${settings.markdown_class} > blockquote`)) {
      const wanted = normalize(quote.textContent);
      let hit = sentFound.get(quote);
      // A passage not found stays so until older entries load above it.
      if (hit !== undefined && hit.missed !== undefined && hit.missed === index) {
        unlink(quote);
        continue;
      }
      if (hit !== undefined && (hit.missed !== undefined || !(hit.range.startContainer.isConnected && normalize(hit.range.toString()) === wanted))) {
        hit = undefined;
      }
      for (let back = index - 1; hit === undefined && back >= 0 && wanted.length > 0; back -= 1) {
        const root = shownOf(entries[back]);
        const range = root === null ? null : findNormalized(root, wanted);
        if (range !== null) {
          hit = { range, entry, comment: commentAfter(quote) };
          sentFound.set(quote, hit);
        }
      }
      if (hit === undefined) {
        sentFound.set(quote, { missed: index });
        unlink(quote);
        continue;
      }
      if (!quote.hasAttribute("data-quote-link")) {
        quote.setAttribute("data-quote-link", "");
        quote.setAttribute("tabindex", "0");
        quote.setAttribute("role", "button");
        quote.setAttribute("title", settings.show_passage);
      }
      found.push(hit);
    }
  });
  sent = found;
  highlight("quote-sent", sent.map((hit) => hit.range));
};
// Scrolls the chat until `rect()` sits in the middle of its view, once now and once the entries
// that came into view took their real height.
const center = (rect) => {
  const box = chat();
  if (box === null) {
    return;
  }
  const scroll = () => {
    const target = rect();
    const view = box.getBoundingClientRect();
    box.scrollTop += target.top - view.top - Math.max(12, (box.clientHeight - target.height) / 2);
  };
  scroll();
  requestAnimationFrame(scroll);
};
let flashTimer = null;
const flash = (range) => {
  highlight("quote-flash", [range]);
  clearTimeout(flashTimer);
  flashTimer = setTimeout(() => highlight("quote-flash", []), settings.flash_ms);
};
const revealRange = (range) => {
  center(() => range.getBoundingClientRect());
  flash(range);
};
const revealEntry = (entry) => {
  const element = shownOf(entry);
  if (element === null) {
    return;
  }
  center(() => element.getBoundingClientRect());
  element.classList.remove(settings.revealed_class);
  void element.offsetWidth;
  element.classList.add(settings.revealed_class);
  element.addEventListener("animationend", () => element.classList.remove(settings.revealed_class), { once: true });
};
// "You replied", while the pointer is on an underlined passage or on the card.
let replied = null;
let pointer = null;
let hideTimer = null;
const repliedCard = (hit) => {
  const card = document.createElement("div");
  card.className = "popover replied-card";
  const head = document.createElement("span");
  head.className = "replied-head";
  head.textContent = settings.you_replied;
  card.appendChild(head);
  if (hit.comment !== "") {
    const comment = document.createElement("p");
    comment.textContent = hit.comment;
    card.appendChild(comment);
  }
  const go = document.createElement("button");
  go.type = "button";
  go.className = "link-button replied-go";
  go.textContent = settings.show_message;
  go.addEventListener("click", () => {
    hideReplied();
    revealEntry(hit.entry);
  });
  card.appendChild(go);
  return card;
};
const hideReplied = () => {
  clearTimeout(hideTimer);
  hideTimer = null;
  document.getElementById(settings.replied_host_id)?.replaceChildren();
  replied = null;
};
const inside = (rect, point, margin) => point.x >= rect.left - margin && point.x <= rect.right + margin
  && point.y >= rect.top - margin && point.y <= rect.bottom + margin;
const hover = () => {
  const host = document.getElementById(settings.replied_host_id);
  const area = layer()?.getBoundingClientRect();
  const view = chat()?.getBoundingClientRect();
  if (host === null || area === undefined || view === undefined || pointer === null) {
    return;
  }
  const card = host.firstElementChild;
  if (card !== null && inside(card.getBoundingClientRect(), pointer, 4)) {
    clearTimeout(hideTimer);
    hideTimer = null;
    return;
  }
  let hit = null;
  let hitRect = null;
  for (const candidate of sent) {
    const rect = lineRects(candidate.range).find((line) => inside(line, pointer, 1));
    if (rect !== undefined) {
      hit = candidate;
      hitRect = rect;
      break;
    }
  }
  if (hit === null) {
    if (replied !== null && hideTimer === null) {
      hideTimer = setTimeout(hideReplied, settings.hide_ms);
    }
    return;
  }
  clearTimeout(hideTimer);
  hideTimer = null;
  if (replied === hit) {
    return;
  }
  replied = hit;
  const shown = repliedCard(hit);
  host.replaceChildren(shown);
  const x = Math.min(hitRect.left, area.right - shown.offsetWidth - 8);
  let y = hitRect.bottom + 6;
  if (y + shown.offsetHeight > view.bottom - 8) {
    y = hitRect.top - 6 - shown.offsetHeight;
  }
  at(shown, x - area.left, y - area.top);
};
let hoverQueued = false;
document.addEventListener("mousemove", (event) => {
  pointer = { x: event.clientX, y: event.clientY };
  if (!hoverQueued) {
    hoverQueued = true;
    requestAnimationFrame(() => {
      hoverQueued = false;
      hover();
    });
  }
}, { passive: true });
let queued = false;
const schedule = () => {
  if (!queued) {
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      place();
    });
  }
};
// The comment field's own height places it in the same frame: a field above its passage that
// grows would cover the passage for a frame.
const resizes = new ResizeObserver((entries) => {
  if (entries.some((entry) => entry.target.classList.contains(settings.field_class))) {
    place();
  } else {
    schedule();
  }
});
let watched = [];
const watch = (box, editor) => {
  const now = [box, box.firstElementChild, document.querySelector(`.${settings.blocks_class}`), layer(), editor].filter((element) => element !== null);
  if (now.length === watched.length && now.every((element, index) => element === watched[index])) {
    return;
  }
  resizes.disconnect();
  now.forEach((element) => resizes.observe(element));
  watched = now;
};
new MutationObserver((records) => {
  const own = layer();
  if (records.some((record) => own === null || !own.contains(record.target))) {
    sentDirty = true;
    schedule();
  }
}).observe(document.body, { childList: true, subtree: true, characterData: true });
document.addEventListener("scroll", () => {
  schedule();
  if (replied !== null) {
    hideReplied();
  }
}, { capture: true, passive: true });
document.addEventListener("selectionchange", schedule);
// A part of the comment field that animates its height has set its first frame.
document.addEventListener(settings.height_event, (event) => {
  if (event.target instanceof Element && event.target.closest(`.${settings.field_class}`) !== null) {
    place();
  }
});
window.addEventListener("resize", schedule);
// A press on Reply keeps the selection it replies to.
document.addEventListener("pointerdown", (event) => {
  if (event.target instanceof Element && event.target.closest(`#${settings.reply_button_id}`) !== null) {
    event.preventDefault();
  }
}, true);
const revealSent = (quote) => {
  const hit = sentFound.get(quote);
  if (hit !== undefined && hit.range !== undefined) {
    revealRange(hit.range);
  }
};
const openNotes = () => document.querySelectorAll(`#${settings.layer_id} .${settings.note_class}[data-open]`);
const openNote = (note) => {
  note.dataset.open = "";
  note.setAttribute(settings.takes_escape_attribute, "");
  schedule();
};
const foldNote = (note) => {
  delete note.dataset.open;
  note.removeAttribute(settings.takes_escape_attribute);
};
const foldNotes = (except) => {
  let folded = false;
  for (const note of openNotes()) {
    if (note !== except) {
      foldNote(note);
      folded = true;
    }
  }
  if (folded) {
    schedule();
  }
  return folded;
};
document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element)) {
    return;
  }
  const note = event.target.closest(`#${settings.layer_id}[data-compact] .${settings.note_class}`);
  foldNotes(note);
  // On one line, a click opens the note; its buttons still act at once.
  if (note !== null && !note.matches(settings.open_note) && event.target.closest("button") === null) {
    event.preventDefault();
    event.stopPropagation();
    openNote(note);
  }
}, true);
// A note on one line shows in full while it holds the focus: its place follows.
document.addEventListener("focusin", (event) => {
  if (event.target instanceof Element && event.target.closest(`#${settings.layer_id}`) !== null) {
    schedule();
  }
});
document.addEventListener("focusout", (event) => {
  if (event.target instanceof Element && event.target.closest(`#${settings.layer_id}`) !== null) {
    schedule();
  }
});
document.addEventListener("mouseout", (event) => {
  if (!(event.target instanceof Element)) {
    return;
  }
  const note = event.target.closest(`.${settings.note_class}[data-open]`);
  if (note !== null && !(event.relatedTarget instanceof Node && note.contains(event.relatedTarget))) {
    foldNote(note);
    schedule();
  }
});
document.addEventListener("click", (event) => {
  caretClick = null;
  if (!(event.target instanceof Element)) {
    return;
  }
  const comment = event.target.closest(`.${settings.comment_text_class}[data-quote]`);
  if (comment !== null) {
    const point = document.caretRangeFromPoint(event.clientX, event.clientY);
    if (point !== null && comment.contains(point.startContainer)) {
      caretClick = { quote: comment.dataset.quote, offset: point.startOffset };
    }
    return;
  }
  if (event.target.closest(`#${settings.reply_button_id}`) !== null) {
    reply();
    return;
  }
  const quote = event.target.closest("[data-quote-link]");
  const own = event.target.closest("a, button");
  const ownInQuote = own !== null && quote !== null && quote.contains(own);
  if (quote !== null && !ownInQuote && document.getSelection()?.isCollapsed !== false) {
    revealSent(quote);
  }
});
document.addEventListener("keydown", (event) => {
  if (event.isComposing) {
    return;
  }
  const command = event.ctrlKey || event.metaKey;
  const target = event.target;
  if (target instanceof Element && target.closest(`[${settings.records_keys_attribute}]`) !== null) {
    return;
  }
  if (command && !event.altKey && !event.shiftKey && event.key.toLowerCase() === settings.reply_key) {
    // Ctrl+R would reload the page.
    event.preventDefault();
    reply();
    return;
  }
  if (command || event.altKey) {
    return;
  }
  // An Esc that folds a note does nothing else: the page's key script leaves it alone.
  if (event.key === "Escape" && foldNotes(null)) {
    event.preventDefault();
    return;
  }
  if (target instanceof HTMLTextAreaElement && target.id === settings.comment_field_id && event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    // That Enter stops the dictation: the page's key script sends it. Its repeats add nothing,
    // even once the dictation's text is in.
    if (!event.repeat && !target.hasAttribute(settings.dictating_attribute)) {
      send({ type: "commit", text: target.value });
    }
    return;
  }
  if (target instanceof Element && target.matches("[data-quote-link]") && (event.key === "Enter" || event.key === " ")) {
    event.preventDefault();
    revealSent(target);
  }
}, true);
for (;;) {
  const command = await dioxus.recv();
  if (command.type === "layout") {
    layout = command;
    schedule();
  } else if (command.type === "reveal") {
    const kept = ranges.get(`${layout.agent}:${command.id}`);
    if (kept !== undefined) {
      revealRange(kept.range);
    }
  }
}
"#;

#[derive(Debug, Serialize)]
struct Settings {
    chat_scroll_id: &'static str,
    entry_class: &'static str,
    layer_id: &'static str,
    reply_button_id: &'static str,
    links_id: &'static str,
    replied_host_id: &'static str,
    comment_field_id: &'static str,
    /// Marks the comment field while a dictation runs into it: Enter there adds no quote.
    dictating_attribute: &'static str,
    /// What a part of the comment field sends once its height changed or started to.
    height_event: &'static str,
    revealed_class: &'static str,
    reply_key: &'static str,
    /// Marks the element that records a shortcut, whose keys the script leaves alone.
    records_keys_attribute: &'static str,
    /// How long a passage the chat scrolled to stays lit.
    flash_ms: u32,
    /// How long "You replied" waits for the pointer to come back before it goes.
    hide_ms: u32,
    you_replied: &'static str,
    show_message: &'static str,
    show_passage: &'static str,
    new_field: &'static str,
    pin_group_class: &'static str,
    note_class: &'static str,
    more_id: &'static str,
    more_item_class: &'static str,
    more_count_class: &'static str,
    /// Matches a note on one line that shows in full: opened by a click, or holding the focus.
    open_note: &'static str,
    /// Holds the copies of the notes the script measures at full size.
    measure_class: &'static str,
    /// Marks an opened note, which Esc folds, for the page's key script.
    takes_escape_attribute: &'static str,
    field_class: &'static str,
    comment_text_class: &'static str,
    leaving_class: &'static str,
    blocks_class: &'static str,
    block_card_class: &'static str,
    user_bubble_class: &'static str,
    markdown_class: &'static str,
    /// The field's form that shows in the strip.
    note_form: &'static str,
    /// The placement of a note that shows.
    beside: &'static str,
}

impl Settings {
    fn new() -> Self {
        Self {
            chat_scroll_id: CHAT_SCROLL_ID,
            entry_class: ENTRY_CLASS,
            layer_id: LAYER_ID,
            reply_button_id: REPLY_BUTTON_ID,
            links_id: LINKS_ID,
            replied_host_id: REPLIED_HOST_ID,
            comment_field_id: COMMENT_FIELD_ID,
            dictating_attribute: DICTATING_ATTRIBUTE,
            height_event: dialog_height::HEIGHT_EVENT,
            revealed_class: REVEALED_CLASS,
            reply_key: REPLY_KEY,
            records_keys_attribute: bridge::RECORDS_KEYS_ATTRIBUTE,
            flash_ms: 1200,
            hide_ms: 250,
            you_replied: "You replied",
            show_message: "Show your message",
            show_passage: SHOW_PASSAGE,
            new_field: NEW_FIELD,
            pin_group_class: PIN_GROUP_CLASS,
            note_class: NOTE_CLASS,
            more_id: MORE_ID,
            more_item_class: MORE_ITEM_CLASS,
            more_count_class: MORE_COUNT_CLASS,
            open_note: OPEN_NOTE,
            measure_class: "quote-measure",
            takes_escape_attribute: bridge::TAKES_ESCAPE_ATTRIBUTE,
            field_class: FIELD_CLASS,
            comment_text_class: COMMENT_TEXT_CLASS,
            leaving_class: LEAVING,
            blocks_class: BLOCKS_CLASS,
            block_card_class: BLOCK_CARD_CLASS,
            user_bubble_class: USER_BUBBLE_CLASS,
            markdown_class: MARKDOWN_CLASS,
            note_form: FieldForm::Note.into(),
            beside: CommentPlacement::Beside.into(),
        }
    }
}

/// What the app tells the script.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum QuoteCommand {
    Layout(Layout),
    /// Scrolls the chat to a quote's passage and lights it for a moment.
    Reveal {
        id: QuoteId,
    },
}

/// The shown agent's quotes and the passage being commented, which the script highlights and
/// places the elements of.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
struct Layout {
    agent: Option<AgentId>,
    quotes: Vec<PassagePlace>,
    editor: Option<EditorPlace>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PassagePlace {
    id: QuoteId,
    anchor: QuoteAnchor,
    raw: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EditorPlace {
    /// The quote whose comment the field changes; `None` for a passage just selected.
    quote: Option<QuoteId>,
    anchor: QuoteAnchor,
    raw: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum QuoteMessage {
    /// Reply on a selection, or Ctrl+R.
    Reply { agent: AgentId, passage: Passage },
    /// Enter in the comment field.
    Commit { text: String },
}

/// The script, for the components that ask it to scroll.
#[derive(Clone, Copy)]
struct QuoteScript(document::Eval);

impl QuoteScript {
    fn reveal(self, id: QuoteId) {
        if let Err(error) = self.0.send(QuoteCommand::Reveal { id }) {
            tracing::warn!("cannot scroll to the quote: {error}");
        }
    }
}

/// Runs the script for the window's life, and keeps it told of the shown agent's quotes.
pub fn use_quotes(cockpit: Cockpit) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(Settings::new()) {
            tracing::warn!("cannot start the quotes' script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<QuoteMessage>().await {
                    Ok(message) => receive(cockpit, message),
                    Err(document::EvalError::Serialization(error)) => {
                        tracing::warn!("cannot read a message of the quotes' script: {error}");
                    }
                    Err(error) => {
                        tracing::warn!("the quotes' script stopped: {error}");
                        return;
                    }
                }
            }
        });
        script
    });
    use_context_provider(|| QuoteScript(script));
    let layout = use_memo(move || layout(&cockpit.state.read(), &cockpit.drafts.read()));
    use_effect(move || {
        if let Err(error) = script.send(QuoteCommand::Layout(layout())) {
            tracing::warn!("cannot update the quotes' script: {error}");
        }
    });
}

fn receive(cockpit: Cockpit, message: QuoteMessage) {
    match message {
        QuoteMessage::Reply { agent, passage } => {
            cockpit.with_drafts(|state, drafts| {
                if state.ui.selected == Some(agent) {
                    quotes::start(drafts, agent, passage);
                }
            });
        }
        QuoteMessage::Commit { text } => {
            cockpit.with_drafts(|state, drafts| {
                quotes::type_comment(drafts, text);
                quotes::commit(drafts, state.prefs.chat.comments);
            });
            bridge::focus_field(COMPOSER_ID);
        }
    }
}

fn layout(state: &AppState, drafts: &Drafts) -> Layout {
    let Some(agent) = state.ui.selected else {
        return Layout::default();
    };
    let quotes = drafts
        .draft(agent)
        .map(|draft| {
            draft
                .quotes
                .iter()
                .map(|quote| PassagePlace {
                    id: quote.id,
                    anchor: quote.passage.anchor,
                    raw: quote.passage.raw.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let editor = shown_editor(drafts, agent).and_then(|editor| {
        let (quote, passage) = match &editor.target {
            EditorTarget::New(passage) => (None, passage),
            EditorTarget::Existing(id) => (Some(*id), &find(drafts, agent, *id)?.passage),
        };
        Some(EditorPlace {
            quote,
            anchor: passage.anchor,
            raw: passage.raw.clone(),
        })
    });
    Layout {
        agent: Some(agent),
        quotes,
        editor,
    }
}

fn shown_editor(drafts: &Drafts, agent: AgentId) -> Option<&QuoteEditor> {
    drafts
        .quote_editor
        .as_ref()
        .filter(|editor| editor.agent == agent)
}

fn find(drafts: &Drafts, agent: AgentId, id: QuoteId) -> Option<&Quote> {
    drafts
        .draft(agent)?
        .quotes
        .iter()
        .find(|quote| quote.id == id)
}

fn field_view(state: &AppState, drafts: &Drafts, agent: AgentId) -> Option<FieldView> {
    let editor = shown_editor(drafts, agent)?.clone();
    let wide = chat_is_wide(state, agent);
    let (number, beside) = match &editor.target {
        EditorTarget::New(_) => (None, state.prefs.chat.comments == CommentPlacement::Beside),
        EditorTarget::Existing(id) => {
            let quotes = drafts
                .draft(agent)
                .map_or(&[][..], |draft| draft.quotes.as_slice());
            let index = quotes.iter().position(|quote| quote.id == *id);
            let beside = index
                .and_then(|index| quotes.get(index))
                .is_some_and(|quote| quote.placement == CommentPlacement::Beside);
            (index.map(quotes::number), beside)
        }
    };
    let form = if wide && beside {
        FieldForm::Note
    } else {
        FieldForm::Card
    };
    Some(FieldView {
        editor,
        number,
        form,
    })
}

/// Whether the chat has the strip on its right, where notes show: the file view takes it.
pub fn chat_is_wide(state: &AppState, agent: AgentId) -> bool {
    !terminals::file_view_open(state, agent)
}

/// The elements the script places over the chat area.
#[component]
pub fn QuoteLayer() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let drafts = cockpit.drafts.read();
    let selected = state.ui.selected;
    let field_view = selected.and_then(|agent| field_view(&state, &drafts, agent));
    // A field that closes plays its exit, in its place.
    let field = use_presence(field_view);
    let Some(agent) = selected else {
        return rsx! {};
    };
    let wide = chat_is_wide(&state, agent);
    let quotes = drafts
        .draft(agent)
        .map(|draft| draft.quotes.clone())
        .unwrap_or_default();
    let editing = shown_editor(&drafts, agent).and_then(|editor| match editor.target {
        EditorTarget::Existing(id) => Some(id),
        EditorTarget::New(_) => None,
    });
    // Every note of a wide chat is drawn: a hidden one fades out rather than going at once.
    let notes: Vec<(usize, Quote)> = quotes
        .iter()
        .enumerate()
        .filter(|(_, quote)| wide && editing != Some(quote.id))
        .map(|(index, quote)| (quotes::number(index), quote.clone()))
        .collect();
    rsx! {
        div { id: LAYER_ID, class: "quote-layer",
            svg { id: LINKS_ID, class: "quote-links", "aria-hidden": "true" }
            div { id: REPLY_BUTTON_ID, class: "quote-reply-place",
                button {
                    class: "popover quote-reply",
                    tabindex: "-1",
                    title: "Reply to the selection (Ctrl+R)",
                    Icon { kind: IconKind::Reply, size: 14 }
                    span { "Reply" }
                    span { class: "key-hint", "Ctrl+R" }
                }
            }
            for (index, quote) in quotes.into_iter().enumerate() {
                QuotePin { key: "{quote.id.get()}", agent, number: quotes::number(index), quote, wide }
            }
            if !notes.is_empty() {
                div { id: MORE_ID, class: "quote-more",
                    span { class: MORE_COUNT_CLASS }
                    span { class: "spacer" }
                    for (number, id, quote) in notes.iter().map(|(number, quote)| (*number, quote.id, quote.clone())) {
                        div { key: "{id.get()}", class: MORE_ITEM_CLASS, "data-quote": "{id.get()}",
                            button { class: "quote-pin", aria_label: "Quote {number}", "{number}" }
                            div { class: "popover quote-pin-card",
                                QuoteCard { agent, number, quote, wide }
                            }
                        }
                    }
                }
            }
            for (number, quote) in notes {
                QuoteNote { key: "{quote.id.get()}", agent, number, quote }
            }
            // A switch draws the other agent at once: the field of the agent that left goes with it.
            if let Some(Shown { value: FieldView { editor, number, form }, leaving, .. }) = field
                && editor.agent == agent
            {
                // Keyed by opening: a new opening mounts a new field, with the draft's text.
                CommentField { key: "{editor.opening}", editor, number, form, leaving }
            }
            div { id: REPLIED_HOST_ID, class: "quote-replied-host" }
        }
    }
}

/// A quote's number beside its passage. While its note does not show, hovering or focusing it
/// shows the passage and the comment.
#[component]
fn QuotePin(agent: AgentId, number: usize, quote: Quote, wide: bool) -> Element {
    let id = quote.id;
    let comment = quote.comment.clone();
    let label = if comment.is_empty() {
        format!("Quote {number}, no comment")
    } else {
        format!("Quote {number}: {comment}")
    };
    rsx! {
        div { class: PIN_GROUP_CLASS, "data-quote": "{id.get()}",
            button { class: "quote-pin", aria_label: "{label}", "{number}" }
            div { class: "popover quote-pin-card",
                QuoteCard { agent, number, quote, wide }
            }
        }
    }
}

/// The card of a quote whose note does not show: its number and its buttons, its passage and its
/// comment.
#[component]
fn QuoteCard(agent: AgentId, number: usize, quote: Quote, wide: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let id = quote.id;
    rsx! {
        div { class: "quote-note-head",
            span { class: "quote-number", "{number}" }
            span { class: "quote-note-state", "Not sent yet" }
            span { class: "spacer" }
            // A comment set to show beside the text that has no room there shows here.
            if wide && quote.placement == CommentPlacement::Hidden {
                button {
                    class: "icon-button tiny",
                    title: "Show beside the text",
                    aria_label: "Show the comment beside the text",
                    onclick: move |_| cockpit.with_drafts(|_, drafts| quotes::set_placement(drafts, agent, id, CommentPlacement::Beside)),
                    Icon { kind: IconKind::Eye, size: 13 }
                }
            }
            EditButton { agent, id }
            RemoveButton { agent, id }
        }
        p { class: "quote-passage", "{quote.passage.text}" }
        CommentText { agent, id, comment: quote.comment }
    }
}

/// A quote's comment in the strip beside its passage.
#[component]
fn QuoteNote(agent: AgentId, number: usize, quote: Quote) -> Element {
    let cockpit: Cockpit = use_context();
    let id = quote.id;
    let placement: &'static str = quote.placement.into();
    rsx! {
        div {
            class: NOTE_CLASS,
            "data-quote": "{id.get()}",
            "data-placement": placement,
            div { class: "quote-note-head",
                span { class: "quote-number", "{number}" }
                span { class: "quote-note-state", "Not sent yet" }
                span { class: "spacer" }
                EditButton { agent, id }
                button {
                    class: "icon-button tiny",
                    title: "Hide: keep the number only",
                    aria_label: "Hide the comment, keep the number only",
                    onclick: move |_| cockpit.with_drafts(|_, drafts| quotes::set_placement(drafts, agent, id, CommentPlacement::Hidden)),
                    Icon { kind: IconKind::EyeOff, size: 13 }
                }
                RemoveButton { agent, id }
            }
            p { class: "quote-passage", "{quote.passage.text}" }
            CommentText { agent, id, comment: quote.comment }
        }
    }
}

/// A quote's comment, which a click, Enter or Space opens in the comment field, in its place.
#[component]
fn CommentText(agent: AgentId, id: QuoteId, comment: String) -> Element {
    let cockpit: Cockpit = use_context();
    let edit = move || cockpit.with_drafts(|_, drafts| quotes::edit(drafts, agent, id));
    let empty = comment.is_empty();
    rsx! {
        p {
            class: "{COMMENT_TEXT_CLASS}",
            class: if empty { "dim" },
            role: "button",
            tabindex: "0",
            title: "Edit the comment",
            "data-quote": "{id.get()}",
            onclick: move |_| edit(),
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Enter || event.key() == Key::Character(" ".to_owned()) {
                    event.prevent_default();
                    edit();
                }
            },
            if empty {
                "No comment"
            } else {
                "{comment}"
            }
        }
    }
}

#[component]
fn EditButton(agent: AgentId, id: QuoteId) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "icon-button tiny",
            title: "Edit the comment",
            aria_label: "Edit the comment",
            onclick: move |_| cockpit.with_drafts(|_, drafts| quotes::edit(drafts, agent, id)),
            Icon { kind: IconKind::Pencil, size: 13 }
        }
    }
}

#[component]
fn RemoveButton(agent: AgentId, id: QuoteId) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "icon-button tiny",
            title: "Remove the quote",
            aria_label: "Remove the quote",
            onclick: move |_| cockpit.with_drafts(|_, drafts| quotes::remove(drafts, agent, id)),
            Icon { kind: IconKind::Close, size: 13 }
        }
    }
}

/// The comment field: a note in the strip, or a card under the passage. Enter adds the quote or
/// saves the comment, and Esc closes it. While a dictation runs into it, its words show under it
/// in the place of the key hints, and Enter and Esc stop and cancel the dictation first.
#[component]
fn CommentField(
    editor: QuoteEditor,
    number: Option<usize>,
    form: FieldForm,
    leaving: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let agent = editor.agent;
    let opening = editor.opening;
    // A memo: the words change twice a second, and only the row under the field shows them.
    let dictating = use_memo(move || voice.state.read().writes_comment(opening))();
    let hints = use_presence((!dictating).then_some(()));
    let (quote, passage, hint) = match &editor.target {
        EditorTarget::New(passage) => (NEW_FIELD.to_owned(), Some(passage.text.clone()), "adds it"),
        EditorTarget::Existing(id) => (id.get().to_string(), None, "saves it"),
    };
    let state = if number.is_some() {
        "Edit the comment"
    } else {
        "New"
    };
    let class = if leaving {
        format!("popover {FIELD_CLASS} {LEAVING}")
    } else {
        format!("popover {FIELD_CLASS}")
    };
    let form: &'static str = form.into();
    rsx! {
        div {
            class,
            inert: leaving.then_some(true),
            "data-quote": "{quote}",
            "data-form": form,
            div { class: "quote-note-head",
                if let Some(number) = number {
                    span { class: "quote-number", "{number}" }
                } else {
                    span { class: "quote-number new" }
                }
                span { class: "quote-note-state", "{state}" }
                span { class: "spacer" }
                CommentMic { agent, opening }
                button {
                    class: "icon-button tiny",
                    title: "Cancel (Esc)",
                    aria_label: "Cancel the comment",
                    onclick: move |_| {
                        cockpit.with_drafts(|_, drafts| quotes::cancel(drafts, Some(agent)));
                    },
                    Icon { kind: IconKind::Close, size: 13 }
                }
            }
            if let Some(passage) = passage {
                p { class: "quote-passage", "{passage}" }
            }
            textarea {
                // The field that leaves gives its id to the next one.
                id: (!leaving).then_some(COMMENT_FIELD_ID),
                class: "quote-field",
                rows: "2",
                aria_label: "Your comment on the quote",
                placeholder: "Your comment",
                initial_value: "{editor.text}",
                "data-dictating": dictating.then_some(""),
                oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| quotes::type_comment(drafts, event.value())),
            }
            div {
                id: (!leaving).then_some(FIELD_FOOT_ID),
                class: "quote-field-foot",
                onmounted: move |_| dialog_height::follow_content(FIELD_FOOT_ID),
                // The hints and the words share one place while they swap.
                div { class: "quote-field-foot-body",
                    if let Some(hints) = hints {
                        span {
                            // The hints of a field just opened take no entrance of their own.
                            class: match (hints.leaving, hints.opening > 1) {
                                (true, _) => "quote-hint leaving",
                                (false, true) => "quote-hint returning",
                                (false, false) => "quote-hint",
                            },
                            kbd { "Enter" }
                            " {hint} · "
                            kbd { "Esc" }
                            " cancels"
                        }
                    }
                    CommentWords { opening }
                }
            }
        }
    }
}

/// The quotes the message will carry, one line each, above the composer's text.
#[component]
pub fn QuoteRows(agent: AgentId, quotes: Vec<Quote>, wide: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let script: QuoteScript = use_context();
    rsx! {
        div { class: "quote-rows", role: "list", aria_label: "Quotes sent with the message",
            for (index, quote) in quotes.into_iter().enumerate() {
                {
                    let id = quote.id;
                    let shown = quote.placement == CommentPlacement::Beside;
                    let other = if shown { CommentPlacement::Hidden } else { CommentPlacement::Beside };
                    let number = quotes::number(index);
                    rsx! {
                        div { key: "{id.get()}", class: "quote-row", role: "listitem",
                            button {
                                class: "quote-row-main",
                                title: SHOW_PASSAGE,
                                onclick: move |_| script.reveal(id),
                                span { class: "quote-number", "{number}" }
                                span { class: "quote-row-passage", "{quote.passage.text}" }
                                span { class: "quote-row-comment", "{quote.comment}" }
                            }
                            if wide {
                                button {
                                    class: if shown { "icon-button tiny" } else { "icon-button tiny off" },
                                    title: if shown { "Hide the note: keep the number only" } else { "Show the note beside the text" },
                                    aria_label: if shown { "Hide the note" } else { "Show the note beside the text" },
                                    aria_pressed: shown,
                                    onclick: move |_| cockpit.with_drafts(|_, drafts| quotes::set_placement(drafts, agent, id, other)),
                                    Icon { kind: if shown { IconKind::Eye } else { IconKind::EyeOff }, size: 13 }
                                }
                            }
                            EditButton { agent, id }
                            RemoveButton { agent, id }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use cockpit_protocol::{AgentStatus, ItemId};

    use super::*;
    use crate::state::quotes::QuoteAnchor;
    use crate::state::{Utf16Offset, state_with, summary};

    fn passage(item: u32, text: &str) -> Passage {
        Passage {
            anchor: QuoteAnchor {
                item: ItemId::new(item),
                start: Utf16Offset::new(4),
                end: Utf16Offset::new(4 + text.len()),
            },
            raw: text.to_owned(),
            text: text.to_owned(),
            code: false,
        }
    }

    /// The script reads these words from the field's `data-form`.
    #[test]
    fn the_field_forms_are_pinned() {
        let words: Vec<&str> = FieldForm::VARIANTS
            .iter()
            .map(|&form| form.into())
            .collect();

        assert_eq!(words, ["note", "card"]);
    }

    /// The script reads each setting by its serde name, and a name it reads that the app does not
    /// send is `undefined` there, with no error.
    #[test]
    fn the_script_reads_the_settings_the_app_sends() {
        let sent: BTreeSet<String> = serde_json::to_value(Settings::new())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let read: BTreeSet<String> = SCRIPT
            .split("settings.")
            .skip(1)
            .map(|rest| {
                rest.chars()
                    .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                    .collect()
            })
            .collect();

        assert_eq!(read, sent);
    }

    #[test]
    fn the_script_sees_the_shown_agents_quotes_and_its_field_alone() {
        let shown = AgentId::new(1);
        let other = AgentId::new(2);
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.ui.selected = Some(shown);
        let mut drafts = Drafts::default();
        quotes::start(&mut drafts, shown, passage(3, "kept"));
        quotes::commit(&mut drafts, CommentPlacement::Beside);
        let id = drafts.draft(shown).map(|draft| draft.quotes[0].id);
        quotes::start(&mut drafts, other, passage(9, "elsewhere"));

        let other_field = layout(&state, &drafts);
        if let Some(id) = id {
            quotes::edit(&mut drafts, shown, id);
        }
        let editing = layout(&state, &drafts);

        assert_eq!(other_field.agent, Some(shown));
        assert_eq!(other_field.quotes.len(), 1);
        assert_eq!(other_field.quotes[0].raw, "kept");
        assert_eq!(other_field.editor, None);
        assert_eq!(
            editing.editor,
            Some(EditorPlace {
                quote: id,
                anchor: passage(3, "kept").anchor,
                raw: "kept".to_owned(),
            })
        );
    }
}
