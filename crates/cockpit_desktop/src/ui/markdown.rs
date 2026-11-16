//! Agent text rendered from the markdown tree, with `path:line` references and links to files
//! as links into the file view, and Markdown images of files shown in place.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use cockpit_protocol::{AgentId, ItemId};
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::render::{self, Block, FileRef, Inline, parse_markdown, parse_markdown_file};
use crate::state::chat_images::{self, ImageKey, ImageSize};
use crate::state::{Loadable, actions};
use crate::ui::code::CodeBlock;
use crate::ui::icons::{Icon, IconKind};
use crate::{bridge, browser};

/// A table cell this short keeps to one line: a name, a number or a status reads badly wrapped,
/// and a table too wide for the column scrolls instead.
const SHORT_CELL_CHARS: usize = 30;

/// The box of the rendered text, whose lines the fold script counts.
pub const MARKDOWN_CLASS: &str = "markdown";

/// `folder` is the agent's, against which relative paths in the text resolve. `file_folder`,
/// absolute, holds the markdown file the text comes from, where its relative links start. `item`
/// is the chat's message that holds the text: an image of a file it shows is read for that message,
/// so that a later message showing the same path, which the agent may have written again, reads it
/// again.
#[component]
pub fn MarkdownView(
    agent: AgentId,
    folder: PathBuf,
    markdown: String,
    file_folder: Option<PathBuf>,
    item: Option<ItemId>,
) -> Element {
    let blocks = match &file_folder {
        Some(file_folder) => parse_markdown_file(&markdown, &folder, file_folder),
        None => parse_markdown(&markdown, &folder),
    };
    rsx! {
        div { class: MARKDOWN_CLASS, {blocks_view(Source { agent, item }, &blocks)} }
    }
}

/// Where the rendered text comes from: its agent, and its message in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Source {
    agent: AgentId,
    item: Option<ItemId>,
}

fn blocks_view(source: Source, blocks: &[Block]) -> Element {
    rsx! {
        for block in blocks.iter() {
            {block_view(source, block)}
        }
    }
}

fn block_view(source: Source, block: &Block) -> Element {
    match block {
        Block::Heading { level, content } => {
            let class = format!("md-heading level-{level}");
            rsx! {
                h3 { class, {inlines_view(source, content)} }
            }
        }
        Block::Paragraph(content) => rsx! {
            p { {inlines_view(source, content)} }
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
                            div { class: "md-list-body", {blocks_view(source, item)} }
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
                                                {inlines_view(source, cell)}
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
                                                {inlines_view(source, cell)}
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
            blockquote { class: "md-quote", {blocks_view(source, blocks)} }
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

fn inlines_view(source: Source, inlines: &[Inline]) -> Element {
    rsx! {
        for inline in inlines.iter() {
            {inline_view(source, inline)}
        }
    }
}

fn inline_view(source: Source, inline: &Inline) -> Element {
    match inline {
        Inline::Text(text) => rsx! { "{text}" },
        Inline::Code(code) => rsx! {
            code { class: "inline-code", "{code}" }
        },
        Inline::Strong(content) => rsx! {
            strong { {inlines_view(source, content)} }
        },
        Inline::Emphasis(content) => rsx! {
            em { {inlines_view(source, content)} }
        },
        Inline::Strikethrough(content) => rsx! {
            del { {inlines_view(source, content)} }
        },
        Inline::Link { url, content } => rsx! {
            WebLink { agent: source.agent, item: source.item, url: url.clone(), content: content.clone() }
        },
        Inline::File(file) => rsx! {
            FileLink { agent: source.agent, file: file.clone() }
        },
        Inline::Image { path, alt } => rsx! {
            ChatImage { agent: source.agent, item: source.item, path: path.clone(), alt: alt.clone() }
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

/// A link to a web address. The app opens it itself: Dioxus would start the browser with the
/// app's environment and hide a failure.
#[component]
fn WebLink(agent: AgentId, item: Option<ItemId>, url: String, content: Vec<Inline>) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        a {
            class: "md-link",
            href: "{url}",
            onclick: move |event: MouseEvent| {
                event.prevent_default();
                let url = url.clone();
                // Not tied to the link, which may leave the screen before the opener is done.
                spawn_forever(async move {
                    if let Err(error) = browser::open(&url).await {
                        tracing::warn!("cannot open {url}: {error}");
                        cockpit.update(|state| state.ui.report(format!("Cannot open {url}: {error}")));
                    }
                });
            },
            {inlines_view(Source { agent, item }, &content)}
        }
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

/// An image file the message shows, at its own size up to a height. A click opens it over the
/// whole window, or in the file view until it is loaded.
#[component]
fn ChatImage(agent: AgentId, item: Option<ItemId>, path: String, alt: String) -> Element {
    let cockpit: Cockpit = use_context();
    // The image this place shows, given back when it shows another or leaves the screen.
    let held = use_hook(|| Rc::new(RefCell::new(None::<(AgentId, ImageKey)>)));
    let showing = Rc::clone(&held);
    use_effect(use_reactive(
        (&agent, &item, &path),
        move |(agent, item, path)| {
            let key = ImageKey::new(item, PathBuf::from(path));
            let before = showing.replace(Some((agent, key.clone())));
            cockpit.update(|state| {
                if let Some((agent, key)) = &before {
                    chat_images::hide(state, *agent, key);
                }
                chat_images::show(state, agent, &key);
            });
        },
    ));
    use_drop(move || {
        let Some((agent, key)) = held.take() else {
            return;
        };
        let mut signal = cockpit.state;
        // The state is gone when the window closes.
        if let Ok(mut state) = signal.try_write() {
            chat_images::hide(&mut state, agent, &key);
        }
    });
    // Rendered again only when the image changes, not on every change of the state.
    let shown = use_memo(use_reactive(
        (&agent, &item, &path),
        move |(agent, item, path)| {
            let key = ImageKey::new(item, PathBuf::from(path));
            chat_images::image(&cockpit.state.read(), agent, &key)
        },
    ));
    let label = if alt.is_empty() {
        path.clone()
    } else {
        alt.clone()
    };
    let body = match shown() {
        Some(Loadable::Loaded(loaded)) => rsx! {
            img {
                src: bridge::image_url(&loaded.image),
                alt: "{label}",
                style: image_style(loaded.size),
            }
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
                // Read before the update: a memo that computes again reads the state, which the
                // update holds.
                let shown = shown();
                cockpit.update(|state| match shown {
                    Some(Loadable::Loaded(loaded)) => {
                        chat_images::open_full_view(state, agent, path, loaded.image);
                    }
                    _ => actions::open_file(state, agent, path, None),
                });
            },
            {body}
        }
    }
}

/// The image's room before the web view decodes it: its own width, narrower when it is taller
/// than the chat lets an image be, with its shape. The style sheet narrows it to the column.
fn image_style(size: Option<ImageSize>) -> String {
    size.map_or_else(String::new, |ImageSize { width, height }| {
        let width_style =
            format!("width: min({width}px, calc(var(--chat-image-height) * {width} / {height}));");
        format!("{width_style} aspect-ratio: {width} / {height};")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wide, short picture keeps its own height: its width is its own, and its shape holds as
    /// the column narrows it. An image without a known size takes the style sheet's limits alone.
    #[test]
    fn an_image_is_drawn_at_its_own_size_up_to_the_chat_height() {
        let strip = ImageSize {
            width: 900,
            height: 60,
        };

        assert_eq!(
            image_style(Some(strip)),
            "width: min(900px, calc(var(--chat-image-height) * 900 / 60)); aspect-ratio: 900 / 60;"
        );
        assert_eq!(image_style(None), "");
    }
}
