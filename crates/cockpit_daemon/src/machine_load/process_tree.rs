//! An agent's processes: its `claude` process and every process under it, found by walking down
//! from `claude` rather than by reading every process of the machine.
//!
//! The kernel lists each thread's children in `/proc/<pid>/task/<tid>/children` (Linux 3.5 and
//! later, with `CONFIG_PROC_CHILDREN`). A process is the child of the thread that started it, so a
//! process with several threads has one list per thread. Where those files do not exist,
//! [`FullScan`] reads every process's parent instead, at a cost that grows with the machine's
//! process count.

use std::collections::{HashMap, HashSet};
use std::fmt::{Arguments, Write};
use std::fs::File;
use std::io;
use std::path::Path;

use cockpit_protocol::ProcessId;
use rustix::process::Resource;

use crate::machine_load::kept_file;
use crate::machine_load::proc_files::{
    self, CHILDREN_FILE, PROC_ROOT, ProcessStat, STAT_FILE, TASK_FOLDER,
};

/// Where the walk reads a process and its children.
pub(super) trait ProcessSource {
    /// `None` for a process that ended, or whose `stat` does not parse.
    fn stat(&mut self, pid: ProcessId) -> Option<ProcessStat>;

    /// Appends the children of `stat`'s process to `children`.
    fn children(&mut self, stat: &ProcessStat, children: &mut Vec<ProcessId>);
}

/// The processes under `root`, `root` first, each read once. A process already in `seen`, which
/// another agent's walk took, is left out, and so is everything past the first `limit`
/// processes. A child whose `stat` names another parent ended, and its pid was reused, between
/// the two reads: it is left out too.
pub(super) fn walk(
    source: &mut impl ProcessSource,
    root: ProcessId,
    limit: usize,
    seen: &mut HashSet<ProcessId>,
) -> Vec<ProcessStat> {
    let mut found = Vec::new();
    let Some(root_stat) = source.stat(root) else {
        return found;
    };
    if !seen.insert(root) {
        return found;
    }
    let mut pending = vec![root_stat];
    let mut children = Vec::new();
    while let Some(stat) = pending.pop() {
        if found.len() >= limit {
            break;
        }
        children.clear();
        source.children(&stat, &mut children);
        for &child in &children {
            if seen.contains(&child) {
                continue;
            }
            let Some(child_stat) = source.stat(child) else {
                continue;
            };
            if child_stat.parent != stat.key.pid {
                continue;
            }
            seen.insert(child);
            pending.push(child_stat);
        }
        found.push(stat);
    }
    found
}

/// Whether this kernel has the children files, checked on the daemon's own main thread.
pub(super) fn children_files_exist() -> bool {
    let own = std::process::id().to_string();
    Path::new(PROC_ROOT)
        .join(&own)
        .join(TASK_FOLDER)
        .join(&own)
        .join(CHILDREN_FILE)
        .exists()
}

/// Thread lists are read again after this many readings even when the thread count stays the
/// same: a thread may end while another starts.
const RELIST_AFTER: u32 = 10;
/// The most files kept open, and at most a quarter of the daemon's limit.
const MAX_KEPT_FILES: usize = 4096;

/// Reads processes through files kept open from one reading to the next: a process's `stat`, and
/// each of its threads' children lists. The files of a process the last reading did not reach are
/// closed by [`ChildrenFiles::sweep`].
#[derive(Debug)]
pub(super) struct ChildrenFiles {
    processes: HashMap<ProcessId, KeptProcess>,
    /// The processes read since the last sweep.
    reached: HashSet<ProcessId>,
    /// Open files, all processes together.
    kept: usize,
    budget: usize,
    buffer: Vec<u8>,
    path: String,
}

#[derive(Debug)]
struct KeptProcess {
    stat: File,
    threads: Option<KeptThreads>,
}

#[derive(Debug)]
struct KeptThreads {
    /// The thread count of the `stat` they were listed for.
    count: u32,
    /// Readings since they were listed.
    age: u32,
    /// Each thread's children list, by thread id.
    children: Vec<(u32, File)>,
}

impl Default for ChildrenFiles {
    fn default() -> Self {
        let limit = rustix::process::getrlimit(Resource::Nofile).current;
        let quarter = limit.map_or(MAX_KEPT_FILES, |limit| {
            usize::try_from(limit / 4).unwrap_or(MAX_KEPT_FILES)
        });
        Self::with_budget(quarter.min(MAX_KEPT_FILES))
    }
}

impl ChildrenFiles {
    pub(super) fn with_budget(budget: usize) -> Self {
        Self {
            processes: HashMap::new(),
            reached: HashSet::new(),
            kept: 0,
            budget,
            buffer: Vec::new(),
            path: String::new(),
        }
    }

    /// Closes the files of the processes not read since the last sweep.
    pub(super) fn sweep(&mut self) {
        let reached = std::mem::take(&mut self.reached);
        self.processes.retain(|pid, _| reached.contains(pid));
        for process in self.processes.values_mut() {
            if let Some(threads) = &mut process.threads {
                threads.age = threads.age.saturating_add(1);
            }
        }
        self.kept = self.processes.values().map(KeptProcess::files).sum();
    }

    fn open_path(&mut self) -> Option<File> {
        File::open(&self.path).ok()
    }

    /// Opens the file at `self.path` and reads it into the buffer. Keeps it open when the budget
    /// allows.
    fn open_and_read(&mut self) -> Option<Option<File>> {
        let file = self.open_path()?;
        kept_file::read_again(&file, &mut self.buffer).ok()?;
        if self.kept < self.budget {
            self.kept += 1;
            Some(Some(file))
        } else {
            Some(None)
        }
    }

    fn set_path(&mut self, pid: ProcessId, rest: Arguments<'_>) {
        self.path.clear();
        let _ = write!(self.path, "{PROC_ROOT}/{}/{rest}", pid.get());
    }

    /// The thread ids of a process, `[pid]` for a process of one thread.
    fn threads(pid: ProcessId, count: u32) -> Vec<u32> {
        if count <= 1 {
            return vec![pid.get()];
        }
        let folder = Path::new(PROC_ROOT)
            .join(pid.get().to_string())
            .join(TASK_FOLDER);
        let Ok(entries) = std::fs::read_dir(folder) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect()
    }

    /// Reads the kept children lists of `pid`. `false` when there are none to trust: never
    /// listed, listed for another thread count, too old, or one of them fails.
    fn read_kept_children(
        &mut self,
        pid: ProcessId,
        count: u32,
        children: &mut Vec<ProcessId>,
    ) -> bool {
        let Some(threads) = self
            .processes
            .get(&pid)
            .and_then(|process| process.threads.as_ref())
        else {
            return false;
        };
        if threads.count != count || threads.age >= RELIST_AFTER {
            return false;
        }
        let before = children.len();
        for (_, file) in &threads.children {
            if kept_file::read_again(file, &mut self.buffer).is_err() {
                children.truncate(before);
                return false;
            }
            if let Ok(text) = std::str::from_utf8(&self.buffer) {
                proc_files::parse_children(text, children);
            }
        }
        true
    }
}

impl KeptProcess {
    fn files(&self) -> usize {
        1 + self
            .threads
            .as_ref()
            .map_or(0, |threads| threads.children.len())
    }
}

impl ProcessSource for ChildrenFiles {
    fn stat(&mut self, pid: ProcessId) -> Option<ProcessStat> {
        self.reached.insert(pid);
        if let Some(process) = self.processes.get(&pid) {
            if kept_file::read_again(&process.stat, &mut self.buffer).is_ok()
                && let Some(stat) = ProcessStat::parse(&String::from_utf8_lossy(&self.buffer))
            {
                return Some(stat);
            }
            // The process ended. Another one may have its pid now.
            if let Some(ended) = self.processes.remove(&pid) {
                self.kept = self.kept.saturating_sub(ended.files());
            }
        }
        self.set_path(pid, format_args!("{STAT_FILE}"));
        let kept = self.open_and_read()?;
        let stat = ProcessStat::parse(&String::from_utf8_lossy(&self.buffer))?;
        if let Some(file) = kept {
            self.processes.insert(
                pid,
                KeptProcess {
                    stat: file,
                    threads: None,
                },
            );
        }
        Some(stat)
    }

    /// Lists the threads again when the kept lists cannot be trusted. A thread already listed
    /// keeps its file: only a new thread's list is opened.
    fn children(&mut self, stat: &ProcessStat, children: &mut Vec<ProcessId>) {
        let pid = stat.key.pid;
        if self.read_kept_children(pid, stat.threads, children) {
            return;
        }
        let mut earlier: HashMap<u32, File> = self
            .processes
            .get_mut(&pid)
            .and_then(|process| process.threads.take())
            .map(|threads| threads.children.into_iter().collect())
            .unwrap_or_default();
        self.kept = self.kept.saturating_sub(earlier.len());
        let mut kept = Vec::new();
        for thread in Self::threads(pid, stat.threads) {
            let file = match earlier.remove(&thread) {
                Some(file) if kept_file::read_again(&file, &mut self.buffer).is_ok() => {
                    self.kept += 1;
                    Some(file)
                }
                Some(_) | None => {
                    self.set_path(pid, format_args!("{TASK_FOLDER}/{thread}/{CHILDREN_FILE}"));
                    let Some(file) = self.open_and_read() else {
                        continue;
                    };
                    file
                }
            };
            if let Ok(text) = std::str::from_utf8(&self.buffer) {
                proc_files::parse_children(text, children);
            }
            kept.extend(file.map(|file| (thread, file)));
        }
        match self.processes.get_mut(&pid) {
            Some(process) => {
                process.threads = Some(KeptThreads {
                    count: stat.threads,
                    age: 0,
                    children: kept,
                });
            }
            // The process's own `stat` is not kept: its threads' files are not either.
            None => self.kept = self.kept.saturating_sub(kept.len()),
        }
    }
}

/// Every process of the machine, read once per sample, for a kernel without the children files.
#[derive(Debug, Default)]
pub(super) struct FullScan {
    stats: HashMap<ProcessId, ProcessStat>,
    children: HashMap<ProcessId, Vec<ProcessId>>,
}

impl FullScan {
    pub(super) fn read() -> io::Result<Self> {
        // Thousands of processes: none of their files is kept.
        let mut files = ChildrenFiles::with_budget(0);
        let mut scan = Self::default();
        for entry in std::fs::read_dir(PROC_ROOT)? {
            let Ok(entry) = entry else {
                continue;
            };
            let pid = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .map(ProcessId::new);
            // The process may end before its `stat` is read.
            let Some(stat) = pid.and_then(|pid| files.stat(pid)) else {
                continue;
            };
            scan.insert(stat);
        }
        Ok(scan)
    }

    fn insert(&mut self, stat: ProcessStat) {
        self.children
            .entry(stat.parent)
            .or_default()
            .push(stat.key.pid);
        self.stats.insert(stat.key.pid, stat);
    }
}

impl ProcessSource for FullScan {
    fn stat(&mut self, pid: ProcessId) -> Option<ProcessStat> {
        self.stats.get(&pid).cloned()
    }

    fn children(&mut self, stat: &ProcessStat, children: &mut Vec<ProcessId>) {
        if let Some(found) = self.children.get(&stat.key.pid) {
            children.extend(found);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// A made-up `/proc`: each process's `stat` line, and its children as the children files
    /// list them.
    #[derive(Debug, Default)]
    pub(in crate::machine_load) struct FakeProc {
        pub(in crate::machine_load) stats: HashMap<ProcessId, String>,
        pub(in crate::machine_load) children: HashMap<ProcessId, Vec<ProcessId>>,
    }

    impl FakeProc {
        /// A process of one thread, with its CPU ticks (own, then with its reaped children) and
        /// its resident pages.
        pub(in crate::machine_load) fn add(
            &mut self,
            pid: u32,
            name: &str,
            parent: u32,
            ticks: (u64, u64),
            pages: u64,
        ) {
            let (own, children) = ticks;
            let line = format!(
                "{pid} ({name}) S {parent} {pid} {pid} 0 -1 0 0 0 0 0 {own} 0 {children} 0 20 0 1 0 {pid}00 0 {pages}"
            );
            self.stats.insert(ProcessId::new(pid), line);
            self.children
                .entry(ProcessId::new(parent))
                .or_default()
                .push(ProcessId::new(pid));
        }
    }

    impl ProcessSource for FakeProc {
        fn stat(&mut self, pid: ProcessId) -> Option<ProcessStat> {
            ProcessStat::parse(self.stats.get(&pid)?)
        }

        fn children(&mut self, stat: &ProcessStat, children: &mut Vec<ProcessId>) {
            if let Some(found) = self.children.get(&stat.key.pid) {
                children.extend(found);
            }
        }
    }

    fn pids(stats: &[ProcessStat]) -> Vec<u32> {
        let mut pids: Vec<u32> = stats.iter().map(|stat| stat.key.pid.get()).collect();
        pids.sort_unstable();
        pids
    }

    /// 1 init; 10 the daemon; 20 and 30 two agents' `claude`; 21 a shell of the first agent
    /// running 22 cargo and 23 rustc; 31 a test runner of the second; 40 a process of someone
    /// else.
    fn machine() -> FakeProc {
        let mut proc = FakeProc::default();
        proc.add(1, "init", 0, (5, 900), 100);
        proc.add(10, "agents-cockpit", 1, (5, 5), 100);
        proc.add(20, "claude", 10, (300, 400), 50_000);
        proc.add(21, "bash", 20, (2, 2), 900);
        proc.add(22, "cargo", 21, (40, 40), 9_000);
        proc.add(23, "rustc", 22, (1_000, 1_000), 200_000);
        proc.add(30, "claude", 10, (200, 200), 40_000);
        proc.add(31, "cargo test", 30, (80, 80), 5_000);
        proc.add(40, "postgres", 1, (7_000, 7_000), 300_000);
        proc
    }

    #[test]
    fn the_walk_finds_every_descendant_and_nothing_else() {
        let mut proc = machine();
        let mut seen = HashSet::new();

        let first = walk(&mut proc, ProcessId::new(20), 100, &mut seen);
        let second = walk(&mut proc, ProcessId::new(30), 100, &mut seen);
        let gone = walk(&mut proc, ProcessId::new(99), 100, &mut seen);

        assert_eq!(first.first().map(|stat| stat.key.pid.get()), Some(20));
        assert_eq!(pids(&first), [20, 21, 22, 23]);
        assert_eq!(pids(&second), [30, 31]);
        assert!(gone.is_empty());
    }

    #[test]
    fn the_walk_stops_at_its_limit_and_takes_each_process_once() {
        let mut proc = machine();
        // A loop in a corrupt table, and a child listed twice.
        proc.children
            .entry(ProcessId::new(23))
            .or_default()
            .extend([ProcessId::new(20), ProcessId::new(22)]);
        proc.children
            .entry(ProcessId::new(20))
            .or_default()
            .push(ProcessId::new(21));
        let mut seen = HashSet::new();

        let all = walk(&mut proc, ProcessId::new(20), 100, &mut seen);
        let limited = walk(&mut proc, ProcessId::new(20), 2, &mut HashSet::new());

        assert_eq!(pids(&all), [20, 21, 22, 23]);
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn a_child_whose_pid_was_reused_is_left_out() {
        let mut proc = machine();
        // 22 is listed under 21, but its stat now names init: another process took its pid.
        proc.stats.insert(
            ProcessId::new(22),
            "22 (sleep) S 1 22 22 0 -1 0 0 0 0 0 1 0 0 0 20 0 1 0 9900 0 10".to_owned(),
        );

        let found = walk(&mut proc, ProcessId::new(20), 100, &mut HashSet::new());

        assert_eq!(pids(&found), [20, 21]);
    }

    #[test]
    fn the_full_scan_finds_children_from_their_parents() {
        let fake = machine();
        let mut scan = FullScan::default();
        for line in fake.stats.values() {
            scan.insert(ProcessStat::parse(line).unwrap());
        }

        let found = walk(&mut scan, ProcessId::new(20), 100, &mut HashSet::new());

        assert_eq!(pids(&found), [20, 21, 22, 23]);
    }

    /// Kills the shell and its sleeps when the test ends, passed or failed.
    struct Tree(Child);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// `shell` run with `-c`, running two `sleep`s, as an agent runs commands.
    fn spawn_tree(shell: &Path) -> Tree {
        let child = Command::new(shell)
            .arg("-c")
            .arg("sleep 30 & sleep 30 & wait")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        Tree(child)
    }

    /// Walks the tree from `shell` with each source this kernel has, once its two `sleep`s run.
    fn walk_each_source(shell: ProcessId) -> Vec<Vec<ProcessStat>> {
        let mut sources: Vec<Box<dyn FnMut() -> Vec<ProcessStat>>> = Vec::new();
        if children_files_exist() {
            sources.push(Box::new(move || {
                walk(
                    &mut ChildrenFiles::default(),
                    shell,
                    100,
                    &mut HashSet::new(),
                )
            }));
        }
        sources.push(Box::new(move || {
            walk(
                &mut FullScan::read().unwrap(),
                shell,
                100,
                &mut HashSet::new(),
            )
        }));
        sources
            .into_iter()
            .map(|mut walk_now| {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut found = walk_now();
                while sleeps(&found) < 2 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                    found = walk_now();
                }
                found
            })
            .collect()
    }

    /// A forked child is named after the shell until it runs `sleep`.
    fn sleeps(found: &[ProcessStat]) -> usize {
        found.iter().filter(|stat| stat.name == "sleep").count()
    }

    #[test]
    fn the_real_proc_shows_a_shell_and_its_children() {
        let tree = spawn_tree(Path::new("/bin/sh"));

        for found in walk_each_source(ProcessId::new(tree.0.id())) {
            let names: Vec<&str> = found.iter().map(|stat| stat.name.as_str()).collect();
            assert_eq!(names.first(), Some(&"sh"), "{names:?}");
            assert_eq!(sleeps(&found), 2, "{names:?}");
        }
    }

    #[test]
    fn a_process_whose_name_is_not_utf8_is_walked() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let folder = tempfile::tempdir().unwrap();
        let shell = folder.path().join(OsStr::from_bytes(b"sh\xff"));
        std::os::unix::fs::symlink("/bin/sh", &shell).unwrap();
        let tree = spawn_tree(&shell);

        for found in walk_each_source(ProcessId::new(tree.0.id())) {
            let names: Vec<&str> = found.iter().map(|stat| stat.name.as_str()).collect();
            assert_eq!(names.first(), Some(&"sh\u{fffd}"), "{names:?}");
            assert_eq!(sleeps(&found), 2, "{names:?}");
        }
    }
}
