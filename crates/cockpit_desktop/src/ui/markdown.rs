//! Agent text rendered from the markdown tree, with `path:line` references and links to the
//! agent's files as links into the file view.

use std::path::PathBuf;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::render::{self, Block, FileRef, Inline, parse_markdown};
use crate::state::actions;
use crate::ui::code::CodeBlock;

/// A table cell this short keeps to one line: a name, a number or a status reads badly wrapped,
/// and a table too wide for the column scrolls instead.
const SHORT_CELL_CHARS: usize = 30;

/// `folder` is the agent's, where the files its links open must be.
#[component]
pub fn MarkdownView(agent: AgentId, folder: PathBuf, markdown: String) -> Element {
    let blocks = parse_markdown(&markdown, &folder);
    rsx! {
        div { class: "markdown", {blocks_view(agent, &blocks)} }
    }
}

fn blocks_view(agent: AgentId, blocks: &[Block]) -> Element {
    rsx! {
        for block in blocks.iter() {
            {block_view(agent, block)}
        }
    }
}

fn block_view(agent: AgentId, block: &Block) -> Element {
    match block {
        Block::Heading { level, content } => {
            let class = format!("md-heading level-{level}");
            rsx! {
                h3 { class, {inlines_view(agent, content)} }
            }
        }
        Block::Paragraph(content) => rsx! {
            p { {inlines_view(agent, content)} }
        },
        Block::List { start, items } => {
            let markers: Vec<String> = match start {
                Some(first) => (0..items.len())
                    .map(|index| {
                        let number = u64::try_from(index)
                            .ok()
                            .and_then(|index| first.checked_add(index))
                            .unwrap_or(*first);
                        format!("{number}.")
                    })
                    .collect(),
                None => vec!["-".to_owned(); items.len()],
            };
            let marker_class = if start.is_some() {
                "list-marker number"
            } else {
                "list-marker"
            };
            rsx! {
                div { class: "md-list", role: "list",
                    for (marker, item) in markers.iter().zip(items.iter()) {
                        div { class: "md-list-item", role: "listitem",
                            span { class: marker_class, aria_hidden: true, "{marker}" }
                            div { class: "md-list-body", {blocks_view(agent, item)} }
                        }
                    }
                }
            }
        }
        Block::Code { language, code } => rsx! {
            CodeBlock { language: language.clone(), code: code.clone() }
        },
        Block::Table { head, rows } => {
            // A short row gets empty cells, so that every row draws the same borders.
            let columns = rows
                .iter()
                .map(Vec::len)
                .chain(std::iter::once(head.len()))
                .max()
                .unwrap_or(0);
            rsx! {
                div { class: "md-table scroll-x",
                    table {
                        if !head.is_empty() {
                            thead {
                                tr {
                                    for index in 0..columns {
                                        th { class: cell_class(head.get(index).map(Vec::as_slice)),
                                            if let Some(cell) = head.get(index) {
                                                {inlines_view(agent, cell)}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        tbody {
                            for row in rows.iter() {
                                tr {
                                    for index in 0..columns {
                                        td { class: cell_class(row.get(index).map(Vec::as_slice)),
                                            if let Some(cell) = row.get(index) {
                                                {inlines_view(agent, cell)}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Block::Quote(blocks) => rsx! {
            blockquote { class: "md-quote", {blocks_view(agent, blocks)} }
        },
        Block::Rule => rsx! {
            hr { class: "md-rule" }
        },
    }
}

fn cell_class(cell: Option<&[Inline]>) -> &'static str {
    match cell {
        Some(cell) if render::plain_text(cell).chars().count() <= SHORT_CELL_CHARS => "short",
        _ => "",
    }
}

fn inlines_view(agent: AgentId, inlines: &[Inline]) -> Element {
    rsx! {
        for inline in inlines.iter() {
            {inline_view(agent, inline)}
        }
    }
}

fn inline_view(agent: AgentId, inline: &Inline) -> Element {
    match inline {
        Inline::Text(text) => rsx! { "{text}" },
        Inline::Code(code) => rsx! {
            code { class: "inline-code", "{code}" }
        },
        Inline::Strong(content) => rsx! {
            strong { {inlines_view(agent, content)} }
        },
        Inline::Emphasis(content) => rsx! {
            em { {inlines_view(agent, content)} }
        },
        Inline::Strikethrough(content) => rsx! {
            del { {inlines_view(agent, content)} }
        },
        // The webview opens web links in the system browser.
        Inline::Link { url, content } => rsx! {
            a { class: "md-link", href: "{url}", {inlines_view(agent, content)} }
        },
        Inline::File(file) => rsx! {
            FileLink { agent, file: file.clone() }
        },
        Inline::LineBreak => rsx! {
            br {}
        },
    }
}

#[component]
fn FileLink(agent: AgentId, file: FileRef) -> Element {
    let cockpit: Cockpit = use_context();
    let FileRef { text, path, line } = file;
    rsx! {
        button {
            class: "file-link mono",
            title: "Open in the file view",
            onclick: move |_| {
                let path = PathBuf::from(&path);
                cockpit.update(|state| actions::open_file(state, agent, path, line));
            },
            "{text}"
        }
    }
}
