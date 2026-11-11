//! The composer: attachments, the prompt, the slash button, the prompt cache timer, the model,
//! effort and permission mode menus, and Send and Stop.

use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use cockpit_protocol::{
    AgentId, AgentStatus, Effort, Image, ImageMediaType, PermissionMode, PromptDelivery,
    SubagentStatus,
};
use dioxus::html::FileData;
use dioxus::prelude::*;

use crate::bridge::{self, ATTACH_INPUT_ID, COMPOSER_ID};
use crate::connection::{Clock, Cockpit};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::keymap::Command;
use crate::state::actions::ImageTooLarge;
use crate::state::sending::{SendingPlace, SendingPrompt};
use crate::state::{Draft, PaletteOrigin, Popover, accounts, actions, agent_map};
use crate::ui::header::cache_left;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::palette::Palette;
use crate::ui::presence::Presence;
use crate::ui::quotes::{QuoteRows, chat_is_wide};
use crate::ui::waiting_prompts::WaitingPrompts;

pub fn mode_icon(mode: PermissionMode) -> IconKind {
    match mode {
        PermissionMode::Default | PermissionMode::DontAsk => IconKind::Shield,
        PermissionMode::AcceptEdits => IconKind::Pencil,
        PermissionMode::Plan => IconKind::List,
        PermissionMode::Auto | PermissionMode::BypassPermissions => IconKind::Bolt,
    }
}

fn placeholder(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::NeedsYou => "Answer above, or tell Claude what to do instead",
        AgentStatus::Working => {
            "Claude reads this at its next step. Alt+Enter waits for the turn to end."
        }
        AgentStatus::Idle => "Ask for a change. Type / for commands and skills.",
        AgentStatus::Exited => "Resume the agent to send a message",
    }
}

/// After a send, a Stop click within this time does nothing. The send empties the message, so the
/// button under the pointer turns into Stop, and the second click of a double click lands on it.
const STOP_AFTER_SEND: Duration = Duration::from_secs(1);

/// What the button at the end of the composer does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndButton {
    /// The agent works or waits for you, and the message is empty.
    Stop,
    /// The agent works or waits for you: Claude reads the message at its next step.
    SendNow,
    /// The agent is idle or has exited. The button is off while the message is empty.
    Send,
}

impl EndButton {
    fn new(status: AgentStatus, draft: &Draft) -> Self {
        match status {
            AgentStatus::Working | AgentStatus::NeedsYou if draft.is_empty() => Self::Stop,
            AgentStatus::Working | AgentStatus::NeedsYou => Self::SendNow,
            AgentStatus::Idle | AgentStatus::Exited => Self::Send,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Stop => "Stop the agent",
            Self::SendNow | Self::Send => "Send the message",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Stop => self.label(),
            Self::SendNow => {
                "Send now: Claude reads it at its next step (Enter). Alt+Enter queues it until the turn ends."
            }
            Self::Send => "Send the message (Enter)",
        }
    }
}

#[component]
pub fn Composer() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    // When this composer last sent: see `STOP_AFTER_SEND`.
    let mut sent_at = use_signal(|| None::<Instant>);
    // The text area is not bound to the draft, whose renders can lag behind typing: it takes the
    // draft's text only when another agent opens or the app changes the draft.
    let shown = use_memo(move || {
        let selected = cockpit.state.read().ui.selected;
        let writes = selected.map(|agent| cockpit.drafts.read().composer_writes(agent));
        (selected, writes)
    });
    use_effect(move || {
        let (agent, _) = shown();
        let (text, caret) = agent
            .and_then(|agent| {
                let drafts = cockpit.drafts.peek();
                let caret = drafts.written_caret(agent);
                drafts.draft(agent).map(|draft| (draft.text.clone(), caret))
            })
            .unwrap_or_default();
        bridge::set_field_text_at(COMPOSER_ID, text, caret);
    });
    let state = cockpit.state.read();
    let drafts = cockpit.drafts.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent = entry.id();
    let summary = &entry.summary;
    let status = accounts::turn_status(summary);
    let draft = drafts.draft(agent).cloned().unwrap_or_default();
    let popover = state.ui.popover;
    let exited = status == AgentStatus::Exited;
    let end = EndButton::new(status, &draft);
    let (cache_text, cache_low) = cache_left(summary.cache_expires_at, now());
    let cache_short = match summary
        .cache_expires_at
        .and_then(|moment| format::minutes_until(moment, now()))
    {
        Some(minutes) => format!("{minutes}m"),
        None => "cold".to_owned(),
    };
    let model = summary.model.clone();
    let model_label = model
        .as_deref()
        .map_or_else(|| "Default model".to_owned(), format::model_label);
    let effort = summary.effort;
    let effort_label = effort.map(format::effort_label).unwrap_or_default();
    let mode = summary.mode;
    let mode_label = format::mode_label(mode);
    let can_send = !exited && !draft.is_empty();
    let accept = accepted_images();
    let title = summary.title.clone();
    let map_status = entry.details.loaded().and_then(agent_map::live_status);
    let map_keys = state.prefs.keys.shown(Command::AgentMap);
    let map_label = match map_status {
        Some(SubagentStatus::NeedsYou) => {
            format!("Agent map: a subagent waits for you ({map_keys})")
        }
        Some(_) => format!("Agent map: subagents running ({map_keys})"),
        None => format!("Agent map ({map_keys})"),
    };
    let waiting = entry.waiting_prompts().to_vec();
    let sending: Vec<SendingPrompt> = entry
        .sending
        .in_place(SendingPlace::Waiting)
        .cloned()
        .collect();
    rsx! {
        div { class: "composer-wrap",
            div { class: "composer-column",
                if let Some(Popover::Palette(origin)) = popover {
                    Palette { agent, origin }
                }
                Presence { open: popover == Some(Popover::Model),
                    ModelMenu { agent, model, effort }
                }
                Presence { open: popover == Some(Popover::Mode),
                    ModeMenu { agent, mode }
                }
                WaitingPrompts { agent, prompts: waiting, sending }
                div { class: "composer",
                    if !draft.quotes.is_empty() {
                        QuoteRows { agent, quotes: draft.quotes.clone(), wide: chat_is_wide(&state, agent) }
                    }
                    if !draft.images.is_empty() {
                        div { class: "attachments",
                            for (index, image) in draft.images.iter().enumerate() {
                                span { key: "{index}", class: "attachment",
                                    img { class: "attachment-thumb", src: bridge::image_url(image), alt: "" }
                                    span { class: "mono", "Image {index + 1}" }
                                    button {
                                        class: "icon-button tiny",
                                        aria_label: "Remove the image",
                                        onclick: move |_| cockpit.with_drafts(|_, drafts| actions::remove_image(drafts, agent, index)),
                                        Icon { kind: IconKind::Close, size: 12 }
                                    }
                                }
                            }
                        }
                    }
                    textarea {
                        id: COMPOSER_ID,
                        class: "composer-input",
                        // The page's script sends the text with this agent, and keeps one undo
                        // history per agent.
                        "data-agent": "{agent.get()}",
                        rows: "2",
                        aria_label: "Message {title}",
                        placeholder: placeholder(status),
                        disabled: exited,
                    }
                    div { class: "composer-tools",
                        label {
                            class: "icon-button small",
                            r#for: ATTACH_INPUT_ID,
                            title: "Attach an image, or paste a screenshot",
                            Icon { kind: IconKind::Plus, size: 16 }
                        }
                        input {
                            id: ATTACH_INPUT_ID,
                            class: "hidden-input",
                            r#type: "file",
                            accept,
                            multiple: true,
                            onchange: move |event: FormEvent| async move {
                                attach(cockpit, agent, event.files()).await;
                            },
                        }
                        button {
                            class: if popover == Some(Popover::Palette(PaletteOrigin::Button)) { "icon-button small active" } else { "icon-button small" },
                            aria_label: "Actions, commands and skills",
                            title: "Actions, commands and skills",
                            onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::toggle_palette(state, drafts, agent)),
                            Icon { kind: IconKind::Slash, size: 16 }
                        }
                        button {
                            class: "icon-button small map-button",
                            aria_label: "{map_label}",
                            title: "{map_label}",
                            onclick: move |_| cockpit.update(agent_map::toggle),
                            Icon { kind: IconKind::Map, size: 16 }
                            if let Some(status) = map_status {
                                span { class: "live-dot {format::subagent_status_class(status)}", aria_hidden: true }
                            }
                        }
                        span {
                            class: if cache_low { "cache-timer warning" } else { "cache-timer" },
                            title: "Prompt cache: {cache_text}",
                            Icon { kind: IconKind::Clock, size: 14 }
                            span { "{cache_short}" }
                        }
                        button {
                            class: "menu-button raised model-button",
                            title: "Model and effort",
                            onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::Model)),
                            span { class: "menu-button-label",
                                "{model_label}"
                                if !effort_label.is_empty() {
                                    span { class: "muted", " {effort_label}" }
                                }
                            }
                            Icon { kind: IconKind::ChevronDown, size: 12 }
                        }
                        span { class: "spacer" }
                        button {
                            class: "menu-button mode-button",
                            title: "Permission mode",
                            onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::Mode)),
                            Icon { kind: mode_icon(mode), size: 14 }
                            span { class: "menu-button-label", "{mode_label}" }
                            Icon { kind: IconKind::ChevronDown, size: 12 }
                        }
                        // One button, so that it changes in place rather than another appearing.
                        button {
                            class: if end == EndButton::Stop { "send-button stop" } else { "send-button" },
                            aria_label: end.label(),
                            title: end.title(),
                            disabled: end == EndButton::Send && !can_send,
                            onclick: move |_| {
                                if end != EndButton::Stop {
                                    sent_at.set(Some(Instant::now()));
                                    cockpit.change_with_drafts(|state, drafts| actions::send_prompt(state, drafts, agent, None, PromptDelivery::Steer));
                                } else if sent_at().is_none_or(|sent| sent.elapsed() >= STOP_AFTER_SEND) {
                                    cockpit.change(|state| actions::interrupt(state, agent));
                                }
                            },
                            span { class: "stop-square", aria_hidden: true }
                            span { class: "send-glyph", aria_hidden: true,
                                Icon { kind: IconKind::Send, size: 16 }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `accept` list of the file input: the image types Claude reads.
fn accepted_images() -> String {
    use strum::VariantArray;

    let types: Vec<String> = ImageMediaType::VARIANTS
        .iter()
        .map(|media_type| bridge::mime_type(*media_type))
        .collect();
    types.join(",")
}

async fn attach(cockpit: Cockpit, agent: AgentId, files: Vec<FileData>) {
    use base64::Engine;

    for file in files {
        let name = file.name();
        let media_type = file
            .content_type()
            .as_deref()
            .and_then(bridge::image_media_type)
            .or_else(|| ImageMediaType::from_file_name(&name));
        let Some(media_type) = media_type else {
            cockpit.update(|state| {
                state.ui.report(format!(
                    "{name} is not an image Claude reads: PNG, JPEG, GIF or WebP."
                ));
            });
            continue;
        };
        // Refused before it is read: a file far over the limit would only fill memory.
        let size = usize::try_from(file.size()).unwrap_or(usize::MAX);
        if let Err(too_large) = ImageTooLarge::check(size) {
            cockpit.update(|state| state.ui.report(too_large.note(&name)));
            continue;
        }
        match file.read_bytes().await {
            Ok(bytes) => {
                let image = Image {
                    media_type,
                    data: BASE64.encode(&bytes),
                };
                let added =
                    cockpit.with_drafts(|_, drafts| actions::add_image(drafts, agent, image));
                if let Err(too_large) = added {
                    cockpit.update(|state| state.ui.report(too_large.note(&name)));
                }
            }
            Err(error) => {
                cockpit.update(|state| state.ui.report(format!("Cannot read {name}: {error}")));
            }
        }
    }
}

#[component]
fn ModelMenu(agent: AgentId, model: Option<String>, effort: Option<Effort>) -> Element {
    use strum::VariantArray;

    let cockpit: Cockpit = use_context();
    let current = model
        .as_deref()
        .map(format::model_label)
        .unwrap_or_default();
    rsx! {
        div { class: "popover menu model-menu",
            span { class: "menu-title", "Model" }
            for choice in MODEL_CHOICES {
                {
                    let chosen = current.starts_with(choice.label);
                    rsx! {
                        button {
                            key: "{choice.alias}",
                            class: if chosen { "menu-item chosen" } else { "menu-item" },
                            aria_pressed: chosen,
                            onclick: move |_| {
                                cockpit.change(|state| {
                                    state.ui.popover = None;
                                    actions::set_model(agent, choice.alias.to_owned())
                                });
                            },
                            "{choice.label}"
                            if chosen {
                                Icon { kind: IconKind::Check, size: 14 }
                            }
                        }
                    }
                }
            }
            div { class: "menu-divider" }
            span { class: "menu-title", "Effort" }
            div { class: "effort-row", role: "group", aria_label: "Effort",
                for level in Effort::VARIANTS.iter().copied() {
                    button {
                        key: "{format::effort_short_label(level)}",
                        class: if effort == Some(level) { "effort chosen" } else { "effort" },
                        aria_pressed: effort == Some(level),
                        title: format::effort_label(level),
                        onclick: move |_| cockpit.change(|_| actions::set_effort(agent, level)),
                        "{format::effort_short_label(level)}"
                    }
                }
            }
        }
    }
}

#[component]
fn ModeMenu(agent: AgentId, mode: PermissionMode) -> Element {
    let cockpit: Cockpit = use_context();
    let mut modes: Vec<PermissionMode> = MODE_CHOICES.to_vec();
    if !modes.contains(&mode) {
        modes.push(mode);
    }
    rsx! {
        div { class: "popover menu mode-menu",
            for choice in modes {
                button {
                    key: "{format::mode_label(choice)}",
                    class: if choice == mode { "menu-item two-lines chosen" } else { "menu-item two-lines" },
                    aria_pressed: choice == mode,
                    onclick: move |_| cockpit.change(|state| {
                        state.ui.popover = None;
                        actions::set_mode(agent, choice)
                    }),
                    span { "{format::mode_label(choice)}" }
                    span { class: "muted small", "{format::mode_description(choice)}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_end_button_stops_a_busy_agent_until_a_message_is_written() {
        let empty = Draft::default();
        let written = Draft {
            text: "Also check the tests.".to_owned(),
            ..Draft::default()
        };
        let end = |status, draft: &Draft| EndButton::new(status, draft);

        assert_eq!(end(AgentStatus::Working, &empty), EndButton::Stop);
        assert_eq!(end(AgentStatus::NeedsYou, &empty), EndButton::Stop);
        assert_eq!(end(AgentStatus::Working, &written), EndButton::SendNow);
        assert_eq!(end(AgentStatus::NeedsYou, &written), EndButton::SendNow);
        assert_eq!(end(AgentStatus::Idle, &empty), EndButton::Send);
        assert_eq!(end(AgentStatus::Idle, &written), EndButton::Send);
        assert_eq!(end(AgentStatus::Exited, &written), EndButton::Send);
    }

    #[test]
    fn the_file_input_accepts_the_protocol_image_types() {
        assert_eq!(
            accepted_images(),
            "image/png,image/jpeg,image/gif,image/webp"
        );
    }
}
