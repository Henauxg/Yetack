//! The copy button, code blocks, and diff rows with syntax colors and word highlights.

use std::collections::BTreeSet;
use std::time::Duration;

use dioxus::html::ScrollBehavior;
use dioxus::prelude::*;

use crate::render::{self, DiffLine, DiffRow, Language, LineKind, MAX_RENDERED_LINES};
use crate::ui::icons::{Icon, IconKind};
use crate::{bridge, format};

/// How long the copy button says "Copied".
const COPIED_FOR: Duration = Duration::from_secs(1);

/// No line updated.
static NO_LINES: BTreeSet<u32> = BTreeSet::new();

/// The rows of a file that stand out: the line a link marks, which scrolls into view, and the
/// lines a read again of the file changed, highlighted for a moment.
#[derive(Debug, Clone, Copy)]
pub struct LineMarks<'lines> {
    pub marked: Option<u32>,
    pub updated: &'lines BTreeSet<u32>,
    /// The class that plays the highlight. Two classes alternate, so that a new update plays it
    /// again on a row that already had one.
    pub updated_class: &'static str,
}

impl LineMarks<'static> {
    pub fn marked(line: Option<u32>) -> Self {
        Self {
            marked: line,
            updated: &NO_LINES,
            updated_class: "",
        }
    }
}

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

/// What a diff shows in place of its rows when the daemon left the edit's texts out.
pub const TOO_LARGE_TO_SHOW: &str = "Too large to show";

/// The one row of a diff whose edit is too large to show.
pub fn too_large_to_show() -> Element {
    rsx! {
        div { class: "lines-not-shown", "{TOO_LARGE_TO_SHOW}" }
    }
}

/// One row of a diff. `marked` highlights the row of that new-side line and scrolls it into view.
pub fn diff_row(row: &DiffRow, numbers: LineNumbers, marks: LineMarks<'_>) -> Element {
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
    diff_line(line, numbers, marks)
}

pub fn diff_line(line: &DiffLine, numbers: LineNumbers, marks: LineMarks<'_>) -> Element {
    let (kind_class, sign, word_class) = match line.kind {
        LineKind::Context => ("context", "", ""),
        LineKind::Added => ("added", "+", "word-added"),
        LineKind::Removed => ("removed", "-", "word-removed"),
    };
    let number = match line.kind {
        LineKind::Removed => line.old_number,
        LineKind::Added | LineKind::Context => line.new_number,
    };
    let in_new_text = line.kind != LineKind::Removed;
    let is_marked = marks.marked.is_some() && in_new_text && number == marks.marked;
    let updated = in_new_text && number.is_some_and(|number| marks.updated.contains(&number));
    let mut class = format!("diff-row {kind_class}");
    if is_marked {
        class.push_str(" marked");
    }
    if updated {
        class.push(' ');
        class.push_str(marks.updated_class);
    }
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
