//! The copy button, code blocks, and diff rows with syntax colors and word highlights.

use std::time::Duration;

use dioxus::html::ScrollBehavior;
use dioxus::prelude::*;

use crate::render::{self, DiffLine, DiffRow, Language, LineKind, MAX_RENDERED_LINES};
use crate::ui::icons::{Icon, IconKind};
use crate::{bridge, format};

/// How long the copy button says "Copied".
const COPIED_FOR: Duration = Duration::from_secs(1);

/// Whether a diff row shows its line numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineNumbers {
    Shown,
    Hidden,
}

/// What the copy button shows beside its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyLook {
    /// "Copy", then "Copied".
    Worded,
    /// Nothing, then "Copied".
    Icon,
    /// Nothing, then a check alone, for a row of controls that a wider button would push aside.
    IconOnly,
}

/// A button that copies `text`, then shows a check for a moment. `label` names what it copies, for
/// its tooltip and screen readers. Without `text`, it is disabled: the file view keeps it in place
/// while the file loads.
#[component]
pub fn CopyButton(text: Option<String>, label: String, look: CopyLook) -> Element {
    let mut copied = use_signal(|| false);
    rsx! {
        button {
            class: "copy-button",
            aria_label: "{label}",
            title: "{label}",
            disabled: text.is_none(),
            onclick: move |_| {
                let Some(text) = text.clone() else {
                    return;
                };
                bridge::copy_to_clipboard(text);
                copied.set(true);
                spawn(async move {
                    tokio::time::sleep(COPIED_FOR).await;
                    copied.set(false);
                });
            },
            if copied() {
                span { class: "copied-check", Icon { kind: IconKind::Check, size: 14 } }
                if look != CopyLook::IconOnly {
                    "Copied"
                }
            } else {
                Icon { kind: IconKind::Copy, size: 14 }
                if look == CopyLook::Worded {
                    "Copy"
                }
            }
        }
    }
}

#[component]
pub fn CodeBlock(language: Option<String>, code: String) -> Element {
    let lines = render::highlight_capped(
        &code,
        language.as_deref().and_then(Language::from_tag),
        MAX_RENDERED_LINES,
    );
    let label = language.unwrap_or_default();
    let copy_label = if label.is_empty() {
        "Copy the code".to_owned()
    } else {
        format!("Copy the {label} block")
    };
    rsx! {
        div { class: "code-block",
            if !label.is_empty() {
                span { class: "code-language mono", "{label}" }
            }
            CopyButton { text: code, label: copy_label, look: CopyLook::Icon }
            div { class: "code-lines mono scroll-x",
                for (index, spans) in lines.rows.into_iter().enumerate() {
                    div { key: "{index}", class: "code-line",
                        for span in spans {
                            span { class: span.class.css_class(), "{span.text}" }
                        }
                    }
                }
                {lines_not_shown(lines.more)}
            }
        }
    }
}

/// The last row of a view cut at `MAX_RENDERED_LINES`, when it left lines out.
pub fn lines_not_shown(more: usize) -> Element {
    if more == 0 {
        return rsx! {};
    }
    let text = format!(
        "{} not shown",
        format::count(more, "more line", "more lines")
    );
    rsx! {
        div { class: "lines-not-shown", "{text}" }
    }
}

/// One row of a diff. `marked` highlights the row of that new-side line and scrolls it into view.
pub fn diff_row(row: &DiffRow, numbers: LineNumbers, marked: Option<u32>) -> Element {
    let line = match row {
        DiffRow::Folded { lines } => {
            let text = if *lines == 1 {
                "1 unchanged line".to_owned()
            } else {
                format!("{lines} unchanged lines")
            };
            return rsx! {
                div { class: "diff-row folded",
                    span { class: "line-gutter",
                        span { class: "line-number" }
                        span { class: "line-sign" }
                    }
                    span { class: "line-text", "{text}" }
                }
            };
        }
        DiffRow::Line(line) => line,
    };
    diff_line(line, numbers, marked)
}

pub fn diff_line(line: &DiffLine, numbers: LineNumbers, marked: Option<u32>) -> Element {
    let (kind_class, sign, word_class) = match line.kind {
        LineKind::Context => ("context", "", ""),
        LineKind::Added => ("added", "+", "word-added"),
        LineKind::Removed => ("removed", "-", "word-removed"),
    };
    let number = match line.kind {
        LineKind::Removed => line.old_number,
        LineKind::Added | LineKind::Context => line.new_number,
    };
    let is_marked = marked.is_some() && line.kind != LineKind::Removed && number == marked;
    let class = if is_marked {
        format!("diff-row {kind_class} marked")
    } else {
        format!("diff-row {kind_class}")
    };
    let number = match numbers {
        LineNumbers::Shown => number.map(|number| number.to_string()).unwrap_or_default(),
        LineNumbers::Hidden => String::new(),
    };
    // The style sheet draws the number and the sign from these attributes, so that a selection
    // of the text, and its copy, leave them out.
    let content = rsx! {
        span { class: "line-gutter",
            span { class: "line-number", "data-text": "{number}" }
            span { class: "line-sign", "data-text": "{sign}" }
        }
        span { class: "line-text",
            for segment in line.segments.iter() {
                span {
                    class: if segment.emphasized {
                        format!("{} {word_class}", segment.class.css_class())
                    } else {
                        segment.class.css_class().to_owned()
                    },
                    "{segment.text}"
                }
            }
        }
    };
    if !is_marked {
        return rsx! {
            div { class, {content} }
        };
    }
    rsx! {
        div {
            class,
            onmounted: move |event| async move {
                // The view may have closed before the scroll: nothing to do then.
                let _ = event.data().scroll_to(ScrollBehavior::Smooth).await;
            },
            {content}
        }
    }
}
