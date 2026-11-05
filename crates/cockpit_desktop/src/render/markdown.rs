//! Agent text as a tree of blocks and inlines, with `path:line` references found in the text.

use std::iter::Peekable;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Nesting deeper than this (quotes in quotes, lists in lists) is flattened, so a hostile text
/// cannot exhaust the stack.
const MAX_DEPTH: usize = 32;

const TASK_DONE: &str = "[x] ";
const TASK_OPEN: &str = "[ ] ";

/// A GitHub-style line anchor, as in `src/main.rs#L12`.
const LINE_ANCHOR: &str = "#L";

/// What separates a URL's scheme from the rest, as in `https://`.
const SCHEME_SEPARATOR: &str = "://";

/// The schemes of the links that open in the browser.
const WEB_SCHEMES: [&str; 2] = ["http", "https"];

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
    Link { url: String, content: Vec<Inline> },
    File(FileRef),
    LineBreak,
}

/// A `path:line` reference, which opens the file view at that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    /// As written, such as "src/main.rs:12".
    pub text: String,
    pub path: String,
    pub line: u32,
}

pub fn parse_markdown(markdown: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder {
        events: Parser::new_ext(markdown, options).peekable(),
    };
    builder.blocks(None, 0)
}

/// The text of inlines without formatting, for notifications and copies.
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    push_plain_text(&mut text, inlines);
    text
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
            Inline::LineBreak => text.push('\n'),
        }
    }
}

/// Builds the tree from pulldown-cmark's events, which always come balanced.
struct Builder<'input> {
    events: Peekable<Parser<'input>>,
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
        finish_inlines(inlines)
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
        finish_inlines(inlines)
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
        finish_inlines(inlines)
    }

    fn push_inline(&mut self, inlines: &mut Vec<Inline>, event: Event<'input>, depth: usize) {
        match event {
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                inlines.push(Inline::Text(text.into_string()));
            }
            Event::Code(code) | Event::InlineMath(code) | Event::DisplayMath(code) => {
                inlines.push(code_span(&code));
            }
            Event::SoftBreak => inlines.push(Inline::Text(" ".to_owned())),
            Event::HardBreak => inlines.push(Inline::LineBreak),
            Event::TaskListMarker(done) => {
                let marker = if done { TASK_DONE } else { TASK_OPEN };
                inlines.push(Inline::Text(marker.to_owned()));
            }
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
                    Tag::Link { dest_url, .. } => inlines.extend(link(&dest_url, content)),
                    // An image shows as its alt text; other tags keep their content.
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

/// A code span that holds only a `path:line` reference opens the file view.
fn code_span(code: &str) -> Inline {
    let trimmed = code.trim();
    match find_file_refs(trimmed).as_slice() {
        [TextPiece::File(file)] => Inline::File(file.clone()),
        _ => Inline::Code(code.to_owned()),
    }
}

/// A link to a local `path:line` or `path#L12` opens the file view instead of a browser. Any
/// other link than a web one keeps only its text: the system would open it with whatever program
/// handles its scheme or file.
fn link(url: &str, content: Vec<Inline>) -> Vec<Inline> {
    if !url.contains(SCHEME_SEPARATOR)
        && let Some((path, line)) = split_link_target(url)
    {
        let text = plain_text(&content);
        return vec![Inline::File(FileRef {
            text: if text.is_empty() {
                url.to_owned()
            } else {
                text
            },
            path: path.to_owned(),
            line,
        })];
    }
    if !is_web_url(url) {
        return content;
    }
    vec![Inline::Link {
        url: url.to_owned(),
        content,
    }]
}

fn is_web_url(url: &str) -> bool {
    url.split_once(SCHEME_SEPARATOR).is_some_and(|(scheme, _)| {
        WEB_SCHEMES
            .iter()
            .any(|web| scheme.eq_ignore_ascii_case(web))
    })
}

/// `path:line` (with an optional `:column` or `-end`) or `path#L12`, taking the whole of `target`.
fn split_link_target(target: &str) -> Option<(&str, u32)> {
    if let Some((path, anchor)) = target.split_once(LINE_ANCHOR) {
        let digits: &str = anchor.split(['-', 'L']).next()?;
        return Some((path, line_number(digits)?)).filter(|(path, _)| is_path(path));
    }
    match find_file_refs(target).as_slice() {
        [TextPiece::File(file)] => {
            let path = target.get(..file.path.len())?;
            Some((path, file.line))
        }
        _ => None,
    }
}

/// Merges adjacent texts, then splits them around `path:line` references.
fn finish_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
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
    let mut finished = Vec::with_capacity(merged.len());
    for inline in merged {
        let Inline::Text(text) = inline else {
            finished.push(inline);
            continue;
        };
        for piece in find_file_refs(&text) {
            match piece {
                TextPiece::Text(part) => finished.push(Inline::Text(part.to_owned())),
                TextPiece::File(file) => finished.push(Inline::File(file)),
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

fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_./~+@-".contains(&byte)
}

/// A path holds a `/` or a `.` and a letter or a digit, so that "12:30" or "foo:12" are not paths.
fn is_path(path: &str) -> bool {
    path.contains(['/', '.']) && path.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

/// A line number from 1 to `u32::MAX`.
fn line_number(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|&line| line > 0)
}

/// The length of the run of ASCII digits at the start of `text`.
fn digits_len(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

/// Splits `text` into plain pieces and `path:line` references. The pieces join back to `text`.
fn find_file_refs(text: &str) -> Vec<TextPiece<'_>> {
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
                .is_some_and(|&byte| is_path_byte(byte))
        {
            path_start -= 1;
        }
        let Some(path) = text.get(path_start..colon) else {
            continue;
        };
        // A path right after a colon is part of a URL, such as "https://example.org:8080".
        let after_colon = path_start > 0 && bytes.get(path_start - 1) == Some(&b':');
        if after_colon || !is_path(path) {
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
            line,
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

    fn text(value: &str) -> Inline {
        Inline::Text(value.to_owned())
    }

    fn file(written: &str, path: &str, line: u32) -> Inline {
        Inline::File(FileRef {
            text: written.to_owned(),
            path: path.to_owned(),
            line,
        })
    }

    /// The inlines of a markdown text made of one paragraph.
    fn paragraph(markdown: &str) -> Vec<Inline> {
        match parse_markdown(markdown).as_slice() {
            [Block::Paragraph(inlines)] => inlines.clone(),
            blocks => panic!("expected one paragraph, got {blocks:?}"),
        }
    }

    fn references(input: &str) -> Vec<(String, String, u32)> {
        find_file_refs(input)
            .into_iter()
            .filter_map(|piece| match piece {
                TextPiece::File(file) => Some((file.text, file.path, file.line)),
                TextPiece::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn headings_paragraphs_rules_and_quotes() {
        let blocks = parse_markdown("# Title\n\nFirst line\nsame paragraph.\n\n---\n\n> Quoted");

        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![text("Title")]
                },
                Block::Paragraph(vec![text("First line same paragraph.")]),
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
    fn tight_and_loose_lists_wrap_their_text_in_paragraphs() {
        let tight = parse_markdown("- one\n- two\n  - nested");
        let loose = parse_markdown("3. first\n\n4. second");

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
    fn task_list_markers_become_text() {
        let blocks = parse_markdown("- [x] done\n- [ ] open");

        let Some(Block::List { items, .. }) = blocks.first() else {
            panic!("expected a list, got {blocks:?}");
        };
        assert_eq!(
            items,
            &vec![
                vec![Block::Paragraph(vec![text("[x] done")])],
                vec![Block::Paragraph(vec![text("[ ] open")])],
            ]
        );
    }

    #[test]
    fn code_blocks_keep_their_language_without_the_final_newline() {
        let blocks =
            parse_markdown("```rust title=\"x\"\nlet a = 1;\n\nlet b = 2;\n```\n\n    indented");

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
        let blocks =
            parse_markdown("| Name | Size |\n| --- | --- |\n| `a.txt` | 3 |\n| b | **4** |");

        assert_eq!(
            blocks,
            vec![Block::Table {
                head: vec![vec![text("Name")], vec![text("Size")]],
                rows: vec![
                    vec![vec![Inline::Code("a.txt".to_owned())], vec![text("3")]],
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
                text(", a chart and <b>this</b>."),
            ]
        );
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

        let joined: String = find_file_refs(input)
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
    fn only_web_links_stay_links() {
        let inlines = paragraph(
            "[a](javascript:alert(1)) [b](file:///etc/hosts) [c](notes.html) [d](HTTPS://example.org)",
        );

        assert_eq!(
            inlines,
            vec![
                text("a b c "),
                Inline::Link {
                    url: "HTTPS://example.org".to_owned(),
                    content: vec![text("d")]
                },
            ]
        );
    }

    #[test]
    fn plain_text_drops_the_formatting() {
        let inlines = paragraph("**Done**: see `x` in [docs](https://example.org) and src/a.rs:2");

        assert_eq!(plain_text(&inlines), "Done: see x in docs and src/a.rs:2");
    }

    #[test]
    fn deep_nesting_is_flattened_without_failing() {
        let markdown = format!("{}deep", "> ".repeat(200));

        let blocks = parse_markdown(&markdown);

        assert_eq!(blocks.len(), 1);
    }
}
