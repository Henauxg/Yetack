//! The event logs: `events/<agent-id>.jsonl` for an agent's events, `events/daemon.jsonl` for the
//! others. Each line is the frame the daemon sent to its clients, so a replay copies lines as
//! they are.
//!
//! An agent's `DetailsChanged` events stay out of its log: each one replaces the last, and each
//! is tens of kilobytes. `details/<agent-id>.json` holds the latest one's frame instead, and a
//! replay sends it in its place.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::io;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use cockpit_protocol::{AgentDetails, AgentId, Event, EventRecord, Frame, Seq, ServerMessage};
use serde::Deserialize;

use crate::store::store_error::StoreError;
use crate::store::{atomic_file, state_dir};

const DAEMON_LOG: &str = "daemon";
const LOG_SUFFIX: &str = ".jsonl";
const DETAILS_SUFFIX: &str = ".json";
/// The temporary copy of a log that loses its details.
const COMPACTING_EXTENSION: &str = "compacting";
/// The event type of `DetailsChanged` in a frame.
const DETAILS_EVENT: &str = "details_changed";
const NEWLINE: u8 = b'\n';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LogName {
    Agent(AgentId),
    /// Events that outlive an agent or belong to none: removals, plan usage, settings.
    Daemon,
}

/// The open log files, and the folder of the agents' latest details. Only the hub writes them.
#[derive(Debug)]
pub(crate) struct EventLogs {
    folder: PathBuf,
    details: PathBuf,
    open: HashMap<LogName, File>,
}

/// An agent's latest details, as `details/<agent-id>.json` holds them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LatestDetails {
    pub(crate) seq: Seq,
    /// The `DetailsChanged` frame, with its `\n`, which a replay sends as it is.
    pub(crate) frame: Vec<u8>,
    pub(crate) details: AgentDetails,
}

/// An agent's latest `DetailsChanged` frame, which a replay sends in place of the older ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailsFrame {
    pub(crate) seq: Seq,
    /// With its `\n`.
    pub(crate) frame: Vec<u8>,
}

/// What a replay read found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplayRead {
    /// The frames, oldest first, each with its `\n`.
    Frames(Vec<Vec<u8>>),
    /// More than the byte budget: a snapshot costs less.
    TooLarge,
}

#[derive(Deserialize)]
struct SeqProbe {
    seq: Seq,
}

/// A log line's sequence number and event type, read without the rest of the event.
#[derive(Deserialize)]
struct KindProbe {
    seq: Seq,
    event: EventKind,
}

#[derive(Deserialize)]
struct EventKind {
    #[serde(rename = "type")]
    kind: String,
}

impl LogName {
    fn file_name(self) -> String {
        match self {
            Self::Agent(agent) => format!("{}{LOG_SUFFIX}", agent.get()),
            Self::Daemon => format!("{DAEMON_LOG}{LOG_SUFFIX}"),
        }
    }

    fn from_file_name(name: &str) -> Option<Self> {
        let stem = name.strip_suffix(LOG_SUFFIX)?;
        if stem == DAEMON_LOG {
            return Some(Self::Daemon);
        }
        stem.parse().ok().map(|raw| Self::Agent(AgentId::new(raw)))
    }
}

impl EventLogs {
    /// `folder` holds the logs, and `details` the agents' latest details.
    pub(crate) fn new(folder: PathBuf, details: PathBuf) -> Self {
        Self {
            folder,
            details,
            open: HashMap::new(),
        }
    }

    /// Replaces an agent's latest details with `frame`, its `DetailsChanged` event.
    pub(crate) fn save_details(&self, agent: AgentId, frame: &Frame) -> Result<(), StoreError> {
        let path = details_path(&self.details, agent);
        atomic_file::write_bytes_unsynced(&path, frame.as_bytes())
            .map_err(|source| StoreError::Write { path, source })
    }

    pub(crate) fn folder(&self) -> &Path {
        &self.folder
    }

    pub(crate) fn path(&self, log: LogName) -> PathBuf {
        self.folder.join(log.file_name())
    }

    /// Appends one frame, which ends with its `\n`.
    pub(crate) fn append(&mut self, log: LogName, frame: &Frame) -> Result<(), StoreError> {
        use std::io::Write;

        let path = self.path(log);
        let error = |source| StoreError::Write {
            path: path.clone(),
            source,
        };
        let file = match self.open.entry(log) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(open_for_append(&path).map_err(error)?),
        };
        let written = file.write_all(frame.as_bytes());
        if written.is_err() {
            // The next append opens the log again, which ends a line the failed write cut.
            self.open.remove(&log);
        }
        written.map_err(error)
    }

    /// Deletes a log, and an agent's latest details with it, as when the agent is removed.
    pub(crate) fn remove(&mut self, log: LogName) -> Result<(), StoreError> {
        self.open.remove(&log);
        remove_if_present(&self.path(log))?;
        if let LogName::Agent(agent) = log {
            remove_if_present(&details_path(&self.details, agent))?;
        }
        Ok(())
    }

    /// Every log in the folder.
    pub(crate) fn list(&self) -> Result<Vec<(LogName, PathBuf)>, StoreError> {
        list_logs(&self.folder)
    }
}

fn list_logs(folder: &Path) -> Result<Vec<(LogName, PathBuf)>, StoreError> {
    let error = |source| StoreError::Read {
        path: folder.to_owned(),
        source,
    };
    let mut logs = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        if let Some(log) = name.to_str().and_then(LogName::from_file_name) {
            logs.push((log, entry.path()));
        }
    }
    Ok(logs)
}

fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreError::Write {
            path: path.to_owned(),
            source,
        }),
    }
}

/// `details/<agent-id>.json`.
pub(crate) fn details_path(folder: &Path, agent: AgentId) -> PathBuf {
    folder.join(format!("{}{DETAILS_SUFFIX}", agent.get()))
}

/// Each agent's latest details in `folder`. A file that does not hold a `DetailsChanged` frame is
/// skipped.
pub(crate) fn read_latest_details(
    folder: &Path,
) -> Result<Vec<(AgentId, LatestDetails)>, StoreError> {
    let error = |source| StoreError::Read {
        path: folder.to_owned(),
        source,
    };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(error(source)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        let Some(agent) = name
            .to_str()
            .and_then(|name| name.strip_suffix(DETAILS_SUFFIX))
            .and_then(|stem| stem.parse().ok())
            .map(AgentId::new)
        else {
            continue;
        };
        let path = entry.path();
        let frame = std::fs::read(&path).map_err(|source| StoreError::Read {
            path: path.clone(),
            source,
        })?;
        match latest_details(frame) {
            Some(latest) => found.push((agent, latest)),
            None => tracing::warn!(path = %path.display(), "skipped details that do not decode"),
        }
    }
    Ok(found)
}

/// The details a `DetailsChanged` frame holds.
fn latest_details(frame: Vec<u8>) -> Option<LatestDetails> {
    let Ok(ServerMessage::Event(EventRecord {
        seq,
        event: Event::DetailsChanged { details, .. },
    })) = serde_json::from_slice(&frame)
    else {
        return None;
    };
    let mut frame = frame;
    if frame.last() != Some(&NEWLINE) {
        frame.push(NEWLINE);
    }
    Some(LatestDetails {
        seq,
        frame,
        details,
    })
}

/// Takes the `DetailsChanged` events out of an agent's log, as a daemon logged them before the
/// details file. The last one becomes the details file at `details`, unless that file holds later
/// details. A log without details stays as it is.
pub(crate) fn move_details_out(log: &Path, details: &Path) -> Result<(), StoreError> {
    use std::io::Write;

    let compacting = log.with_extension(COMPACTING_EXTENSION);
    let write_error = |source| StoreError::Write {
        path: compacting.clone(),
        source,
    };
    let mut kept = BufWriter::new(
        state_dir::owner_only()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&compacting)
            .map_err(write_error)?,
    );
    let mut last: Option<(Seq, Vec<u8>)> = None;
    let mut written = Ok(());
    for_each_line(log, |line| {
        match serde_json::from_slice::<KindProbe>(line) {
            Ok(probe) if probe.event.kind == DETAILS_EVENT => {
                last = Some((probe.seq, line.to_vec()));
            }
            Ok(_) | Err(_) => {
                written = kept
                    .write_all(line)
                    .and_then(|()| kept.write_all(&[NEWLINE]));
            }
        }
        written.is_ok()
    })?;
    written.map_err(write_error)?;
    let Some((seq, frame)) = last else {
        drop(kept);
        return remove_if_present(&compacting);
    };
    let file = kept
        .into_inner()
        .map_err(|error| write_error(error.into_error()))?;
    file.sync_all().map_err(write_error)?;
    let newer_saved = std::fs::read(details)
        .ok()
        .and_then(latest_details)
        .is_some_and(|saved| saved.seq >= seq);
    if !newer_saved {
        let mut frame = frame;
        frame.push(NEWLINE);
        atomic_file::write_bytes_unsynced(details, &frame).map_err(|source| StoreError::Write {
            path: details.to_owned(),
            source,
        })?;
    }
    std::fs::rename(&compacting, log).map_err(|source| StoreError::Write {
        path: log.to_owned(),
        source,
    })
}

/// Opens a log for appending. A last line cut by a crash gets its `\n`, so the next line starts
/// on a line of its own.
fn open_for_append(path: &Path) -> io::Result<File> {
    use std::io::Write;

    let mut file = state_dir::owner_only()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    let length = file.metadata()?.len();
    if let Some(last) = length.checked_sub(1) {
        let mut byte = [0];
        read_exact_at(&file, &mut byte, last)?;
        if byte != [NEWLINE] {
            file.write_all(&[NEWLINE])?;
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;

    file.read_exact_at(buffer, offset)
}

/// Moves the file's cursor, which an append ignores. A file on disk reads short only at its end.
#[cfg(windows)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;

    if file.seek_read(buffer, offset)? < buffer.len() {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// Calls `visit` with each event of one log, oldest first, as it reads them: a log never sits in
/// memory whole. A line that does not decode is skipped.
pub(crate) fn for_each_event(
    path: &Path,
    mut visit: impl FnMut(EventRecord),
) -> Result<(), StoreError> {
    for_each_line(path, |line| {
        match serde_json::from_slice::<ServerMessage>(line) {
            Ok(ServerMessage::Event(record)) => visit(record),
            Ok(_) | Err(_) => {
                tracing::warn!(path = %path.display(), "skipped a log line that is not an event")
            }
        }
        true
    })
}

/// The frames of every event after `after`, up to `upto`, from all the logs in `folder`, with the
/// agents' latest `details` frames in their places.
pub(crate) fn read_replay(
    folder: &Path,
    after: Seq,
    upto: Option<Seq>,
    details: Vec<DetailsFrame>,
    max_bytes: usize,
) -> Result<ReplayRead, StoreError> {
    let Some(upto) = upto else {
        return Ok(ReplayRead::Frames(Vec::new()));
    };
    let mut frames: Vec<(Seq, Vec<u8>)> = details
        .into_iter()
        .filter(|details| details.seq > after && details.seq <= upto)
        .map(|details| (details.seq, details.frame))
        .collect();
    let mut total_bytes: usize = frames.iter().map(|(_, frame)| frame.len()).sum();
    if total_bytes > max_bytes {
        return Ok(ReplayRead::TooLarge);
    }
    for (_, path) in list_logs(folder)? {
        let mut too_large = false;
        for_each_line(&path, |line| {
            let Ok(SeqProbe { seq }) = serde_json::from_slice(line) else {
                return true;
            };
            if seq <= after || seq > upto {
                return true;
            }
            total_bytes = total_bytes.saturating_add(line.len() + 1);
            if total_bytes > max_bytes {
                too_large = true;
                return false;
            }
            let mut frame = line.to_vec();
            frame.push(NEWLINE);
            frames.push((seq, frame));
            true
        })?;
        if too_large {
            return Ok(ReplayRead::TooLarge);
        }
    }
    frames.sort_by_key(|(seq, _)| *seq);
    Ok(ReplayRead::Frames(
        frames.into_iter().map(|(_, frame)| frame).collect(),
    ))
}

/// Calls `visit` with each line, without its `\n`, until it returns `false`. A missing file has
/// no lines.
fn for_each_line(path: &Path, mut visit: impl FnMut(&[u8]) -> bool) -> Result<(), StoreError> {
    use std::io::BufRead;

    let error = |source| StoreError::Read {
        path: path.to_owned(),
        source,
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(error(source)),
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(NEWLINE, &mut line).map_err(error)? == 0 {
            return Ok(());
        }
        let content = line.strip_suffix(&[NEWLINE]).unwrap_or(&line);
        if !content.is_empty() && !visit(content) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use cockpit_protocol::{ChangedFile, Event, PlanUsage, Settings};

    use super::*;

    fn frame(seq: u64, event: Event) -> Frame {
        Frame::encode(&ServerMessage::Event(EventRecord {
            seq: Seq::new(seq),
            event,
        }))
        .unwrap()
    }

    fn removed(agent: u32) -> Event {
        Event::AgentRemoved {
            agent: AgentId::new(agent),
        }
    }

    fn read_events(path: &Path) -> Vec<EventRecord> {
        let mut records = Vec::new();
        for_each_event(path, |record| records.push(record)).unwrap();
        records
    }

    fn seqs(path: &Path) -> Vec<u64> {
        read_events(path)
            .iter()
            .map(|record| record.seq.get())
            .collect()
    }

    #[test]
    fn file_names_round_trip() {
        for log in [LogName::Daemon, LogName::Agent(AgentId::new(12))] {
            assert_eq!(LogName::from_file_name(&log.file_name()), Some(log));
        }
        assert_eq!(LogName::from_file_name("12.json"), None);
        assert_eq!(LogName::from_file_name("notes.jsonl"), None);
    }

    #[test]
    fn appends_and_reads_back() {
        let temp = tempfile::tempdir().unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        let log = LogName::Agent(AgentId::new(1));

        logs.append(log, &frame(1, removed(7))).unwrap();
        logs.append(log, &frame(2, removed(8))).unwrap();

        let records = read_events(&logs.path(log));
        let seqs: Vec<u64> = records.iter().map(|record| record.seq.get()).collect();
        assert_eq!(seqs, [1, 2]);
        assert_eq!(records[1].event, removed(8));
    }

    /// A restore applies each event as its line is read, before the rest of the log is there.
    #[cfg(unix)]
    #[test]
    fn events_reach_the_visitor_as_their_lines_are_read() {
        use std::io::Write;
        use std::sync::mpsc;
        use std::time::Duration;

        use rustix::fs::Mode;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(LogName::Daemon.file_name());
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, Mode::RUSR | Mode::WUSR).unwrap();
        let (sender, seen) = mpsc::channel();
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            for_each_event(&reader_path, |record| {
                let _ = sender.send(record.seq.get());
            })
        });

        // Opening a FIFO waits for its other end.
        let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
        writer.write_all(frame(1, removed(7)).as_bytes()).unwrap();
        let first = seen.recv_timeout(Duration::from_secs(5));
        writer.write_all(frame(2, removed(8)).as_bytes()).unwrap();
        drop(writer);

        assert_eq!(first, Ok(1), "the first event comes before the log ends");
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)), Ok(2));
        assert!(reader.join().unwrap().is_ok());
    }

    #[test]
    fn a_line_cut_by_a_crash_is_skipped_and_the_next_one_starts_fresh() {
        let temp = tempfile::tempdir().unwrap();
        let log = LogName::Daemon;
        let path = temp.path().join(log.file_name());
        let mut first = frame(1, removed(7)).as_bytes().to_vec();
        first.extend_from_slice(b"{\"type\":\"event\",\"seq\":2,\"ev");
        std::fs::write(&path, first).unwrap();

        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        logs.append(log, &frame(3, removed(9))).unwrap();

        assert_eq!(seqs(&path), [1, 3]);
    }

    /// A write that fails, as on a full disk, may leave part of its line in the file.
    #[test]
    fn the_append_after_a_failed_one_starts_on_a_line_of_its_own() {
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        let log = LogName::Daemon;
        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        logs.append(log, &frame(1, removed(7))).unwrap();
        let path = logs.path(log);
        let mut cut = OpenOptions::new().append(true).open(&path).unwrap();
        cut.write_all(b"{\"type\":\"event\",\"seq\":2,\"ev")
            .unwrap();
        // A handle that cannot write stands for the one whose write failed.
        logs.open.insert(log, File::open(&path).unwrap());

        let failed = logs.append(log, &frame(2, removed(8)));
        logs.append(log, &frame(3, removed(9))).unwrap();

        assert!(failed.is_err());
        assert_eq!(seqs(&path), [1, 3]);
    }

    #[test]
    fn a_replay_merges_the_logs_in_seq_order_within_the_range() {
        let temp = tempfile::tempdir().unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        let agent = LogName::Agent(AgentId::new(1));
        logs.append(agent, &frame(1, removed(1))).unwrap();
        logs.append(
            LogName::Daemon,
            &frame(2, Event::SettingsChanged(Settings::default())),
        )
        .unwrap();
        logs.append(agent, &frame(3, removed(3))).unwrap();
        logs.append(
            LogName::Daemon,
            &frame(4, Event::PlanUsageChanged(PlanUsage::default())),
        )
        .unwrap();
        logs.append(agent, &frame(5, removed(5))).unwrap();

        let read = read_replay(
            temp.path(),
            Seq::new(1),
            Some(Seq::new(4)),
            Vec::new(),
            1 << 20,
        )
        .unwrap();

        let ReplayRead::Frames(frames) = read else {
            panic!("expected frames, got {read:?}");
        };
        let seqs: Vec<u64> = frames
            .iter()
            .map(|frame| serde_json::from_slice::<SeqProbe>(frame).unwrap().seq.get())
            .collect();
        assert_eq!(seqs, [2, 3, 4]);
        assert!(frames.iter().all(|frame| frame.ends_with(b"\n")));
    }

    #[test]
    fn a_replay_over_the_budget_is_too_large() {
        let temp = tempfile::tempdir().unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        for seq in 1..=10 {
            logs.append(LogName::Daemon, &frame(seq, removed(1)))
                .unwrap();
        }

        let read = read_replay(
            temp.path(),
            Seq::new(0),
            Some(Seq::new(10)),
            Vec::new(),
            100,
        )
        .unwrap();

        assert_eq!(read, ReplayRead::TooLarge);
    }

    #[test]
    fn removing_a_log_deletes_its_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        let log = LogName::Agent(AgentId::new(4));
        logs.append(log, &frame(1, removed(1))).unwrap();

        logs.remove(log).unwrap();

        assert!(!logs.path(log).exists());
        assert!(logs.list().unwrap().is_empty());
    }

    fn details_changed(agent: u32, files: u32) -> Event {
        Event::DetailsChanged {
            agent: AgentId::new(agent),
            details: AgentDetails {
                changed_files: (0..files)
                    .map(|index| ChangedFile {
                        path: PathBuf::from(format!("src/file_{index}.rs")),
                        lines_added: 1,
                        lines_removed: 0,
                    })
                    .collect(),
                ..AgentDetails::default()
            },
        }
    }

    fn seq_of(frame: &[u8]) -> u64 {
        serde_json::from_slice::<SeqProbe>(frame).unwrap().seq.get()
    }

    #[test]
    fn moving_the_details_out_keeps_the_other_lines_and_the_last_details() {
        let temp = tempfile::tempdir().unwrap();
        let details = temp.path().join("details");
        std::fs::create_dir(&details).unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), details.clone());
        let log = LogName::Agent(AgentId::new(1));
        logs.append(log, &frame(1, removed(7))).unwrap();
        logs.append(log, &frame(2, details_changed(1, 1))).unwrap();
        logs.append(log, &frame(3, removed(8))).unwrap();
        logs.append(log, &frame(4, details_changed(1, 2))).unwrap();
        let saved = details_path(&details, AgentId::new(1));

        move_details_out(&logs.path(log), &saved).unwrap();

        assert_eq!(seqs(&logs.path(log)), [1, 3]);
        let latest = read_latest_details(&details).unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].1.seq, Seq::new(4));
        assert_eq!(latest[0].1.details.changed_files.len(), 2);
        // A second start finds nothing to move.
        move_details_out(&logs.path(log), &saved).unwrap();
        assert_eq!(seqs(&logs.path(log)), [1, 3]);
    }

    #[test]
    fn a_details_file_later_than_the_log_stays() {
        let temp = tempfile::tempdir().unwrap();
        let details = temp.path().join("details");
        std::fs::create_dir(&details).unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), details.clone());
        let log = LogName::Agent(AgentId::new(1));
        logs.append(log, &frame(4, details_changed(1, 1))).unwrap();
        logs.save_details(AgentId::new(1), &frame(9, details_changed(1, 3)))
            .unwrap();

        move_details_out(&logs.path(log), &details_path(&details, AgentId::new(1))).unwrap();

        let latest = read_latest_details(&details).unwrap();
        assert_eq!(latest[0].1.seq, Seq::new(9));
        assert!(seqs(&logs.path(log)).is_empty());
    }

    #[test]
    fn a_replay_puts_the_latest_details_in_their_place() {
        let temp = tempfile::tempdir().unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), temp.path().join("details"));
        let agent = LogName::Agent(AgentId::new(1));
        logs.append(agent, &frame(1, removed(1))).unwrap();
        logs.append(agent, &frame(3, removed(3))).unwrap();
        let details = vec![
            DetailsFrame {
                seq: Seq::new(2),
                frame: frame(2, details_changed(1, 1)).as_bytes().to_vec(),
            },
            DetailsFrame {
                seq: Seq::new(1),
                frame: frame(1, details_changed(2, 1)).as_bytes().to_vec(),
            },
        ];

        let read = read_replay(
            temp.path(),
            Seq::new(0),
            Some(Seq::new(3)),
            details,
            1 << 20,
        )
        .unwrap();

        let ReplayRead::Frames(frames) = read else {
            panic!("expected frames, got {read:?}");
        };
        let seqs: Vec<u64> = frames.iter().map(|frame| seq_of(frame)).collect();
        assert_eq!(seqs, [1, 1, 2, 3]);
    }

    #[test]
    fn removing_an_agents_log_deletes_its_details() {
        let temp = tempfile::tempdir().unwrap();
        let details = temp.path().join("details");
        std::fs::create_dir(&details).unwrap();
        let mut logs = EventLogs::new(temp.path().to_owned(), details.clone());
        logs.save_details(AgentId::new(4), &frame(1, details_changed(4, 1)))
            .unwrap();

        logs.remove(LogName::Agent(AgentId::new(4))).unwrap();

        assert!(read_latest_details(&details).unwrap().is_empty());
    }
}
