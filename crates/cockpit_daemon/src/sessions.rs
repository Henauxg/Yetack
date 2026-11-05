//! Past Claude sessions on this machine, for `sessions.list` and `sessions.resume`.
//!
//! Claude Code writes each session to `<claude folder>/projects/<project>/<session id>.jsonl`.
//! A session's title, folder and first prompt come from the head and the tail of its file, as
//! the Python Agent SDK reads them; the number of prompts needs the whole file.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cockpit_protocol::{ClaudeSessionId, PastSession, Timestamp};
use serde_json::Value;

use crate::read_file;

const PROJECTS_FOLDER: &str = "projects";
const SESSION_EXTENSION: &str = "jsonl";
/// The bytes read from each end of a file for its title and folder.
const CHUNK_BYTES: u64 = 64 * 1024;
/// The newest sessions listed.
const MAX_SESSIONS: usize = 100;
const TITLE_MAX_CHARS: usize = 200;

/// The keys of Claude Code's session lines that matter here.
const TYPE_KEY: &str = "type";
const USER_TYPE: &str = "user";
const MESSAGE_KEY: &str = "message";
const CONTENT_KEY: &str = "content";
const TEXT_KEY: &str = "text";
const TEXT_TYPE: &str = "text";
const CWD_KEY: &str = "cwd";
const SIDECHAIN_KEY: &str = "isSidechain";
const META_KEY: &str = "isMeta";
const COMPACT_SUMMARY_KEY: &str = "isCompactSummary";
const TOOL_RESULT_MARK: &str = "\"tool_result\"";
/// Titles in the order they win: one the user set, then one Claude wrote, then the last prompt
/// and the summary.
const TITLE_KEYS: [&str; 4] = ["customTitle", "aiTitle", "lastPrompt", "summary"];
/// How a slash command's prompt starts; such a prompt does not make a title.
const COMMAND_MARK: &str = "<command-name>";

/// The newest sessions, newest first.
pub(crate) fn list(claude_folder: &Path) -> Vec<PastSession> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    let projects = claude_folder.join(PROJECTS_FOLDER);
    let Ok(project_folders) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    for project in project_folders.flatten() {
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if session_id(&path).is_none() {
                continue;
            }
            if let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) {
                files.push((modified, path));
            }
        }
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let newest = files.into_iter().take(MAX_SESSIONS);
    newest.filter_map(|(_, path)| read_session(&path)).collect()
}

/// The session with this id, in any project.
pub(crate) fn find(claude_folder: &Path, id: &ClaudeSessionId) -> Option<PastSession> {
    if !is_session_id(id.as_str()) {
        return None;
    }
    let file_name = format!("{}.{SESSION_EXTENSION}", id.as_str());
    let projects = std::fs::read_dir(claude_folder.join(PROJECTS_FOLDER)).ok()?;
    projects
        .flatten()
        .map(|project| project.path().join(&file_name))
        .find(|path| path.is_file())
        .and_then(|path| read_session(&path))
}

fn read_session(path: &Path) -> Option<PastSession> {
    let id = session_id(path)?;
    let mut file = read_file::open_to_read(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let head = read_chunk(&mut file, 0)?;
    let tail_start = metadata.len().saturating_sub(CHUNK_BYTES);
    let tail = if tail_start == 0 {
        head.clone()
    } else {
        read_chunk(&mut file, tail_start)?
    };
    let head_lines = json_lines(&head);
    let tail_lines = json_lines(&tail);
    if head_lines
        .first()
        .is_some_and(|line| line.get(SIDECHAIN_KEY) == Some(&Value::Bool(true)))
    {
        return None;
    }
    let folder = head_lines
        .iter()
        .find_map(|line| line.get(CWD_KEY)?.as_str())
        .map(PathBuf::from)?;
    let title = title(&head_lines, &tail_lines)?;
    let last_activity = metadata
        .modified()
        .ok()
        .and_then(Timestamp::from_system_time)?;
    Some(PastSession {
        id: ClaudeSessionId::new(id),
        title,
        folder,
        last_activity,
        turns: count_prompts(file),
    })
}

fn title(head: &[Value], tail: &[Value]) -> Option<String> {
    let last_string = |lines: &[Value], key: &str| {
        lines
            .iter()
            .rev()
            .find_map(|line| line.get(key)?.as_str().map(str::to_owned))
    };
    let named = TITLE_KEYS
        .iter()
        .find_map(|key| last_string(tail, key).or_else(|| last_string(head, key)));
    let title = named.or_else(|| head.iter().find_map(first_prompt))?;
    Some(shorten(&title))
}

/// The text of a prompt that makes a title: not a slash command.
fn first_prompt(line: &Value) -> Option<String> {
    prompt_text(line).filter(|text| !text.starts_with(COMMAND_MARK))
}

/// The text of a user's prompt line; not a tool result or a meta line.
fn prompt_text(line: &Value) -> Option<String> {
    if !is_prompt(line) {
        return None;
    }
    let content = line.get(MESSAGE_KEY)?.get(CONTENT_KEY)?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get(TYPE_KEY).and_then(Value::as_str) == Some(TEXT_TYPE))
            .find_map(|block| block.get(TEXT_KEY)?.as_str().map(str::to_owned))?,
        _ => return None,
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn is_prompt(line: &Value) -> bool {
    let flag = |key| line.get(key) == Some(&Value::Bool(true));
    line.get(TYPE_KEY).and_then(Value::as_str) == Some(USER_TYPE)
        && !flag(META_KEY)
        && !flag(SIDECHAIN_KEY)
        && !flag(COMPACT_SUMMARY_KEY)
}

/// The user's prompts in the whole file, slash commands included. A tool result is a user line too, so lines naming one
/// are skipped before parsing.
fn count_prompts(mut file: File) -> u32 {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return 0;
    }
    let mut count: u32 = 0;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    while reader
        .read_until(b'\n', &mut line)
        .is_ok_and(|read| read > 0)
    {
        let text = String::from_utf8_lossy(&line);
        if !text.contains(TOOL_RESULT_MARK)
            && let Ok(value) = serde_json::from_str::<Value>(&text)
            && prompt_text(&value).is_some()
        {
            count = count.saturating_add(1);
        }
        line.clear();
    }
    count
}

fn read_chunk(file: &mut File, start: u64) -> Option<String> {
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(CHUNK_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The lines of a chunk that parse. A chunk cut in a line loses that line.
fn json_lines(chunk: &str) -> Vec<Value> {
    chunk
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn session_id(path: &Path) -> Option<String> {
    if path.extension()?.to_str()? != SESSION_EXTENSION {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    is_session_id(stem).then(|| stem.to_owned())
}

/// A UUID, as Claude Code names sessions. It also keeps a client's id from naming another path.
fn is_session_id(text: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == GROUPS.len()
        && groups.iter().zip(GROUPS).all(|(group, length)| {
            group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

fn shorten(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= TITLE_MAX_CHARS {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(TITLE_MAX_CHARS - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FIRST: &str = "11111111-2222-3333-4444-555555555555";
    const SECOND: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    fn write_session(claude: &Path, project: &str, id: &str, lines: &[Value]) {
        let folder = claude.join(PROJECTS_FOLDER).join(project);
        std::fs::create_dir_all(&folder).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(folder.join(format!("{id}.{SESSION_EXTENSION}")), text).unwrap();
    }

    fn prompt(text: &str) -> Value {
        json!({"type": "user", "cwd": "/srv/work/shop", "message": {"role": "user", "content": text}})
    }

    #[test]
    fn lists_sessions_with_their_title_folder_and_prompts() {
        let temp = tempfile::tempdir().unwrap();
        write_session(
            temp.path(),
            "-srv-work-shop",
            FIRST,
            &[
                json!({"type": "permission-mode"}),
                prompt("<command-name>/init</command-name>"),
                prompt("Fix the cart total\nplease"),
                json!({"type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
                ]}}),
                prompt("Now add a test"),
            ],
        );
        write_session(
            temp.path(),
            "-srv-work-blog",
            SECOND,
            &[
                prompt("Draft a post"),
                json!({"type": "custom-title", "customTitle": "Blog post"}),
            ],
        );
        write_session(
            temp.path(),
            "-srv-work-blog",
            "not-a-session",
            &[prompt("x")],
        );

        let mut sessions = list(temp.path());
        sessions.sort_by(|left, right| left.title.cmp(&right.title));

        let summary: Vec<(&str, &str, u32)> = sessions
            .iter()
            .map(|session| (session.id.as_str(), session.title.as_str(), session.turns))
            .collect();
        assert_eq!(
            summary,
            [
                (SECOND, "Blog post", 1),
                (FIRST, "Fix the cart total please", 3)
            ]
        );
        assert_eq!(sessions[1].folder, PathBuf::from("/srv/work/shop"));
    }

    #[test]
    fn finds_a_session_by_id_and_refuses_an_id_that_is_a_path() {
        let temp = tempfile::tempdir().unwrap();
        write_session(temp.path(), "-srv-work-shop", FIRST, &[prompt("Hello")]);

        let found = find(temp.path(), &ClaudeSessionId::new(FIRST.to_owned()));
        let missing = find(temp.path(), &ClaudeSessionId::new(SECOND.to_owned()));
        let path = find(
            temp.path(),
            &ClaudeSessionId::new("../../etc/passwd".to_owned()),
        );

        assert_eq!(found.map(|session| session.title), Some("Hello".to_owned()));
        assert_eq!(missing, None);
        assert_eq!(path, None);
    }

    #[test]
    fn a_sidechain_or_a_session_without_a_folder_is_left_out() {
        let temp = tempfile::tempdir().unwrap();
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[
                json!({"type": "user", "isSidechain": true, "cwd": "/srv", "message": {"content": "x"}}),
            ],
        );
        write_session(
            temp.path(),
            "p",
            SECOND,
            &[json!({"type": "user", "message": {"content": "no folder"}})],
        );

        assert!(list(temp.path()).is_empty());
    }

    #[test]
    fn a_fifo_named_as_a_session_is_skipped_at_once() {
        let temp = tempfile::tempdir().unwrap();
        write_session(temp.path(), "p", FIRST, &[prompt("Hello")]);
        let fifo = temp
            .path()
            .join(PROJECTS_FOLDER)
            .join("p")
            .join(format!("{SECOND}.{SESSION_EXTENSION}"));
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).unwrap();
        let claude = temp.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();

        // Opening a FIFO waits for a writer: the listing runs in a thread the test can leave
        // behind.
        std::thread::spawn(move || {
            let _ = sender.send(list(&claude));
        });

        let sessions = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a list without a writer on the FIFO");
        let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
        assert_eq!(ids, [FIRST]);
    }
}
