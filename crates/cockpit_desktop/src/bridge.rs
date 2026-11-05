//! The page's script. A key's default action (typing a letter, a new line on Enter) can be
//! prevented only in the page, so the script decides which key presses to take, from the mode
//! Rust sends it, and passes them on with the window's focus and pasted images.

use cockpit_protocol::{Image, ImageMediaType};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::{AppState, KeyPress, Popover, WaitingKind, actions, handle_key};

/// The composer's text area, whose keys the script treats apart.
pub const COMPOSER_ID: &str = "composer";

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
    /// Up, Down, Enter and Tab move in the command palette.
    palette: bool,
    dialog: bool,
}

impl KeyMode {
    fn of(state: &AppState) -> Self {
        let waiting = state
            .selected()
            .and_then(|entry| entry.transcript.waiting())
            .map(|waiting| waiting.kind);
        Self {
            answer_letters: matches!(waiting, Some(WaitingKind::Approval | WaitingKind::Plan)),
            answer_numbers: waiting == Some(WaitingKind::Question),
            palette: state.ui.popover == Some(Popover::Palette),
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
    let mode = use_memo(move || KeyMode::of(&cockpit.state.read()));
    use_effect(move || {
        if let Err(error) = script.send(mode()) {
            tracing::warn!("cannot update the page's script: {error}");
        }
    });
}

fn receive(cockpit: Cockpit, message: BridgeMessage) {
    match message {
        BridgeMessage::Key(press) => cockpit.change(|state| handle_key(state, &press)),
        BridgeMessage::Focus { focused } => {
            cockpit.update(|state| state.ui.window_focused = focused);
        }
        BridgeMessage::Image { media_type, data } => {
            let Some(media_type) = image_media_type(&media_type) else {
                tracing::warn!("cannot attach an image of type {media_type}");
                return;
            };
            let image = Image { media_type, data };
            cockpit.update(|state| {
                if let Some(agent) = state.ui.selected {
                    actions::add_image(state, agent, image);
                }
            });
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
}
