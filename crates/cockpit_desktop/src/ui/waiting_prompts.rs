//! The prompts Claude has not read yet, pinned above the composer so they stay in view.

use cockpit_protocol::{AgentId, COMMAND_PREFIX, WaitingPrompt, WaitingState};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::state::actions;
use crate::state::sending::SendingPrompt;
use crate::ui::icons::{Icon, IconKind};

/// `sending` are the messages on their way that the daemon will most likely add here.
#[component]
pub fn WaitingPrompts(
    agent: AgentId,
    prompts: Vec<WaitingPrompt>,
    sending: Vec<SendingPrompt>,
) -> Element {
    if prompts.is_empty() && sending.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "waiting-prompts", role: "list", aria_label: "Messages Claude has not read yet",
            for prompt in prompts {
                WaitingRow { key: "{prompt.id.get()}", agent, prompt }
            }
            for prompt in sending {
                SendingRow { key: "sending-{prompt.id.get()}", prompt }
            }
        }
    }
}

/// A message on its way, until the daemon's echo replaces it.
#[component]
fn SendingRow(prompt: SendingPrompt) -> Element {
    let images = (!prompt.images.is_empty())
        .then(|| format!("+{}", format::count(prompt.images.len(), "image", "images")));
    rsx! {
        div { class: "waiting-row sending", role: "listitem",
            span { class: "waiting-state", title: "On its way to the agent", "Sending" }
            span { class: "waiting-text", title: "{prompt.text}", "{prompt.text}" }
            if let Some(images) = images {
                span { class: "waiting-images", "{images}" }
            }
        }
    }
}

#[component]
fn WaitingRow(agent: AgentId, prompt: WaitingPrompt) -> Element {
    let cockpit: Cockpit = use_context();
    let id = prompt.id;
    let (class, label, title) = match prompt.state {
        WaitingState::Sent => (
            "waiting-row sent",
            "Next step",
            "Claude has it, and reads it at its next step",
        ),
        WaitingState::Queued => (
            "waiting-row queued",
            "After this turn",
            "Sent when the turn ends",
        ),
    };
    let images = (prompt.image_count > 0).then(|| {
        let count = usize::try_from(prompt.image_count).unwrap_or(usize::MAX);
        format!("+{}", format::count(count, "image", "images"))
    });
    rsx! {
        div { class, role: "listitem",
            span { class: "waiting-state", title, "{label}" }
            span { class: "waiting-text", title: "{prompt.text}", "{prompt.text}" }
            if let Some(images) = images {
                span { class: "waiting-images", "{images}" }
            }
            // A slash command waits for the turn's end: the daemon refuses to send it sooner.
            if prompt.state == WaitingState::Queued && !prompt.text.starts_with(COMMAND_PREFIX) {
                button {
                    class: "icon-button tiny",
                    aria_label: "Send now",
                    title: "Send now: Claude reads it at its next step",
                    onclick: move |_| cockpit.change(|_| actions::send_queued(agent, id)),
                    Icon { kind: IconKind::Send, size: 13 }
                }
            }
            if prompt.state == WaitingState::Queued {
                button {
                    class: "icon-button tiny",
                    aria_label: "Cancel the message",
                    title: "Cancel the message",
                    onclick: move |_| cockpit.change(|_| actions::cancel_queued(agent, id)),
                    Icon { kind: IconKind::Close, size: 13 }
                }
            }
        }
    }
}
