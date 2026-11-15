//! "Open in browser" for a web page on a server, which the browser on this computer cannot reach.
//! The daemon reads the files the page names, and `browser::open_copy` writes them with the page
//! to a folder on this computer. The page's own text is the one the file view shows.
//!
//! The files read are those the page's `src` and `href` attributes name by a relative address,
//! such as its images, style sheets and scripts. A file that a script or a style sheet loads is
//! not read, and neither is one the daemon does not send: a binary file other than an image.

use std::path::PathBuf;

use base64::Engine;
use cockpit_protocol::{AgentId, ReadFileParams, Request, ResponseBody};

use super::app_state::AppState;
use super::requests::{Outgoing, Purpose};
use crate::format;
use crate::machine::MachineId;
use crate::path_style::PathStyle;

/// The most files a copy reads besides the page.
const MAX_FILES: usize = 32;

/// The most missing files a note names.
const MAX_NAMED: usize = 3;

/// The extensions of a web page's file, whatever their case.
const PAGE_EXTENSIONS: [&str; 2] = ["html", "htm"];

/// The attributes whose address names a file the page shows, runs or links to.
const FILE_ATTRIBUTES: [&str; 2] = ["src", "href"];

/// What starts an escaped byte in an address, as in `%20`.
const ESCAPE: char = '%';

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

    /// Whether the daemon is reading the files of a copy of `page`.
    pub fn reading(&self, page: &str) -> bool {
        self.copies
            .iter()
            .any(|copy| copy.style.same_path(&copy.page, page))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PageCopy {
    id: CopyId,
    style: PathStyle,
    /// The folder on the agent's machine that holds the page and every file it names. The copy
    /// keeps their places under it, so that the page's addresses still lead to them.
    root: String,
    /// The page's path on the agent's machine, with its text.
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

/// A copy whose files the daemon sent or refused, for `browser::open_copy` to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyCopy {
    /// The page's names under the copy's folder.
    pub page: Vec<String>,
    /// Each file read, the page first, by its names under the copy's folder.
    pub files: Vec<(Vec<String>, Vec<u8>)>,
    /// The paths of the files the page names that the daemon did not send.
    pub missing: Vec<String>,
}

/// Whether `path` names a web page: its extension is `.html` or `.htm`, whatever its case.
pub fn is_page(path: &str) -> bool {
    PathStyle::of_text(path)
        .file_name(path)
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            PAGE_EXTENSIONS
                .iter()
                .any(|page| extension.eq_ignore_ascii_case(page))
        })
}

/// Starts a copy of the page at `page`, an absolute path on the agent's machine whose text is
/// `text`: asks the daemon for each file the page names, up to [`MAX_FILES`].
pub fn start(state: &mut AppState, agent: AgentId, page: &str, text: String) -> Vec<Outgoing> {
    let style = PathStyle::of_text(page);
    let Some(folder) = style.parent(page) else {
        return Vec::new();
    };
    let mut paths: Vec<String> = Vec::new();
    for reference in relative_references(&text) {
        if paths.len() >= MAX_FILES {
            break;
        }
        let path = style.resolve(folder, &reference);
        let known =
            style.same_path(&path, page) || paths.iter().any(|known| style.same_path(known, &path));
        if !known {
            paths.push(path);
        }
    }
    let root = common_folder(style, folder, &paths);
    let id = CopyId(state.page_copies.next);
    state.page_copies.next += 1;
    let requests = paths
        .iter()
        .map(|path| Outgoing {
            request: Request::AgentReadFile(ReadFileParams {
                agent,
                path: PathBuf::from(path),
            }),
            purpose: Purpose::PageCopy {
                copy: id,
                path: path.clone(),
            },
            to: MachineId::THIS,
        })
        .collect();
    let files = paths
        .into_iter()
        .map(|path| CopiedFile {
            path,
            bytes: FileBytes::Waiting,
        })
        .collect();
    state.page_copies.copies.push(PageCopy {
        id,
        style,
        root,
        page: page.to_owned(),
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
        Some(ResponseBody::Image { image, .. }) => base64::engine::general_purpose::STANDARD
            .decode(image.data)
            .map_or(FileBytes::Failed, FileBytes::Read),
        Some(_) | None => FileBytes::Failed,
    };
    for file in copy.files.iter_mut().filter(|file| file.path == path) {
        file.bytes = bytes.clone();
    }
}

/// Takes out the copies that have all their files, read or failed.
pub fn take_ready(state: &mut AppState) -> Vec<ReadyCopy> {
    let (ready, waiting) = std::mem::take(&mut state.page_copies.copies)
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
    let mut files = vec![(page.clone(), text.into_bytes())];
    let mut missing = Vec::new();
    for file in copied {
        match file.bytes {
            FileBytes::Read(bytes) => files.push((under_root(&file.path), bytes)),
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

/// The paths that `html`'s `src` and `href` attributes name by a relative address, each once,
/// in the page's order.
fn relative_references(html: &str) -> Vec<String> {
    let mut references: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(|character: char| character.is_ascii_whitespace()) {
        rest = rest.get(at + 1..).unwrap_or_default();
        let Some((address, after)) = attribute_value(rest) else {
            continue;
        };
        rest = after;
        if let Some(path) = relative_path(address)
            && !references.contains(&path)
        {
            references.push(path);
        }
    }
    references
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
/// that names a folder or nothing.
fn relative_path(address: &str) -> Option<String> {
    let address = address.trim();
    let end = address.find(['?', '#']).unwrap_or(address.len());
    let path = address.get(..end)?;
    let from_root = path.starts_with(['/', '\\']);
    let folder = path.is_empty() || path.ends_with(['/', '\\']);
    if from_root || folder || has_scheme(path) {
        return None;
    }
    let decoded = unescaped(path)?;
    (!decoded.chars().any(char::is_control)).then_some(decoded)
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
    use cockpit_protocol::{Image, ImageMediaType};

    use super::*;
    use crate::daemon_label::DaemonLabel;

    fn agent() -> AgentId {
        AgentId::new(7)
    }

    fn state() -> AppState {
        AppState::new(DaemonLabel::ssh("build-box".to_owned()))
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
        })
    }

    fn names(path: &str) -> Vec<String> {
        path.split('/').map(str::to_owned).collect()
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
            relative_references(html),
            [
                "style.css",
                "shots/a b.png",
                "../shared/app.js",
                "next.html"
            ]
        );
    }

    #[test]
    fn a_broken_escape_or_a_control_character_names_no_file() {
        assert!(relative_references(r#" src="a%2.png" src="a%ZZ.png""#).is_empty());
        assert_eq!(
            relative_references(r#" src="a%0A.png" src="b.png""#),
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
                    data: "iVBO".to_owned(),
                },
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
                    (names("pages/index.html"), html.as_bytes().to_vec()),
                    (names("pages/shots/a.png"), vec![0x89, 0x50, 0x4e]),
                    (names("css/site.css"), b"body {}".to_vec()),
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

        assert!(state.page_copies.reading(page));
        assert!(!state.page_copies.reading("/srv/site/other.html"));
        assert!(take_ready(&mut state).is_empty());
        assert!(state.page_copies.reading(page));
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
                files: vec![(vec!["index.html".to_owned()], b"<p>Hi</p>".to_vec())],
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

    #[test]
    fn an_address_above_the_drive_stays_on_it() {
        let mut state = state();

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
