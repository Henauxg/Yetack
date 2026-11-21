//! The Messages block: the messages the user sent to the agent, oldest first with their times,
//! then the prompts Claude has not read yet. The block marks the message being read: at the chat's
//! bottom the latest one on screen, scrolled up the one whose answer shows at the top. A click on a
//! sent message scrolls the chat to it.

use std::path::{Path, PathBuf};

use cockpit_protocol::{AgentId, ItemId, Timestamp, WaitingPrompt};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::render::{blocks_plain_text, parse_markdown};
use crate::state::RevealPlace;
use crate::ui::blocks::KEPT_ROWS;
use crate::ui::chat_scroll::InView;

/// The characters of a message that its row holds. The row shows one line of them.
const SHOWN_CHARS: usize = 400;

/// How many characters of a message's Markdown its row parses: a pasted log can be megabytes long.
const READ_CHARS: usize = 4 * SHOWN_CHARS;

/// A sent message's row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentRow {
    item: ItemId,
    text: String,
    sent_at: Option<Timestamp>,
}

/// The rows of an agent's sent messages.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentRows {
    agent: AgentId,
    rows: Vec<SentRow>,
}

/// The rows of the agent's sent messages. Parsing their Markdown takes time, so the rows are built
/// again only when a message comes or another agent shows.
fn use_sent_rows(cockpit: Cockpit) -> Memo<Option<SentRows>> {
    let sent = use_memo(move || {
        let state = cockpit.state.read();
        let entry = state.selected()?;
        let items: Vec<ItemId> = entry.transcript.sent_messages().map(|(id, _)| id).collect();
        Some((entry.id(), items))
    });
    use_memo(move || {
        let (agent, _) = sent()?;
        let state = cockpit.state.peek();
        let entry = state.selected().filter(|entry| entry.id() == agent)?;
        let folder = &entry.summary.folder;
        let rows = entry
            .transcript
            .sent_messages()
            .map(|(item, message)| SentRow {
                item,
                text: shown_text(&message.text, message.images.len(), folder).unwrap_or_default(),
                sent_at: message.sent_at,
            })
            .collect();
        Some(SentRows { agent, rows })
    })
}

#[component]
pub fn MessagesBody(
    agent: AgentId,
    folder: PathBuf,
    waiting: Vec<WaitingPrompt>,
    in_view: Signal<Option<InView>>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    let rows = use_sent_rows(cockpit);
    let marked = use_memo(move || {
        let view = in_view()?;
        let state = cockpit.state.read();
        let entry = state.selected().filter(|entry| entry.id() == view.agent)?;
        entry
            .transcript
            .message_read(view.top, view.bottom, !view.at_bottom)
    });
    let marked = marked();
    let rows = rows()
        .filter(|sent| sent.agent == agent)
        .map(|sent| sent.rows)
        .unwrap_or_default();
    // The list stacks from the bottom, so it opens on the latest messages: its first child is the
    // last row.
    rsx! {
        div { class: "message-list scroll", role: "list", "data-kept-rows": KEPT_ROWS,
            for prompt in waiting.into_iter().rev() {
                {
                    let image_count = usize::try_from(prompt.image_count).unwrap_or(usize::MAX);
                    let text = shown_text(&prompt.text, image_count, &folder).unwrap_or_default();
                    let ago = prompt.sent_at.map(|sent_at| format::ago(sent_at, now));
                    rsx! {
                        div { key: "waiting-{prompt.id.get()}", class: "message-row waiting", role: "listitem",
                            div { class: "message-entry",
                                span { class: "message-meta",
                                    span { "You · not read yet" }
                                    if let Some(ago) = ago {
                                        span { "{ago}" }
                                    }
                                }
                                span { class: "message-text", "{text}" }
                            }
                        }
                    }
                }
            }
            for row in rows.into_iter().rev() {
                {
                    let here = marked == Some(row.item);
                    let item = row.item;
                    let who = if here { "You · in view" } else { "You" };
                    let ago = row.sent_at.map(|sent_at| format::ago(sent_at, now));
                    rsx! {
                        div { key: "{item.get()}", class: if here { "message-row in-view" } else { "message-row" }, role: "listitem",
                            button {
                                class: "message-entry message-link",
                                aria_current: if here { "location" } else { "false" },
                                title: "Show this message in the chat",
                                onclick: move |_| cockpit.update(|state| {
                                    state.ui.reveal_in_chat(agent, item, RevealPlace::Top);
                                }),
                                span { class: "message-meta",
                                    span { "{who}" }
                                    if let Some(ago) = ago {
                                        span { "{ago}" }
                                    }
                                }
                                span { class: "message-text", "{row.text}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A message's text without its Markdown, on one line and cut to what a line can show, or its
/// images' count when it has no text. Markdown that shows no text, such as `---`, shows as
/// written. `folder` is the agent's folder, which the chat's bubble also parses with.
fn shown_text(text: &str, image_count: usize, folder: &Path) -> Option<String> {
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
    fn a_row_shows_the_text_without_markdown_cut_or_the_images_count() {
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
