//! Agent text as a tree of blocks and inlines, with `path:line` references found in the text.

use std::borrow::Cow;
use std::iter::{self, Peekable};
use std::path::Path;

use cockpit_protocol::ImageMediaType;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::path_style::PathStyle;

/// Nesting deeper than this (quotes in quotes, lists in lists) is flattened, so a hostile text
/// cannot exhaust the stack.
const MAX_DEPTH: usize = 32;

/// A task list item's checkbox, in plain text.
const TASK_DONE: &str = "[x] ";
const TASK_OPEN: &str = "[ ] ";

/// What starts a link's fragment, as in `docs/plan.md#decisions`.
const FRAGMENT: char = '#';

/// What starts a GitHub-style line fragment, as in `src/main.rs#L12`.
const LINE_FRAGMENT: char = 'L';

/// What separates a URL's scheme from the rest, as in `https://`.
const SCHEME_SEPARATOR: &str = "://";

/// The schemes of the links that open in the browser.
const WEB_SCHEMES: [&str; 2] = ["http", "https"];

/// The starts of the bare web addresses that a text turns into links.
const WEB_URL_STARTS: [&str; 2] = ["http://", "https://"];
/// What both of [`WEB_URL_STARTS`] start with.
const WEB_URL_PREFIX: &str = "http";

/// File names that a code span may hold alone, as in `Makefile`: names without an extension,
/// and names whose extension is too often a field's to count alone.
const FILE_NAMES: [&str; 16] = [
    "Makefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "LICENSE",
    "README",
    "CHANGELOG",
    ".env",
    ".envrc",
    ".gitignore",
    ".gitattributes",
    ".dockerignore",
    ".editorconfig",
    "Cargo.lock",
    "flake.lock",
    "yarn.lock",
];

/// The extensions of a file name a code span holds without a folder, as in `Cargo.toml`. With a
/// folder, as in `src/lexer.rs`, any extension is a file's. Extensions that are also common
/// fields, as `log` in `console.log` or `env` in `process.env`, count only with a folder.
const FILE_EXTENSIONS: [&str; 36] = [
    "rs", "toml", "md", "txt", "json", "jsonl", "yaml", "yml", "ini", "sh", "bash", "zsh", "nix",
    "py", "mjs", "tsx", "jsx", "go", "java", "kt", "cpp", "hpp", "cs", "rb", "php", "sql", "proto",
    "html", "css", "scss", "xml", "csv", "png", "jpg", "jpeg", "pdf",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// `start` is the first number of an ordered list, `None` for bullets. Each item is a list of
    /// blocks; a tight item's text is wrapped in a `Paragraph`.
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    /// `code` without its final newline.
    Code {
        language: Option<String>,
        code: String,
    },
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Quote(Vec<Block>),
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Link {
        url: String,
        content: Vec<Inline>,
    },
    File(FileRef),
    /// An image file the message shows, which the daemon sends.
    Image {
        path: String,
        alt: String,
    },
    LineBreak,
    /// A task list item's checkbox.
    Task {
        done: bool,
    },
}

/// A reference to a file, which opens the file view: a `path:line` in the text, or a link to a
/// file in the agent's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    /// As written, such as "src/main.rs:12", or the link's text.
    pub text: String,
    pub path: String,
    pub line: Option<u32>,
}

/// `folder` is the agent's, whose path rules (Unix or Windows) the paths in the text follow.
pub fn parse_markdown(markdown: &str, folder: &Path) -> Vec<Block> {
    build_blocks(markdown, folder, None)
}

/// The text of a markdown file in `file_folder`, absolute, where its relative links start, as
/// in an editor.
pub fn parse_markdown_file(markdown: &str, folder: &Path, file_folder: &Path) -> Vec<Block> {
    build_blocks(markdown, folder, Some(file_folder))
}

fn build_blocks(markdown: &str, folder: &Path, file_folder: Option<&Path>) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder {
        events: Parser::new_ext(markdown, options).peekable(),
        file_folder,
        style: PathStyle::of(folder),
    };
    builder.blocks(None, 0)
}

/// The text of inlines without formatting, for notifications and copies.
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    push_plain_text(&mut text, inlines);
    text
}

/// The text of blocks without formatting: a line for each heading, paragraph, list item and table
/// row, and a code block's own lines.
pub fn blocks_plain_text(blocks: &[Block]) -> String {
    let mut lines: Vec<String> = Vec::new();
    push_blocks_text(&mut lines, blocks);
    lines.join("\n")
}

fn push_blocks_text(lines: &mut Vec<String>, blocks: &[Block]) {
    for block in blocks {
        match block {
            Block::Heading { content, .. } | Block::Paragraph(content) => {
                lines.push(plain_text(content));
            }
            Block::List { items, .. } => {
                for item in items {
                    push_blocks_text(lines, item);
                }
            }
            Block::Code { code, .. } => lines.push(code.clone()),
            Block::Table { head, rows } => {
                let table_rows = iter::once(head).chain(rows);
                for row in table_rows {
                    let cells: Vec<String> = row.iter().map(|cell| plain_text(cell)).collect();
                    lines.push(cells.join(" "));
                }
            }
            Block::Quote(content) => push_blocks_text(lines, content),
            Block::Rule => {}
        }
    }
}

fn push_plain_text(text: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(part) | Inline::Code(part) => text.push_str(part),
            Inline::Strong(content)
            | Inline::Emphasis(content)
            | Inline::Strikethrough(content)
            | Inline::Link { content, .. } => push_plain_text(text, content),
            Inline::File(file) => text.push_str(&file.text),
            Inline::Image { alt, .. } => text.push_str(alt),
            Inline::LineBreak => text.push('\n'),
            Inline::Task { done } => text.push_str(if *done { TASK_DONE } else { TASK_OPEN }),
        }
    }
}

/// Builds the tree from pulldown-cmark's events, which always come balanced.
struct Builder<'input> {
    events: Peekable<Parser<'input>>,
    /// The folder of the file the text is in, where its relative links start.
    file_folder: Option<&'input Path>,
    /// The rules of the agent's folder, which the paths in the text follow.
    style: PathStyle,
}

impl<'input> Builder<'input> {
    /// Blocks until `end`, or until the events run out when `end` is `None`.
    fn blocks(&mut self, end: Option<&TagEnd>, depth: usize) -> Vec<Block> {
        let mut blocks = Vec::new();
        loop {
            // A tight list item holds its text without a paragraph.
            if self.events.peek().is_some_and(starts_inline) {
                let content = self.inline_run(depth);
                if !content.is_empty() {
                    blocks.push(Block::Paragraph(content));
                }
                continue;
            }
            let Some(event) = self.events.next() else {
                return blocks;
            };
            let tag = match event {
                Event::End(tag_end) if Some(&tag_end) == end => return blocks,
                Event::Rule => {
                    blocks.push(Block::Rule);
                    continue;
                }
                Event::Start(tag) => tag,
                // Inline events were taken above; a stray end has nothing to close.
                _ => continue,
            };
            let tag_end = tag.to_end();
            if depth >= MAX_DEPTH {
                let content = self.flattened(&tag_end);
                if !content.is_empty() {
                    blocks.push(Block::Paragraph(content));
                }
                continue;
            }
            match tag {
                Tag::Paragraph => blocks.push(Block::Paragraph(self.inlines(&tag_end, depth))),
                Tag::Heading { level, .. } => blocks.push(Block::Heading {
                    level: heading_number(level),
                    content: self.inlines(&tag_end, depth),
                }),
                Tag::BlockQuote(_) => {
                    blocks.push(Block::Quote(self.blocks(Some(&tag_end), depth + 1)));
                }
                Tag::CodeBlock(kind) => blocks.push(self.code_block(kind, &tag_end)),
                Tag::List(start) => blocks.push(self.list(start, &tag_end, depth)),
                Tag::Table(_) => blocks.push(self.table(&tag_end, depth)),
                Tag::HtmlBlock => {
                    let text = self.raw_text(&tag_end);
                    if !text.trim().is_empty() {
                        blocks.push(Block::Paragraph(vec![Inline::Text(text)]));
                    }
                }
                // Footnote definitions, metadata and other containers: their blocks only.
                _ => blocks.extend(self.blocks(Some(&tag_end), depth + 1)),
            }
        }
    }

    fn code_block(&mut self, kind: CodeBlockKind<'input>, end: &TagEnd) -> Block {
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_owned),
            CodeBlockKind::Indented => None,
        };
        let mut code = self.raw_text(end);
        if code.ends_with('\n') {
            code.pop();
        }
        Block::Code { language, code }
    }

    fn list(&mut self, start: Option<u64>, end: &TagEnd, depth: usize) -> Block {
        let mut items = Vec::new();
        while let Some(event) = self.events.next() {
            match event {
                Event::Start(Tag::Item) => {
                    items.push(self.blocks(Some(&TagEnd::Item), depth + 1));
                }
                Event::End(tag_end) if &tag_end == end => break,
                _ => {}
            }
        }
        Block::List { start, items }
    }

    fn table(&mut self, end: &TagEnd, depth: usize) -> Block {
        let mut head = Vec::new();
        let mut rows = Vec::new();
        let mut in_head = false;
        while let Some(event) = self.events.next() {
            match event {
                Event::Start(Tag::TableHead) => in_head = true,
                Event::End(TagEnd::TableHead) => in_head = false,
                Event::Start(Tag::TableRow) if !in_head => rows.push(Vec::new()),
                Event::Start(Tag::TableCell) => {
                    let cell = self.inlines(&TagEnd::TableCell, depth);
                    if in_head {
                        head.push(cell);
                    } else if let Some(row) = rows.last_mut() {
                        row.push(cell);
                    }
                }
                Event::End(tag_end) if &tag_end == end => break,
                _ => {}
            }
        }
        Block::Table { head, rows }
    }

    /// The text of every event until `end`, for code blocks and HTML blocks.
    fn raw_text(&mut self, end: &TagEnd) -> String {
        let mut text = String::new();
        for event in self.events.by_ref() {
            match event {
                Event::End(tag_end) if &tag_end == end => break,
                Event::Text(part)
                | Event::Html(part)
                | Event::InlineHtml(part)
                | Event::Code(part) => text.push_str(&part),
                Event::SoftBreak | Event::HardBreak => text.push('\n'),
                _ => {}
            }
        }
        text
    }

    /// The text of every event until `end`, without structure. It loops instead of recursing, for
    /// content nested past `MAX_DEPTH`.
    fn flattened(&mut self, end: &TagEnd) -> Vec<Inline> {
        let mut open: Vec<TagEnd> = Vec::new();
        let mut inlines = Vec::new();
        for event in self.events.by_ref() {
            match event {
                Event::Start(tag) => open.push(tag.to_end()),
                Event::End(tag_end) => {
                    if open.pop().is_none() && &tag_end == end {
                        break;
                    }
                }
                Event::Text(part)
                | Event::Code(part)
                | Event::Html(part)
                | Event::InlineHtml(part)
                | Event::InlineMath(part)
                | Event::DisplayMath(part) => inlines.push(Inline::Text(part.into_string())),
                Event::SoftBreak | Event::HardBreak | Event::Rule => {
                    inlines.push(Inline::Text(" ".to_owned()));
                }
                Event::TaskListMarker(_) | Event::FootnoteReference(_) => {}
            }
        }
        finish_inlines(inlines, self.style)
    }

    /// Inline events up to the next block-level event, without consuming it.
    fn inline_run(&mut self, depth: usize) -> Vec<Inline> {
        let mut inlines = Vec::new();
        while self.events.peek().is_some_and(starts_inline) {
            let Some(event) = self.events.next() else {
                break;
            };
            self.push_inline(&mut inlines, event, depth);
        }
        finish_inlines(inlines, self.style)
    }

    /// Inlines until `end`.
    fn inlines(&mut self, end: &TagEnd, depth: usize) -> Vec<Inline> {
        let mut inlines = Vec::new();
        while let Some(event) = self.events.next() {
            if let Event::End(tag_end) = &event
                && tag_end == end
            {
                break;
            }
            self.push_inline(&mut inlines, event, depth);
        }
        finish_inlines(inlines, self.style)
    }

    fn push_inline(&mut self, inlines: &mut Vec<Inline>, event: Event<'input>, depth: usize) {
        match event {
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                inlines.push(Inline::Text(text.into_string()));
            }
            Event::Code(code) | Event::InlineMath(code) | Event::DisplayMath(code) => {
                inlines.push(code_span(&code, self.style));
            }
            // A single newline breaks the line, as GitHub comments and claude.ai show it: Claude
            // often writes one item per line without list markers.
            Event::SoftBreak | Event::HardBreak => inlines.push(Inline::LineBreak),
            Event::TaskListMarker(done) => inlines.push(Inline::Task { done }),
            Event::FootnoteReference(label) => inlines.push(Inline::Text(format!("[{label}]"))),
            Event::Start(tag) => {
                let tag_end = tag.to_end();
                if depth >= MAX_DEPTH {
                    inlines.extend(self.flattened(&tag_end));
                    return;
                }
                let content = self.inlines(&tag_end, depth + 1);
                match tag {
                    Tag::Emphasis => inlines.push(Inline::Emphasis(content)),
                    Tag::Strong => inlines.push(Inline::Strong(content)),
                    Tag::Strikethrough => inlines.push(Inline::Strikethrough(content)),
                    Tag::Link { dest_url, .. } => {
                        inlines.extend(link(&dest_url, content, self.file_folder, self.style));
                    }
                    Tag::Image { dest_url, .. } => {
                        inlines.push(image(&dest_url, content, self.file_folder, self.style));
                    }
                    // Other tags keep their content.
                    _ => inlines.extend(content),
                }
            }
            Event::End(_) | Event::Rule => {}
        }
    }
}

/// Whether `event` belongs to running text rather than starting or ending a block.
fn starts_inline(event: &Event<'_>) -> bool {
    match event {
        Event::Text(_)
        | Event::Code(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::Html(_)
        | Event::InlineHtml(_)
        | Event::FootnoteReference(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::TaskListMarker(_) => true,
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Superscript
                | Tag::Subscript
                | Tag::Link { .. }
                | Tag::Image { .. }
        ),
        Event::End(_) | Event::Rule => false,
    }
}

fn heading_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// A code span that holds only a `path:line` reference, or only a file's path, opens the file
/// view.
fn code_span(code: &str, style: PathStyle) -> Inline {
    let trimmed = code.trim();
    match find_file_refs(trimmed, style).as_slice() {
        [TextPiece::File(file)] => Inline::File(file.clone()),
        _ if is_bare_file(trimmed, style) => Inline::File(FileRef {
            text: trimmed.to_owned(),
            path: trimmed.to_owned(),
            line: None,
        }),
        _ => Inline::Code(code.to_owned()),
    }
}

/// Whether `code` is a file's path alone, written as agents write one: in a folder with an
/// extension (`src/lexer.rs`, `/tmp/shot.png`), with a known extension (`Cargo.toml`), or with a
/// known name (`Makefile`, `.env`). A relative path whose first name holds a dot, as in
/// `github.com/a/b.rs`, is a web address. The daemon still decides whether it sends the file.
fn is_bare_file(code: &str, style: PathStyle) -> bool {
    let after_drive = code.get(style.drive_len(code)..).unwrap_or_default();
    if after_drive.is_empty()
        || !after_drive.bytes().all(|byte| is_path_byte(byte, style))
        || !is_file_target(code, style)
    {
        return false;
    }
    let Some(name) = style.file_name(code) else {
        return false;
    };
    if code.ends_with(|character| style.is_separator(character)) {
        return false;
    }
    if FILE_NAMES.contains(&name) {
        return true;
    }
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let is_extension = !stem.is_empty()
        && !extension.is_empty()
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !extension.bytes().all(|byte| byte.is_ascii_digit());
    let in_folder = code.contains(|character| style.is_separator(character));
    let domain = in_folder
        && !style.is_absolute(code)
        && style
            .first_name(code)
            .is_some_and(|first| first.get(1..).is_some_and(|rest| rest.contains('.')));
    is_extension
        && !domain
        && (in_folder
            || FILE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension)))
}

/// An image file the message shows, by the same rule as a link to a file. A web image is not
/// fetched: it shows as a link with its alt text, and anything else as that text.
fn image(url: &str, content: Vec<Inline>, file_folder: Option<&Path>, style: PathStyle) -> Inline {
    let alt = plain_text(&content);
    if is_web_url(url) {
        return Inline::Link {
            url: url.to_owned(),
            content: vec![Inline::Text(if alt.is_empty() {
                url.to_owned()
            } else {
                alt
            })],
        };
    }
    // The daemon sends the same types as images.
    let is_image = |path: &str| {
        style
            .file_name(path)
            .and_then(ImageMediaType::from_file_name)
            .is_some()
    };
    match local_file_target(url, file_folder, style) {
        Some((path, None)) if is_image(&path) => Inline::Image {
            path: path.into_owned(),
            alt,
        },
        _ => Inline::Text(alt),
    }
}

/// A link to a file opens the file view instead of a browser: an absolute path, which the daemon
/// checks, or a relative one. Any other link than a web one keeps only its text: the system would
/// open it with whatever program handles its scheme or file.
fn link(
    url: &str,
    content: Vec<Inline>,
    file_folder: Option<&Path>,
    style: PathStyle,
) -> Vec<Inline> {
    if is_web_url(url) {
        return vec![Inline::Link {
            url: url.to_owned(),
            content: unlinked(content),
        }];
    }
    let Some((path, line)) = local_file_target(url, file_folder, style) else {
        return content;
    };
    let text = plain_text(&content);
    vec![Inline::File(FileRef {
        text: if text.is_empty() {
            url.to_owned()
        } else {
            text
        },
        path: path.into_owned(),
        line,
    })]
}

/// A web link's content, whose links, files and images become their text: a click anywhere in
/// the link opens the link's own address.
fn unlinked(content: Vec<Inline>) -> Vec<Inline> {
    let mut flat = Vec::with_capacity(content.len());
    for inline in content {
        match inline {
            Inline::Link { content, .. } => flat.extend(unlinked(content)),
            // A file without a line here comes from a code span: a link's text cannot hold
            // another link.
            Inline::File(file) if file.line.is_none() => flat.push(Inline::Code(file.text)),
            Inline::File(file) => flat.push(Inline::Text(file.text)),
            Inline::Image { alt, .. } => flat.push(Inline::Text(alt)),
            Inline::Strong(content) => flat.push(Inline::Strong(unlinked(content))),
            Inline::Emphasis(content) => flat.push(Inline::Emphasis(unlinked(content))),
            Inline::Strikethrough(content) => flat.push(Inline::Strikethrough(unlinked(content))),
            Inline::Text(_) | Inline::Code(_) | Inline::LineBreak | Inline::Task { .. } => {
                flat.push(inline);
            }
        }
    }
    merge_texts(flat)
}

/// An http or https address the browser opener reads as one. It opens an address that `url` cannot
/// parse, such as `https://x:99999`, as a local file.
fn is_web_url(url: &str) -> bool {
    let web_scheme = url.split_once(SCHEME_SEPARATOR).is_some_and(|(scheme, _)| {
        WEB_SCHEMES
            .iter()
            .any(|web| scheme.eq_ignore_ascii_case(web))
    });
    web_scheme && url::Url::parse(url).is_ok()
}

/// The path and line of a link to a file: an absolute path, or one relative to the agent's
/// folder, then an optional `:line` (with `:column` or `-end`) or `#L12` (with `-L20`). Another
/// fragment, such as a heading's, opens the file at its top. A colon followed by anything but a
/// line, as in `javascript:` or `mailto:`, makes it no file. A Windows drive's colon, as in
/// `C:\`, is the path's. With a `file_folder`, a relative path starts there, and comes out
/// absolute.
fn local_file_target<'target>(
    target: &'target str,
    file_folder: Option<&Path>,
    style: PathStyle,
) -> Option<(Cow<'target, str>, Option<u32>)> {
    let (location, fragment) = match target.split_once(FRAGMENT) {
        Some((location, fragment)) => (location, Some(fragment)),
        None => (target, None),
    };
    let drive = style.drive_len(location);
    let colon = location.get(drive..)?.find(':').map(|at| drive + at);
    let (path, suffix_line) = match colon {
        Some(at) => {
            let suffix = location.get(at + 1..)?;
            (location.get(..at)?, Some(line_suffix(suffix)?))
        }
        None => (location, None),
    };
    let fragment_line = fragment
        .and_then(|fragment| fragment.strip_prefix(LINE_FRAGMENT))
        .and_then(|anchor| line_number(anchor.split('-').next().unwrap_or_default()));
    let line = suffix_line.or(fragment_line);
    let relative = !style.is_absolute(path)
        && !path.starts_with(|character| style.is_separator(character) || character == '~')
        && style.first_name(path).is_some();
    let path = match file_folder {
        Some(file_folder) if relative => {
            Cow::Owned(style.resolve(&file_folder.to_string_lossy(), path))
        }
        Some(_) | None => Cow::Borrowed(path),
    };
    is_file_target(&path, style).then_some((path, line))
}

/// The line of what follows a path's colon: `12`, `12:5`, `40-52` or `12:5-7`.
fn line_suffix(suffix: &str) -> Option<u32> {
    let line_len = digits_len(suffix);
    let line = line_number(suffix.get(..line_len)?)?;
    let mut rest = suffix.get(line_len..)?;
    for separator in [':', '-'] {
        if let Some(after) = rest.strip_prefix(separator) {
            let extra = digits_len(after);
            if extra == 0 {
                return None;
            }
            rest = after.get(extra..)?;
        }
    }
    rest.is_empty().then_some(line)
}

/// Whether `path` names a file the file view can ask the daemon for: absolute, or relative to the
/// agent's folder, and without `..`, which the daemon refuses as well. The daemon decides which
/// files it reads. A `?`, a leading `~` or a leading `//` belongs to a URL, a shell or a network
/// share.
fn is_file_target(path: &str, style: PathStyle) -> bool {
    if path.contains('?') || path.starts_with('~') || path.chars().any(char::is_control) {
        return false;
    }
    let leading_separators = path
        .chars()
        .take(2)
        .take_while(|&character| style.is_separator(character))
        .count();
    // `\folder` on Windows: a drive's root, not a file.
    let drive_root = !style.is_absolute(path) && leading_separators == 1;
    leading_separators < 2
        && !drive_root
        && style.first_name(path).is_some()
        && !style.goes_up(path)
}

fn merge_texts(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut merged: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        if let Inline::Text(text) = &inline
            && let Some(Inline::Text(last)) = merged.last_mut()
        {
            last.push_str(text);
            continue;
        }
        merged.push(inline);
    }
    merged
}

/// Merges adjacent texts, then splits them around `path:line` references.
fn finish_inlines(inlines: Vec<Inline>, style: PathStyle) -> Vec<Inline> {
    let merged = merge_texts(inlines);
    let mut finished = Vec::with_capacity(merged.len());
    for inline in merged {
        let Inline::Text(text) = inline else {
            finished.push(inline);
            continue;
        };
        for (between, url) in split_web_urls(&text) {
            for piece in find_file_refs(between, style) {
                match piece {
                    TextPiece::Text(part) => finished.push(Inline::Text(part.to_owned())),
                    TextPiece::File(file) => finished.push(Inline::File(file)),
                }
            }
            if let Some(url) = url {
                finished.push(Inline::Link {
                    url: url.to_owned(),
                    content: vec![Inline::Text(url.to_owned())],
                });
            }
        }
    }
    finished
}

#[derive(Debug, PartialEq, Eq)]
enum TextPiece<'text> {
    Text(&'text str),
    File(FileRef),
}

/// The bare web addresses in `text`, each with the text before it, then the text after the last.
/// An address starts at a word's start with "http://" or "https://" and runs to a space. A
/// sentence's last punctuation and a closing parenthesis it does not open stay out of it.
fn split_web_urls(text: &str) -> Vec<(&str, Option<&str>)> {
    let lower = text.to_ascii_lowercase();
    let mut pieces = Vec::new();
    let mut plain_start = 0;
    for (start, _) in lower.match_indices(WEB_URL_PREFIX) {
        let rest = lower.get(start..).unwrap_or_default();
        let Some(scheme) = WEB_URL_STARTS
            .iter()
            .find(|scheme| rest.starts_with(*scheme))
        else {
            continue;
        };
        let word_start = text
            .get(..start)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|before| before.is_whitespace() || "([<\"'".contains(before));
        if start < plain_start || !word_start {
            continue;
        }
        let rest = text.get(start..).unwrap_or_default();
        let length = rest
            .find(|character: char| character.is_whitespace() || "<>\"`".contains(character))
            .unwrap_or(rest.len());
        let url = trim_url_end(rest.get(..length).unwrap_or_default());
        if url.len() <= scheme.len() || !is_web_url(url) {
            continue;
        }
        pieces.push((text.get(plain_start..start).unwrap_or_default(), Some(url)));
        plain_start = start + url.len();
    }
    pieces.push((text.get(plain_start..).unwrap_or_default(), None));
    pieces
}

/// The address without what ends the sentence around it: a final `.`, `,`, `:`, `;`, `!`, `?`,
/// `'`, or a `)` that no `(` in the address opens.
fn trim_url_end(url: &str) -> &str {
    let opened = url.matches('(').count();
    let mut closed = url.matches(')').count();
    let mut kept = url;
    while let Some(last) = kept.chars().next_back() {
        if last == ')' && closed > opened {
            closed -= 1;
        } else if !".,:;!?'".contains(last) {
            break;
        }
        kept = kept.get(..kept.len() - last.len_utf8()).unwrap_or_default();
    }
    kept
}

fn is_path_byte(byte: u8, style: PathStyle) -> bool {
    byte.is_ascii_alphanumeric()
        || b"_./~+@-".contains(&byte)
        || style.is_separator(char::from(byte))
}

/// A path holds a separator or a `.` and a letter or a digit, so that "12:30" or "foo:12" are
/// not paths.
fn is_path(path: &str, style: PathStyle) -> bool {
    let marked = path.contains(|character| character == '.' || style.is_separator(character));
    marked && path.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

/// Where a path that starts at `start` starts once a drive before it is counted, as `C:` before
/// `\work\a.rs`.
fn drive_start(text: &str, start: usize, style: PathStyle) -> usize {
    let Some(drive_at) = start.checked_sub(2) else {
        return start;
    };
    let alone = drive_at == 0
        || text
            .as_bytes()
            .get(drive_at - 1)
            .is_some_and(|&byte| !is_path_byte(byte, style) && byte != b':');
    let drive = text.get(drive_at..).map_or(0, |rest| style.drive_len(rest));
    if alone && drive > 0 { drive_at } else { start }
}

/// A line number from 1 to `u32::MAX`.
fn line_number(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|&line| line > 0)
}

/// A `path:line` reference in a line of text, at bytes `start..end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRef {
    pub start: usize,
    pub end: usize,
    pub path: String,
    pub line: u32,
}

/// The `path:line` references in one line of text, such as a terminal's, found as in the chat.
pub fn line_refs(text: &str, style: PathStyle) -> Vec<LineRef> {
    let mut at = 0;
    let mut found = Vec::new();
    for piece in find_file_refs(text, style) {
        match piece {
            TextPiece::Text(plain) => at += plain.len(),
            TextPiece::File(file) => {
                let end = at + file.text.len();
                if let Some(line) = file.line {
                    found.push(LineRef {
                        start: at,
                        end,
                        path: file.path,
                        line,
                    });
                }
                at = end;
            }
        }
    }
    found
}

/// The length of the run of ASCII digits at the start of `text`.
fn digits_len(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

/// Splits `text` into plain pieces and `path:line` references. The pieces join back to `text`.
fn find_file_refs(text: &str, style: PathStyle) -> Vec<TextPiece<'_>> {
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let mut plain_start = 0;
    let mut search_from = 0;
    while let Some(offset) = text.get(search_from..).and_then(|rest| rest.find(':')) {
        let colon = search_from + offset;
        search_from = colon + 1;
        let mut path_start = colon;
        while path_start > plain_start
            && bytes
                .get(path_start - 1)
                .is_some_and(|&byte| is_path_byte(byte, style))
        {
            path_start -= 1;
        }
        let with_drive = drive_start(text, path_start, style);
        let path_start = if with_drive >= plain_start {
            with_drive
        } else {
            path_start
        };
        let Some(path) = text.get(path_start..colon) else {
            continue;
        };
        // A path right after a colon is part of a URL, such as "https://example.org:8080".
        let after_colon = path_start > 0 && bytes.get(path_start - 1) == Some(&b':');
        // A letter past ASCII, as in "Données/a.rs:3", cuts the path: its end alone is no file.
        let cut = text
            .get(..path_start)
            .and_then(|before| before.chars().next_back())
            .is_some_and(|character| !character.is_ascii() && character.is_alphanumeric());
        if after_colon || cut || !is_path(path, style) {
            continue;
        }
        let rest = text.get(colon + 1..).unwrap_or_default();
        let line_len = digits_len(rest);
        let Some(line) = rest.get(..line_len).and_then(line_number) else {
            continue;
        };
        let mut end = colon + 1 + line_len;
        // An optional ":column", then an optional "-end".
        for separator in [':', '-'] {
            let tail = text.get(end..).unwrap_or_default();
            if let Some(after) = tail.strip_prefix(separator) {
                let extra = digits_len(after);
                if extra > 0 {
                    end += 1 + extra;
                }
            }
        }
        // A reference glued to more path characters, such as "a.rs:12abc", is not one.
        if bytes
            .get(end)
            .is_some_and(|&byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            continue;
        }
        let (Some(before), Some(written)) =
            (text.get(plain_start..path_start), text.get(path_start..end))
        else {
            continue;
        };
        if !before.is_empty() {
            pieces.push(TextPiece::Text(before));
        }
        pieces.push(TextPiece::File(FileRef {
            text: written.to_owned(),
            path: path.to_owned(),
            line: Some(line),
        }));
        plain_start = end;
        search_from = end;
    }
    if let Some(rest) = text.get(plain_start..)
        && !rest.is_empty()
    {
        pieces.push(TextPiece::Text(rest));
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent's folder in these tests.
    const FOLDER: &str = "/work/calc";

    fn parse(markdown: &str) -> Vec<Block> {
        parse_markdown(markdown, Path::new(FOLDER))
    }

    fn text(value: &str) -> Inline {
        Inline::Text(value.to_owned())
    }

    fn file(written: &str, path: &str, line: u32) -> Inline {
        Inline::File(FileRef {
            text: written.to_owned(),
            path: path.to_owned(),
            line: Some(line),
        })
    }

    /// The inlines of a markdown text made of one paragraph.
    fn paragraph(markdown: &str) -> Vec<Inline> {
        match parse(markdown).as_slice() {
            [Block::Paragraph(inlines)] => inlines.clone(),
            blocks => panic!("expected one paragraph, got {blocks:?}"),
        }
    }

    fn references(input: &str) -> Vec<(String, String, u32)> {
        find_file_refs(input, PathStyle::Unix)
            .into_iter()
            .filter_map(|piece| match piece {
                TextPiece::File(file) => Some((file.text, file.path, file.line?)),
                TextPiece::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn headings_paragraphs_rules_and_quotes() {
        let blocks = parse("# Title\n\nFirst line\nsame paragraph.\n\n---\n\n> Quoted");

        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![text("Title")]
                },
                Block::Paragraph(vec![
                    text("First line"),
                    Inline::LineBreak,
                    text("same paragraph.")
                ]),
                Block::Rule,
                Block::Quote(vec![Block::Paragraph(vec![text("Quoted")])]),
            ]
        );
    }

    #[test]
    fn inline_formatting_and_hard_breaks() {
        let inlines = paragraph("Some **bold**, *italic*, ~~gone~~ and `code`  \nnext");

        assert_eq!(
            inlines,
            vec![
                text("Some "),
                Inline::Strong(vec![text("bold")]),
                text(", "),
                Inline::Emphasis(vec![text("italic")]),
                text(", "),
                Inline::Strikethrough(vec![text("gone")]),
                text(" and "),
                Inline::Code("code".to_owned()),
                Inline::LineBreak,
                text("next"),
            ]
        );
    }

    #[test]
    fn single_line_breaks_show_as_line_breaks_outside_code_blocks() {
        let blocks = parse(
            "The slowest cases:\ndeep_nesting: 2.4 ms\nlong_sum: **1.1 ms**\n\n```\nfirst\nsecond\n```",
        );

        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![
                    text("The slowest cases:"),
                    Inline::LineBreak,
                    text("deep_nesting: 2.4 ms"),
                    Inline::LineBreak,
                    text("long_sum: "),
                    Inline::Strong(vec![text("1.1 ms")]),
                ]),
                Block::Code {
                    language: None,
                    code: "first\nsecond".to_owned()
                },
            ]
        );
    }

    #[test]
    fn tight_and_loose_lists_wrap_their_text_in_paragraphs() {
        let tight = parse("- one\n- two\n  - nested");
        let loose = parse("3. first\n\n4. second");

        assert_eq!(
            tight,
            vec![Block::List {
                start: None,
                items: vec![
                    vec![Block::Paragraph(vec![text("one")])],
                    vec![
                        Block::Paragraph(vec![text("two")]),
                        Block::List {
                            start: None,
                            items: vec![vec![Block::Paragraph(vec![text("nested")])]],
                        },
                    ],
                ],
            }]
        );
        assert_eq!(
            loose,
            vec![Block::List {
                start: Some(3),
                items: vec![
                    vec![Block::Paragraph(vec![text("first")])],
                    vec![Block::Paragraph(vec![text("second")])],
                ],
            }]
        );
    }

    #[test]
    fn a_files_relative_links_start_from_its_own_folder() {
        let docs = Path::new("/work/calc/docs");
        let links = |markdown: &str| match parse_markdown_file(markdown, Path::new(FOLDER), docs)
            .as_slice()
        {
            [Block::Paragraph(inlines)] => inlines.clone(),
            blocks => panic!("expected one paragraph, got {blocks:?}"),
        };
        let link = |text: &str, path: &str, line: Option<u32>| {
            Inline::File(FileRef {
                text: text.to_owned(),
                path: path.to_owned(),
                line,
            })
        };

        assert_eq!(
            links("[plan](plan.md)"),
            [link("plan", "/work/calc/docs/plan.md", None)]
        );
        assert_eq!(
            links("[root](../AGENTS.md#L3)"),
            [link("root", "/work/calc/AGENTS.md", Some(3))]
        );
        assert_eq!(
            links("[abs](/work/calc/src/main.rs)"),
            [link("abs", "/work/calc/src/main.rs", None)]
        );
        // Out of the agent's folder, the daemon decides. No file at all: the link's text alone.
        assert_eq!(
            links("[out](../../etc/passwd)"),
            [link("out", "/work/etc/passwd", None)]
        );
        assert_eq!(links("[here](./)"), [text("here")]);
        assert_eq!(links("[top](#steps)"), [text("top")]);
    }

    #[test]
    fn task_list_markers_become_checkboxes() {
        let blocks = parse("- [x] done\n- [ ] open");

        let Some(Block::List { items, .. }) = blocks.first() else {
            panic!("expected a list, got {blocks:?}");
        };
        assert_eq!(
            items,
            &vec![
                vec![Block::Paragraph(vec![
                    Inline::Task { done: true },
                    text("done")
                ])],
                vec![Block::Paragraph(vec![
                    Inline::Task { done: false },
                    text("open")
                ])],
            ]
        );
        assert_eq!(blocks_plain_text(&blocks), "[x] done\n[ ] open");
    }

    #[test]
    fn code_blocks_keep_their_language_without_the_final_newline() {
        let blocks = parse("```rust title=\"x\"\nlet a = 1;\n\nlet b = 2;\n```\n\n    indented");

        assert_eq!(
            blocks,
            vec![
                Block::Code {
                    language: Some("rust".to_owned()),
                    code: "let a = 1;\n\nlet b = 2;".to_owned()
                },
                Block::Code {
                    language: None,
                    code: "indented".to_owned()
                },
            ]
        );
    }

    #[test]
    fn tables_split_head_and_rows() {
        let blocks = parse("| Name | Size |\n| --- | --- |\n| `alpha` | 3 |\n| b | **4** |");

        assert_eq!(
            blocks,
            vec![Block::Table {
                head: vec![vec![text("Name")], vec![text("Size")]],
                rows: vec![
                    vec![vec![Inline::Code("alpha".to_owned())], vec![text("3")]],
                    vec![vec![text("b")], vec![Inline::Strong(vec![text("4")])]],
                ],
            }]
        );
    }

    #[test]
    fn links_images_and_html_keep_their_text() {
        let inlines = paragraph(
            "See [the docs](https://example.org/docs), ![a chart](chart.png) and <b>this</b>.",
        );

        assert_eq!(
            inlines,
            vec![
                text("See "),
                Inline::Link {
                    url: "https://example.org/docs".to_owned(),
                    content: vec![text("the docs")]
                },
                text(", "),
                Inline::Image {
                    path: "chart.png".to_owned(),
                    alt: "a chart".to_owned()
                },
                text(" and <b>this</b>."),
            ]
        );
    }

    #[test]
    fn only_an_image_file_shows_and_a_web_image_stays_a_link() {
        assert_eq!(
            paragraph("![mockup](/tmp/shots/to-handle.PNG)"),
            vec![Inline::Image {
                path: "/tmp/shots/to-handle.PNG".to_owned(),
                alt: "mockup".to_owned()
            }]
        );
        assert_eq!(
            paragraph("![logo](https://example.org/logo.png)"),
            vec![Inline::Link {
                url: "https://example.org/logo.png".to_owned(),
                content: vec![text("logo")]
            }]
        );
        for target in [
            "notes.md",
            "chart.png:12",
            "../chart.png",
            "//host/chart.png",
        ] {
            assert_eq!(
                paragraph(&format!("![a chart]({target})")),
                vec![text("a chart")],
                "{target}"
            );
        }
    }

    /// The browser opener would open these as files of the app's folder.
    #[test]
    fn an_address_that_does_not_parse_stays_text() {
        for address in ["https://x:99999/a", "https://[bad/a"] {
            assert_eq!(paragraph(address), vec![text(address)], "{address}");
            assert_eq!(
                paragraph(&format!("[label]({address})")),
                vec![text("label")],
                "{address}"
            );
        }
    }

    #[test]
    fn bare_web_addresses_become_links_without_the_sentence_around_them() {
        let link = |url: &str| Inline::Link {
            url: url.to_owned(),
            content: vec![text(url)],
        };

        assert_eq!(
            paragraph("See https://app.example.org/p/21f0c6 for the ticket."),
            vec![
                text("See "),
                link("https://app.example.org/p/21f0c6"),
                text(" for the ticket."),
            ]
        );
        assert_eq!(
            paragraph("(the docs: https://example.org/wiki/A_(b)), then HTTP://example.org!"),
            vec![
                text("(the docs: "),
                link("https://example.org/wiki/A_(b)"),
                text("), then "),
                link("HTTP://example.org"),
                text("!"),
            ]
        );
        assert_eq!(
            paragraph("xhttps://example.org and https:// alone"),
            vec![text("xhttps://example.org and https:// alone")]
        );
    }

    /// A click anywhere in a web link opens its own address, so nothing inside it is a link.
    #[test]
    fn a_web_link_holds_no_other_link_file_or_image() {
        assert_eq!(
            paragraph(
                "[see **https://a.example.org** or ![shot](/tmp/a.png)](https://b.example.org)"
            ),
            vec![Inline::Link {
                url: "https://b.example.org".to_owned(),
                content: vec![
                    text("see "),
                    Inline::Strong(vec![text("https://a.example.org")]),
                    text(" or shot"),
                ]
            }]
        );
    }

    /// Text with many addresses not at a word's start, as in a pasted log, is read once.
    #[test]
    fn a_long_text_of_addresses_splits_in_linear_time() {
        let mut long = "url=http://a.example.org ".repeat(16_000);
        long.push_str("https://b.example.org");
        let closed = format!("https://c.example.org{}", ")".repeat(64_000));
        let started = std::time::Instant::now();

        let pieces = split_web_urls(&long);
        let trimmed = trim_url_end(&closed);

        assert_eq!(pieces.len(), 2);
        assert_eq!(trimmed, "https://c.example.org");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn references_in_text_become_file_links() {
        let inlines =
            paragraph("The loop is at src/server/accept.rs:135, see also (lib/util.rs:7).");

        assert_eq!(
            inlines,
            vec![
                text("The loop is at "),
                file("src/server/accept.rs:135", "src/server/accept.rs", 135),
                text(", see also ("),
                file("lib/util.rs:7", "lib/util.rs", 7),
                text(")."),
            ]
        );
    }

    #[test]
    fn a_terminal_line_gives_its_references_by_their_place() {
        let line = "  --> src/main.rs:12:5 and https://example.org:8080 ";

        let found = line_refs(line, PathStyle::Unix);

        assert_eq!(
            found,
            [LineRef {
                start: 6,
                end: 22,
                path: "src/main.rs".to_owned(),
                line: 12,
            }]
        );
        assert_eq!(line.get(6..22), Some("src/main.rs:12:5"));
    }

    #[test]
    fn a_path_cut_by_a_letter_past_ascii_is_no_reference() {
        assert_eq!(
            references("/home/u/Données/app/src/main.rs:5 and café/a.rs:3, but é a.rs:4"),
            [("a.rs:4".to_owned(), "a.rs".to_owned(), 4)]
        );
    }

    #[test]
    fn a_code_span_holding_only_a_reference_becomes_a_file_link() {
        let inlines = paragraph("At `docs/notes.md:4` and `run a.rs:3 now`.");

        assert_eq!(
            inlines,
            vec![
                text("At "),
                file("docs/notes.md:4", "docs/notes.md", 4),
                text(" and "),
                Inline::Code("run a.rs:3 now".to_owned()),
                text("."),
            ]
        );
    }

    #[test]
    fn a_code_span_holding_only_a_files_path_becomes_a_file_link() {
        let linked = |code: &str| {
            let inlines = paragraph(&format!("See `{code}`."));
            matches!(
                inlines.as_slice(),
                [_, Inline::File(FileRef { path, line: None, .. }), _] if path == code
            )
        };

        for code in [
            "src/lexer.rs",
            "/tmp/mockup/bubble-on.png",
            "docs/plan.md",
            "Cargo.toml",
            "README.md",
            "Makefile",
            ".env",
            "crates/app/.gitignore",
            "scripts/build.v2",
            "Cargo.lock",
            "logs/build.log",
            "src/main.c",
            ".github/workflows/ci.yml",
        ] {
            assert!(linked(code), "{code}");
        }
        for code in [
            "cargo test",
            "self.items",
            "main",
            "--release",
            "v1.2",
            "src/",
            "src/v1.2",
            "~/notes.md",
            "../secret.md",
            "Option<String>",
            "std::fs",
            "a.rs b.rs",
            "console.log",
            "process.env",
            "self.lock",
            "node.js",
            "rect.h",
            "github.com/owner/repo/blob/main/src/lib.rs",
            "example.com/index.html",
        ] {
            assert!(!linked(code), "{code}");
        }
    }

    #[test]
    fn a_files_path_on_windows_links_with_its_drive() {
        let linked = |code: &str| {
            let blocks = parse_markdown(&format!("See `{code}`."), Path::new(r"C:\work"));
            matches!(
                blocks.as_slice(),
                [Block::Paragraph(inlines)]
                    if matches!(inlines.as_slice(), [_, Inline::File(file), _] if file.path == code)
            )
        };

        assert!(linked(r"C:\work\a.rs"));
        assert!(linked(r"src\lib.rs"));
        assert!(!linked(r"C:\work\"));
        assert!(!linked(r"\\server\share\a.rs"));
        assert!(!linked(r"C:\..\a.rs"));
    }

    #[test]
    fn a_files_code_span_in_a_web_link_stays_code() {
        assert_eq!(
            paragraph("[`src/lib.rs`](https://example.org/src/lib.rs)"),
            vec![Inline::Link {
                url: "https://example.org/src/lib.rs".to_owned(),
                content: vec![Inline::Code("src/lib.rs".to_owned())],
            }]
        );
    }

    #[test]
    fn a_column_or_a_range_stays_in_the_written_text() {
        assert_eq!(
            references("main.rs:12:5 and tests/case.rs:40-52."),
            vec![
                ("main.rs:12:5".to_owned(), "main.rs".to_owned(), 12),
                (
                    "tests/case.rs:40-52".to_owned(),
                    "tests/case.rs".to_owned(),
                    40
                ),
            ]
        );
        assert_eq!(
            references("~/work/a_b+c@d-e.txt:9"),
            vec![(
                "~/work/a_b+c@d-e.txt:9".to_owned(),
                "~/work/a_b+c@d-e.txt".to_owned(),
                9
            )]
        );
    }

    #[test]
    fn times_words_urls_and_line_zero_are_not_references() {
        for input in [
            "at 12:30 today",
            "foo:12",
            "https://example.org:8080/a",
            "a.b:0",
            "a.rs:12abc",
            "file.rs:",
            "../:3",
        ] {
            assert_eq!(references(input), Vec::new(), "{input}");
        }
    }

    #[test]
    fn pieces_join_back_to_the_text() {
        let input = "x src/a.rs:1 y b.md:2:3, ok";

        let joined: String = find_file_refs(input, PathStyle::Unix)
            .into_iter()
            .map(|piece| match piece {
                TextPiece::Text(part) => part.to_owned(),
                TextPiece::File(file) => file.text,
            })
            .collect();

        assert_eq!(joined, input);
    }

    #[test]
    fn local_links_to_a_line_become_file_links() {
        let inlines = paragraph(
            "[the check](src/check.rs:21), [anchor](lib/b.rs#L8-L9) and [site](https://example.org/a.rs:3)",
        );

        assert_eq!(
            inlines,
            vec![
                file("the check", "src/check.rs", 21),
                text(", "),
                file("anchor", "lib/b.rs", 8),
                text(" and "),
                Inline::Link {
                    url: "https://example.org/a.rs:3".to_owned(),
                    content: vec![text("site")]
                },
            ]
        );
    }

    #[test]
    fn links_to_files_in_the_folder_open_the_file_view() {
        let opened = |target: &str| -> Option<(String, Option<u32>)> {
            match paragraph(&format!("[x]({target})")).as_slice() {
                [Inline::File(file)] => Some((file.path.clone(), file.line)),
                _ => None,
            }
        };

        let cases = [
            ("AGENTS.md", "AGENTS.md", None),
            ("./docs/plan.md", "./docs/plan.md", None),
            ("docs/plan.md#decisions", "docs/plan.md", None),
            ("src/lexer.rs#L17", "src/lexer.rs", Some(17)),
            ("src/lexer.rs:17:5", "src/lexer.rs", Some(17)),
            ("Makefile:3", "Makefile", Some(3)),
            ("/work/calc/src/error.rs", "/work/calc/src/error.rs", None),
            (
                "/work/calc/src/error.rs#L2",
                "/work/calc/src/error.rs",
                Some(2),
            ),
        ];
        for (target, path, line) in cases {
            assert_eq!(opened(target), Some((path.to_owned(), line)), "{target}");
        }
    }

    #[test]
    fn on_windows_links_and_references_take_drives_and_backslashes() {
        let folder = Path::new(r"C:\work\calc");
        let opened = |target: &str| -> Option<(String, Option<u32>)> {
            match parse_markdown(&format!("[x]({target})"), folder).as_slice() {
                [Block::Paragraph(inlines)] => match inlines.as_slice() {
                    [Inline::File(file)] => Some((file.path.clone(), file.line)),
                    _ => None,
                },
                _ => None,
            }
        };
        let cases = [
            (r"src\lexer.rs:17", r"src\lexer.rs", Some(17)),
            (
                r"C:\work\calc\src\error.rs:12",
                r"C:\work\calc\src\error.rs",
                Some(12),
            ),
            ("c:/WORK/calc/src/a.rs#L3", "c:/WORK/calc/src/a.rs", Some(3)),
            (r"C:\work\calc\README.md", r"C:\work\calc\README.md", None),
            (r"C:\work\other\a.rs", r"C:\work\other\a.rs", None),
            (r"D:\a.rs", r"D:\a.rs", None),
        ];
        for (target, path, line) in cases {
            assert_eq!(opened(target), Some((path.to_owned(), line)), "{target}");
        }
        let refused = [
            r"\work\calc\a.rs",
            r"..\secrets.txt",
            r"\\server\share\a.rs",
        ];
        for target in refused {
            assert_eq!(opened(target), None, "{target}");
        }

        let windows = PathStyle::Windows;
        let found: Vec<(String, u32)> = find_file_refs(
            r"see C:\work\calc\src\main.rs:12, src\lib.rs:3 and https://example.org:8080",
            windows,
        )
        .into_iter()
        .filter_map(|piece| match piece {
            TextPiece::File(file) => Some((file.path, file.line?)),
            TextPiece::Text(_) => None,
        })
        .collect();
        assert_eq!(
            found,
            [
                (r"C:\work\calc\src\main.rs".to_owned(), 12),
                (r"src\lib.rs".to_owned(), 3)
            ]
        );
    }

    #[test]
    fn links_that_would_leave_the_app_keep_only_their_text() {
        let targets = [
            "javascript:alert(1)",
            "JavaScript:void(0)",
            "file:///etc/hosts",
            "mailto:someone@example.org",
            "data:text/html,hello",
            "ftp://example.org/a.txt",
            "//example.org/a.rs",
            "../secrets.txt",
            "/work/calc/../other/a.rs",
            "~/notes.md",
            "notes.md?raw=1",
            "src/a.rs:twelve",
            "src/a.rs:0",
            "#heading",
            "./",
        ];
        for target in targets {
            assert_eq!(
                paragraph(&format!("[label]({target})")),
                vec![text("label")],
                "{target}"
            );
        }
        assert_eq!(
            paragraph("[d](HTTPS://example.org)"),
            vec![Inline::Link {
                url: "HTTPS://example.org".to_owned(),
                content: vec![text("d")]
            }]
        );
    }

    /// The daemon decides which files it reads: a link to one outside the agent's folder asks it.
    #[test]
    fn links_to_absolute_paths_open_the_file_view_wherever_the_file_is() {
        for target in [
            "/etc/hosts",
            "/work/calculator/src/a.rs",
            "/tmp/shots/a.png",
        ] {
            assert_eq!(
                paragraph(&format!("[label]({target})")),
                vec![Inline::File(FileRef {
                    text: "label".to_owned(),
                    path: target.to_owned(),
                    line: None,
                })],
                "{target}"
            );
        }
    }

    #[test]
    fn a_link_to_a_file_without_text_shows_its_target() {
        assert_eq!(
            paragraph("See [](AGENTS.md)."),
            vec![
                text("See "),
                Inline::File(FileRef {
                    text: "AGENTS.md".to_owned(),
                    path: "AGENTS.md".to_owned(),
                    line: None,
                }),
                text("."),
            ]
        );
    }

    #[test]
    fn plain_text_drops_the_formatting() {
        let inlines = paragraph("**Done**: see `x` in [docs](https://example.org) and src/a.rs:2");

        assert_eq!(plain_text(&inlines), "Done: see x in docs and src/a.rs:2");
    }

    #[test]
    fn blocks_plain_text_puts_each_block_on_its_lines() {
        let blocks = parse(
            "## Plan\n\n> - one\n>   - **two**\n\n---\n\n```sh\ncargo test\ncargo fmt\n```\n\n| a | b |\n| - | - |\n| `1` | 2 |",
        );

        assert_eq!(
            blocks_plain_text(&blocks),
            "Plan\none\ntwo\ncargo test\ncargo fmt\na b\n1 2"
        );
    }

    #[test]
    fn deep_nesting_is_flattened_without_failing() {
        let markdown = format!("{}deep", "> ".repeat(200));

        let blocks = parse(&markdown);

        assert_eq!(blocks.len(), 1);
    }
}
