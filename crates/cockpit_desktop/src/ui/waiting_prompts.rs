//! The prompts Claude has not read yet, pinned above the composer so they stay in view.

use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, COMMAND_PREFIX, WaitingPrompt, WaitingState};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::state::sending::{self, SendingPrompt};
use crate::state::{MessageHold, RevealPlace, actions};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::transcript;

/// `sending` are the messages on their way that the daemon will most likely add here.
/// `hold` is what a message Claude has waits for.
#[component]
pub fn WaitingPrompts(
    agent: AgentId,
    prompts: Vec<WaitingPrompt>,
    sending: Vec<SendingPrompt>,
    hold: Option<MessageHold>,
) -> Element {
    if prompts.is_empty() && sending.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "waiting-prompts", role: "list", aria_label: "Messages Claude has not read yet",
            for prompt in prompts {
                WaitingRow { key: "{prompt.id.get()}", agent, prompt, hold: hold.clone() }
            }
            for prompt in sending {
                SendingRow { key: "sending-{prompt.id.get()}", agent, prompt }
            }
        }
    }
}

/// A message on its way, until the daemon's echo replaces it.
#[component]
fn SendingRow(agent: AgentId, prompt: SendingPrompt) -> Element {
    let cockpit: Cockpit = use_context();
    let clock: Clock = use_context();
    let images = (!prompt.images.is_empty())
        .then(|| format!("+{}", format::count(prompt.images.len(), "image", "images")));
    // The countdown follows the clock's tick, once a second.
    let left = prompt.held_until().map(|until| {
        let _ = (clock.0)();
        let millis = until.saturating_duration_since(Instant::now()).as_millis();
        millis.div_ceil(1000).max(1)
    });
    let id = prompt.id;
    rsx! {
        div { class: if left.is_some() { "waiting-row sending held" } else { "waiting-row sending" }, role: "listitem",
            if let Some(left) = left {
                span { class: "waiting-state", title: "Leaves in a moment: Cancel can still stop it", "Sending in {left} s" }
            } else {
                span { class: "waiting-state", title: "On its way to the agent", "Sending" }
            }
            span { class: "waiting-text", title: "{prompt.text}", "{prompt.text}" }
            if let Some(images) = images {
                span { class: "waiting-images", "{images}" }
            }
            if left.is_some() {
                button {
                    class: "icon-button tiny",
                    aria_label: "Send now",
                    title: "Send now",
                    onclick: move |_| cockpit.change(|state| sending::send_now(state, agent, id)),
                    Icon { kind: IconKind::Send, size: 13 }
                }
                button {
                    class: "icon-button tiny",
                    aria_label: "Cancel the message",
                    title: "Cancel: the text goes back to the composer",
                    onclick: move |_| cockpit.update_with_drafts(|state, drafts| sending::cancel(state, drafts, agent, id)),
                    Icon { kind: IconKind::Close, size: 13 }
                }
            }
        }
    }
}

#[component]
fn WaitingRow(agent: AgentId, prompt: WaitingPrompt, hold: Option<MessageHold>) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let id = prompt.id;
    let hold = hold.filter(|_| prompt.state == WaitingState::Sent);
    let (class, label, title) = match (prompt.state, &hold) {
        (WaitingState::Sent, Some(MessageHold::Answer { .. })) => (
            "waiting-row sent blocked",
            "Sent · waits for ",
            "Claude has it, and reads it once you answer".to_owned(),
        ),
        (WaitingState::Sent, Some(MessageHold::Calls(call))) => (
            "waiting-row sent blocked",
            "Sent · waits for ",
            match call.others {
                0 => "Claude has it, and reads it when this call ends. Stop the call in the chat to have it read now.".to_owned(),
                others => format!("Claude has it, and reads it when its {} running calls end", others + 1),
            },
        ),
        (WaitingState::Sent, None) => (
            "waiting-row sent",
            "Sent · read at the next step",
            "Claude has it, and reads it at its next step".to_owned(),
        ),
        (WaitingState::Queued, _) => (
            "waiting-row queued",
            "After this turn",
            "Sent when the turn ends".to_owned(),
        ),
    };
    // What the label's link shows in the chat, its name, and the call's time.
    let held_by = hold.map(|hold| match hold {
        MessageHold::Answer { item } => (item, "your answer".to_owned(), None),
        MessageHold::Calls(call) => {
            // The time follows the clock's tick, once a second.
            let _ = now();
            let elapsed = call
                .since
                .map(|since| format::duration(Duration::from_secs(since.elapsed().as_secs())));
            let name = match call.others {
                0 => transcript::call_name(&call.tool),
                others => format!("{} +{others}", transcript::call_name(&call.tool)),
            };
            (call.item, name, elapsed)
        }
    });
    let images = (prompt.image_count > 0).then(|| {
        let count = usize::try_from(prompt.image_count).unwrap_or(usize::MAX);
        format!("+{}", format::count(count, "image", "images"))
    });
    rsx! {
        div { class, role: "listitem",
            span { class: "waiting-state", title,
                "{label}"
                if let Some((item, name, elapsed)) = held_by {
                    button {
                        class: "waiting-call",
                        title: "Show it in the chat",
                        onclick: move |_| cockpit.update(|state| {
                            let entry = transcript::entry_item(state, agent, item);
                            state.ui.reveal_in_chat(agent, entry, RevealPlace::Middle);
                        }),
                        "{name} ↑"
                    }
                    if let Some(elapsed) = elapsed {
                        " · {elapsed}"
                    }
                }
            }
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
