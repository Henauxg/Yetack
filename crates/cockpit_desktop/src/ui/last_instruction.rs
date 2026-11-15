//! The last instruction: over the top of the chat, the message the user sent that the entries in
//! view answer, once that message has scrolled out above. The eye in the Messages block hides it.

use std::path::Path;

use cockpit_protocol::{AgentId, ItemId, Timestamp};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::prefs::LastMessage;
use crate::render::{blocks_plain_text, parse_markdown};
use crate::state::RevealPlace;
use crate::ui::chat_scroll::InView;
use crate::ui::presence::{Shown, use_presence};

/// The bar's message, which covers the top of the chat for the find.
pub(super) const MESSAGE_CLASS: &str = "last-instruction-message";

/// The characters of the message the bar holds. It shows one line of them.
const SHOWN_CHARS: usize = 400;

/// How many characters of the message's Markdown the bar parses. The bar renders again at each
/// change of the state, and a pasted log can be megabytes long.
const READ_CHARS: usize = 4 * SHOWN_CHARS;

/// What the bar shows: the message's agent and item, which a click scrolls to, its text and when
/// it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Instruction {
    agent: AgentId,
    item: ItemId,
    text: String,
    sent_at: Option<Timestamp>,
}

#[component]
pub fn LastInstruction(in_view: Signal<Option<InView>>) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let instruction = use_memo(move || {
        let view = in_view()?;
        let state = cockpit.state.read();
        if state.prefs.chat.last_message == LastMessage::Hidden {
            return None;
        }
        let entry = state.selected().filter(|entry| entry.id() == view.agent)?;
        let (item, message) = entry.transcript.message_before(view.top)?;
        Some(Instruction {
            agent: view.agent,
            item,
            text: shown_text(&message.text, message.images.len(), &entry.summary.folder)?,
            sent_at: message.sent_at,
        })
    });
    let selected = use_memo(move || cockpit.state.read().ui.selected);
    // The eye hides the bar: it fades out. Another agent's chat shows at once, without it.
    let Some(Shown {
        value:
            Instruction {
                agent,
                item,
                text,
                sent_at,
            },
        leaving,
        ..
    }) = use_presence(instruction())
    else {
        return rsx! {};
    };
    if leaving && selected() != Some(agent) {
        return rsx! {};
    }
    let ago = sent_at.map(|sent_at| format::ago(sent_at, now()));
    rsx! {
        div { class: if leaving { "last-instruction leaving" } else { "last-instruction" }, inert: leaving.then_some(true),
            div { class: "last-instruction-column",
                button {
                    class: MESSAGE_CLASS,
                    title: "Show this message in the chat",
                    onclick: move |_| cockpit.update(|state| {
                        state.ui.reveal_in_chat(agent, item, RevealPlace::Top);
                    }),
                    span { class: "last-instruction-head",
                        span { "Your message" }
                        if let Some(ago) = ago {
                            span { "{ago}" }
                        }
                    }
                    span { class: "last-instruction-text", "{text}" }
                }
            }
        }
    }
}

/// A message's text without its Markdown, on one line and cut to what a line can show, or its
/// images' count when it has no text. Markdown that shows no text, such as `---`, shows as
/// written. `folder` is the agent's folder, which the chat's bubble also parses with.
pub fn shown_text(text: &str, image_count: usize, folder: &Path) -> Option<String> {
    let markdown: String = text.chars().take(READ_CHARS).collect();
    let plain = blocks_plain_text(&parse_markdown(&markdown, folder));
    let line: Option<String> = [plain.as_str(), markdown.as_str()]
        .into_iter()
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|line| !line.is_empty());
    if let Some(line) = line {
        return Some(line.chars().take(SHOWN_CHARS).collect());
    }
    (image_count > 0).then(|| format::count(image_count, "image", "images"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_shows_the_text_without_markdown_cut_or_the_images_count() {
        let shown = |text: &str, images| shown_text(text, images, Path::new("/work"));
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
