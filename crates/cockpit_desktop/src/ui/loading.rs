//! What stands for content on its way: the skeleton of a conversation's first page, and of the
//! older messages above it.

use dioxus::prelude::*;

/// The widths of the skeleton's text lines, as a paragraph's lines vary.
const ANSWER_LINES: [&str; 3] = ["94%", "88%", "52%"];
const CLOSING_LINES: [&str; 2] = ["90%", "64%"];

/// The widths of the older messages' rows.
const OLDER_LINES: [&str; 4] = ["62%", "48%", "80%", "55%"];

/// Rows where the older messages will go, until they come.
#[component]
pub fn OlderSkeleton() -> Element {
    rsx! {
        div { class: "older-skeleton", role: "status", aria_label: "Loading older messages",
            for width in OLDER_LINES {
                div { class: "skeleton-tool", aria_hidden: true,
                    span { class: "skeleton skeleton-dot" }
                    span { class: "skeleton skeleton-line", style: "width: {width}" }
                }
            }
        }
    }
}

/// A message, an answer, a tool line and the end of an answer, in the transcript's spacing.
#[component]
pub fn TranscriptSkeleton() -> Element {
    rsx! {
        div { class: "transcript-skeleton", role: "status", aria_label: "Loading the conversation",
            span { class: "skeleton skeleton-bubble", aria_hidden: true }
            div { aria_hidden: true,
                for width in ANSWER_LINES {
                    span { class: "skeleton skeleton-line", style: "width: {width}" }
                }
            }
            div { class: "skeleton-tool", aria_hidden: true,
                span { class: "skeleton skeleton-dot" }
                span { class: "skeleton skeleton-line", style: "width: 34%" }
            }
            div { aria_hidden: true,
                for width in CLOSING_LINES {
                    span { class: "skeleton skeleton-line", style: "width: {width}" }
                }
            }
        }
    }
}
