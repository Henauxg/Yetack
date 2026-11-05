//! The page's script. A key's default action (typing a letter, a new line on Enter) can be
//! prevented only in the page, so the script decides which key presses to take, from the mode
//! Rust sends it, and passes them on with the window's focus and pasted images.

use cockpit_protocol::{Image, ImageMediaType};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::{AppState, KeyPress, PaletteOrigin, Popover, WaitingKind, actions, handle_key};

/// The composer's text area, whose keys the script treats apart.
pub const COMPOSER_ID: &str = "composer";

/// The new agent dialog's folder field.
pub const FOLDER_FIELD_ID: &str = "new-agent-folder";

/// The hidden file input the composer's attach button opens.
pub const ATTACH_INPUT_ID: &str = "attach-input";

/// The filter field of the palette the slash button opens.
pub const PALETTE_FILTER_ID: &str = "palette-filter";

/// The palette's highlighted entry, kept in view as the keys move it.
pub const HIGHLIGHTED_ENTRY_ID: &str = "palette-highlighted";

/// What a note calls an image pasted into the window.
const PASTED_IMAGE: &str = "The pasted image";

/// Waits for its settings, then passes on events. Message and field names are serde's names for
/// `BridgeMessage`, `KeyPress`, `KeyTarget` and `KeyMode`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
let mode = { answer_letters: false, answer_numbers: false, palette: false, dialog: false };
const send = (message) => dioxus.send(message);
(async () => {
  for (;;) {
    mode = await dioxus.recv();
  }
})();
window.addEventListener("focus", () => send({ type: "focus", focused: true }));
window.addEventListener("blur", () => send({ type: "focus", focused: false }));
document.addEventListener("keydown", (event) => {
  if (event.isComposing) {
    return;
  }
  const target = event.target;
  const composer = target !== null && target.id === settings.composer_id;
  const field = !composer && target !== null
    && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable);
  const where = composer ? "composer" : field ? "field" : "page";
  const key = event.key;
  const command = event.ctrlKey || event.metaKey;
  const plain = !command && !event.altKey;
  const letter = key.length === 1 ? key.toLowerCase() : "";
  const answers = (mode.answer_letters && (letter === "y" || letter === "n"))
    || (mode.answer_numbers && (/^[1-9]$/.test(key) || key === "Enter"));
  const text = composer ? target.value : "";
  let take = false;
  if (command && !event.altKey && !event.shiftKey && (letter === "k" || letter === "n")) {
    take = true;
  } else if (key === "Escape") {
    take = true;
  } else if (where === "composer" && plain) {
    take = (mode.palette && ["ArrowUp", "ArrowDown", "Enter", "Tab"].includes(key))
      || (text === "" && answers)
      || (key === "Enter" && !event.shiftKey);
  } else if (where === "page" && plain && !mode.dialog && answers) {
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
const { field_id, text } = await dioxus.recv();
for (let frame = 0; frame < 30; frame++) {
  const field = document.getElementById(field_id);
  if (field !== null) {
    field.value = text;
    field.selectionStart = field.selectionEnd = text.length;
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

/// Brings a dialog's highlighted row into view, after the render that moved the highlight.
const SCROLL_TO_HIGHLIGHTED_SCRIPT: &str = r#"
await new Promise((resolve) => requestAnimationFrame(resolve));
document.querySelector(".dialog .highlighted")?.scrollIntoView({ block: "nearest" });
"#;

#[derive(Debug, Serialize)]
struct FieldText {
    field_id: &'static str,
    text: String,
}

#[derive(Debug, Serialize)]
struct Settings {
    composer_id: &'static str,
}

/// Which plain keys the script takes, beyond Ctrl+K, Ctrl+N, Esc and the composer's Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct KeyMode {
    /// Y and N answer an approval or a plan.
    answer_letters: bool,
    /// 1 to 9 and Enter answer a question.
    answer_numbers: bool,
    /// Up, Down, Enter and Tab move in the command palette opened by typing `/`. The one the
    /// slash button opens takes them in its own filter field.
    palette: bool,
    dialog: bool,
}

impl KeyMode {
    fn new(state: &AppState) -> Self {
        let waiting = state
            .selected()
            .and_then(|entry| entry.transcript.waiting())
            .map(|waiting| waiting.kind);
        Self {
            answer_letters: matches!(waiting, Some(WaitingKind::Approval | WaitingKind::Plan)),
            answer_numbers: waiting == Some(WaitingKind::Question),
            palette: state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)),
            dialog: state.ui.dialog.is_some(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeMessage {
    Key(KeyPress),
    Focus { focused: bool },
    Image { media_type: String, data: String },
}

/// Runs the script for the window's life, and keeps its key mode in step with the state.
pub fn use_bridge(cockpit: Cockpit) {
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(Settings {
            composer_id: COMPOSER_ID,
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
        BridgeMessage::Focus { focused } => {
            cockpit.update(|state| state.ui.window_focused = focused);
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
    let script = document::eval(FIELD_TEXT_SCRIPT);
    let message = FieldText { field_id, text };
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

/// For a list moved by the keyboard: its highlighted row may sit below the scrolled view.
pub fn scroll_to_highlighted() {
    document::eval(SCROLL_TO_HIGHLIGHTED_SCRIPT);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let mode = KeyMode {
            answer_letters: true,
            answer_numbers: false,
            palette: true,
            dialog: false,
        };
        let field_text = FieldText {
            field_id: COMPOSER_ID,
            text: "Run the tests".to_owned(),
        };

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({ "composer_id": "composer" })
        );
        assert_eq!(
            serde_json::to_value(mode).unwrap(),
            json!({
                "answer_letters": true,
                "answer_numbers": false,
                "palette": true,
                "dialog": false,
            })
        );
        assert_eq!(
            serde_json::to_value(field_text).unwrap(),
            json!({ "field_id": "composer", "text": "Run the tests" })
        );
    }
}
