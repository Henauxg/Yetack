//! The Messages block: the messages the user sent to the agent, oldest first with their times,
//! then the prompts Claude has not read yet. The block marks the message being read: at the chat's
//! bottom the latest one on screen, scrolled up the one whose answer shows at the top. A click on a sent message scrolls the chat to it. The eye in the
//! block's head shows or hides the last message over the chat.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, ItemId, Timestamp, WaitingPrompt};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::prefs::LastMessage;
use crate::state::RevealPlace;
use crate::ui::chat_scroll::InView;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::last_instruction::shown_text;

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
        div { class: "message-list scroll", role: "list",
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

/// The eye in the block's head: shows or hides the last message over the chat.
#[component]
pub fn LastMessageToggle() -> Element {
    let cockpit: Cockpit = use_context();
    let shown = cockpit.state.read().prefs.chat.last_message == LastMessage::Shown;
    let (label, icon) = if shown {
        ("Hide your last message over the chat", IconKind::Eye)
    } else {
        ("Show your last message over the chat", IconKind::EyeOff)
    };
    rsx! {
        button {
            class: "block-head-action",
            aria_pressed: shown,
            aria_label: label,
            title: label,
            onclick: move |_| cockpit.update(|state| {
                state.prefs.chat.last_message = state.prefs.chat.last_message.toggled();
            }),
            Icon { kind: icon, size: 14 }
        }
    }
}
