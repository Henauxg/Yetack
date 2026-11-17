//! The prompts Claude has not read yet, pinned above the composer so they stay in view.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, COMMAND_PREFIX, Image, PromptId, WaitingPrompt, WaitingState};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::state::sending::{self, SendingPrompt};
use crate::state::{MessageHold, RevealPlace, actions, chat_images};
use crate::ui::find_folds::FoldKey;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::kept_folds::KeptFolds;
use crate::ui::transcript;
use crate::{bridge, format};

/// `sending` are the messages on their way that the daemon will most likely add here.
/// `hold` is what a message Claude has waits for. `images` are the images of the waiting messages
/// this window sent.
#[component]
pub fn WaitingPrompts(
    agent: AgentId,
    prompts: Vec<WaitingPrompt>,
    sending: Vec<SendingPrompt>,
    hold: Option<MessageHold>,
    images: BTreeMap<PromptId, Vec<Arc<Image>>>,
) -> Element {
    if prompts.is_empty() && sending.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "waiting-prompts", role: "list", aria_label: "Messages Claude has not read yet",
            for prompt in prompts {
                WaitingRow { key: "{prompt.id.get()}", agent, images: images.get(&prompt.id).cloned(), prompt, hold: hold.clone() }
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
    let images: Vec<Arc<Image>> = prompt.images.iter().cloned().map(Arc::new).collect();
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
            if !images.is_empty() {
                Thumbnails { agent, images }
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
fn WaitingRow(
    agent: AgentId,
    prompt: WaitingPrompt,
    hold: Option<MessageHold>,
    /// The message's images, when this window sent it. Else the row shows their count.
    images: Option<Vec<Arc<Image>>>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let kept: KeptFolds = use_context();
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
    let count = (prompt.image_count > 0).then(|| {
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
                            let place = transcript::chat_place(state, agent, item);
                            if let Some(run) = place.run {
                                kept.open(agent, FoldKey::Run(run));
                            }
                            state.ui.reveal_in_chat(agent, place.entry, RevealPlace::Middle);
                        }),
                        "{name} ↑"
                    }
                    if let Some(elapsed) = elapsed {
                        " · {elapsed}"
                    }
                }
            }
            span { class: "waiting-text", title: "{prompt.text}", "{prompt.text}" }
            match (images, count) {
                (Some(images), _) if !images.is_empty() => rsx! {
                    Thumbnails { agent, images }
                },
                (_, Some(count)) => rsx! {
                    span { class: "waiting-images", title: "This window does not have the images: the daemon sends only their count", "{count}" }
                },
                _ => rsx! {},
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

/// The most images a row shows. The others show as their count.
const SHOWN_THUMBNAILS: usize = 3;

/// A message's images as small buttons, each showing its image over the whole window.
#[component]
fn Thumbnails(agent: AgentId, images: Vec<Arc<Image>>) -> Element {
    let cockpit: Cockpit = use_context();
    let more = images.len().saturating_sub(SHOWN_THUMBNAILS);
    rsx! {
        span { class: "waiting-thumbs",
            for (index, image) in images.into_iter().enumerate().take(SHOWN_THUMBNAILS) {
                button {
                    key: "{index}",
                    class: "waiting-thumb",
                    title: "Show the image",
                    aria_label: "Show image {index + 1}",
                    onclick: {
                        let image = Arc::clone(&image);
                        move |_| {
                            let image = Arc::clone(&image);
                            cockpit.update(|state| chat_images::open_attached_image(state, agent, image));
                        }
                    },
                    img { src: bridge::image_url(&image), alt: "" }
                }
            }
            if more > 0 {
                span { class: "waiting-images", "+{more}" }
            }
        }
    }
}
