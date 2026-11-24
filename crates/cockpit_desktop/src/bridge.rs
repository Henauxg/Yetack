//! The page's script. A key's default action (typing a letter, a new line on Enter) can be
//! prevented only in the page, so the script decides which key presses to take, from the mode
//! Rust sends it, and passes them on with the window's focus and pasted images.

use cockpit_protocol::{AgentId, Base64, Image, ImageMediaType, ItemBody};
use dioxus::dioxus_core::{queue_effect, spawn_forever};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::keymap::{Command, Reach};
use crate::state::sidebar_keys::SidebarItem;
use crate::state::voice::{Field, VoiceState, dictation_target};
use crate::state::{
    ANSWER_KEYS, AppState, Dialog, KeyPress, PAGE_RELOAD_KEY, PaletteOrigin, Popover, Travel,
    Utf16Offset, WaitingKind, accounts, actions, agent_map, find, handle_key, sidebar_keys,
};
use crate::ui::{
    COMMENT_FIELD_ID, FILE_VIEW_ID, FILES_PANE_ID, FIND_FIELD_IDS, focus_after_find,
    sidebar_item_id,
};
use crate::voice_desk::{VoiceCommand, VoiceDesk};

/// The composer's text area, whose keys the script treats apart.
pub const COMPOSER_ID: &str = "composer";

/// The new agent dialog's folder field.
pub const FOLDER_FIELD_ID: &str = "new-agent-folder";

/// The "Open file" dialog's path field.
pub const OPEN_FILE_FIELD_ID: &str = "open-file-path";

/// Marks the element that records a new shortcut: the script leaves its keys to it.
pub const RECORDS_KEYS_ATTRIBUTE: &str = "data-records-keys";

/// Marks an element that Esc closes by itself, such as an opened quote note: while one is on the
/// page, the script leaves Esc to it, so that Esc closes nothing else and never stops an agent.
pub const TAKES_ESCAPE_ATTRIBUTE: &str = "data-takes-escape";

/// The new agent dialog's first message.
pub const NEW_AGENT_PROMPT_ID: &str = "new-agent-prompt";

/// The hidden file input the composer's attach button opens.
pub const ATTACH_INPUT_ID: &str = "attach-input";

/// The filter field of the palette the slash button opens.
pub const PALETTE_FILTER_ID: &str = "palette-filter";

/// The palette's highlighted entry, kept in view as the keys move it.
pub const HIGHLIGHTED_ENTRY_ID: &str = "palette-highlighted";

/// The class of the row the keys move to in a list.
pub const HIGHLIGHTED_CLASS: &str = "highlighted";

/// The field that replaces the agent's title in the header while it is renamed.
pub const HEADER_TITLE_FIELD_ID: &str = "agent-title-field";

/// The field that replaces an agent's title in its sidebar row while it is renamed.
pub const ROW_TITLE_FIELD_ID: &str = "row-title-field";

/// A pause in typing this long ends an undo step, as in common text editors.
const UNDO_PAUSE_MS: u32 = 500;

/// The undo steps a text field keeps, and as many redo steps.
const UNDO_STEPS: u16 = 200;

/// Ctrl+F looks for a selection of at most a line: a longer one need not cross to the app whole.
const MAX_SELECTION: usize = 1_000;

/// What a note calls an image pasted into the window.
const PASTED_IMAGE: &str = "The pasted image";

/// Waits for its settings, then passes on events. Message and field names are serde's names for
/// `BridgeMessage`, `KeyPress`, `KeyTarget` and `KeyMode`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
let mode = { answer_letters: false, answer_numbers: false, question_tabs: false, palette: false, dialog: false, bindings: [], talk: null, dictating: false, enter_stops: null };
const send = (message) => dioxus.send(message);
// The talk key's binding while it is down: its release, or the window's blur, stops the
// dictation it holds. Kept from the press, since the mode may drop the talk key meanwhile.
let talking = null;
// An Enter that stopped a dictation is down: its repeats send nothing, even once the text is in.
let enterStopped = false;
// The sidebar row or machine header that has the focus, as the app names it.
const sidebarItem = (element) => {
  const data = element?.dataset;
  if (data?.rowAgent !== undefined) {
    return { kind: "agent", id: Number(data.rowAgent) };
  }
  if (data?.headMachine !== undefined) {
    return { kind: "machine", id: Number(data.headMachine) };
  }
  if (data?.headProject !== undefined) {
    return { kind: "project", id: data.headProject };
  }
  return null;
};
(async () => {
  for (;;) {
    mode = await dioxus.recv();
  }
})();
window.addEventListener("focus", () => send({ type: "focus", focused: true }));
window.addEventListener("blur", () => {
  send({ type: "focus", focused: false });
  // A key that comes up in another window sends no keyup here.
  enterStopped = false;
  if (talking !== null) {
    talking = null;
    send({ type: "talk", action: "blur" });
  }
});
// The mouse's back (3) and forward (4) buttons go through the file view's history over the file
// view and the files pane. Nowhere do they leave the page, as the web view would.
const BACK_BUTTON = 3;
const FORWARD_BUTTON = 4;
const sideButton = (event) => event.button === BACK_BUTTON || event.button === FORWARD_BUTTON;
for (const name of ["mousedown", "auxclick"]) {
  document.addEventListener(name, (event) => {
    if (sideButton(event)) {
      event.preventDefault();
    }
  });
}
document.addEventListener("mouseup", (event) => {
  if (!sideButton(event)) {
    return;
  }
  event.preventDefault();
  const over = event.target instanceof Element
    && event.target.closest(`#${settings.file_view_id}, #${settings.files_pane_id}`) !== null;
  if (over) {
    send({ type: "file_history", travel: event.button === FORWARD_BUTTON ? "forward" : "back" });
  }
});
// A window that opens behind another gets no blur.
send({ type: "focus", focused: document.hasFocus() });
// A menu that takes the focus remembers where it took it from, for Esc to give it back. A menu
// opened from another keeps the first one's.
document.addEventListener("focusin", (event) => {
  if (!(event.target instanceof Element) || !event.target.matches('[role="menu"][data-own-keys]')) {
    return;
  }
  const from = event.relatedTarget;
  if (!(from instanceof Element) || from.closest('[role="menu"]') === null) {
    window[settings.menu_focus_key] = from;
  }
});
// Undo and redo in text fields. WebKitGTK binds no key to its own, which also goes out of step
// when the app writes a field's text, so the script keeps a history per field.
const textInputTypes = ["text", "search", "url", "email"];
const isTextField = (element) => (element instanceof HTMLTextAreaElement
  || (element instanceof HTMLInputElement && textInputTypes.includes(element.type)))
  && !element.readOnly && !element.disabled;
const groupedKinds = ["insertText", "insertCompositionText", "deleteContentBackward", "deleteContentForward"];
const histories = new WeakMap();
const snapshot = (field) => ({ value: field.value, start: field.selectionStart, end: field.selectionEnd });
const remember = (steps, step) => {
  steps.push(step);
  if (steps.length > settings.undo_steps) {
    steps.shift();
  }
};
// One history per field, and per agent in the composer, whose text area the agents share.
const historyOf = (field) => {
  let scopes = histories.get(field);
  if (scopes === undefined) {
    scopes = new Map();
    histories.set(field, scopes);
  }
  const scope = field.dataset.agent ?? "";
  let history = scopes.get(scope);
  if (history === undefined) {
    history = { undo: [], redo: [], last: snapshot(field), kind: "", at: 0, wroteWord: false };
    scopes.set(scope, history);
  }
  return history;
};
// A text the app wrote since the field's last edit, such as a palette pick, is a step of its own.
const catchUp = (field) => {
  const history = historyOf(field);
  if (field.value !== history.last.value) {
    remember(history.undo, history.last);
    history.redo = [];
    history.last = snapshot(field);
    history.kind = "";
  }
  return history;
};
document.addEventListener("beforeinput", (event) => {
  const field = event.target;
  if (!isTextField(field)) {
    return;
  }
  const history = catchUp(field);
  const kind = event.inputType;
  const now = Date.now();
  // Typing, or deleting, makes one step until a pause, another kind of edit, or a new word once
  // the step holds one.
  const word = /\S/.test(event.data ?? "");
  const newWord = word && history.wroteWord && /\s/.test(field.value.charAt(field.selectionStart - 1));
  const grouped = groupedKinds.includes(kind) && kind === history.kind
    && now - history.at < settings.undo_pause_ms && !newWord;
  if (!grouped) {
    remember(history.undo, snapshot(field));
    history.wroteWord = false;
  }
  history.wroteWord = history.wroteWord || word;
  history.redo = [];
  history.kind = kind;
  history.at = now;
}, true);
document.addEventListener("input", (event) => {
  if (isTextField(event.target)) {
    historyOf(event.target).last = snapshot(event.target);
  }
}, true);
const undo = (field, redo) => {
  const history = catchUp(field);
  const step = (redo ? history.redo : history.undo).pop();
  if (step === undefined) {
    return;
  }
  remember(redo ? history.undo : history.redo, snapshot(field));
  field.value = step.value;
  field.setSelectionRange(step.start, step.end);
  history.last = snapshot(field);
  history.kind = "";
  // The fields follow the keyboard alone: an input event gives the app the restored text.
  field.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: redo ? "historyRedo" : "historyUndo" }));
};
document.addEventListener("keydown", (event) => {
  if (event.isComposing) {
    return;
  }
  const target = event.target;
  if (target !== null && target.closest(`[${settings.records_keys_attribute}]`) !== null) {
    return;
  }
  // Backspace in a path field goes up a folder only from the end of its text, with nothing
  // selected. Anywhere else it deletes, and the app's handler does not see it.
  if (event.key === "Backspace" && target !== null && settings.path_field_ids.includes(target.id)
    && (target.selectionStart !== target.value.length || target.selectionEnd !== target.value.length)) {
    event.stopPropagation();
    return;
  }
  const composer = target !== null && target.id === settings.composer_id;
  const field = !composer && target !== null
    && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable);
  const where = composer ? "composer" : field ? "field" : "page";
  const key = event.key;
  // Esc first stops a dictation, and drops its text.
  if (key === "Escape" && mode.dictating && !event.repeat) {
    event.preventDefault();
    talking = null;
    send({ type: "talk", action: "cancel" });
    return;
  }
  const command = event.ctrlKey || event.metaKey;
  const plain = !command && !event.altKey;
  const comment = target !== null && target.id === settings.comment_field_id;
  // Enter stops the dictation, as the talk key does, and the next one sends the message or adds
  // the quote. A button keeps its Enter.
  const stops = mode.enter_stops === "message"
    ? composer || (where === "page" && !(target !== null && target.matches('button, [role="button"]')))
    : mode.enter_stops === "comment" && comment;
  if (key === "Enter" && stops && plain && !event.shiftKey && !mode.dialog) {
    event.preventDefault();
    if (!event.repeat) {
      enterStopped = true;
      send({ type: "talk", action: "stop" });
    }
    return;
  }
  if (key === "Enter" && event.repeat && enterStopped) {
    event.preventDefault();
    return;
  }
  const letter = key.length === 1 ? key.toLowerCase() : "";
  if (command && !event.altKey && (letter === "z" || (letter === "y" && !event.shiftKey)) && isTextField(target)) {
    event.preventDefault();
    undo(target, letter === "y" || event.shiftKey);
    return;
  }
  // Left and Right switch questions from the composer, the page itself or the question card.
  // Elsewhere they scroll what has the focus.
  const questionArrow = mode.question_tabs && !event.shiftKey && (key === "ArrowLeft" || key === "ArrowRight")
    && (composer || target === null || target === document.body || target.closest(".question-card") !== null);
  const answers = (mode.answer_letters && settings.answer_keys.includes(letter))
    || (mode.answer_numbers && (/^[1-9]$/.test(key) || key === "Enter"))
    || questionArrow;
  // The arrows on a sidebar row or machine header move the selection and fold the machines.
  const focused = sidebarItem(target);
  if (focused !== null && plain && !event.shiftKey && !mode.dialog && settings.sidebar_keys.includes(key)) {
    event.preventDefault();
    send({ type: "sidebar_key", key, focused });
    return;
  }
  const text = composer ? target.value : "";
  // A command's shortcut, as the Settings dialog sets it. A key reads as one character in lower
  // case, as the app reads it.
  const pressed = key.length === 1 ? key.toLowerCase() : key;
  // The talk key dictates: its press and its release go to the app, and its repeats nowhere.
  const talk = mode.talk;
  if (talk != null && pressed === talk.key && talk.ctrl === command && talk.alt === event.altKey
    && talk.shift === event.shiftKey && (where !== "field" || comment) && !mode.dialog) {
    event.preventDefault();
    if (!event.repeat && talking === null) {
      talking = talk;
      send({ type: "talk", action: "press", field: comment ? "comment" : "message" });
    }
    return;
  }
  const bound = mode.bindings.find((binding) => binding.key === pressed && binding.ctrl === command
    && binding.alt === event.altKey && binding.shift === event.shiftKey);
  const titleField = target !== null && settings.title_field_ids.includes(target.id);
  // A block's head moves the block with Alt+Up and Alt+Down, rather than the selected agent.
  const moves = event.altKey && !command && !event.shiftKey && (key === "ArrowUp" || key === "ArrowDown");
  if (moves && target !== null && target.closest("[data-own-moves]") !== null) {
    return;
  }
  let take = false;
  if (bound !== undefined) {
    take = bound.reach === "everywhere"
      || (bound.reach === "page_or_title" && !mode.dialog && (where === "page" || titleField))
      || (bound.reach === "outside_fields" && where !== "field" && !mode.dialog)
      || (bound.reach === "page" && where === "page" && !mode.dialog);
    // Where its command does not reach, F5 runs nothing, and still never reloads the page.
    if (!take && key === settings.page_reload_key) {
      event.preventDefault();
      return;
    }
  } else if (key === "Escape") {
    take = document.querySelector(`[${settings.takes_escape_attribute}]`) === null;
  } else if (key === settings.page_reload_key) {
    // Taken everywhere, whatever its shortcut runs: it never reloads the page.
    take = true;
  } else if (mode.map) {
    // The map moves and folds with its keys wherever the focus is, but in a text field or in a
    // row's menu. Enter on one of its buttons still presses the button.
    const mapKey = settings.map_keys.includes(key) || (mode.answer_letters && settings.answer_keys.includes(letter));
    take = where !== "field" && plain && !event.shiftKey && mapKey
      && !(target !== null && target.closest("[data-own-keys]") !== null)
      && !(key === "Enter" && target !== null && target.matches('button, [role="button"]'));
  } else if (where === "composer" && event.altKey && !command && !event.shiftKey && key === "Enter") {
    // Alt+Enter queues the message until the turn ends.
    take = true;
  } else if (where === "composer" && plain) {
    take = (mode.palette && (["ArrowUp", "ArrowDown", "Tab"].includes(key) || (key === "Enter" && !event.shiftKey)))
      || (text === "" && answers)
      || (key === "Enter" && !event.shiftKey);
  } else if (where === "page" && plain && !mode.dialog
    // A tree or a menu takes its own keys: Enter or a digit there acts on it, and answers nothing.
    && !(target !== null && target.closest("[data-own-keys]") !== null)
    && (answers || (mode.send_again && key === "Enter" && !event.shiftKey))) {
    take = !(key === "Enter" && target !== null && target.matches('button, [role="button"]'));
  }
  if (!take) {
    return;
  }
  event.preventDefault();
  // A command's shortcut takes the page's selection with it, which Ctrl+F looks for. A selection
  // longer than the app takes goes as none: cut, it could end in half a character.
  const selected = bound === undefined || where !== "page" ? "" : document.getSelection()?.toString() ?? "";
  const selection = selected.length > settings.max_selection ? "" : selected;
  send({
    type: "key",
    key,
    ctrl: event.ctrlKey,
    shift: event.shiftKey,
    alt: event.altKey,
    meta: event.metaKey,
    target: where,
    composer_text: text,
    pane: target !== null && target.closest(`#${settings.file_view_id}`) !== null ? "file" : "chat",
    selection,
    in_find: target !== null && settings.find_field_ids.includes(target.id),
  });
}, true);
// The talk key's release, or the release of one of its modifiers, stops the dictation it holds.
document.addEventListener("keyup", (event) => {
  if (event.key === "Enter") {
    enterStopped = false;
  }
  const talk = talking;
  if (talk === null) {
    return;
  }
  const released = event.key.length === 1 ? event.key.toLowerCase() : event.key;
  const modifier = (talk.ctrl && (released === "Control" || released === "Meta"))
    || (talk.alt && released === "Alt") || (talk.shift && released === "Shift");
  if (released === talk.key || modifier) {
    talking = null;
    send({ type: "talk", action: "release" });
  }
}, true);
// The composer's text goes to the app with the caret, which tells the "/" word being typed, and
// so does the new agent dialog's first message.
document.addEventListener("input", (event) => {
  const target = event.target;
  if (target === null) {
    return;
  }
  const caret = target.selectionStart ?? target.value.length;
  if (target.id === settings.new_agent_prompt_id) {
    send({ type: "new_agent_prompt_input", text: target.value, caret });
    return;
  }
  if (target.id !== settings.composer_id) {
    return;
  }
  const agent = Number(target.dataset.agent);
  if (Number.isInteger(agent)) {
    send({ type: "composer_input", agent, text: target.value, caret });
  }
});
document.addEventListener("paste", (event) => {
  const items = event.clipboardData ? Array.from(event.clipboardData.items) : [];
  for (const item of items) {
    if (item.kind !== "file" || !item.type.startsWith("image/")) {
      continue;
    }
    const file = item.getAsFile();
    if (file === null) {
      continue;
    }
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result);
      send({ type: "image", media_type: file.type, data: url.slice(url.indexOf(",") + 1) });
    };
    reader.readAsDataURL(file);
  }
});
await new Promise(() => {});
"#;

/// Copies the text it receives, with the older API as a fallback where the clipboard API is
/// refused.
const COPY_SCRIPT: &str = r#"
const text = await dioxus.recv();
try {
  await navigator.clipboard.writeText(text);
} catch (error) {
  const area = document.createElement("textarea");
  area.value = text;
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  document.execCommand("copy");
  area.remove();
}
"#;

/// Puts the text it receives in a text field, caret at the end. The field may not be on the page
/// yet, as the composer when the agent was just selected: it then waits a few frames for it.
const FIELD_TEXT_SCRIPT: &str = r#"
const { field_id, text, caret } = await dioxus.recv();
for (let frame = 0; frame < 30; frame++) {
  const field = document.getElementById(field_id);
  if (field !== null) {
    field.value = text;
    field.selectionStart = field.selectionEnd = caret ?? text.length;
    break;
  }
  await new Promise((resolve) => requestAnimationFrame(resolve));
}
"#;

/// Gives the focus to the field whose id it receives.
const FOCUS_SCRIPT: &str = r#"
const field_id = await dioxus.recv();
document.getElementById(field_id)?.focus();
"#;

/// Gives the focus back to the element whose id it receives when it lost it, at each of the next 10
/// frames, about 160 ms.
const KEEP_FOCUS_SCRIPT: &str = r#"
const id = await dioxus.recv();
for (let frame = 0; frame < 10; frame++) {
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const element = document.getElementById(id);
  if (element !== null && document.activeElement !== element) {
    element.focus();
  }
}
"#;

/// Gives the focus to the element whose id it receives when the focus is in an inert part of it,
/// once the page shows the render.
const FOCUS_OUT_OF_INERT_SCRIPT: &str = r#"
const id = await dioxus.recv();
await new Promise((resolve) => requestAnimationFrame(resolve));
const element = document.getElementById(id);
const active = document.activeElement;
if (element !== null && active !== null && element.contains(active) && active.closest("[inert]") !== null) {
  element.focus({ preventScroll: true });
}
"#;

/// Gives the focus to the field whose id it receives, the caret at the end of its text.
const FOCUS_AT_END_SCRIPT: &str = r#"
const field_id = await dioxus.recv();
const field = document.getElementById(field_id);
if (field !== null) {
  field.focus();
  field.setSelectionRange(field.value.length, field.value.length);
}
"#;

/// Focuses the field whose id it receives, and types the text it receives next at its end, as a
/// key press would: its `input` event reaches the app as typing does, so keys typed after it come
/// after it.
const TYPE_AT_END_SCRIPT: &str = r#"
const field_id = await dioxus.recv();
const text = await dioxus.recv();
const field = document.getElementById(field_id);
if (field !== null) {
  field.focus();
  // The composer's undo history adds a step on beforeinput, as for a key press.
  field.dispatchEvent(new InputEvent("beforeinput", { bubbles: true, cancelable: true, data: text, inputType: "insertText" }));
  const end = field.value.length;
  field.setRangeText(text, end, end, "end");
  field.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: "insertText" }));
}
"#;

/// Types the text it receives at the caret of the field whose id it receives, as one step of its
/// undo history, with a space on each side where a word touches it.
#[cfg(feature = "voice")]
const INSERT_AT_CARET_SCRIPT: &str = r#"
const field_id = await dioxus.recv();
const text = await dioxus.recv();
const field = document.getElementById(field_id);
if (field !== null) {
  field.focus();
  const start = field.selectionStart;
  const end = field.selectionEnd;
  const before = field.value.slice(0, start);
  const after = field.value.slice(end);
  const typed = (before === "" || /\s$/.test(before) ? "" : " ") + text
    + (after === "" || /^\s/.test(after) ? "" : " ");
  // Not typing's "insertText": the undo history would join the text to the keys typed just
  // before or after it.
  const inputType = "insertReplacementText";
  field.dispatchEvent(new InputEvent("beforeinput", { bubbles: true, cancelable: true, data: typed, inputType }));
  field.setRangeText(typed, start, end, "end");
  field.dispatchEvent(new InputEvent("input", { bubbles: true, data: typed, inputType }));
}
"#;

/// Gives the focus to the field whose id it receives, its text selected.
const SELECT_SCRIPT: &str = r#"
const field_id = await dioxus.recv();
const field = document.getElementById(field_id);
field?.focus();
field?.select();
"#;

/// Clicks the element whose id it receives.
const CLICK_SCRIPT: &str = r#"
const element_id = await dioxus.recv();
document.getElementById(element_id)?.click();
"#;

/// Scrolls the element whose id it receives into view, as little as needed.
const REVEAL_SCRIPT: &str = r#"
const element_id = await dioxus.recv();
document.getElementById(element_id)?.scrollIntoView({ block: "nearest" });
"#;

/// Answers whether the window has the focus. It runs after the blur that asks, so it sees the
/// window's own loss of focus when that caused the blur.
const WINDOW_FOCUSED_SCRIPT: &str = "return document.hasFocus();";

/// Brings into view the row the selector it receives finds.
const SCROLL_TO_HIGHLIGHTED_SCRIPT: &str = r#"
const selector = await dioxus.recv();
document.querySelector(selector)?.scrollIntoView({ block: "nearest" });
"#;

/// Gives the focus to the element whose id it receives, or with no id, to the element the last
/// menu took it from. An element gone or inert keeps nothing.
const FOCUS_BACK_SCRIPT: &str = r#"
const { menu_focus_key, element_id } = await dioxus.recv();
const element = element_id === null ? window[menu_focus_key] : document.getElementById(element_id);
if (element instanceof HTMLElement && element.isConnected && element.closest("[inert]") === null) {
  element.focus({ preventScroll: true });
}
"#;

/// The page's global that keeps the element a menu took the focus from.
const MENU_FOCUS_KEY: &str = "cockpitFocusBeforeMenu";

#[derive(Debug, Serialize)]
struct FocusBack {
    menu_focus_key: &'static str,
    element_id: Option<&'static str>,
}

/// A list whose highlighted row the keys keep in view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightedList {
    Dialog,
    FilesPane,
}

impl HighlightedList {
    /// Finds the list's highlighted row on the page.
    fn selector(self) -> String {
        match self {
            Self::Dialog => format!(".dialog .{HIGHLIGHTED_CLASS}"),
            Self::FilesPane => format!("#{FILES_PANE_ID} .{HIGHLIGHTED_CLASS}"),
        }
    }
}

#[derive(Debug, Serialize)]
struct FieldText {
    field_id: &'static str,
    text: String,
    /// `None` puts the caret at the end.
    caret: Option<Utf16Offset>,
}

#[derive(Debug, Serialize)]
struct Settings {
    composer_id: &'static str,
    /// A quote's comment field, where the talk key dictates too.
    comment_field_id: &'static str,
    /// Its text goes to the app with the caret, as the composer's does, for its "/" palette.
    new_agent_prompt_id: &'static str,
    /// The fields where Backspace at the end of the text goes up a folder.
    path_field_ids: [&'static str; 2],
    /// The fields where F2 moves the title edit to the other place.
    title_field_ids: [&'static str; 2],
    /// A pause in typing this long, in milliseconds, ends an undo step.
    undo_pause_ms: u32,
    /// The undo steps a field keeps.
    undo_steps: u16,
    /// The keys the agent map takes.
    map_keys: [&'static str; 5],
    /// The keys a sidebar row or machine header takes.
    sidebar_keys: [&'static str; 4],
    /// The letters that answer an approval or a plan.
    answer_keys: [&'static str; 2],
    /// The key that would reload the page.
    page_reload_key: &'static str,
    records_keys_attribute: &'static str,
    takes_escape_attribute: &'static str,
    /// The page's global that keeps the element a menu took the focus from.
    menu_focus_key: &'static str,
    /// Keys pressed in the file view open and close its find.
    file_view_id: &'static str,
    /// Over it and over the file view, the mouse's back and forward buttons move through the
    /// file view's history.
    files_pane_id: &'static str,
    /// The most UTF-16 units of the selection a shortcut takes with it.
    max_selection: usize,
    /// The finds' fields, where Esc closes the find.
    find_field_ids: [&'static str; 2],
}

/// A command's shortcut, and where the script takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Binding {
    ctrl: bool,
    alt: bool,
    shift: bool,
    key: String,
    reach: Reach,
}

/// Which keys the script takes, beyond Esc and the composer's Enter: the commands' shortcuts, and
/// the plain keys of the open card, palette or map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct KeyMode {
    /// Y and N answer an approval or a plan: in the agent map, the selected subagent's.
    answer_letters: bool,
    /// 1 to 9 and Enter answer a question.
    answer_numbers: bool,
    /// Left and Right show another question of a card with several.
    question_tabs: bool,
    /// Up, Down, Enter and Tab move in the command palette opened by typing `/`. The one the
    /// slash button opens takes them in its own filter field.
    palette: bool,
    dialog: bool,
    /// Up, Down, Left, Right and Enter move and fold in the agent map.
    map: bool,
    /// Enter, outside text fields, presses the sign-in card's "Send the message again".
    send_again: bool,
    bindings: Vec<Binding>,
    /// The talk key, while it can start a dictation or stop the one that runs. The script sends
    /// its press and its release itself.
    talk: Option<Binding>,
    /// A dictation runs: Esc stops it first.
    dictating: bool,
    /// The field on screen a dictation runs into, where Enter stops it rather than send the
    /// message or add the quote.
    enter_stops: Option<Field>,
}

impl KeyMode {
    fn new(state: &AppState, voice: &VoiceState) -> Self {
        let map = matches!(state.ui.dialog, Some(Dialog::AgentMap(_)));
        // In the map, the keys answer the selected subagent's card, not the agent's newest one.
        let waiting = if map {
            agent_map::selected_waiting(state)
        } else {
            state
                .selected()
                .and_then(|entry| entry.transcript.waiting())
        };
        let question_tabs = !map
            && waiting.as_ref().is_some_and(|waiting| {
                state
                    .selected()
                    .and_then(|entry| entry.transcript.get(waiting.item))
                    .is_some_and(|item| {
                        matches!(&item.body, ItemBody::Question(question) if question.questions.len() > 1)
                    })
            });
        let waiting = waiting.map(|waiting| waiting.kind);
        let dictating = voice.dictating().is_some();
        let talks = voice.available
            && state.prefs.voice.enabled
            && (dictating || dictation_target(state).is_some());
        Self {
            answer_letters: matches!(waiting, Some(WaitingKind::Approval | WaitingKind::Plan)),
            answer_numbers: !map && waiting == Some(WaitingKind::Question),
            question_tabs,
            palette: state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)),
            dialog: state.ui.dialog.is_some(),
            map,
            send_again: state
                .ui
                .selected
                .is_some_and(|agent| accounts::sends_again(state, agent)),
            bindings: bindings(state),
            talk: talks.then(|| binding(state, Command::Talk)),
            dictating,
            enter_stops: voice
                .writing()
                .filter(|dictation| state.ui.selected == Some(dictation.agent))
                .map(|dictation| dictation.to.field()),
        }
    }
}

fn bindings(state: &AppState) -> Vec<Binding> {
    use strum::VariantArray;

    let mut bindings = Vec::new();
    for &command in Command::VARIANTS {
        // The talk key has its own field, for its release.
        if command != Command::Talk {
            bindings.push(binding(state, command));
        }
    }
    bindings
}

fn binding(state: &AppState, command: Command) -> Binding {
    let chord = state.prefs.keys.chord(command);
    Binding {
        ctrl: chord.ctrl,
        alt: chord.alt,
        shift: chord.shift,
        key: chord.key,
        reach: command.reach(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeMessage {
    Key(KeyPress),
    /// An arrow key on the sidebar element that has the focus.
    SidebarKey {
        key: String,
        focused: SidebarItem,
    },
    /// The composer's text, with the caret.
    ComposerInput {
        agent: AgentId,
        text: String,
        caret: Utf16Offset,
    },
    /// The new agent dialog's first message, with the caret.
    NewAgentPromptInput {
        text: String,
        caret: Utf16Offset,
    },
    Focus {
        focused: bool,
    },
    /// The mouse's back or forward button, over the file view or the files pane.
    FileHistory {
        travel: Travel,
    },
    Image {
        media_type: String,
        data: String,
    },
    Talk {
        action: TalkAction,
        /// Where a press happened. The other actions name no field.
        #[serde(default)]
        field: Field,
    },
}

/// What the talk key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TalkAction {
    Press,
    Release,
    /// The window lost the focus while the key was down.
    Blur,
    /// Esc, while a dictation runs.
    Cancel,
    /// Enter, in the field on screen a dictation runs into.
    Stop,
}

/// Runs the script for the window's life, and keeps its key mode in step with the state.
pub fn use_bridge(cockpit: Cockpit, voice: VoiceDesk) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(Settings {
            composer_id: COMPOSER_ID,
            comment_field_id: COMMENT_FIELD_ID,
            new_agent_prompt_id: NEW_AGENT_PROMPT_ID,
            path_field_ids: [OPEN_FILE_FIELD_ID, FOLDER_FIELD_ID],
            title_field_ids: [HEADER_TITLE_FIELD_ID, ROW_TITLE_FIELD_ID],
            undo_pause_ms: UNDO_PAUSE_MS,
            undo_steps: UNDO_STEPS,
            map_keys: agent_map::MAP_KEYS,
            sidebar_keys: sidebar_keys::SIDEBAR_KEYS,
            answer_keys: ANSWER_KEYS,
            page_reload_key: PAGE_RELOAD_KEY,
            records_keys_attribute: RECORDS_KEYS_ATTRIBUTE,
            takes_escape_attribute: TAKES_ESCAPE_ATTRIBUTE,
            menu_focus_key: MENU_FOCUS_KEY,
            file_view_id: FILE_VIEW_ID,
            files_pane_id: FILES_PANE_ID,
            max_selection: MAX_SELECTION,
            find_field_ids: FIND_FIELD_IDS,
        }) {
            tracing::warn!("cannot start the page's script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<BridgeMessage>().await {
                    Ok(message) => receive(cockpit, voice, message),
                    Err(error) => {
                        tracing::warn!("the page's script stopped: {error}");
                        return;
                    }
                }
            }
        });
        script
    });
    let mode = use_memo(move || KeyMode::new(&cockpit.state.read(), &voice.state.read()));
    use_effect(move || {
        if let Err(error) = script.send(mode()) {
            tracing::warn!("cannot update the page's script: {error}");
        }
    });
}

fn receive(cockpit: Cockpit, voice: VoiceDesk, message: BridgeMessage) {
    match message {
        BridgeMessage::Talk { action, field } => voice.send(match action {
            TalkAction::Press => VoiceCommand::Press(field),
            TalkAction::Release => VoiceCommand::Release,
            TalkAction::Blur => VoiceCommand::Blur,
            TalkAction::Cancel => VoiceCommand::Cancel,
            TalkAction::Stop => VoiceCommand::Stop,
        }),
        BridgeMessage::Key(press) => {
            let mut find_closed = false;
            cockpit.change_with_drafts(|state, drafts| {
                let open = |state: &AppState| {
                    state
                        .ui
                        .selected
                        .is_some_and(|agent| find::is_open(state, press.pane, agent))
                };
                let was_open = open(state);
                let requests = handle_key(state, drafts, &press);
                find_closed = press.in_find && was_open && !open(state);
                requests
            });
            // The field leaves with its find: the focus goes back to the pane.
            if find_closed {
                focus_after_find(press.pane);
            }
        }
        BridgeMessage::SidebarKey { key, focused } => {
            let mut next = None;
            cockpit.update(|state| next = sidebar_keys::press(state, &focused, &key));
            if let Some(item) = next {
                focus_element(sidebar_item_id(item));
            }
        }
        BridgeMessage::ComposerInput { agent, text, caret } => {
            cockpit.type_in_composer(agent, text, caret);
        }
        BridgeMessage::NewAgentPromptInput { text, caret } => {
            cockpit.type_in_new_agent_prompt(&text, caret);
        }
        BridgeMessage::Focus { focused } => {
            cockpit.set_window_focused(focused);
        }
        BridgeMessage::FileHistory { travel } => {
            cockpit.update(|state| actions::travel_selected(state, travel));
        }
        BridgeMessage::Image { media_type, data } => {
            let Some(media_type) = image_media_type(&media_type) else {
                tracing::warn!("cannot attach an image of type {media_type}");
                return;
            };
            let image = Image {
                media_type,
                data: Base64::new(data),
            };
            let added = cockpit.with_drafts(|state, drafts| {
                let agent = state.ui.selected?;
                Some(actions::add_image(drafts, agent, image))
            });
            if let Some(Err(too_large)) = added {
                cockpit.update(|state| state.ui.report(too_large.note(PASTED_IMAGE)));
            }
        }
    }
}

/// The protocol's serde names are the MIME types, so its parsing decides.
pub fn image_media_type(mime: &str) -> Option<ImageMediaType> {
    serde_json::from_value(serde_json::Value::String(mime.to_owned())).ok()
}

pub fn mime_type(media_type: ImageMediaType) -> String {
    match serde_json::to_value(media_type) {
        Ok(serde_json::Value::String(mime)) => mime,
        Ok(_) | Err(_) => String::new(),
    }
}

/// A `data:` URL that shows an image in the page.
pub fn image_url(image: &Image) -> String {
    format!(
        "data:{};base64,{}",
        mime_type(image.media_type),
        image.data.as_str()
    )
}

pub fn copy_to_clipboard(text: String) {
    let script = document::eval(COPY_SCRIPT);
    if let Err(error) = script.send(text) {
        tracing::warn!("cannot copy: {error}");
    }
}

/// For a field that follows the keyboard alone: a text the app chose reaches it only this way.
pub fn set_field_text(field_id: &'static str, text: String) {
    set_field_text_at(field_id, text, None);
}

/// As `set_field_text`, with the caret at `caret`, or at the end with `None`.
pub fn set_field_text_at(field_id: &'static str, text: String, caret: Option<Utf16Offset>) {
    let script = document::eval(FIELD_TEXT_SCRIPT);
    let message = FieldText {
        field_id,
        text,
        caret,
    };
    if let Err(error) = script.send(message) {
        tracing::warn!("cannot fill the field {field_id}: {error}");
    }
}

pub fn focus_field(field_id: &'static str) {
    let script = document::eval(FOCUS_SCRIPT);
    if let Err(error) = script.send(field_id) {
        tracing::warn!("cannot focus the field {field_id}: {error}");
    }
}

pub fn focus_element(element_id: String) {
    let script = document::eval(FOCUS_SCRIPT);
    if let Err(error) = script.send(&element_id) {
        tracing::warn!("cannot focus {element_id}: {error}");
    }
}

/// Keeps the focus on the element whose id it gets for the next frames: an element a render moves
/// loses the focus, and the render reaches the page at a time no script knows.
pub fn keep_focus(element_id: String) {
    let script = document::eval(KEEP_FOCUS_SCRIPT);
    if let Err(error) = script.send(&element_id) {
        tracing::warn!("cannot keep the focus on {element_id}: {error}");
    }
}

/// Gives the focus to the element `element_id` when a render made the focused element inside it
/// inert.
pub fn focus_out_of_inert(element_id: String) {
    let script = document::eval(FOCUS_OUT_OF_INERT_SCRIPT);
    if let Err(error) = script.send(&element_id) {
        tracing::warn!("cannot give the focus to {element_id}: {error}");
    }
}

pub fn focus_field_at_end(field_id: &'static str) {
    let script = document::eval(FOCUS_AT_END_SCRIPT);
    if let Err(error) = script.send(field_id) {
        tracing::warn!("cannot focus the field {field_id}: {error}");
    }
}

/// Focuses a field and types `text` at its end, as the user would.
pub fn type_at_end(field_id: &'static str, text: String) {
    let script = document::eval(TYPE_AT_END_SCRIPT);
    if let Err(error) = script.send(field_id).and_then(|()| script.send(text)) {
        tracing::warn!("cannot type into the field {field_id}: {error}");
    }
}

/// Focuses a field and types `text` at its caret, as dictation does.
#[cfg(feature = "voice")]
pub fn insert_at_caret(field_id: &'static str, text: &str) {
    let script = document::eval(INSERT_AT_CARET_SCRIPT);
    if let Err(error) = script.send(field_id).and_then(|()| script.send(text)) {
        tracing::warn!("cannot type into the field {field_id}: {error}");
    }
}

/// Focuses a title field as its edit asks: its text selected, or the caret at the end.
pub fn focus_title_field(field_id: &'static str, select_all: bool) {
    if select_all {
        select_field(field_id);
    } else {
        focus_field_at_end(field_id);
    }
}

pub fn select_field(field_id: &'static str) {
    let script = document::eval(SELECT_SCRIPT);
    if let Err(error) = script.send(field_id) {
        tracing::warn!("cannot select the field {field_id}: {error}");
    }
}

/// For a file input, the click opens the file chooser.
pub fn click(element_id: &'static str) {
    let script = document::eval(CLICK_SCRIPT);
    if let Err(error) = script.send(element_id) {
        tracing::warn!("cannot click {element_id}: {error}");
    }
}

pub fn reveal(element_id: &'static str) {
    let script = document::eval(REVEAL_SCRIPT);
    if let Err(error) = script.send(element_id) {
        tracing::warn!("cannot scroll {element_id} into view: {error}");
    }
}

/// For a field whose edit a blur saves, such as a title being renamed: runs `save` when the focus
/// went elsewhere in the window, and `keep` when the window itself lost it (Alt+Tab), so the field
/// stays open with its text. Every such field uses this rule.
pub fn save_on_blur(save: impl FnOnce() + 'static, keep: impl FnOnce() + 'static) {
    // Not tied to the field's component, which the save may remove.
    spawn_forever(async move {
        match document::eval(WINDOW_FOCUSED_SCRIPT).join::<bool>().await {
            Ok(true) => save(),
            Ok(false) => keep(),
            Err(error) => {
                tracing::warn!("cannot tell whether the window has the focus, saving: {error}");
                save();
            }
        }
    });
}

/// For a list moved by the keyboard: its highlighted row may sit below the scrolled view. Call it
/// from the event handler that moved the highlight.
pub fn scroll_to_highlighted(list: HighlightedList) {
    // The render's changes reach the page through a websocket, often after a script sent now,
    // which would scroll to the row highlighted before. Dioxus runs effects once the page has
    // applied them. An effect runs outside any component, so the document is looked up here.
    let page = document::document();
    queue_effect(move || send_scroll(page.as_ref(), list));
}

/// As `scroll_to_highlighted`, from an effect, which runs once the page holds the row.
pub fn scroll_to_highlighted_now(list: HighlightedList) {
    send_scroll(document::document().as_ref(), list);
}

fn send_scroll(page: &dyn document::Document, list: HighlightedList) {
    let script = page.eval(SCROLL_TO_HIGHLIGHTED_SCRIPT.to_owned());
    if let Err(error) = script.send(list.selector()) {
        tracing::warn!("cannot scroll the highlighted row into view: {error}");
    }
}

/// Gives the focus to the element `element_id`, or with `None`, to the element the last menu took
/// it from.
pub fn give_focus_back(element_id: Option<&'static str>) {
    let script = document::eval(FOCUS_BACK_SCRIPT);
    let message = FocusBack {
        menu_focus_key: MENU_FOCUS_KEY,
        element_id,
    };
    if let Err(error) = script.send(message) {
        tracing::warn!("cannot give the focus back: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use cockpit_protocol::{
        AgentStatus, AnswerState, AskedQuestion, ItemId, PendingId, Question, Selection,
        TranscriptItem,
    };
    use strum::VariantArray;

    use super::*;
    use crate::machine::MachineId;
    use crate::state::find::FindPane;
    use crate::state::projects::ProjectKey;
    use crate::state::quotes::Opening;
    use crate::state::voice::{Destination, VoicePhase};
    use crate::state::{state_with, summary};

    #[test]
    fn the_files_pane_scrolls_its_own_highlighted_row() {
        assert_eq!(
            HighlightedList::FilesPane.selector(),
            "#files-pane .highlighted"
        );
        assert_eq!(HighlightedList::Dialog.selector(), ".dialog .highlighted");
    }

    #[test]
    fn media_types_follow_the_protocol() {
        assert_eq!(image_media_type("image/png"), Some(ImageMediaType::Png));
        assert_eq!(image_media_type("image/tiff"), None);
        assert_eq!(mime_type(ImageMediaType::Webp), "image/webp");
    }

    #[test]
    fn the_arrows_switch_questions_only_on_a_card_with_several_outside_the_map() {
        let with_questions = |count: usize| {
            let mut state = state_with(vec![summary(1, AgentStatus::NeedsYou)]);
            state.ui.selected = Some(AgentId::new(1));
            let asked = AskedQuestion {
                header: "Scope".to_owned(),
                text: "Which tests?".to_owned(),
                options: Vec::new(),
                selection: Selection::Single,
            };
            if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
                entry.transcript.append(TranscriptItem {
                    id: ItemId::new(3),
                    parent: None,
                    body: ItemBody::Question(Question {
                        pending: PendingId::new("request-3".to_owned()),
                        questions: vec![asked; count],
                        answer: AnswerState::Waiting,
                    }),
                });
            }
            state
        };
        let mut mapped = with_questions(2);
        agent_map::toggle(&mut mapped);

        let voice = VoiceState::default();
        assert!(KeyMode::new(&with_questions(2), &voice).question_tabs);
        assert!(!KeyMode::new(&with_questions(1), &voice).question_tabs);
        assert!(!KeyMode::new(&mapped, &voice).question_tabs);
    }

    #[test]
    fn the_script_takes_the_talk_key_only_while_dictation_can_run() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Exited),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        let ready = VoiceState {
            available: true,
            ..VoiceState::default()
        };
        let recording = VoiceState {
            phase: VoicePhase::Recording {
                agent: AgentId::new(1),
                to: Destination::Message,
                since: Instant::now(),
                latched: true,
            },
            ..ready.clone()
        };

        let on = KeyMode::new(&state, &ready);
        let unavailable = KeyMode::new(&state, &VoiceState::default());
        state.ui.selected = Some(AgentId::new(2));
        let exited = KeyMode::new(&state, &ready);
        // A recording that runs still stops from an exited agent.
        let exited_recording = KeyMode::new(&state, &recording);
        state.prefs.voice.enabled = false;
        let off = KeyMode::new(&state, &recording);

        assert_eq!(
            on.talk.map(|talk| (talk.ctrl, talk.key)),
            Some((true, " ".to_owned()))
        );
        assert_eq!(on.bindings.len() + 1, Command::VARIANTS.len());
        assert_eq!(unavailable.talk, None);
        assert_eq!(exited.talk, None);
        assert!(exited_recording.talk.is_some());
        assert_eq!(off.talk, None);
        assert!(!on.dictating);
        assert!(exited_recording.dictating);
    }

    #[test]
    fn enter_stops_a_dictation_in_the_field_it_writes_into_on_screen() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        let ready = VoiceState {
            available: true,
            ..VoiceState::default()
        };
        let recording = |to| VoiceState {
            phase: VoicePhase::Recording {
                agent: AgentId::new(1),
                to,
                since: Instant::now(),
                latched: true,
            },
            ..ready.clone()
        };
        let finishing = |to| VoiceState {
            phase: VoicePhase::Finishing {
                agent: AgentId::new(1),
                to,
            },
            ..ready.clone()
        };
        let comment = Destination::Comment {
            opening: Opening::default(),
        };
        let enter_stops =
            |state: &AppState, voice: &VoiceState| KeyMode::new(state, voice).enter_stops;

        assert_eq!(enter_stops(&state, &ready), None);
        assert_eq!(
            enter_stops(&state, &recording(Destination::Message)),
            Some(Field::Message)
        );
        assert_eq!(
            enter_stops(&state, &recording(comment)),
            Some(Field::Comment)
        );
        // The last words are not in yet: Enter sends nothing, and adds no quote.
        assert_eq!(
            enter_stops(&state, &finishing(Destination::Message)),
            Some(Field::Message)
        );
        assert_eq!(
            enter_stops(&state, &finishing(comment)),
            Some(Field::Comment)
        );
        state.ui.selected = Some(AgentId::new(2));
        assert_eq!(enter_stops(&state, &recording(comment)), None);
    }

    #[test]
    fn key_presses_parse_from_the_script() {
        let line = r#"{"type":"key","key":"y","ctrl":false,"shift":false,"alt":false,"meta":false,"target":"page","composer_text":""}"#;
        let find = r#"{"type":"key","key":"f","ctrl":true,"shift":false,"alt":false,"meta":false,"target":"page","composer_text":"","pane":"file","selection":"lexer"}"#;

        let message: BridgeMessage = serde_json::from_str(line).unwrap();
        let find: BridgeMessage = serde_json::from_str(find).unwrap();

        assert!(
            matches!(message, BridgeMessage::Key(press) if press.key == "y" && press.pane == FindPane::Chat)
        );
        assert!(matches!(
            find,
            BridgeMessage::Key(press) if press.pane == FindPane::File && press.selection == "lexer"
        ));
    }

    #[test]
    fn sidebar_keys_parse_from_the_script() {
        let row = r#"{"type":"sidebar_key","key":"ArrowDown","focused":{"kind":"agent","id":3}}"#;
        let head =
            r#"{"type":"sidebar_key","key":"ArrowLeft","focused":{"kind":"machine","id":1}}"#;
        let project =
            r#"{"type":"sidebar_key","key":"ArrowLeft","focused":{"kind":"project","id":"none"}}"#;

        let row: BridgeMessage = serde_json::from_str(row).unwrap();
        let head: BridgeMessage = serde_json::from_str(head).unwrap();
        let project: BridgeMessage = serde_json::from_str(project).unwrap();

        assert!(matches!(
            row,
            BridgeMessage::SidebarKey { focused: SidebarItem::Agent(agent), .. }
                if agent == AgentId::new(3)
        ));
        assert!(matches!(
            head,
            BridgeMessage::SidebarKey { focused: SidebarItem::Machine(machine), .. }
                if machine == MachineId::new(1)
        ));
        assert!(matches!(
            project,
            BridgeMessage::SidebarKey {
                focused: SidebarItem::Project(ProjectKey::None),
                ..
            }
        ));
    }

    #[test]
    fn a_mouse_side_button_message_parses_from_the_script() {
        let back = r#"{"type":"file_history","travel":"back"}"#;
        let forward = r#"{"type":"file_history","travel":"forward"}"#;

        let back: BridgeMessage = serde_json::from_str(back).unwrap();
        let forward: BridgeMessage = serde_json::from_str(forward).unwrap();

        assert!(matches!(
            back,
            BridgeMessage::FileHistory {
                travel: Travel::Back
            }
        ));
        assert!(matches!(
            forward,
            BridgeMessage::FileHistory {
                travel: Travel::Forward
            }
        ));
    }

    #[test]
    fn focus_and_image_messages_parse_from_the_script() {
        let focus = r#"{"type":"focus","focused":false}"#;
        let image = r#"{"type":"image","media_type":"image/png","data":"iVBORw0KGgo="}"#;

        let focus: BridgeMessage = serde_json::from_str(focus).unwrap();
        let image: BridgeMessage = serde_json::from_str(image).unwrap();

        assert!(matches!(focus, BridgeMessage::Focus { focused: false }));
        assert!(matches!(
            image,
            BridgeMessage::Image { media_type, data } if media_type == "image/png" && data == "iVBORw0KGgo="
        ));
    }

    /// The scripts read these fields by name: a rename in Rust would make them read `undefined`,
    /// with no error.
    #[test]
    fn the_fields_the_scripts_read_are_pinned() {
        use serde_json::json;

        let settings = Settings {
            composer_id: COMPOSER_ID,
            comment_field_id: COMMENT_FIELD_ID,
            new_agent_prompt_id: NEW_AGENT_PROMPT_ID,
            path_field_ids: [OPEN_FILE_FIELD_ID, FOLDER_FIELD_ID],
            title_field_ids: [HEADER_TITLE_FIELD_ID, ROW_TITLE_FIELD_ID],
            undo_pause_ms: 500,
            undo_steps: 200,
            map_keys: agent_map::MAP_KEYS,
            sidebar_keys: sidebar_keys::SIDEBAR_KEYS,
            answer_keys: ANSWER_KEYS,
            page_reload_key: PAGE_RELOAD_KEY,
            records_keys_attribute: RECORDS_KEYS_ATTRIBUTE,
            takes_escape_attribute: TAKES_ESCAPE_ATTRIBUTE,
            menu_focus_key: MENU_FOCUS_KEY,
            file_view_id: FILE_VIEW_ID,
            files_pane_id: FILES_PANE_ID,
            max_selection: 1_000,
            find_field_ids: FIND_FIELD_IDS,
        };
        let mode = KeyMode {
            answer_letters: true,
            answer_numbers: false,
            question_tabs: false,
            palette: true,
            dialog: false,
            map: false,
            send_again: true,
            bindings: vec![Binding {
                ctrl: false,
                alt: true,
                shift: false,
                key: "ArrowUp".to_owned(),
                reach: Reach::OutsideFields,
            }],
            talk: Some(Binding {
                ctrl: true,
                alt: false,
                shift: false,
                key: " ".to_owned(),
                reach: Reach::OutsideFields,
            }),
            dictating: true,
            enter_stops: Some(Field::Comment),
        };
        let field_text = FieldText {
            field_id: COMPOSER_ID,
            text: "Run the tests".to_owned(),
            caret: Some(Utf16Offset::new(4)),
        };
        let input = r#"{"type":"composer_input","agent":3,"text":"please run /rev","caret":15}"#;
        let prompt_input = r#"{"type":"new_agent_prompt_input","text":"/re","caret":3}"#;

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({
                "composer_id": "composer",
                "comment_field_id": "quote-comment",
                "new_agent_prompt_id": "new-agent-prompt",
                "path_field_ids": ["open-file-path", "new-agent-folder"],
                "title_field_ids": ["agent-title-field", "row-title-field"],
                "undo_pause_ms": 500,
                "undo_steps": 200,
                "map_keys": ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter"],
                "sidebar_keys": ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"],
                "answer_keys": ["y", "n"],
                "page_reload_key": "F5",
                "records_keys_attribute": "data-records-keys",
                "takes_escape_attribute": "data-takes-escape",
                "menu_focus_key": "cockpitFocusBeforeMenu",
                "file_view_id": "file-view",
                "files_pane_id": "files-pane",
                "max_selection": 1000,
                "find_field_ids": ["chat-find-field", "file-find-field"],
            })
        );
        assert_eq!(
            serde_json::to_value(mode).unwrap(),
            json!({
                "answer_letters": true,
                "answer_numbers": false,
                "question_tabs": false,
                "palette": true,
                "dialog": false,
                "map": false,
                "send_again": true,
                "bindings": [{
                    "ctrl": false,
                    "alt": true,
                    "shift": false,
                    "key": "ArrowUp",
                    "reach": "outside_fields",
                }],
                "talk": {
                    "ctrl": true,
                    "alt": false,
                    "shift": false,
                    "key": " ",
                    "reach": "outside_fields",
                },
                "dictating": true,
                "enter_stops": "comment",
            })
        );
        for (action, expected) in [
            ("press", TalkAction::Press),
            ("release", TalkAction::Release),
            ("blur", TalkAction::Blur),
            ("cancel", TalkAction::Cancel),
            ("stop", TalkAction::Stop),
        ] {
            let message = format!(r#"{{"type":"talk","action":"{action}"}}"#);
            assert!(matches!(
                serde_json::from_str(&message).unwrap(),
                BridgeMessage::Talk { action, field: Field::Message } if action == expected
            ));
        }
        for (field, expected) in [("message", Field::Message), ("comment", Field::Comment)] {
            let message = format!(r#"{{"type":"talk","action":"press","field":"{field}"}}"#);
            assert!(matches!(
                serde_json::from_str(&message).unwrap(),
                BridgeMessage::Talk { action: TalkAction::Press, field } if field == expected
            ));
        }
        assert_eq!(
            serde_json::to_value(field_text).unwrap(),
            json!({ "field_id": "composer", "text": "Run the tests", "caret": 4 })
        );
        assert_eq!(
            serde_json::to_value(FocusBack {
                menu_focus_key: MENU_FOCUS_KEY,
                element_id: None,
            })
            .unwrap(),
            json!({ "menu_focus_key": "cockpitFocusBeforeMenu", "element_id": null })
        );
        assert!(matches!(
            serde_json::from_str::<BridgeMessage>(input).unwrap(),
            BridgeMessage::ComposerInput { agent, text, caret }
                if agent == AgentId::new(3)
                    && text == "please run /rev"
                    && caret == Utf16Offset::new(15)
        ));
        assert!(matches!(
            serde_json::from_str::<BridgeMessage>(prompt_input).unwrap(),
            BridgeMessage::NewAgentPromptInput { text, caret }
                if text == "/re" && caret == Utf16Offset::new(3)
        ));
    }
}
