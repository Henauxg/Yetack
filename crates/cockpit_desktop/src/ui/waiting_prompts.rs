//! The prompts Claude has not read yet, pinned above the composer so they stay in view.

use cockpit_protocol::{AgentId, COMMAND_PREFIX, WaitingPrompt, WaitingState};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::state::actions;
use crate::ui::icons::{Icon, IconKind};

#[component]
pub fn WaitingPrompts(agent: AgentId, prompts: Vec<WaitingPrompt>) -> Element {
    if prompts.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "waiting-prompts", role: "list", aria_label: "Messages Claude has not read yet",
            for prompt in prompts {
                WaitingRow { key: "{prompt.id.get()}", agent, prompt }
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
