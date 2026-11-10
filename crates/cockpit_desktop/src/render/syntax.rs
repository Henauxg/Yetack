//! Syntax colors: syntect's default syntaxes, mapped onto the few classes of the mockup palette.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::LazyLock;

use strum::IntoStaticStr;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use super::capped::{self, Capped};

/// The default syntaxes, loaded once: loading them takes a few milliseconds.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Scopes that decide the class wherever they sit in the stack, before the innermost scope is
/// looked at: a JSON key is a string inside a key, and the key wins.
static OUTER_RULES: LazyLock<Vec<(Scope, TokenClass)>> = LazyLock::new(|| {
    scope_rules(&[
        ("comment", TokenClass::Comment),
        ("meta.structure.dictionary.key", TokenClass::Type),
        ("meta.mapping.key", TokenClass::Type),
    ])
});

/// Checked from the innermost scope outward; the first scope that matches a rule decides. Within
/// one scope, the first matching rule wins, so a narrow rule comes before a wider one.
static INNER_RULES: LazyLock<Vec<(Scope, TokenClass)>> = LazyLock::new(|| {
    scope_rules(&[
        // Operators and assignment targets stay plain, as in the mockup.
        ("keyword.operator", TokenClass::Plain),
        ("variable.other.readwrite.assignment", TokenClass::Plain),
        ("string", TokenClass::String),
        ("constant.character", TokenClass::String),
        ("markup.raw", TokenClass::String),
        ("constant.numeric", TokenClass::Number),
        ("variable.other", TokenClass::Number),
        ("meta.group.expansion", TokenClass::Number),
        ("punctuation.definition.list_item", TokenClass::Number),
        ("markup.list.numbered.bullet", TokenClass::Number),
        ("markup.list.unnumbered.bullet", TokenClass::Number),
        ("support.macro", TokenClass::Macro),
        ("support.function.macro", TokenClass::Macro),
        ("entity.name.function.macro", TokenClass::Macro),
        ("variable.parameter.option", TokenClass::Macro),
        ("keyword", TokenClass::Keyword),
        ("storage", TokenClass::Keyword),
        ("constant.language", TokenClass::Keyword),
        ("markup.heading", TokenClass::Keyword),
        ("support.function", TokenClass::Keyword),
        ("entity.name.type", TokenClass::Type),
        ("entity.name.struct", TokenClass::Type),
        ("entity.name.enum", TokenClass::Type),
        ("entity.name.class", TokenClass::Type),
        ("entity.name.tag", TokenClass::Type),
        ("support.type", TokenClass::Type),
        ("support.class", TokenClass::Type),
        ("meta.annotation", TokenClass::Type),
        ("meta.attribute", TokenClass::Type),
        ("meta.link.inline.description", TokenClass::Type),
        ("punctuation.definition.link", TokenClass::Type),
        ("markup.underline.link", TokenClass::Comment),
        ("punctuation.definition.metadata", TokenClass::Comment),
    ])
});

/// Syntect's name of the syntax whose capitalized words are types, which its grammar leaves
/// unscoped in many places (parameters, generics).
const RUST_SYNTAX: &str = "Rust";

/// Syntect's syntax that colors nothing; it reads as no language.
const PLAIN_TEXT_SYNTAX: &str = "Plain Text";

/// The extension that names syntect's shell syntax.
const SHELL_EXTENSION: &str = "sh";

/// Code block tags that name a syntax by another word than its extensions and its name.
const TAG_ALIASES: &[(&str, &str)] = &[
    ("shell", SHELL_EXTENSION),
    ("console", SHELL_EXTENSION),
    ("zsh", SHELL_EXTENSION),
];

/// Past this size, code is shown without colors: the parser is too slow for the UI thread.
const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;

/// The mockup palette (R3-parts): keywords and headings #c4a7e7, strings and inline code #f6c177,
/// types, keys and link text #9ccfd8, macros and flags #ebbcba, numbers, variables and list
/// markers #eb6f92, comments #939393, plain #f3f3f3. The stylesheet colors the classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IntoStaticStr)]
#[strum(serialize_all = "kebab-case", prefix = "tok-")]
pub enum TokenClass {
    Plain,
    Keyword,
    String,
    Type,
    Macro,
    Number,
    Comment,
}

impl TokenClass {
    pub fn css_class(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub class: TokenClass,
}

/// A syntax of syntect's default set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language(usize);

impl Language {
    /// From a code block's info string ("rust", "rs", "sql", "json", "sh", "bash", "markdown",
    /// "md", ...), case-insensitive. Only the first word of the info string counts. `None` for an
    /// unknown or empty tag.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let word = tag
            .split(|character: char| {
                character.is_whitespace() || character == ',' || character == '{'
            })
            .find(|word| !word.is_empty())?
            .to_ascii_lowercase();
        let token = TAG_ALIASES
            .iter()
            .find(|(alias, _)| *alias == word)
            .map_or(word.as_str(), |(_, extension)| extension);
        Self::from_syntax(SYNTAXES.find_syntax_by_token(token)?)
    }

    /// From a file's extension, then its file name (such as `Makefile`).
    pub fn from_path(path: &Path) -> Option<Self> {
        let by_extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| SYNTAXES.find_syntax_by_extension(extension));
        let syntax = by_extension.or_else(|| {
            let file_name = path.file_name()?.to_str()?;
            SYNTAXES.find_syntax_by_extension(file_name)
        })?;
        Self::from_syntax(syntax)
    }

    fn from_syntax(syntax: &SyntaxReference) -> Option<Self> {
        if syntax.name == PLAIN_TEXT_SYNTAX {
            return None;
        }
        SYNTAXES
            .syntaxes()
            .iter()
            .position(|candidate| std::ptr::eq(candidate, syntax))
            .map(Self)
    }

    fn syntax(self) -> Option<&'static SyntaxReference> {
        SYNTAXES.syntaxes().get(self.0)
    }
}

/// One `Vec<Span>` per line of `code`, as `str::lines` splits it (no line endings, a final newline
/// adds no line). An empty line is an empty Vec. `None` gives one plain span per non-empty line.
/// Adjacent spans of the same class are merged.
///
/// The grammar is most of a chat's Rust work, and a chat draws the same code again, as on a
/// switch back to its agent: the result is kept for that text and language ([`HighlightCache`]).
pub fn highlight(code: &str, language: Option<Language>) -> Vec<Vec<Span>> {
    let colored = language.filter(|_| code.len() <= MAX_HIGHLIGHT_BYTES);
    let Some((language, syntax)) =
        colored.and_then(|language| Some((language, language.syntax()?)))
    else {
        return code.lines().map(plain_line).collect();
    };
    let key = CacheKey::new(code, language);
    if let Some(lines) = CACHE.with_borrow(|cache| cache.get(&key)) {
        return lines;
    }
    let lines = highlight_with(code, syntax);
    CACHE.with_borrow_mut(|cache| cache.insert(key, lines.clone()));
    lines
}

/// The text kept in [`CACHE`], in bytes of source code. The spans take about twice as much.
const MAX_CACHED_BYTES: usize = 4 * 1024 * 1024;

thread_local! {
    /// The chat renders on one thread: its cache needs no lock.
    static CACHE: RefCell<HighlightCache> = RefCell::new(HighlightCache::new(MAX_CACHED_BYTES));
}

/// A text by its language, hash and length. Two texts that share all three would share colors:
/// with a 64-bit hash, no chat meets that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    language: Language,
    hash: u64,
    bytes: usize,
}

impl CacheKey {
    fn new(code: &str, language: Language) -> Self {
        let mut hasher = DefaultHasher::new();
        code.hash(&mut hasher);
        Self {
            language,
            hash: hasher.finish(),
            bytes: code.len(),
        }
    }
}

/// Highlighted texts, the oldest dropped first once they pass `max_bytes` of source code.
#[derive(Debug, Default)]
struct HighlightCache {
    max_bytes: usize,
    bytes: usize,
    lines: HashMap<CacheKey, Vec<Vec<Span>>>,
    order: VecDeque<CacheKey>,
}

impl HighlightCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            ..Self::default()
        }
    }

    fn get(&self, key: &CacheKey) -> Option<Vec<Vec<Span>>> {
        self.lines.get(key).cloned()
    }

    /// Keeps `lines` for `key`. A text larger than the whole cache is not kept.
    fn insert(&mut self, key: CacheKey, lines: Vec<Vec<Span>>) {
        if key.bytes > self.max_bytes || self.lines.contains_key(&key) {
            return;
        }
        while self.bytes + key.bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if self.lines.remove(&oldest).is_some() {
                self.bytes -= oldest.bytes;
            }
        }
        self.bytes += key.bytes;
        self.order.push_back(key);
        self.lines.insert(key, lines);
    }
}

fn highlight_with(code: &str, syntax: &SyntaxReference) -> Vec<Vec<Span>> {
    let capitalized_types = syntax.name == RUST_SYNTAX;
    let mut parser = Some(LineParser::new(syntax));
    let mut lines = Vec::new();
    for line in code.lines() {
        // A grammar error leaves the parser in an unknown state: the rest stays plain.
        let spans = parser.as_mut().and_then(|parser| parser.spans(line));
        let Some(spans) = spans else {
            parser = None;
            lines.push(plain_line(line));
            continue;
        };
        let spans = if capitalized_types {
            mark_capitalized_types(spans)
        } else {
            spans
        };
        lines.push(merge_spans(spans));
    }
    lines
}

/// `highlight` of the first `max_lines` lines of `code` only.
pub fn highlight_capped(
    code: &str,
    language: Option<Language>,
    max_lines: usize,
) -> Capped<Vec<Span>> {
    let (head, more) = capped::split_lines(code, max_lines);
    Capped {
        rows: highlight(head, language),
        more,
    }
}

/// Parses a text line by line, keeping the grammar's state between lines.
struct LineParser {
    state: ParseState,
    stack: ScopeStack,
}

impl LineParser {
    fn new(syntax: &SyntaxReference) -> Self {
        Self {
            state: ParseState::new(syntax),
            stack: ScopeStack::new(),
        }
    }

    /// `None` when the grammar fails on the line.
    fn spans(&mut self, line: &str) -> Option<Vec<Span>> {
        // The default syntaxes expect each line with its newline.
        let operations = self
            .state
            .parse_line(&format!("{line}\n"), &SYNTAXES)
            .ok()?;
        let mut spans = Vec::new();
        let mut start = 0;
        for (position, operation) in operations {
            let end = position.min(line.len());
            if end > start {
                push_span(&mut spans, line.get(start..end)?, classify(&self.stack));
                start = end;
            }
            self.stack.apply(&operation).ok()?;
        }
        if start < line.len() {
            push_span(&mut spans, line.get(start..)?, classify(&self.stack));
        }
        Some(spans)
    }
}

fn classify(stack: &ScopeStack) -> TokenClass {
    let scopes = stack.as_slice();
    for &scope in scopes {
        if let Some(class) = matching_rule(&OUTER_RULES, scope) {
            return class;
        }
    }
    for &scope in scopes.iter().rev() {
        if let Some(class) = matching_rule(&INNER_RULES, scope) {
            return class;
        }
    }
    TokenClass::Plain
}

fn matching_rule(rules: &[(Scope, TokenClass)], scope: Scope) -> Option<TokenClass> {
    rules
        .iter()
        .find(|(prefix, _)| prefix.is_prefix_of(scope))
        .map(|&(_, class)| class)
}

/// A rule whose scope does not parse is left out; the rules are written above, so none fails.
fn scope_rules(rules: &[(&str, TokenClass)]) -> Vec<(Scope, TokenClass)> {
    let mut parsed = Vec::with_capacity(rules.len());
    for &(name, class) in rules {
        if let Ok(scope) = Scope::new(name) {
            parsed.push((scope, class));
        }
    }
    parsed
}

fn plain_line(line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    push_span(&mut spans, line, TokenClass::Plain);
    spans
}

fn push_span(spans: &mut Vec<Span>, text: &str, class: TokenClass) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.class == class
    {
        last.text.push_str(text);
        return;
    }
    spans.push(Span {
        text: text.to_owned(),
        class,
    });
}

fn merge_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut merged = Vec::with_capacity(spans.len());
    for span in spans {
        push_span(&mut merged, &span.text, span.class);
    }
    merged
}

/// Splits the plain spans so that a word starting with a capital letter becomes a `Type`.
fn mark_capitalized_types(spans: Vec<Span>) -> Vec<Span> {
    let mut marked = Vec::with_capacity(spans.len());
    for span in spans {
        if span.class != TokenClass::Plain {
            marked.push(span);
            continue;
        }
        let mut word_start: Option<usize> = None;
        let mut last_end = 0;
        // A sentinel space ends the last word.
        let characters = span.text.char_indices().chain([(span.text.len(), ' ')]);
        for (index, character) in characters {
            let in_word = character.is_alphanumeric() || character == '_';
            match (word_start, in_word) {
                (None, true) => word_start = Some(index),
                (Some(start), false) => {
                    let word = span.text.get(start..index).unwrap_or_default();
                    if word.starts_with(|character: char| character.is_ascii_uppercase()) {
                        push_span(
                            &mut marked,
                            span.text.get(last_end..start).unwrap_or_default(),
                            TokenClass::Plain,
                        );
                        push_span(&mut marked, word, TokenClass::Type);
                        last_end = index;
                    }
                    word_start = None;
                }
                (None, false) | (Some(_), true) => {}
            }
        }
        push_span(
            &mut marked,
            span.text.get(last_end..).unwrap_or_default(),
            TokenClass::Plain,
        );
    }
    marked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(language: Language) -> &'static str {
        language
            .syntax()
            .map_or(PLAIN_TEXT_SYNTAX, |syntax| syntax.name.as_str())
    }

    /// The class of the first span whose text is exactly `text`, on any line.
    fn class_of(lines: &[Vec<Span>], text: &str) -> TokenClass {
        lines
            .iter()
            .flatten()
            .find(|span| span.text.trim() == text)
            .map(|span| span.class)
            .unwrap_or_else(|| panic!("no span {text:?} in {lines:?}"))
    }

    /// The class of the first span that contains `text`, on any line.
    fn class_containing(lines: &[Vec<Span>], text: &str) -> TokenClass {
        lines
            .iter()
            .flatten()
            .find(|span| span.text.contains(text))
            .map(|span| span.class)
            .unwrap_or_else(|| panic!("no span with {text:?} in {lines:?}"))
    }

    fn language(tag: &str) -> Option<Language> {
        let language = Language::from_tag(tag);
        assert!(language.is_some(), "{tag} is a known tag");
        language
    }

    #[test]
    fn a_text_drawn_again_takes_its_kept_colors() {
        let rust = language("rust").unwrap();
        let code = "fn total(items: &[Item]) -> u32 {\n    items.len() as u32\n}\n";
        let fresh = highlight_with(code, rust.syntax().unwrap());

        let first = highlight(code, Some(rust));
        let kept = CACHE.with_borrow(|cache| cache.get(&CacheKey::new(code, rust)));
        let again = highlight(code, Some(rust));

        assert_eq!(first, fresh);
        assert_eq!(kept.as_ref(), Some(&fresh));
        assert_eq!(again, fresh);
    }

    #[test]
    fn the_cache_drops_its_oldest_texts_past_its_size() {
        let rust = language("rust").unwrap();
        let key = |code: &str| CacheKey::new(code, rust);
        let mut cache = HighlightCache::new(10);
        let lines = vec![plain_line("x")];

        cache.insert(key("abcd"), lines.clone());
        cache.insert(key("efgh"), lines.clone());
        cache.insert(key("ijkl"), lines.clone());
        cache.insert(key("too long a text"), lines.clone());

        assert_eq!(cache.get(&key("abcd")), None, "the oldest went");
        assert!(cache.get(&key("efgh")).is_some());
        assert!(cache.get(&key("ijkl")).is_some());
        assert_eq!(cache.get(&key("too long a text")), None);
        assert_eq!(cache.bytes, 8);
    }

    fn line_texts(lines: &[Vec<Span>]) -> Vec<String> {
        lines
            .iter()
            .map(|spans| spans.iter().map(|span| span.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn tags_and_paths_find_their_syntax() {
        let names = |tag: &str| Language::from_tag(tag).map(name);

        assert_eq!(names("rust"), Some("Rust"));
        assert_eq!(names("RS"), Some("Rust"));
        assert_eq!(names("rust,ignore"), Some("Rust"));
        assert_eq!(names("sql"), Some("SQL"));
        assert_eq!(names("json"), Some("JSON"));
        assert_eq!(names("markdown"), Some("Markdown"));
        assert_eq!(names("md"), Some("Markdown"));
        assert_eq!(names("bash"), names("sh"));
        assert_eq!(names("shell"), names("sh"));
        assert!(names("sh").is_some());
        assert_eq!(names(""), None);
        assert_eq!(names("txt"), None);
        assert_eq!(names("no-such-language"), None);
        assert_eq!(names("nix"), None);

        let path_name = |path: &str| Language::from_path(Path::new(path)).map(name);
        assert_eq!(path_name("src/main.rs"), Some("Rust"));
        assert_eq!(path_name("queries/report.sql"), Some("SQL"));
        assert_eq!(path_name("Makefile"), Some("Makefile"));
        assert_eq!(path_name("notes"), None);
    }

    #[test]
    fn rust_colors_keywords_strings_comments_numbers_macros_and_types() {
        let code = "use std::fmt;\n\n#[derive(Debug)]\npub fn total(items: &[Item]) -> u32 {\n    let count = 7; // a note\n    println!(\"{count} items\");\n    Ok(count)\n}\n";

        let lines = highlight(code, language("rust"));

        assert_eq!(lines.len(), 8);
        assert_eq!(lines.get(1), Some(&Vec::new()), "an empty line has no span");
        assert_eq!(line_texts(&lines), code.lines().collect::<Vec<&str>>());
        assert_eq!(class_of(&lines, "use"), TokenClass::Keyword);
        assert_eq!(class_of(&lines, "#[derive(Debug)]"), TokenClass::Type);
        assert_eq!(class_of(&lines, "pub"), TokenClass::Keyword);
        assert_eq!(class_of(&lines, "Item"), TokenClass::Type);
        assert_eq!(class_of(&lines, "7"), TokenClass::Number);
        assert_eq!(class_of(&lines, "// a note"), TokenClass::Comment);
        assert_eq!(class_of(&lines, "println!"), TokenClass::Macro);
        assert_eq!(class_of(&lines, "\"{count} items\""), TokenClass::String);
        assert_eq!(class_containing(&lines, "="), TokenClass::Plain);
        assert_eq!(class_of(&lines, "Ok"), TokenClass::Type);
    }

    #[test]
    fn sql_colors_keywords_strings_numbers_and_comments() {
        let code = "-- Open orders\nSELECT id FROM orders WHERE state = 'open' LIMIT 20;";

        let lines = highlight(code, language("sql"));

        assert_eq!(class_of(&lines, "-- Open orders"), TokenClass::Comment);
        assert_eq!(class_of(&lines, "SELECT"), TokenClass::Keyword);
        assert_eq!(class_of(&lines, "'open'"), TokenClass::String);
        assert_eq!(class_of(&lines, "20"), TokenClass::Number);
    }

    #[test]
    fn json_keys_differ_from_string_values() {
        let code = "{\n  \"name\": \"value\",\n  \"size\": 3,\n  \"on\": true\n}";

        let lines = highlight(code, language("json"));

        assert_eq!(class_of(&lines, "\"name\""), TokenClass::Type);
        assert_eq!(class_of(&lines, "\"value\""), TokenClass::String);
        assert_eq!(class_of(&lines, "3"), TokenClass::Number);
        assert_eq!(class_of(&lines, "true"), TokenClass::Keyword);
    }

    #[test]
    fn bash_colors_variables_flags_strings_and_comments() {
        let code = "# Build it\nset -eu\ntarget=\"$1\"\nmake --jobs 4 \"$target\" ${OUT}";

        let lines = highlight(code, language("bash"));

        assert_eq!(class_of(&lines, "# Build it"), TokenClass::Comment);
        assert_eq!(class_of(&lines, "set"), TokenClass::Keyword);
        assert_eq!(class_of(&lines, "--jobs"), TokenClass::Macro);
        assert_eq!(class_containing(&lines, "target"), TokenClass::Plain);
        assert_eq!(class_of(&lines, "${OUT}"), TokenClass::Number);
        let quoted = lines.get(2).expect("the assignment line");
        assert!(
            quoted
                .iter()
                .any(|span| span.text == "$1" && span.class == TokenClass::Number),
            "{quoted:?}"
        );
    }

    #[test]
    fn markdown_colors_headings_list_markers_links_and_code() {
        let code = "## Setup\n\n- Read [the guide](docs/guide.md) first.\n- Run `make`.";

        let lines = highlight(code, language("md"));

        assert_eq!(class_of(&lines, "## Setup"), TokenClass::Keyword);
        assert_eq!(class_of(&lines, "-"), TokenClass::Number);
        assert_eq!(class_of(&lines, "[the guide]"), TokenClass::Type);
        assert_eq!(class_of(&lines, "(docs/guide.md)"), TokenClass::Comment);
        assert_eq!(class_of(&lines, "`make`"), TokenClass::String);
    }

    #[test]
    fn without_a_language_each_line_is_one_plain_span() {
        let lines = highlight("first line\n\nthird line\n", None);

        assert_eq!(
            lines,
            vec![
                vec![Span {
                    text: "first line".to_owned(),
                    class: TokenClass::Plain
                }],
                Vec::new(),
                vec![Span {
                    text: "third line".to_owned(),
                    class: TokenClass::Plain
                }],
            ]
        );
    }

    #[test]
    fn multi_line_strings_keep_their_color_on_later_lines() {
        let code = "let text = \"first\nsecond\";";

        let lines = highlight(code, language("rust"));

        assert_eq!(class_of(&lines, "second\""), TokenClass::String);
    }

    #[test]
    fn a_code_block_shows_its_first_five_thousand_lines() {
        let code: String = (1..=5_003)
            .map(|number| format!("x = {number}\n"))
            .collect();

        let shown = highlight_capped(&code, language("python"), capped::MAX_RENDERED_LINES);

        assert_eq!((shown.rows.len(), shown.more), (5_000, 3));
        let last: Vec<&str> = shown
            .rows
            .last()
            .into_iter()
            .flatten()
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(last.concat(), "x = 5000");
    }

    #[test]
    fn css_classes_are_pinned() {
        let classes = [
            TokenClass::Plain,
            TokenClass::Keyword,
            TokenClass::String,
            TokenClass::Type,
            TokenClass::Macro,
            TokenClass::Number,
            TokenClass::Comment,
        ]
        .map(TokenClass::css_class);

        assert_eq!(
            classes,
            [
                "tok-plain",
                "tok-keyword",
                "tok-string",
                "tok-type",
                "tok-macro",
                "tok-number",
                "tok-comment"
            ]
        );
    }
}
