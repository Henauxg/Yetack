//! The Settings dialog, from the gear in the sidebar's foot.

use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::prefs::CommentPlacement;
use crate::state::settings;
use crate::ui::icons::{Icon, IconKind};

const COMMENTS_LABEL_ID: &str = "setting-comments";

fn placement_label(placement: CommentPlacement) -> &'static str {
    match placement {
        CommentPlacement::Beside => "Beside the text",
        CommentPlacement::Hidden => "Hidden",
    }
}

#[component]
pub fn SettingsDialog() -> Element {
    let cockpit: Cockpit = use_context();
    let comments = cockpit.state.read().prefs.chat.comments;
    let close = move |_| cockpit.update(|state| state.ui.close_dialog());
    rsx! {
        div {
            class: "dialog settings",
            role: "dialog",
            aria_label: "Settings",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Settings" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-body",
                h3 { class: "settings-title", "Chat" }
                div { class: "setting-row",
                    div { class: "setting-text",
                        span { id: COMMENTS_LABEL_ID, class: "setting-label", "Comments on quotes" }
                        span { class: "setting-help",
                            "Where a new comment shows while it waits to be sent. Hidden, it shows as its quote's number in the text: hover the number to read it. You can still show or hide each comment."
                        }
                    }
                    div { class: "segmented", role: "group", aria_labelledby: COMMENTS_LABEL_ID,
                        for &placement in CommentPlacement::VARIANTS {
                            button {
                                key: "{placement_label(placement)}",
                                class: if placement == comments { "segment chosen" } else { "segment" },
                                aria_pressed: placement == comments,
                                onclick: move |_| cockpit.update(|state| settings::set_comment_placement(state, placement)),
                                "{placement_label(placement)}"
                            }
                        }
                    }
                }
            }
        }
    }
}
