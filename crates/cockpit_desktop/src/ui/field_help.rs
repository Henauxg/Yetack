//! A question mark beside a field's label, whose tooltip explains the field.

use dioxus::prelude::*;

use super::icons::{Icon, IconKind};

/// A question mark that shows `text` while the pointer is on it or it has the keyboard focus. The
/// tooltip spans the field's group, which `.field-group` positions. `field` names the field for a
/// screen reader, since a form has several of these buttons.
#[component]
pub fn FieldHelp(#[props(into)] field: String, #[props(into)] text: String) -> Element {
    rsx! {
        span { class: "field-help tooltip-anchor",
            button {
                class: "field-help-button",
                r#type: "button",
                aria_label: "About {field}",
                "aria-description": "{text}",
                Icon { kind: IconKind::Question, size: 14 }
            }
            span { class: "tooltip field-help-text", role: "tooltip", "{text}" }
        }
    }
}
