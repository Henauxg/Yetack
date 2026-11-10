//! Agent text rendered from the markdown tree, with `path:line` references and links to files
//! as links into the file view, and Markdown images of files shown in place.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::bridge;
use crate::connection::Cockpit;
use crate::render::{self, Block, FileRef, Inline, parse_markdown, parse_markdown_file};
use crate::state::{Loadable, actions, chat_images};
use crate::ui::code::CodeBlock;
use crate::ui::icons::{Icon, IconKind};

/// A table cell this short keeps to one line: a name, a number or a status reads badly wrapped,
/// and a table too wide for the column scrolls instead.
const SHORT_CELL_CHARS: usize = 30;

/// `folder` is the agent's, against which relative paths in the text resolve. `file_folder`,
/// absolute, holds the markdown file the text comes from, where its relative links start.
#[component]
pub fn MarkdownView(
    agent: AgentId,
    folder: PathBuf,
    markdown: String,
    file_folder: Option<PathBuf>,
) -> Element {
    let blocks = match &file_folder {
        Some(file_folder) => parse_markdown_file(&markdown, &folder, file_folder),
        None => parse_markdown(&markdown, &folder),
    };
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
                            // A task's checkbox stands for its bullet.
                            if start.is_some() || !is_task(item) {
                                span { class: marker_class, aria_hidden: true, "{marker}" }
                            }
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

/// Whether a list item starts with a task's checkbox.
fn is_task(item: &[Block]) -> bool {
    matches!(
        item.first(),
        Some(Block::Paragraph(content)) if matches!(content.first(), Some(Inline::Task { .. }))
    )
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
        Inline::Image { path, alt } => rsx! {
            ChatImage { agent, path: path.clone(), alt: alt.clone() }
        },
        Inline::LineBreak => rsx! {
            br {}
        },
        Inline::Task { done } => rsx! {
            span {
                class: if *done { "md-task done" } else { "md-task" },
                role: "img",
                aria_label: if *done { "Done" } else { "To do" },
                if *done {
                    Icon { kind: IconKind::Check, size: 11 }
                }
            }
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

/// An image file the message shows, at a fixed height so that the text below does not move when
/// it arrives. A click opens it over the whole window, or in the file view until it is loaded.
#[component]
fn ChatImage(agent: AgentId, path: String, alt: String) -> Element {
    let cockpit: Cockpit = use_context();
    // The image this place shows, given back when it shows another or leaves the screen.
    let held = use_hook(|| Rc::new(RefCell::new(None::<(AgentId, PathBuf)>)));
    let showing = Rc::clone(&held);
    use_effect(use_reactive((&agent, &path), move |(agent, path)| {
        let file = PathBuf::from(path);
        let before = showing.replace(Some((agent, file.clone())));
        cockpit.update(|state| {
            if let Some((agent, path)) = &before {
                chat_images::hide(state, *agent, path);
            }
            chat_images::show(state, agent, &file);
        });
    }));
    use_drop(move || {
        let Some((agent, path)) = held.take() else {
            return;
        };
        let mut signal = cockpit.state;
        // The state is gone when the window closes.
        if let Ok(mut state) = signal.try_write() {
            chat_images::hide(&mut state, agent, &path);
        }
    });
    // Rendered again only when the image changes, not on every change of the state.
    let shown = use_memo(use_reactive((&agent, &path), move |(agent, path)| {
        chat_images::image(&cockpit.state.read(), agent, Path::new(&path))
    }));
    let label = if alt.is_empty() {
        path.clone()
    } else {
        alt.clone()
    };
    let body = match shown() {
        Some(Loadable::Loaded(image)) => rsx! {
            img { src: bridge::image_url(&image), alt: "{label}" }
        },
        Some(Loadable::Failed(message)) => rsx! {
            span { class: "chat-image-error", "{label}: {message}" }
        },
        Some(Loadable::NotRequested | Loadable::Loading) | None => rsx! {
            span { class: "skeleton chat-image-wait", role: "status", aria_label: "Loading {label}" }
        },
    };
    rsx! {
        button {
            class: "chat-image",
            title: "{path}",
            onclick: move |_| {
                let path = PathBuf::from(&path);
                cockpit.update(|state| match shown() {
                    Some(Loadable::Loaded(image)) => {
                        chat_images::open_full_view(state, agent, path, image);
                    }
                    _ => actions::open_file(state, agent, path, None),
                });
            },
            {body}
        }
    }
}
