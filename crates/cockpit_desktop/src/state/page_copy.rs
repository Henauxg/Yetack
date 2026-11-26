//! "Open in browser" for a web page on a server, which the browser on this computer cannot reach.
//! The daemon reads the files the page names, and `page_files::open_copy` writes them with the
//! page to a folder on this computer. The page's own text is the one the file view shows.
//!
//! The files read are those the page's `src` and `href` attributes name by a relative address,
//! such as its images, style sheets and scripts. As an IDE serves a page from its project, a copy
//! takes only pages and web files ([`WEB_EXTENSIONS`]) in the page's folder or the agent's: a
//! file the page names elsewhere, or a key or a script for the shell, stays on the server. A file
//! that a script or a style sheet loads is not read, and neither is one the daemon does not send:
//! a binary file other than an image.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, IgnoredSearch, ReadFileParams, Request, ResponseBody};

use super::app_state::AppState;
use super::requests::{Outgoing, Purpose};
use crate::format;
use crate::machine::MachineId;
use crate::path_style::{self, PathStyle};

/// The most files a copy reads besides the page.
const MAX_FILES: usize = 32;

/// The most missing files a note names.
const MAX_NAMED: usize = 3;

/// The extensions of a web page's file, whatever their case.
const PAGE_EXTENSIONS: [&str; 2] = ["html", "htm"];

/// The extensions of the files a copy takes besides pages, whatever their case: what a page shows
/// or runs in the browser.
const WEB_EXTENSIONS: [&str; 14] = [
    "css", "js", "mjs", "json", "map", "svg", "txt", "xml", "csv", "png", "jpg", "jpeg", "gif",
    "webp",
];

/// The attributes whose address names a file the page shows, runs or links to.
const FILE_ATTRIBUTES: [&str; 2] = ["src", "href"];

/// What starts an escaped byte in an address, as in `%20`.
const ESCAPE: char = '%';

/// The names that end a path in a folder: the folder itself and the one above.
const FOLDER_NAMES: [&str; 2] = [path_style::CURRENT, path_style::PARENT];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyId(u64);

/// The copies whose files the daemon is reading.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageCopies {
    next: u64,
    copies: Vec<PageCopy>,
}

impl PageCopies {
    /// Whether a copy has all its files, read or failed.
    pub fn any_ready(&self) -> bool {
        self.copies.iter().any(PageCopy::is_ready)
    }

    /// Whether the daemon is reading the files of `agent`'s copy of `page`.
    pub fn is_reading(&self, agent: AgentId, page: &str) -> bool {
        self.copies.iter().any(|copy| {
            copy.agent == agent
                && copy
                    .style
                    .same_path(&copy.page, &copy.style.resolve(page, ""))
        })
    }

    /// Keeps the copies of the agents `keep` accepts. A forgotten agent's page does not open.
    pub fn retain_agents(&mut self, keep: impl Fn(AgentId) -> bool) {
        self.copies.retain(|copy| keep(copy.agent));
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PageCopy {
    id: CopyId,
    agent: AgentId,
    style: PathStyle,
    /// The folder on the agent's machine that holds the page and every file the copy reads. The
    /// copy keeps their places under it, so that the page's addresses still lead to them.
    root: String,
    /// The page's path on the agent's machine, without "." and "..".
    page: String,
    text: String,
    files: Vec<CopiedFile>,
}

impl PageCopy {
    fn is_ready(&self) -> bool {
        self.files
            .iter()
            .all(|file| file.bytes != FileBytes::Waiting)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CopiedFile {
    /// Its path on the agent's machine.
    path: String,
    bytes: FileBytes,
}

#[derive(Debug, Clone, PartialEq)]
enum FileBytes {
    Waiting,
    Read(Vec<u8>),
    Failed,
}

/// A copy whose files the daemon sent or refused, for `page_files::open_copy` to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyCopy {
    /// The page's names under the copy's folder.
    pub page: Vec<String>,
    /// Each file read, the page first.
    pub files: Vec<ReadyFile>,
    /// The paths of the files the page names that the daemon did not send.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyFile {
    /// The file's names under the copy's folder.
    pub names: Vec<String>,
    pub bytes: Vec<u8>,
}

/// Whether `path` names a web page: its extension is `.html` or `.htm`, whatever its case.
pub fn is_page(path: &str) -> bool {
    has_extension(path, &PAGE_EXTENSIONS)
}

/// Whether the extension of `path`'s file is one of `extensions`, whatever its case.
fn has_extension(path: &str, extensions: &[&str]) -> bool {
    PathStyle::of_text(path)
        .file_name(path)
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            extensions
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Starts a copy of the page at `page`, an absolute path on the agent's machine whose text is
/// `text`: asks the daemon for each file the page names, up to [`MAX_FILES`]. Does nothing while
/// the agent's copy of that page is reading.
pub fn start(state: &mut AppState, agent: AgentId, page: &str, text: String) -> Vec<Outgoing> {
    if state.page_copies.is_reading(agent, page) {
        return Vec::new();
    }
    let style = PathStyle::of_text(page);
    let page = style.resolve(page, "");
    let Some(folder) = style.parent(&page) else {
        return Vec::new();
    };
    let agent_folder = state
        .agents
        .get(&agent)
        .map(|entry| style.resolve(&entry.summary.folder.to_string_lossy(), ""));
    let inside = |path: &str, holder: &str| {
        style
            .strip_prefix(path, holder)
            .is_some_and(|rest| !rest.is_empty())
    };
    let mut paths: Vec<String> = Vec::new();
    let mut left_out: Vec<String> = Vec::new();
    for reference in relative_references(&text) {
        if paths.len() + left_out.len() >= MAX_FILES {
            break;
        }
        let path = style.resolve(folder, &reference);
        // Both are resolved, so they name the same file when they read the same.
        let known = style.same_name(&path, &page)
            || paths
                .iter()
                .chain(&left_out)
                .any(|known| style.same_name(known, &path));
        if known {
            continue;
        }
        let in_project = inside(&path, folder)
            || agent_folder
                .as_deref()
                .is_some_and(|agent_folder| inside(&path, agent_folder));
        let web_file = is_page(&path) || has_extension(&path, &WEB_EXTENSIONS);
        if in_project && web_file {
            paths.push(path);
        } else {
            left_out.push(path);
        }
    }
    let root = common_folder(style, folder, &paths);
    let id = CopyId(state.page_copies.next);
    state.page_copies.next += 1;
    let requests: Vec<Outgoing> = paths
        .iter()
        .map(|path| Outgoing {
            request: Request::AgentReadFile(ReadFileParams {
                agent,
                path: PathBuf::from(path),
                ignored: IgnoredSearch::Quick,
            }),
            purpose: Purpose::PageCopy {
                copy: id,
                path: path.clone(),
            },
            to: MachineId::THIS,
        })
        .collect();
    let waiting = paths.into_iter().map(|path| CopiedFile {
        path,
        bytes: FileBytes::Waiting,
    });
    let failed = left_out.into_iter().map(|path| CopiedFile {
        path,
        bytes: FileBytes::Failed,
    });
    let files: Vec<CopiedFile> = waiting.chain(failed).collect();
    state.page_copies.copies.push(PageCopy {
        id,
        agent,
        style,
        root,
        page,
        text,
        files,
    });
    requests
}

/// The daemon's answer for `path`, a file of the copy: its text or image, or `None` when it
/// sent neither.
pub(super) fn answered(
    state: &mut AppState,
    copy: CopyId,
    path: &str,
    answer: Option<ResponseBody>,
) {
    let Some(copy) = state
        .page_copies
        .copies
        .iter_mut()
        .find(|known| known.id == copy)
    else {
        return;
    };
    let bytes = match answer {
        Some(ResponseBody::File { text, .. }) => FileBytes::Read(text.into_bytes()),
        Some(ResponseBody::Image { image, .. }) => image
            .data
            .decode()
            .map_or(FileBytes::Failed, FileBytes::Read),
        Some(_) | None => FileBytes::Failed,
    };
    if let Some(file) = copy.files.iter_mut().find(|file| file.path == path) {
        file.bytes = bytes;
    }
}

/// Takes out the copies that have all their files, read or failed.
pub fn take_ready(state: &mut AppState) -> Vec<ReadyCopy> {
    let (ready, waiting): (Vec<PageCopy>, Vec<PageCopy>) =
        std::mem::take(&mut state.page_copies.copies)
            .into_iter()
            .partition(PageCopy::is_ready);
    state.page_copies.copies = waiting;
    ready.into_iter().map(ready_copy).collect()
}

fn ready_copy(copy: PageCopy) -> ReadyCopy {
    let PageCopy {
        style,
        root,
        page,
        text,
        files: copied,
        ..
    } = copy;
    let under_root = |path: &str| -> Vec<String> {
        let rest = style.strip_prefix(path, &root).unwrap_or(path);
        style.names(rest).map(str::to_owned).collect()
    };
    let page = under_root(&page);
    let mut files = vec![ReadyFile {
        names: page.clone(),
        bytes: text.into_bytes(),
    }];
    let mut missing = Vec::new();
    for file in copied {
        match file.bytes {
            FileBytes::Read(bytes) => files.push(ReadyFile {
                names: under_root(&file.path),
                bytes,
            }),
            FileBytes::Waiting | FileBytes::Failed => missing.push(file.path),
        }
    }
    ReadyCopy {
        page,
        files,
        missing,
    }
}

/// What a note says of the files a copy lacks, by their names: `None` when it lacks none.
pub fn missing_note(missing: &[String]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let named: Vec<&str> = missing
        .iter()
        .take(MAX_NAMED)
        .map(|path| PathStyle::of_text(path).file_name(path).unwrap_or(path))
        .collect();
    let mut names = named.join(", ");
    let more = missing.len().saturating_sub(MAX_NAMED);
    if more > 0 {
        names.push_str(&format!(" and {more} more"));
    }
    let files = format::count(missing.len(), "file", "files");
    Some(format!(
        "The browser shows the page without {files} it names: {names}."
    ))
}

/// The deepest folder that holds `folder` and each of `paths`, all absolute with no "..".
fn common_folder(style: PathStyle, folder: &str, paths: &[String]) -> String {
    let mut root = folder;
    for path in paths {
        while style.strip_prefix(path, root).is_none_or(str::is_empty) {
            match style.parent(root) {
                Some(parent) if parent != root => root = parent,
                _ => break,
            }
        }
    }
    root.to_owned()
}

/// The paths that `html`'s `src` and `href` attributes name by a relative address, in the page's
/// order, as the caller takes them.
fn relative_references(html: &str) -> impl Iterator<Item = String> {
    let mut rest = html;
    std::iter::from_fn(move || {
        while let Some(at) = rest.find(|character: char| character.is_ascii_whitespace()) {
            rest = rest.get(at + 1..).unwrap_or_default();
            let Some((address, after)) = attribute_value(rest) else {
                continue;
            };
            rest = after;
            if let Some(path) = relative_path(address) {
                return Some(path);
            }
        }
        None
    })
}

/// The address of a `src` or `href` attribute that starts `text`, and the text after it.
fn attribute_value(text: &str) -> Option<(&str, &str)> {
    let name = FILE_ATTRIBUTES.iter().find(|name| {
        text.get(..name.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(name))
    })?;
    let after_name = text.get(name.len()..)?.trim_start();
    let value = after_name.strip_prefix('=')?.trim_start();
    match value.chars().next()? {
        quote @ ('"' | '\'') => {
            let inside = value.get(1..)?;
            let end = inside.find(quote)?;
            Some((inside.get(..end)?, inside.get(end + 1..)?))
        }
        _ => {
            let end = value
                .find(|character: char| character.is_ascii_whitespace() || character == '>')
                .unwrap_or(value.len());
            Some((value.get(..end)?, value.get(end..)?))
        }
    }
}

/// The path a relative address names, without its query and fragment, its escapes decoded.
/// `None` for an address with a scheme, such as `https:` or `data:`, one from a root, and one
/// that names a folder or nothing. The decoded path is checked, so that `%2F` starts no root.
fn relative_path(address: &str) -> Option<String> {
    let address = address.trim();
    let end = address.find(['?', '#']).unwrap_or(address.len());
    let path = unescaped(address.get(..end)?)?;
    // A browser reads `\` in an address as `/`.
    let separator = |character: char| PathStyle::Windows.is_separator(character);
    let from_root = path.starts_with(separator);
    let last = path.rsplit(separator).next().unwrap_or_default();
    let folder = last.is_empty() || FOLDER_NAMES.contains(&last);
    let control = path.chars().any(char::is_control);
    (!from_root && !folder && !control && !has_scheme(&path)).then_some(path)
}

/// Whether `address` starts with a scheme and its colon, as `mailto:` does. A Windows drive,
/// as in `C:/`, counts as one.
fn has_scheme(address: &str) -> bool {
    let Some((scheme, _)) = address.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// `address` with each `%XX` replaced by its byte. `None` when an escape is broken or the bytes
/// are not UTF-8.
fn unescaped(address: &str) -> Option<String> {
    let mut bytes: Vec<u8> = Vec::with_capacity(address.len());
    let mut rest = address;
    while let Some(at) = rest.find(ESCAPE) {
        bytes.extend_from_slice(rest.get(..at)?.as_bytes());
        let digits = rest.get(at + 1..at + 3)?;
        bytes.push(u8::from_str_radix(digits, 16).ok()?);
        rest = rest.get(at + 3..)?;
    }
    bytes.extend_from_slice(rest.as_bytes());
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentStatus, Base64, Image, ImageMediaType};

    use super::*;
    use crate::state::{Drafts, apply, state_with, summary};

    /// The agent's folder, which holds the pages of most tests.
    const AGENT_FOLDER: &str = "/srv/site";

    fn agent() -> AgentId {
        AgentId::new(7)
    }

    fn state() -> AppState {
        let mut agent = summary(7, AgentStatus::Idle);
        agent.folder = PathBuf::from(AGENT_FOLDER);
        state_with(vec![agent])
    }

    fn asked(requests: &[Outgoing]) -> Vec<String> {
        requests
            .iter()
            .map(|outgoing| match &outgoing.purpose {
                Purpose::PageCopy { path, .. } => path.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    fn copy_of(requests: &[Outgoing]) -> CopyId {
        match requests.first().map(|outgoing| &outgoing.purpose) {
            Some(Purpose::PageCopy { copy, .. }) => *copy,
            other => panic!("{other:?}"),
        }
    }

    fn text(text: &str) -> Option<ResponseBody> {
        Some(ResponseBody::File {
            path: None,
            text: text.to_owned(),
            edits: None,
            ignored: false,
        })
    }

    fn names(path: &str) -> Vec<String> {
        path.split('/').map(str::to_owned).collect()
    }

    fn file(path: &str, bytes: &[u8]) -> ReadyFile {
        ReadyFile {
            names: names(path),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn a_page_is_an_html_or_htm_file_whatever_its_case() {
        for page in ["/srv/site/index.html", r"C:\mock\Page.HTM", "/srv/.html"] {
            assert!(is_page(page), "{page}");
        }
        for other in [
            "/srv/site/index.html.txt",
            "/srv/site/html",
            "/srv/site/",
            "notes.md",
        ] {
            assert!(!is_page(other), "{other}");
        }
    }

    #[test]
    fn the_references_are_the_relative_addresses_of_src_and_href() {
        let html = r##"<!doctype html>
<link rel="stylesheet" HREF='style.css?v=2'>
<img src=shots/a%20b.png alt="x"><img
  src = "shots/a%20b.png">
<script src="../shared/app.js"></script>
<a href="#top">top</a> <a href="https://example.org/x.png">web</a>
<a href="mailto:someone@example.org">mail</a> <img src="data:image/png;base64,AAAA">
<img src="/from/root.png"> <a href="docs/">folder</a> <img data-src="lazy.png">
<img srcset="big.png 2x"> <script>img.src = "dynamic.png";</script>
<a href="next.html#part">next</a>"##;

        assert_eq!(
            relative_references(html).collect::<Vec<_>>(),
            [
                "style.css",
                "shots/a b.png",
                "shots/a b.png",
                "../shared/app.js",
                "next.html"
            ]
        );
    }

    #[test]
    fn a_broken_escape_or_a_control_character_names_no_file() {
        assert_eq!(
            relative_references(r#" src="a%2.png" src="a%ZZ.png""#).count(),
            0
        );
        assert_eq!(
            relative_references(r#" src="a%0A.png" src="b.png""#).collect::<Vec<_>>(),
            ["b.png"]
        );
    }

    #[test]
    fn a_copy_reads_each_file_the_page_names_once() {
        let mut state = state();
        let html = r#" <img src="a.png"> <img src="./a.png"> <img src="index.html">
            <link href="../css/site.css">"#;

        let requests = start(
            &mut state,
            agent(),
            "/srv/site/pages/index.html",
            html.to_owned(),
        );

        assert_eq!(
            asked(&requests),
            ["/srv/site/pages/a.png", "/srv/site/css/site.css"]
        );
        assert!(!state.page_copies.any_ready());
    }

    #[test]
    fn a_ready_copy_keeps_the_places_of_its_files_under_their_common_folder() {
        let mut state = state();
        let html =
            r#" <img src="shots/a.png"> <link href="../css/site.css"> <script src="gone.js">"#;
        let requests = start(
            &mut state,
            agent(),
            "/srv/site/pages/index.html",
            html.to_owned(),
        );
        let copy = copy_of(&requests);

        answered(
            &mut state,
            copy,
            "/srv/site/pages/shots/a.png",
            Some(ResponseBody::Image {
                path: PathBuf::from("/srv/site/pages/shots/a.png"),
                image: Image {
                    media_type: ImageMediaType::Png,
                    data: Base64::new("iVBO".to_owned()),
                },
                ignored: false,
            }),
        );
        answered(&mut state, copy, "/srv/site/css/site.css", text("body {}"));
        assert!(!state.page_copies.any_ready());
        answered(&mut state, copy, "/srv/site/pages/gone.js", None);
        assert!(state.page_copies.any_ready());

        let ready = take_ready(&mut state);

        assert_eq!(
            ready,
            [ReadyCopy {
                page: names("pages/index.html"),
                files: vec![
                    file("pages/index.html", html.as_bytes()),
                    file("pages/shots/a.png", &[0x89, 0x50, 0x4e]),
                    file("css/site.css", b"body {}"),
                ],
                missing: vec!["/srv/site/pages/gone.js".to_owned()],
            }]
        );
        assert!(take_ready(&mut state).is_empty());
    }

    #[test]
    fn a_copy_is_reading_until_it_is_taken() {
        let mut state = state();
        let page = "/srv/site/index.html";

        start(
            &mut state,
            agent(),
            page,
            r#" <img src="a.png">"#.to_owned(),
        );

        assert!(state.page_copies.is_reading(agent(), page));
        assert!(
            !state
                .page_copies
                .is_reading(agent(), "/srv/site/other.html")
        );
        assert!(take_ready(&mut state).is_empty());
        assert!(state.page_copies.is_reading(agent(), page));
    }

    #[test]
    fn the_note_names_the_first_missing_files() {
        let paths = |count: usize| -> Vec<String> {
            (1..=count)
                .map(|index| format!("/srv/site/{index}.woff"))
                .collect()
        };

        assert_eq!(missing_note(&[]), None);
        assert_eq!(
            missing_note(&paths(1)).as_deref(),
            Some("The browser shows the page without 1 file it names: 1.woff.")
        );
        assert_eq!(
            missing_note(&paths(5)).as_deref(),
            Some(
                "The browser shows the page without 5 files it names: 1.woff, 2.woff, 3.woff \
                 and 2 more."
            )
        );
    }

    /// As an IDE serves a page from its project: a file the page names elsewhere, or a file no
    /// browser needs, such as a key or a script for the shell, stays on the server.
    #[test]
    fn a_copy_takes_only_web_files_from_the_pages_or_the_agents_folder() {
        let mut state = state();
        let html = r#" <link href="../style.css"> <img src="shot.PNG"> <a href="run.bat">
            <a href="../../../home/dev/.ssh/id_ed25519"> <link href="../../other/theme.css">
            <a href="notes/.env">"#;

        let requests = start(
            &mut state,
            agent(),
            "/srv/site/mock/index.html",
            html.to_owned(),
        );
        let copy = copy_of(&requests);
        answered(&mut state, copy, "/srv/site/style.css", text("body {}"));
        answered(&mut state, copy, "/srv/site/mock/shot.PNG", text("png"));

        assert_eq!(
            asked(&requests),
            ["/srv/site/style.css", "/srv/site/mock/shot.PNG"]
        );
        let ready = take_ready(&mut state);
        assert_eq!(
            ready.first().map(|ready| ready.missing.clone()),
            Some(vec![
                "/srv/site/mock/run.bat".to_owned(),
                "/home/dev/.ssh/id_ed25519".to_owned(),
                "/srv/other/theme.css".to_owned(),
                "/srv/site/mock/notes/.env".to_owned(),
            ])
        );
    }

    #[test]
    fn a_page_that_names_no_file_is_ready_at_once() {
        let mut state = state();

        let requests = start(
            &mut state,
            agent(),
            r"C:\mock\index.html",
            "<p>Hi</p>".to_owned(),
        );

        assert!(requests.is_empty());
        assert_eq!(
            take_ready(&mut state),
            [ReadyCopy {
                page: vec!["index.html".to_owned()],
                files: vec![file("index.html", b"<p>Hi</p>")],
                missing: Vec::new(),
            }]
        );
    }

    #[test]
    fn a_copy_reads_at_most_its_limit_of_files() {
        let mut state = state();
        let html: String = (0..MAX_FILES + 3)
            .map(|index| format!(r#" <img src="{index}.png">"#))
            .collect();

        let requests = start(&mut state, agent(), "/srv/index.html", html);

        assert_eq!(requests.len(), MAX_FILES);
    }

    /// The daemon sends a page of up to 4 MiB, and the scan runs on a click.
    #[test]
    fn a_page_of_many_addresses_is_read_in_linear_time() {
        use std::time::{Duration, Instant};

        let html: String = (0..60_000)
            .map(|index| format!(" src={index}.png"))
            .collect();
        let started = Instant::now();

        let requests = start(&mut state(), agent(), "/srv/index.html", html);

        assert_eq!(requests.len(), MAX_FILES);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn an_escape_does_not_make_an_address_absolute() {
        let html = r#" src="%2Fetc%2Fpasswd" src="%5C%5Chost%5Cshare%5Ca.png"
            src="C%3A%5Cother.png" src="a%2Fb.png""#;

        assert_eq!(relative_references(html).collect::<Vec<_>>(), ["a/b.png"]);
    }

    #[test]
    fn an_address_that_ends_in_a_folder_names_no_file() {
        let html = r#" href="." href=".." href="docs/.." href="docs/../a.png""#;

        assert_eq!(
            relative_references(html).collect::<Vec<_>>(),
            ["docs/../a.png"]
        );
    }

    #[test]
    fn a_copy_is_reading_for_its_own_agent_only() {
        let mut state = state();
        let page = "/srv/site/index.html";

        start(
            &mut state,
            agent(),
            page,
            r#" <img src="a.png">"#.to_owned(),
        );

        assert!(state.page_copies.is_reading(agent(), page));
        assert!(!state.page_copies.is_reading(AgentId::new(8), page));
    }

    #[test]
    fn a_second_click_while_reading_asks_for_nothing() {
        let mut state = state();
        let page = "/srv/site/index.html";
        let html = r#" <img src="a.png">"#;

        let first = start(&mut state, agent(), page, html.to_owned());
        let second = start(&mut state, agent(), page, html.to_owned());

        assert_eq!(asked(&first), ["/srv/site/a.png"]);
        assert!(second.is_empty());
    }

    #[test]
    fn a_forgotten_agent_opens_no_copy() {
        let mut state = state();
        let requests = start(
            &mut state,
            agent(),
            "/srv/site/index.html",
            r#" <img src="a.png">"#.to_owned(),
        );
        let copy = copy_of(&requests);

        let mut drafts = Drafts::default();
        apply::forget_agent(&mut state, &mut drafts, agent());
        answered(&mut state, copy, "/srv/site/a.png", None);

        assert!(take_ready(&mut state).is_empty());
    }

    #[test]
    fn a_page_path_that_goes_up_keeps_its_place() {
        let mut state = state();
        let requests = start(
            &mut state,
            agent(),
            "/srv/site/pages/../index.html",
            r#" <img src="a.png"> <img src="index.html">"#.to_owned(),
        );
        let copy = copy_of(&requests);
        answered(&mut state, copy, "/srv/site/a.png", text("png"));

        assert_eq!(asked(&requests), ["/srv/site/a.png"]);
        let ready = take_ready(&mut state);
        assert_eq!(
            ready.first().map(|ready| ready.page.clone()),
            Some(names("index.html"))
        );
    }

    #[test]
    fn an_address_above_the_drive_stays_on_it() {
        let mut at_the_root = summary(7, AgentStatus::Idle);
        at_the_root.folder = PathBuf::from(r"C:\");
        let mut state = state_with(vec![at_the_root]);

        let requests = start(
            &mut state,
            agent(),
            r"C:\mock\index.html",
            r#" <img src="../../../shot.png">"#.to_owned(),
        );
        let copy = copy_of(&requests);
        answered(&mut state, copy, r"C:\shot.png", text("png"));

        assert_eq!(asked(&requests), [r"C:\shot.png"]);
        let ready = take_ready(&mut state);
        assert_eq!(
            ready.first().map(|ready| ready.page.clone()),
            Some(names("mock/index.html"))
        );
    }
}
