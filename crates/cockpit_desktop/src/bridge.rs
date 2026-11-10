//! The page's script. A key's default action (typing a letter, a new line on Enter) can be
//! prevented only in the page, so the script decides which key presses to take, from the mode
//! Rust sends it, and passes them on with the window's focus and pasted images.

use cockpit_protocol::{AgentId, Image, ImageMediaType};
use dioxus::dioxus_core::{queue_effect, spawn_forever};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::sidebar_keys::SidebarItem;
use crate::state::{
    ANSWER_KEYS, AppState, CLOSE_AGENT_KEY, COMMAND_KEYS, Dialog, KeyPress, MARK_KEY,
    PaletteOrigin, Popover, RELOAD_KEY, Utf16Offset, WaitingKind, accounts, actions, agent_map,
    file_reread, handle_key, sidebar_keys,
};
use crate::ui::sidebar_item_id;

/// The composer's text area, whose keys the script treats apart.
pub const COMPOSER_ID: &str = "composer";

/// The new agent dialog's folder field.
pub const FOLDER_FIELD_ID: &str = "new-agent-folder";

/// The "Open file" dialog's path field.
pub const OPEN_FILE_FIELD_ID: &str = "open-file-path";

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

/// What a note calls an image pasted into the window.
const PASTED_IMAGE: &str = "The pasted image";

/// Waits for its settings, then passes on events. Message and field names are serde's names for
/// `BridgeMessage`, `KeyPress`, `KeyTarget` and `KeyMode`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
let mode = { answer_letters: false, answer_numbers: false, palette: false, dialog: false };
const send = (message) => dioxus.send(message);
// The sidebar row or machine header that has the focus, as the app names it.
const sidebarItem = (element) => {
  const data = element?.dataset;
  if (data?.rowAgent !== undefined) {
    return { kind: "agent", id: Number(data.rowAgent) };
  }
  if (data?.headMachine !== undefined) {
    return { kind: "machine", id: Number(data.headMachine) };
  }
  return null;
};
(async () => {
  for (;;) {
    mode = await dioxus.recv();
  }
})();
window.addEventListener("focus", () => send({ type: "focus", focused: true }));
window.addEventListener("blur", () => send({ type: "focus", focused: false }));
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
  const command = event.ctrlKey || event.metaKey;
  const plain = !command && !event.altKey;
  const letter = key.length === 1 ? key.toLowerCase() : "";
  if (command && !event.altKey && (letter === "z" || (letter === "y" && !event.shiftKey)) && isTextField(target)) {
    event.preventDefault();
    undo(target, letter === "y" || event.shiftKey);
    return;
  }
  const answers = (mode.answer_letters && settings.answer_keys.includes(letter))
    || (mode.answer_numbers && (/^[1-9]$/.test(key) || key === "Enter"));
  // The arrows on a sidebar row or machine header move the selection and fold the machines.
  const focused = sidebarItem(target);
  if (focused !== null && plain && !event.shiftKey && !mode.dialog && settings.sidebar_keys.includes(key)) {
    event.preventDefault();
    send({ type: "sidebar_key", key, focused });
    return;
  }
  const text = composer ? target.value : "";
  let take = false;
  if (command && !event.altKey && !event.shiftKey && settings.command_keys.includes(letter)) {
    take = true;
  } else if (command && !event.altKey && event.shiftKey && letter === settings.close_agent_key) {
    // Ctrl+Shift+W closes the selected agent. The file view takes Ctrl+W alone.
    take = true;
  } else if (command && !event.altKey && event.shiftKey && letter === settings.mark_key) {
    // Ctrl+Shift+H marks the selected agent as one to handle.
    take = true;
  } else if (key === "Escape") {
    take = true;
  } else if (key === settings.reload_key && plain && !event.shiftKey) {
    // F5 reads the shown file again. Taken everywhere, it never reloads the page.
    take = true;
  } else if (key === "F2" && plain && !event.shiftKey) {
    // F2 renames the selected agent, and moves an open title field between the header and the
    // agent's row.
    take = !mode.dialog && (where === "page" || (target !== null && settings.title_field_ids.includes(target.id)));
  } else if (event.altKey && !command && !event.shiftKey && (key === "ArrowUp" || key === "ArrowDown")) {
    // Alt+Up and Alt+Down move the selected agent in the sidebar.
    take = where !== "field" && !mode.dialog;
  } else if (mode.map) {
    // The map moves and folds with its keys wherever the focus is, but in a text field. Enter on
    // one of its buttons still presses the button.
    const mapKey = settings.map_keys.includes(key) || (mode.answer_letters && settings.answer_keys.includes(letter));
    take = where !== "field" && plain && !event.shiftKey && mapKey
      && !(key === "Enter" && target !== null && target.tagName === "BUTTON");
  } else if (where === "composer" && event.altKey && !command && !event.shiftKey && key === "Enter") {
    // Alt+Enter queues the message until the turn ends.
    take = true;
  } else if (where === "composer" && plain) {
    take = (mode.palette && ["ArrowUp", "ArrowDown", "Enter", "Tab"].includes(key))
      || (text === "" && answers)
      || (key === "Enter" && !event.shiftKey);
  } else if (where === "page" && plain && !mode.dialog
    // A tree or a menu takes its own keys: Enter or a digit there acts on it, and answers nothing.
    && !(target !== null && target.closest("[data-own-keys]") !== null)
    && (answers || (mode.send_again && key === "Enter" && !event.shiftKey))) {
    take = !(key === "Enter" && target !== null && target.tagName === "BUTTON");
  }
  if (!take) {
    return;
  }
  event.preventDefault();
  send({
    type: "key",
    key,
    ctrl: event.ctrlKey,
    shift: event.shiftKey,
    alt: event.altKey,
    meta: event.metaKey,
    target: where,
    composer_text: text,
  });
}, true);
// The composer's text goes to the app with the caret, which tells the "/" word being typed.
document.addEventListener("input", (event) => {
  const target = event.target;
  if (target === null || target.id !== settings.composer_id) {
    return;
  }
  const agent = Number(target.dataset.agent);
  if (Number.isInteger(agent)) {
    send({ type: "composer_input", agent, text: target.value, caret: target.selectionStart ?? target.value.length });
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

/// Gives the focus to the field whose id it receives, the caret at the end of its text.
const FOCUS_AT_END_SCRIPT: &str = r#"
const field_id = await dioxus.recv();
const field = document.getElementById(field_id);
if (field !== null) {
  field.focus();
  field.setSelectionRange(field.value.length, field.value.length);
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

/// Brings into view a dialog's row that has the class it receives.
const SCROLL_TO_HIGHLIGHTED_SCRIPT: &str = r#"
const class_name = await dioxus.recv();
document.querySelector(`.dialog .${class_name}`)?.scrollIntoView({ block: "nearest" });
"#;

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
    /// The letters that take Ctrl alone.
    command_keys: [&'static str; 5],
    /// The letter that closes the selected agent with Ctrl+Shift.
    close_agent_key: &'static str,
    /// The letter that marks the selected agent as one to handle, with Ctrl+Shift.
    mark_key: &'static str,
    /// The key that reads the shown file again.
    reload_key: &'static str,
}

/// Which plain keys the script takes, beyond Ctrl+K, Ctrl+N, Ctrl+O, Ctrl+M, Ctrl+Shift+W,
/// Ctrl+Shift+H, Esc, F2, Alt+Up, Alt+Down and the composer's Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct KeyMode {
    /// Y and N answer an approval or a plan: in the agent map, the selected subagent's.
    answer_letters: bool,
    /// 1 to 9 and Enter answer a question.
    answer_numbers: bool,
    /// Up, Down, Enter and Tab move in the command palette opened by typing `/`. The one the
    /// slash button opens takes them in its own filter field.
    palette: bool,
    dialog: bool,
    /// Up, Down, Left, Right and Enter move and fold in the agent map.
    map: bool,
    /// Enter, outside text fields, presses the sign-in card's "Send the message again".
    send_again: bool,
}

impl KeyMode {
    fn new(state: &AppState) -> Self {
        let map = matches!(state.ui.dialog, Some(Dialog::AgentMap(_)));
        // In the map, the keys answer the selected subagent's card, not the agent's newest one.
        let waiting = if map {
            agent_map::selected_waiting(state)
        } else {
            state
                .selected()
                .and_then(|entry| entry.transcript.waiting())
        };
        let waiting = waiting.map(|waiting| waiting.kind);
        Self {
            answer_letters: matches!(waiting, Some(WaitingKind::Approval | WaitingKind::Plan)),
            answer_numbers: !map && waiting == Some(WaitingKind::Question),
            palette: state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)),
            dialog: state.ui.dialog.is_some(),
            map,
            send_again: state
                .ui
                .selected
                .is_some_and(|agent| accounts::sends_again(state, agent)),
        }
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
    Focus {
        focused: bool,
    },
    Image {
        media_type: String,
        data: String,
    },
}

/// Runs the script for the window's life, and keeps its key mode in step with the state.
pub fn use_bridge(cockpit: Cockpit) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(Settings {
            composer_id: COMPOSER_ID,
            path_field_ids: [OPEN_FILE_FIELD_ID, FOLDER_FIELD_ID],
            title_field_ids: [HEADER_TITLE_FIELD_ID, ROW_TITLE_FIELD_ID],
            undo_pause_ms: UNDO_PAUSE_MS,
            undo_steps: UNDO_STEPS,
            map_keys: agent_map::MAP_KEYS,
            sidebar_keys: sidebar_keys::SIDEBAR_KEYS,
            answer_keys: ANSWER_KEYS,
            command_keys: COMMAND_KEYS,
            close_agent_key: CLOSE_AGENT_KEY,
            mark_key: MARK_KEY,
            reload_key: RELOAD_KEY,
        }) {
            tracing::warn!("cannot start the page's script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<BridgeMessage>().await {
                    Ok(message) => receive(cockpit, message),
                    Err(error) => {
                        tracing::warn!("the page's script stopped: {error}");
                        return;
                    }
                }
            }
        });
        script
    });
    let mode = use_memo(move || KeyMode::new(&cockpit.state.read()));
    use_effect(move || {
        if let Err(error) = script.send(mode()) {
            tracing::warn!("cannot update the page's script: {error}");
        }
    });
}

fn receive(cockpit: Cockpit, message: BridgeMessage) {
    match message {
        BridgeMessage::Key(press) => {
            cockpit.change_with_drafts(|state, drafts| handle_key(state, drafts, &press));
        }
        BridgeMessage::SidebarKey { key, focused } => {
            let mut next = None;
            cockpit.update(|state| next = sidebar_keys::press(state, focused, &key));
            if let Some(item) = next {
                focus_element(sidebar_item_id(item));
            }
        }
        BridgeMessage::ComposerInput { agent, text, caret } => {
            cockpit.type_in_composer(agent, text, caret);
        }
        BridgeMessage::Focus { focused } => {
            cockpit.update(|state| file_reread::window_focused(state, focused));
        }
        BridgeMessage::Image { media_type, data } => {
            let Some(media_type) = image_media_type(&media_type) else {
                tracing::warn!("cannot attach an image of type {media_type}");
                return;
            };
            let image = Image { media_type, data };
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
    format!("data:{};base64,{}", mime_type(image.media_type), image.data)
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

pub fn focus_field_at_end(field_id: &'static str) {
    let script = document::eval(FOCUS_AT_END_SCRIPT);
    if let Err(error) = script.send(field_id) {
        tracing::warn!("cannot focus the field {field_id}: {error}");
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
pub fn scroll_to_highlighted() {
    // The render's changes reach the page through a websocket, often after a script sent now,
    // which would scroll to the row highlighted before. Dioxus runs effects once the page has
    // applied them. An effect runs outside any component, so the document is looked up here.
    let page = document::document();
    queue_effect(move || {
        let script = page.eval(SCROLL_TO_HIGHLIGHTED_SCRIPT.to_owned());
        if let Err(error) = script.send(HIGHLIGHTED_CLASS) {
            tracing::warn!("cannot scroll the highlighted row into view: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::MachineId;

    #[test]
    fn media_types_follow_the_protocol() {
        assert_eq!(image_media_type("image/png"), Some(ImageMediaType::Png));
        assert_eq!(image_media_type("image/tiff"), None);
        assert_eq!(mime_type(ImageMediaType::Webp), "image/webp");
    }

    #[test]
    fn key_presses_parse_from_the_script() {
        let line = r#"{"type":"key","key":"y","ctrl":false,"shift":false,"alt":false,"meta":false,"target":"page","composer_text":""}"#;

        let message: BridgeMessage = serde_json::from_str(line).unwrap();

        assert!(matches!(message, BridgeMessage::Key(press) if press.key == "y"));
    }

    #[test]
    fn sidebar_keys_parse_from_the_script() {
        let row = r#"{"type":"sidebar_key","key":"ArrowDown","focused":{"kind":"agent","id":3}}"#;
        let head =
            r#"{"type":"sidebar_key","key":"ArrowLeft","focused":{"kind":"machine","id":1}}"#;

        let row: BridgeMessage = serde_json::from_str(row).unwrap();
        let head: BridgeMessage = serde_json::from_str(head).unwrap();

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
            path_field_ids: [OPEN_FILE_FIELD_ID, FOLDER_FIELD_ID],
            title_field_ids: [HEADER_TITLE_FIELD_ID, ROW_TITLE_FIELD_ID],
            undo_pause_ms: 500,
            undo_steps: 200,
            map_keys: agent_map::MAP_KEYS,
            sidebar_keys: sidebar_keys::SIDEBAR_KEYS,
            answer_keys: ANSWER_KEYS,
            command_keys: COMMAND_KEYS,
            close_agent_key: CLOSE_AGENT_KEY,
            mark_key: MARK_KEY,
            reload_key: RELOAD_KEY,
        };
        let mode = KeyMode {
            answer_letters: true,
            answer_numbers: false,
            palette: true,
            dialog: false,
            map: false,
            send_again: true,
        };
        let field_text = FieldText {
            field_id: COMPOSER_ID,
            text: "Run the tests".to_owned(),
            caret: Some(Utf16Offset::new(4)),
        };
        let input = r#"{"type":"composer_input","agent":3,"text":"please run /rev","caret":15}"#;

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({
                "composer_id": "composer",
                "path_field_ids": ["open-file-path", "new-agent-folder"],
                "title_field_ids": ["agent-title-field", "row-title-field"],
                "undo_pause_ms": 500,
                "undo_steps": 200,
                "map_keys": ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter"],
                "sidebar_keys": ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"],
                "answer_keys": ["y", "n"],
                "command_keys": ["k", "n", "o", "m", "b"],
                "close_agent_key": "w",
                "mark_key": "h",
                "reload_key": "F5",
            })
        );
        assert_eq!(
            serde_json::to_value(mode).unwrap(),
            json!({
                "answer_letters": true,
                "answer_numbers": false,
                "palette": true,
                "dialog": false,
                "map": false,
                "send_again": true,
            })
        );
        assert_eq!(
            serde_json::to_value(field_text).unwrap(),
            json!({ "field_id": "composer", "text": "Run the tests", "caret": 4 })
        );
        assert!(matches!(
            serde_json::from_str::<BridgeMessage>(input).unwrap(),
            BridgeMessage::ComposerInput { agent, text, caret }
                if agent == AgentId::new(3)
                    && text == "please run /rev"
                    && caret == Utf16Offset::new(15)
        ));
    }
}
