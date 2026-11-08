//! Past Claude sessions on this machine, for `sessions.list` and `sessions.resume`.
//!
//! Claude Code writes each session to `<claude folder>/projects/<project>/<session id>.jsonl`.
//! A session's title, folder and first prompt come from the head and the tail of its file, as
//! the Python Agent SDK reads them; the number of prompts needs the whole file.
//!
//! A session's transcript comes from the same lines as on `claude`'s stdout, through
//! [`PastTranscript`]. The file holds other lines too (titles, file history snapshots, API
//! requests): they read as lines of an unknown type, which show nothing.

use std::cmp::Reverse;
use std::fs::File;
use std::io;
use std::io::{BufRead, BufReader, Read, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use claude_protocol::{MessageOrigin, OutputLine, TypedCommand};
use cockpit_protocol::{
    AccountId, ClaudeSessionId, CostUsd, MAX_LINE_BYTES, MAX_TITLE_CHARS, PastSession, Timestamp,
    TranscriptItem,
};
use serde::Deserialize;
use serde_json::Value;
use strum::{IntoStaticStr, VariantArray};

use crate::read_file;
use crate::store::PastFile;
use crate::transcript::PastTranscript;

const PROJECTS_FOLDER: &str = "projects";
const SESSION_EXTENSION: &str = "jsonl";
/// The bytes read from each end of a file for its title and folder.
const CHUNK_BYTES: u64 = 64 * 1024;
/// The newest sessions listed.
const MAX_SESSIONS: usize = 100;

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
const ORIGIN_KEY: &str = "origin";
const COST_STATE_TYPE: &str = "cost-state";
const TOTAL_COST_KEY: &str = "totalCostUSD";
const TOOL_RESULT_MARK: &str = "\"tool_result\"";
const NEWLINE: u8 = b'\n';

/// The keys of the titles, in the order they win: one the user set, then one Claude wrote, then
/// the last prompt and the summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "camelCase")]
enum TitleKey {
    CustomTitle,
    AiTitle,
    LastPrompt,
    Summary,
}

/// An account's Claude Code folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountFolder {
    pub(crate) account: AccountId,
    pub(crate) folder: PathBuf,
}

/// A session file found in an account's folder.
struct SessionFile {
    modified: SystemTime,
    path: PathBuf,
    account: AccountId,
}

/// The newest sessions of all the accounts, newest first.
pub(crate) fn list(accounts: &[AccountFolder]) -> Vec<PastSession> {
    let mut files: Vec<SessionFile> = Vec::new();
    for account in accounts {
        add_session_files(account, &mut files);
    }
    files.sort_by_key(|file| Reverse(file.modified));
    let newest = files.into_iter().take(MAX_SESSIONS);
    newest
        .filter_map(|file| read_session(&file.path, file.account))
        .collect()
}

fn add_session_files(account: &AccountFolder, files: &mut Vec<SessionFile>) {
    let projects = account.folder.join(PROJECTS_FOLDER);
    let Ok(project_folders) = std::fs::read_dir(&projects) else {
        return;
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
                files.push(SessionFile {
                    modified,
                    path,
                    account: account.account,
                });
            }
        }
    }
}

/// A session `sessions.resume` takes over, and its transcript so far.
pub(crate) struct FoundSession {
    pub(crate) session: PastSession,
    pub(crate) past: SessionPast,
}

/// The transcript of a session file's first `file.bytes`.
pub(crate) struct SessionPast {
    pub(crate) items: Vec<TranscriptItem>,
    pub(crate) file: PastFile,
}

/// The session with this id, in any project of the account's folder, and the transcript of its
/// whole lines.
pub(crate) fn find(account: &AccountFolder, id: &ClaudeSessionId) -> Option<FoundSession> {
    let path = find_file(&account.folder, id)?;
    let session = read_session(&path, account.account)?;
    let past = read_past(&path, &session.folder, id, None);
    Some(FoundSession { session, past })
}

/// The transcript of the part of a session file an agent shows, read again by a restarted
/// daemon. It has no items when the file is gone.
pub(crate) fn past_items(
    claude_folder: &Path,
    folder: &Path,
    file: &PastFile,
) -> Vec<TranscriptItem> {
    let Some(path) = find_file(claude_folder, &file.session) else {
        return Vec::new();
    };
    read_past(&path, folder, &file.session, Some(file.bytes)).items
}

/// The transcript of the file's whole lines, within its first `limit` bytes when given. It has no
/// items when the file cannot be opened.
fn read_past(path: &Path, folder: &Path, id: &ClaudeSessionId, limit: Option<u64>) -> SessionPast {
    match read_file::open_to_read(path) {
        Ok(file) => past_within(file, limit, folder, id),
        Err(_) => past_within(io::empty(), limit, folder, id),
    }
}

/// The transcript of `file`'s whole lines, within its first `limit` bytes when given. A line that
/// does not read, or is longer than [`MAX_LINE_BYTES`], is skipped, and a read error ends the
/// transcript there.
fn past_within(
    file: impl Read,
    limit: Option<u64>,
    folder: &Path,
    id: &ClaudeSessionId,
) -> SessionPast {
    // The agent's own lines follow the limit: they are never read.
    let mut reader = BufReader::new(file.take(limit.unwrap_or(u64::MAX)));
    let mut transcript = PastTranscript::new(folder.to_owned());
    let mut bytes: u64 = 0;
    let mut line = Vec::new();
    loop {
        match next_line(&mut reader, &mut line, MAX_LINE_BYTES) {
            Ok(Some(SessionLine::Whole { bytes: read })) => {
                bytes = bytes.saturating_add(read);
                if let Some(shown) = past_line(&line) {
                    transcript.add(shown);
                }
            }
            Ok(Some(SessionLine::TooLong { bytes: read })) => bytes = bytes.saturating_add(read),
            // A line without its newline is still being written.
            Ok(Some(SessionLine::Unfinished) | None) | Err(_) => break,
        }
    }
    SessionPast {
        items: transcript.into_items(),
        file: PastFile {
            session: id.clone(),
            bytes,
        },
    }
}

/// A line of a session file, unless it is a subagent's, which the file does not tie to the call
/// that started the subagent.
///
/// A user line marked as meta, or as the summary after a compaction, is what `claude`'s stdout
/// marks synthetic: context Claude Code adds. The transcript shows it only where a skill's
/// instructions or a summary is due, or when another source than the user wrote it.
fn past_line(line: &[u8]) -> Option<OutputLine> {
    let value: Value = serde_json::from_slice(line).ok()?;
    let flag = |key| value.get(key) == Some(&Value::Bool(true));
    if flag(SIDECHAIN_KEY) {
        return None;
    }
    let synthetic = flag(META_KEY) || flag(COMPACT_SUMMARY_KEY);
    let mut line = OutputLine::try_from(value).ok()?;
    if synthetic && let OutputLine::User(message) = &mut line {
        message.is_synthetic = true;
    }
    Some(line)
}

/// The total cost Claude Code restores when it resumes the session: the one its last `cost-state`
/// line records, or 0 without one. `None` when the file cannot be found or read.
///
/// Claude Code writes a `cost-state` line when its process exits. A process killed before that
/// writes none, so the next `--resume` restores the last line's cost.
/// A session's folder as a user writes it. An agent of an older daemon ran in the verbatim form,
/// `\\?\C:\work`, which Claude Code recorded.
fn shown_folder(cwd: &str) -> PathBuf {
    #[cfg(windows)]
    return PathBuf::from(crate::windows_path::shown(cwd));
    #[cfg(not(windows))]
    PathBuf::from(cwd)
}

pub(crate) fn restored_cost(claude_folder: &Path, id: &ClaudeSessionId) -> Option<CostUsd> {
    let file = read_file::open_to_read(&find_file(claude_folder, id)?).ok()?;
    let mut reader = BufReader::new(file);
    let mut cost = 0.0;
    let mut line = Vec::new();
    loop {
        match next_line(&mut reader, &mut line, MAX_LINE_BYTES) {
            Ok(Some(SessionLine::Whole { .. } | SessionLine::Unfinished)) => {}
            Ok(Some(SessionLine::TooLong { .. })) => continue,
            Ok(None) => return Some(CostUsd::new(cost)),
            Err(_) => return None,
        }
        let text = String::from_utf8_lossy(&line);
        if !text.contains(COST_STATE_TYPE) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get(TYPE_KEY).and_then(Value::as_str) == Some(COST_STATE_TYPE)
            && let Some(total) = value.get(TOTAL_COST_KEY).and_then(Value::as_f64)
        {
            cost = total;
        }
    }
}

/// How [`next_line`] found a line of a session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLine {
    /// A line, with its newline, is in the buffer.
    Whole { bytes: u64 },
    /// The file ends before the line's newline, as when Claude Code is still writing it. The line
    /// is in the buffer when it is not too long.
    Unfinished,
    /// A line longer than the maximum was skipped, as `claude`'s output skips one: the buffer is
    /// empty.
    TooLong { bytes: u64 },
}

/// Reads the next line of a session file into `line`, keeping at most `max_bytes` of it, its
/// newline included. `None` at the end of the file.
fn next_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<Option<SessionLine>> {
    line.clear();
    let mut bytes: usize = 0;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            let end = (bytes > 0).then_some(SessionLine::Unfinished);
            return Ok(end);
        }
        let newline = available.iter().position(|&byte| byte == NEWLINE);
        let length = newline.map_or(available.len(), |end| end + 1);
        bytes = bytes.saturating_add(length);
        if bytes <= max_bytes {
            line.extend_from_slice(available.get(..length).unwrap_or_default());
        } else {
            line.clear();
        }
        reader.consume(length);
        if newline.is_some() {
            let read = u64::try_from(bytes).unwrap_or(u64::MAX);
            let found = if bytes <= max_bytes {
                SessionLine::Whole { bytes: read }
            } else {
                SessionLine::TooLong { bytes: read }
            };
            return Ok(Some(found));
        }
    }
}

fn find_file(claude_folder: &Path, id: &ClaudeSessionId) -> Option<PathBuf> {
    if !is_session_id(id.as_str()) {
        return None;
    }
    let file_name = format!("{}.{SESSION_EXTENSION}", id.as_str());
    let projects = std::fs::read_dir(claude_folder.join(PROJECTS_FOLDER)).ok()?;
    projects
        .flatten()
        .map(|project| project.path().join(&file_name))
        .find(|path| path.is_file())
}

fn read_session(path: &Path, account: AccountId) -> Option<PastSession> {
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
        .map(shown_folder)?;
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
        account,
    })
}

fn title(head: &[Value], tail: &[Value]) -> Option<String> {
    let last_string = |lines: &[Value], key: &str| {
        lines
            .iter()
            .rev()
            .find_map(|line| line.get(key)?.as_str().map(str::to_owned))
    };
    let named = TitleKey::VARIANTS.iter().find_map(|&key| {
        let key: &str = key.into();
        last_string(tail, key).or_else(|| last_string(head, key))
    });
    let title = named.or_else(|| head.iter().find_map(first_prompt))?;
    Some(shorten(&title))
}

/// The text of a prompt that makes a title: not a slash command.
fn first_prompt(line: &Value) -> Option<String> {
    prompt_text(line).filter(|text| TypedCommand::parse(text).is_none())
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
        && !written_by_claude_code(line)
}

/// A message Claude Code writes itself, such as at the end of a background task, has an origin
/// other than `human`.
fn written_by_claude_code(line: &Value) -> bool {
    line.get(ORIGIN_KEY)
        .and_then(|origin| MessageOrigin::deserialize(origin).ok())
        .is_some_and(|origin| !origin.is_human())
}

/// The user's prompts in the whole file, slash commands included. A tool result is a user line
/// too, so lines naming one are skipped before parsing, and so is a line longer than
/// [`MAX_LINE_BYTES`].
fn count_prompts(mut file: File) -> u32 {
    use std::io::Seek;

    if file.seek(SeekFrom::Start(0)).is_err() {
        return 0;
    }
    let mut count: u32 = 0;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        match next_line(&mut reader, &mut line, MAX_LINE_BYTES) {
            Ok(Some(SessionLine::Whole { .. } | SessionLine::Unfinished)) => {}
            Ok(Some(SessionLine::TooLong { .. })) => continue,
            Ok(None) | Err(_) => return count,
        }
        let text = String::from_utf8_lossy(&line);
        if !text.contains(TOOL_RESULT_MARK)
            && let Ok(value) = serde_json::from_str::<Value>(&text)
            && prompt_text(&value).is_some()
        {
            count = count.saturating_add(1);
        }
    }
}

fn read_chunk(file: &mut File, start: u64) -> Option<String> {
    use std::io::Seek;

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

/// The text, cut at [`MAX_TITLE_CHARS`].
pub(crate) fn shorten(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_TITLE_CHARS {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(MAX_TITLE_CHARS - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::time::Duration;

    use cockpit_protocol::ItemBody;
    #[cfg(unix)]
    use rustix::fs::Mode;
    use serde_json::json;

    use super::*;

    const FIRST: &str = "11111111-2222-3333-4444-555555555555";
    const SECOND: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    fn default_account(folder: &Path) -> AccountFolder {
        AccountFolder {
            account: AccountId::DEFAULT,
            folder: folder.to_owned(),
        }
    }

    fn write_session(claude: &Path, project: &str, id: &str, lines: &[Value]) {
        let folder = claude.join(PROJECTS_FOLDER).join(project);
        std::fs::create_dir_all(&folder).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(folder.join(format!("{id}.{SESSION_EXTENSION}")), text).unwrap();
    }

    fn prompt(text: &str) -> Value {
        json!({
            "type": "user", "cwd": "/srv/work/shop",
            "message": {"role": "user", "content": text}
        })
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

        let mut sessions = list(&[default_account(temp.path())]);
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

    /// Two accounts' folders list together, newest first, each session with its account.
    #[test]
    fn each_accounts_sessions_list_with_their_account() {
        let temp = tempfile::tempdir().unwrap();
        let personal = temp.path().join("claude");
        let work = temp.path().join("claude-work");
        write_session(
            &personal,
            "-srv-work-blog",
            FIRST,
            &[prompt("Draft a post")],
        );
        write_session(&work, "-srv-work-shop", SECOND, &[prompt("Fix the cart")]);
        let work_account = AccountFolder {
            account: AccountId::new(2),
            folder: work,
        };
        let older = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let first_file = personal.join(format!(
            "projects/-srv-work-blog/{FIRST}.{SESSION_EXTENSION}"
        ));
        OpenOptions::new()
            .write(true)
            .open(first_file)
            .unwrap()
            .set_modified(older)
            .unwrap();

        let sessions = list(&[default_account(&personal), work_account.clone()]);
        let found = find(&work_account, &session(SECOND)).unwrap();
        let elsewhere = find(&default_account(&personal), &session(SECOND));

        let listed: Vec<(&str, AccountId, &str)> = sessions
            .iter()
            .map(|session| (session.id.as_str(), session.account, session.title.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                (SECOND, AccountId::new(2), "Fix the cart"),
                (FIRST, AccountId::DEFAULT, "Draft a post"),
            ]
        );
        assert_eq!(found.session.account, AccountId::new(2));
        assert!(elsewhere.is_none());
    }

    #[test]
    fn a_message_claude_code_wrote_is_not_a_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let mut typed = prompt("Fix the cart total");
        typed["origin"] = json!({"kind": "human"});
        let mut notification = prompt(
            "<task-notification>\n<status>completed</status>\n\
             <summary>Background command \"make\" completed</summary>\n</task-notification>",
        );
        notification["origin"] = json!({"kind": "task-notification"});
        write_session(temp.path(), "p", FIRST, &[notification, typed]);

        let sessions = list(&[default_account(temp.path())]);

        let summary: Vec<(&str, u32)> = sessions
            .iter()
            .map(|session| (session.title.as_str(), session.turns))
            .collect();
        assert_eq!(summary, [("Fix the cart total", 1)]);
    }

    fn session(id: &str) -> ClaudeSessionId {
        ClaudeSessionId::new(id.to_owned())
    }

    fn texts(items: &[TranscriptItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match &item.body {
                ItemBody::UserMessage(message) => format!("user: {}", message.text),
                ItemBody::AssistantText { markdown } => format!("text: {markdown}"),
                ItemBody::Notice { text, .. } => format!("notice: {text}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_past_of_a_session_file_comes_out_in_order_without_what_it_does_not_show() {
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[
                json!({"type": "file-history-snapshot", "snapshot": {"files": []}}),
                prompt("Fix the cart total"),
                json!({"type": "user", "isMeta": true, "message": {
                    "role": "user", "content": "<system-reminder>Some context</system-reminder>"
                }}),
                json!({"type": "assistant", "message": {"content": [
                    {"type": "text", "text": "The total is fixed."}
                ]}}),
                json!({"type": "assistant", "isSidechain": true, "message": {"content": [
                    {"type": "text", "text": "A subagent's answer"}
                ]}}),
                json!({"type": "system", "subtype": "compact_boundary", "isMeta": false,
                       "compactMetadata": {"trigger": "auto"}}),
                json!({"type": "user", "isCompactSummary": true, "message": {
                    "role": "user", "content": "This session is being continued"
                }}),
                json!({"type": "user", "isMeta": true, "origin": {"kind": "peer"}, "message": {
                    "role": "user", "content": "Another session asks for the logs"
                }}),
                json!({"type": "ai-title", "aiTitle": "Cart total"}),
            ],
        );
        let path = temp
            .path()
            .join(PROJECTS_FOLDER)
            .join("p")
            .join(format!("{FIRST}.jsonl"));
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"type":"user","message":"#).unwrap();

        let found = find(&default_account(temp.path()), &session(FIRST)).unwrap();

        assert_eq!(
            texts(&found.past.items),
            [
                "user: Fix the cart total",
                "text: The total is fixed.",
                "notice: Conversation compacted",
                "notice: Conversation summary",
                "notice: peer: Another session asks for the logs",
            ]
        );
        let ItemBody::Notice { detail, .. } = &found.past.items[3].body else {
            panic!("expected the summary");
        };
        assert_eq!(detail.as_deref(), Some("This session is being continued"));
        let ids: Vec<u32> = found.past.items.iter().map(|item| item.id.get()).collect();
        assert_eq!(ids, [1997, 1998, 1999, 2000, 2001]);
        assert_eq!(
            found.past.file.bytes, whole,
            "the line being written is left out"
        );
        assert_eq!(found.past.file.session, session(FIRST));
    }

    #[test]
    fn a_past_skills_instructions_show_folded_and_other_meta_lines_stay_hidden() {
        let temp = tempfile::tempdir().unwrap();
        let instructions =
            "Base directory for this skill: /srv/skills/greet\n\n# Greet\n\nSay hello.";
        let meta = |text: &str| {
            json!({"type": "user", "isMeta": true, "message": {
                "role": "user", "content": [{"type": "text", "text": text}]
            }})
        };
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[
                prompt("/greet world"),
                json!({"type": "assistant", "message": {"content": [
                    {"type": "tool_use", "id": "t1", "name": "Skill", "input": {"skill": "greet"}}
                ]}}),
                json!({"type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "Launching skill: greet"}
                ]}, "toolUseResult": {"commandName": "greet", "success": true}}),
                meta(instructions),
                meta("<system-reminder>Some context</system-reminder>"),
            ],
        );

        let found = find(&default_account(temp.path()), &session(FIRST)).unwrap();

        let shown: Vec<&ItemBody> = found.past.items.iter().map(|item| &item.body).collect();
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert_eq!(
            shown[2],
            &ItemBody::Notice {
                text: "Skill instructions: /greet".to_owned(),
                detail: Some(instructions.to_owned()),
            }
        );
    }

    #[test]
    fn a_restarted_daemon_reads_the_same_past_however_the_file_grew() {
        let temp = tempfile::tempdir().unwrap();
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[prompt("Before"), prompt("Also before")],
        );
        let found = find(&default_account(temp.path()), &session(FIRST)).unwrap();
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[
                prompt("Before"),
                prompt("Also before"),
                prompt("The agent's own"),
            ],
        );

        let again = past_items(temp.path(), Path::new("/srv/work/shop"), &found.past.file);
        let gone = past_items(
            temp.path(),
            Path::new("/srv/work/shop"),
            &PastFile {
                session: session(SECOND),
                bytes: 10,
            },
        );

        assert_eq!(again, found.past.items);
        assert!(gone.is_empty());
    }

    #[test]
    fn a_long_session_file_shows_its_newest_items_under_a_notice() {
        let temp = tempfile::tempdir().unwrap();
        let prompts: Vec<Value> = (0..2_005)
            .map(|index| prompt(&format!("Prompt {index}")))
            .collect();
        write_session(temp.path(), "p", FIRST, &prompts);

        let found = find(&default_account(temp.path()), &session(FIRST)).unwrap();

        let items = &found.past.items;
        assert_eq!(items.len(), 2_001);
        assert!(
            matches!(items[0].body, ItemBody::Notice { .. }),
            "{:?}",
            items[0]
        );
        assert_eq!(texts(&items[1..2]), ["user: Prompt 5"]);
        assert_eq!(texts(&items[2_000..]), ["user: Prompt 2004"]);
    }

    #[test]
    fn a_line_reads_whole_or_is_skipped_when_too_long_or_unfinished() {
        let mut reader = BufReader::with_capacity(2, &b"ab\ncdefgh\nij"[..]);
        let mut line = Vec::new();
        let mut found = Vec::new();

        while let Some(read) = next_line(&mut reader, &mut line, 4).unwrap() {
            found.push((read, String::from_utf8_lossy(&line).into_owned()));
        }

        assert_eq!(
            found,
            [
                (SessionLine::Whole { bytes: 3 }, "ab\n".to_owned()),
                (SessionLine::TooLong { bytes: 7 }, String::new()),
                (SessionLine::Unfinished, "ij".to_owned()),
            ]
        );
    }

    #[test]
    fn a_line_longer_than_the_maximum_is_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let long = prompt(&"x".repeat(MAX_LINE_BYTES));
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[prompt("Before"), long, prompt("After")],
        );
        let path = temp
            .path()
            .join(PROJECTS_FOLDER)
            .join("p")
            .join(format!("{FIRST}.jsonl"));
        let whole = std::fs::metadata(&path).unwrap().len();

        let found = find(&default_account(temp.path()), &session(FIRST)).unwrap();

        assert_eq!(texts(&found.past.items), ["user: Before", "user: After"]);
        assert_eq!(found.past.file.bytes, whole);
        assert_eq!(found.session.turns, 2, "the long prompt is not counted");
    }

    /// Fails a test that reads it.
    struct Unreadable;

    impl Read for Unreadable {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("the past was read beyond its limit");
        }
    }

    #[test]
    fn a_restarted_daemon_reads_nothing_beyond_the_past() {
        let past = format!("{}\n", prompt("Before"));
        let limit = u64::try_from(past.len()).unwrap();
        let file = io::Cursor::new(past.into_bytes()).chain(Unreadable);

        let read = past_within(
            file,
            Some(limit),
            Path::new("/srv/work/shop"),
            &session(FIRST),
        );

        assert_eq!(texts(&read.items), ["user: Before"]);
        assert_eq!(read.file.bytes, limit);
    }

    #[test]
    fn the_restored_cost_is_the_last_cost_states() {
        let temp = tempfile::tempdir().unwrap();
        let cost_state = |total: f64| json!({"type": "cost-state", "sessionId": FIRST, "totalCostUSD": total, "modelUsage": {}});
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[
                prompt("Hello"),
                cost_state(0.25),
                prompt("Again"),
                cost_state(0.75),
                prompt("A turn of a process that was killed"),
            ],
        );
        write_session(temp.path(), "p", SECOND, &[prompt("Hello")]);

        assert_eq!(
            restored_cost(temp.path(), &session(FIRST)),
            Some(CostUsd::new(0.75))
        );
        assert_eq!(
            restored_cost(temp.path(), &session(SECOND)),
            Some(CostUsd::new(0.0))
        );
        assert_eq!(
            restored_cost(
                temp.path(),
                &session("00000000-0000-4000-8000-000000000009")
            ),
            None
        );
    }

    #[test]
    fn title_keys_are_pinned() {
        let keys: Vec<&str> = TitleKey::VARIANTS.iter().map(|&key| key.into()).collect();

        assert_eq!(keys, ["customTitle", "aiTitle", "lastPrompt", "summary"]);
    }

    #[test]
    fn finds_a_session_by_id_and_refuses_an_id_that_is_a_path() {
        let temp = tempfile::tempdir().unwrap();
        write_session(temp.path(), "-srv-work-shop", FIRST, &[prompt("Hello")]);

        let found = find(
            &default_account(temp.path()),
            &ClaudeSessionId::new(FIRST.to_owned()),
        );
        let missing = find(
            &default_account(temp.path()),
            &ClaudeSessionId::new(SECOND.to_owned()),
        );
        let path = find(
            &default_account(temp.path()),
            &ClaudeSessionId::new("../../etc/passwd".to_owned()),
        );

        assert_eq!(
            found.map(|found| found.session.title),
            Some("Hello".to_owned())
        );
        assert!(missing.is_none());
        assert!(path.is_none());
    }

    #[test]
    fn a_sidechain_or_a_session_without_a_folder_is_left_out() {
        let temp = tempfile::tempdir().unwrap();
        write_session(
            temp.path(),
            "p",
            FIRST,
            &[json!({
                "type": "user", "isSidechain": true, "cwd": "/srv",
                "message": {"content": "x"}
            })],
        );
        write_session(
            temp.path(),
            "p",
            SECOND,
            &[json!({"type": "user", "message": {"content": "no folder"}})],
        );

        assert!(list(&[default_account(temp.path())]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_named_as_a_session_is_skipped_at_once() {
        let temp = tempfile::tempdir().unwrap();
        write_session(temp.path(), "p", FIRST, &[prompt("Hello")]);
        let fifo = temp
            .path()
            .join(PROJECTS_FOLDER)
            .join("p")
            .join(format!("{SECOND}.{SESSION_EXTENSION}"));
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR).unwrap();
        let claude = temp.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();

        // Opening a FIFO waits for a writer: the listing runs in a thread the test can leave
        // behind.
        std::thread::spawn(move || {
            let _ = sender.send(list(&[default_account(&claude)]));
        });

        let sessions = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("a list without a writer on the FIFO");
        let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
        assert_eq!(ids, [FIRST]);
    }
}
