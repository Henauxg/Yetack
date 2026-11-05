//! The composer: attachments, the prompt, the `/` palette, the prompt cache timer, the model,
//! effort and permission mode menus, and Send and Stop.

use base64::engine::general_purpose::STANDARD as BASE64;
use cockpit_protocol::{
    AgentId, AgentStatus, CommandKind, Effort, Image, ImageMediaType, PermissionMode, SlashCommand,
};
use dioxus::html::FileData;
use dioxus::prelude::*;

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::{Clock, Cockpit};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::state::actions::Direction;
use crate::state::{Popover, actions};
use crate::ui::header::cache_left;
use crate::ui::icons::{Icon, IconKind};

/// The hidden file input the attach button opens.
const ATTACH_INPUT_ID: &str = "attach-input";

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
        AgentStatus::Working => "Add a message; Claude reads it after the current step",
        AgentStatus::Idle => "Ask for a change. Type / for commands and skills.",
        AgentStatus::Exited => "Resume the agent to send a message",
    }
}

/// The buttons at the end of the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndButtons {
    Send,
    /// While the agent works, Stop takes the place of Send.
    Stop,
    /// While the agent waits for you, Esc does not stop it, and Send tells Claude what to do
    /// instead.
    StopAndSend,
}

impl EndButtons {
    fn new(status: AgentStatus) -> Self {
        match status {
            AgentStatus::Working => Self::Stop,
            AgentStatus::NeedsYou => Self::StopAndSend,
            AgentStatus::Idle | AgentStatus::Exited => Self::Send,
        }
    }

    fn stop(self) -> bool {
        matches!(self, Self::Stop | Self::StopAndSend)
    }

    fn send(self) -> bool {
        matches!(self, Self::Send | Self::StopAndSend)
    }
}

#[component]
pub fn Composer() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    // The text area is not bound to the draft, whose renders can lag behind typing: it takes the
    // draft's text only when another agent opens or the app changes the draft.
    let shown = use_memo(move || {
        let state = cockpit.state.read();
        (state.ui.selected, state.ui.composer_writes)
    });
    use_effect(move || {
        let (agent, _) = shown();
        let text = agent
            .and_then(|agent| {
                let state = cockpit.state.peek();
                state.ui.draft(agent).map(|draft| draft.text.clone())
            })
            .unwrap_or_default();
        bridge::set_composer_text(text);
    });
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent = entry.id();
    let summary = &entry.summary;
    let status = summary.status;
    let draft = state.ui.draft(agent).cloned().unwrap_or_default();
    let popover = state.ui.popover;
    let commands = entry
        .details
        .loaded()
        .map(|details| details.commands.clone())
        .unwrap_or_default();
    let palette = state.ui.palette.clone();
    let filtered: Vec<SlashCommand> = actions::filtered_commands(&commands, &palette.query)
        .into_iter()
        .cloned()
        .collect();
    let working = status == AgentStatus::Working;
    let exited = status == AgentStatus::Exited;
    let buttons = EndButtons::new(status);
    let stop_label = if working {
        "Stop the agent (Esc)"
    } else {
        "Stop the agent"
    };
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
    rsx! {
        div { class: "composer-wrap",
            div { class: "composer-column",
                if popover == Some(Popover::Palette) {
                    CommandPalette { agent, commands: filtered, query: palette.query, highlighted: palette.highlighted }
                }
                if popover == Some(Popover::Model) {
                    ModelMenu { agent, model, effort }
                }
                if popover == Some(Popover::Mode) {
                    ModeMenu { agent, mode }
                }
                div { class: "composer",
                    if !draft.images.is_empty() {
                        div { class: "attachments",
                            for (index, image) in draft.images.iter().enumerate() {
                                span { key: "{index}", class: "attachment",
                                    img { class: "attachment-thumb", src: bridge::image_url(image), alt: "" }
                                    span { class: "mono", "Image {index + 1}" }
                                    button {
                                        class: "icon-button tiny",
                                        aria_label: "Remove the image",
                                        onclick: move |_| cockpit.update(|state| actions::remove_image(state, agent, index)),
                                        Icon { kind: IconKind::Close, size: 12 }
                                    }
                                }
                            }
                        }
                    }
                    textarea {
                        id: COMPOSER_ID,
                        class: "composer-input",
                        rows: "2",
                        aria_label: "Message {title}",
                        placeholder: placeholder(status),
                        disabled: exited,
                        oninput: move |event: FormEvent| {
                            cockpit.change(|state| actions::composer_input(state, agent, event.value()));
                        },
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
                            class: if popover == Some(Popover::Palette) { "icon-button small active" } else { "icon-button small" },
                            aria_label: "Commands and skills",
                            title: "Commands and skills",
                            onclick: move |_| cockpit.change(|state| actions::toggle_palette(state, agent)),
                            Icon { kind: IconKind::Slash, size: 16 }
                        }
                        span {
                            class: if cache_low { "cache-timer warning" } else { "cache-timer" },
                            title: "Prompt cache: {cache_text}",
                            Icon { kind: IconKind::Clock, size: 14 }
                            span { "{cache_short}" }
                        }
                        button {
                            class: "menu-button raised",
                            onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::Model)),
                            "{model_label}"
                            if !effort_label.is_empty() {
                                span { class: "muted", "{effort_label}" }
                            }
                            Icon { kind: IconKind::ChevronDown, size: 12 }
                        }
                        span { class: "spacer" }
                        button {
                            class: "menu-button",
                            onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::Mode)),
                            Icon { kind: mode_icon(mode), size: 14 }
                            "{mode_label}"
                            Icon { kind: IconKind::ChevronDown, size: 12 }
                        }
                        if buttons.stop() {
                            if working {
                                span { class: "key-hint", aria_hidden: true, "Esc" }
                            }
                            button {
                                class: "send-button",
                                aria_label: "{stop_label}",
                                title: "{stop_label}",
                                onclick: move |_| cockpit.change(|state| actions::interrupt(state, agent)),
                                Icon { kind: IconKind::Stop, size: 12 }
                            }
                        }
                        if buttons.send() {
                            button {
                                class: "send-button",
                                aria_label: "Send the message",
                                title: "Send the message (Enter)",
                                disabled: !can_send,
                                onclick: move |_| cockpit.change(|state| actions::send_prompt(state, agent, None)),
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

fn media_type_from_name(name: &str) -> Option<ImageMediaType> {
    let extension = name.rsplit_once('.')?.1.to_lowercase();
    match extension.as_str() {
        "png" => Some(ImageMediaType::Png),
        "jpg" | "jpeg" => Some(ImageMediaType::Jpeg),
        "gif" => Some(ImageMediaType::Gif),
        "webp" => Some(ImageMediaType::Webp),
        _ => None,
    }
}

async fn attach(cockpit: Cockpit, agent: AgentId, files: Vec<FileData>) {
    use base64::Engine;

    for file in files {
        let name = file.name();
        let media_type = file
            .content_type()
            .as_deref()
            .and_then(bridge::image_media_type)
            .or_else(|| media_type_from_name(&name));
        let Some(media_type) = media_type else {
            cockpit.update(|state| {
                state.ui.report(format!(
                    "{name} is not an image Claude reads: PNG, JPEG, GIF or WebP."
                ));
            });
            continue;
        };
        match file.read_bytes().await {
            Ok(bytes) => {
                let image = Image {
                    media_type,
                    data: BASE64.encode(&bytes),
                };
                cockpit.update(|state| actions::add_image(state, agent, image));
            }
            Err(error) => {
                cockpit.update(|state| state.ui.report(format!("Cannot read {name}: {error}")));
            }
        }
    }
}

#[component]
fn CommandPalette(
    agent: AgentId,
    commands: Vec<SlashCommand>,
    query: String,
    highlighted: usize,
) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div { class: "popover palette", role: "dialog", aria_label: "Commands",
            div { class: "palette-head",
                span { class: "mono muted", "/" }
                input {
                    class: "palette-input",
                    r#type: "text",
                    aria_label: "Filter commands",
                    placeholder: "Type to filter commands and skills",
                    value: "{query}",
                    oninput: move |event: FormEvent| {
                        cockpit.update(|state| {
                            state.ui.palette.query = event.value();
                            state.ui.palette.highlighted = 0;
                        });
                    },
                    onkeydown: move |event: KeyboardEvent| match event.key() {
                        Key::ArrowDown => cockpit.update(|state| actions::palette_move(state, agent, Direction::Down)),
                        Key::ArrowUp => cockpit.update(|state| actions::palette_move(state, agent, Direction::Up)),
                        Key::Enter => cockpit.update(|state| actions::palette_pick(state, agent, None)),
                        _ => {}
                    },
                }
            }
            div { class: "palette-list scroll",
                if commands.is_empty() {
                    span { class: "palette-empty", "No command or skill matches." }
                }
                // Keyed by position: Claude Code may list two commands under one name.
                for (index, command) in commands.into_iter().enumerate() {
                    button {
                        key: "{index}",
                        class: if index == highlighted { "palette-item highlighted" } else { "palette-item" },
                        onclick: move |_| cockpit.update(|state| actions::palette_pick(state, agent, Some(index))),
                        span { class: "mono", "/{command.name}" }
                        span { class: "palette-description", "{command.description}" }
                        span { class: "palette-kind",
                            match command.kind {
                                CommandKind::Skill => "skill",
                                CommandKind::Command => "command",
                            }
                        }
                    }
                }
            }
            span { class: "palette-foot", "Up and Down to move, Enter to insert, Esc to close" }
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
                                cockpit.change(|state| actions::set_model(state, agent, choice.alias.to_owned()));
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
                    onclick: move |_| cockpit.change(|state| actions::set_mode(state, agent, choice)),
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
    fn image_types_come_from_the_file_name_when_missing() {
        assert_eq!(media_type_from_name("shot.PNG"), Some(ImageMediaType::Png));
        assert_eq!(
            media_type_from_name("photo.jpeg"),
            Some(ImageMediaType::Jpeg)
        );
        assert_eq!(media_type_from_name("notes.txt"), None);
        assert_eq!(media_type_from_name("no-extension"), None);
    }

    #[test]
    fn stop_shows_beside_send_while_the_agent_waits_for_you() {
        let shown = |status| {
            let buttons = EndButtons::new(status);
            (buttons.stop(), buttons.send())
        };

        assert_eq!(shown(AgentStatus::NeedsYou), (true, true));
        assert_eq!(shown(AgentStatus::Working), (true, false));
        assert_eq!(shown(AgentStatus::Idle), (false, true));
        assert_eq!(shown(AgentStatus::Exited), (false, true));
    }

    #[test]
    fn the_file_input_accepts_the_protocol_image_types() {
        assert_eq!(
            accepted_images(),
            "image/png,image/jpeg,image/gif,image/webp"
        );
    }
}
