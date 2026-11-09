//! The last instruction: over the top of the chat, the message the user sent that the entries in
//! view answer, once that message has scrolled out above.

use std::path::Path;

use cockpit_protocol::{AgentId, ItemId, UserMessage};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::render::{blocks_plain_text, parse_markdown};
use crate::ui::chat_scroll::TopEntry;

/// The characters of the message the bar holds. It shows two lines of them.
const SHOWN_CHARS: usize = 400;

/// How many characters of the message's Markdown the bar parses. The bar renders again at each
/// change of the state, and a pasted log can be megabytes long.
const READ_CHARS: usize = 4 * SHOWN_CHARS;

/// What the bar shows: the message's agent and item, which a click scrolls to, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Instruction {
    agent: AgentId,
    item: ItemId,
    text: String,
}

#[component]
pub fn LastInstruction(top_entry: Signal<Option<TopEntry>>) -> Element {
    let cockpit: Cockpit = use_context();
    let instruction = use_memo(move || {
        let top = top_entry()?;
        let state = cockpit.state.read();
        let entry = state.selected().filter(|entry| entry.id() == top.agent)?;
        let (item, message) = entry.transcript.message_before(top.item)?;
        Some(Instruction {
            agent: top.agent,
            item,
            text: shown_text(message, &entry.summary.folder)?,
        })
    });
    let Some(Instruction { agent, item, text }) = instruction() else {
        return rsx! {};
    };
    rsx! {
        div { class: "last-instruction",
            div { class: "last-instruction-column",
                button {
                    class: "last-instruction-message",
                    title: "Show this message in the chat",
                    onclick: move |_| cockpit.update(|state| state.ui.reveal_in_chat(agent, item)),
                    span { class: "last-instruction-text", "{text}" }
                }
            }
        }
    }
}

/// The message's text without its Markdown, on one line and cut to what two lines can show, or
/// its images' count when it has no text. Markdown that shows no text, such as `---`, shows as
/// written. `folder` is the agent's folder, which the chat's bubble also parses with.
fn shown_text(message: &UserMessage, folder: &Path) -> Option<String> {
    let markdown: String = message.text.chars().take(READ_CHARS).collect();
    let plain = blocks_plain_text(&parse_markdown(&markdown, folder));
    let line: Option<String> = [plain.as_str(), markdown.as_str()]
        .into_iter()
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|line| !line.is_empty());
    if let Some(line) = line {
        return Some(line.chars().take(SHOWN_CHARS).collect());
    }
    (!message.images.is_empty()).then(|| format::count(message.images.len(), "image", "images"))
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Image, ImageMediaType};

    use super::*;

    fn message(text: &str, images: usize) -> UserMessage {
        let image = Image {
            media_type: ImageMediaType::Png,
            data: String::new(),
        };
        UserMessage {
            text: text.to_owned(),
            images: vec![image; images],
        }
    }

    #[test]
    fn the_bar_shows_the_text_without_markdown_cut_or_the_images_count() {
        let shown = |text: &str, images| shown_text(&message(text, images), Path::new("/work"));
        let long = "é".repeat(READ_CHARS);

        assert_eq!(
            shown("Fix it:\n\n- count from **1**\n- test `parse`\n", 1),
            Some("Fix it: count from 1 test parse".to_owned())
        );
        assert_eq!(shown(&long, 0), Some("é".repeat(SHOWN_CHARS)));
        assert_eq!(shown(" ", 2), Some("2 images".to_owned()));
        assert_eq!(shown("", 0), None);
        assert_eq!(shown("---", 0), Some("---".to_owned()));
        assert_eq!(
            shown("[1]: https://example.org", 1),
            Some("[1]: https://example.org".to_owned())
        );
    }
}
