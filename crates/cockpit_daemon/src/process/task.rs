//! The task that owns one `claude` child process.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitStatus;
use std::time::{Duration, SystemTime};

use claude_protocol::{
    ControlRequest, ControlRequestId, ControlResponse, IncomingRequest, InputLine, OutputLine,
    ParseError,
};
use cockpit_protocol::{ProcessId, Timestamp};
use serde_json::Value;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::process::claude_command::ClaudeCommand;
#[cfg(windows)]
use crate::process::job::{self, Job};
use crate::process::line_reader::{LineReader, ReadLine};
use crate::process::process_error::ProcessError;
use crate::process::program;
#[cfg(windows)]
use crate::process::program::Launch;
use crate::store::{Direction, RawLog};

/// How long Claude Code has to answer one of the daemon's control requests.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `claude` has to exit after its stdin closes, before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// Images and long tool results make long lines; a line past this is skipped.
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STDERR_LINE_BYTES: usize = 64 * 1024;
/// Stderr lines waiting for the task before the stderr reader stops reading.
const STDERR_BUFFER: usize = 64;

const CONTROL_ID_PREFIX: &str = "cockpit-";
/// What a control request the daemon cannot handle gets as an answer.
const UNHANDLED_REQUEST: &str = "agents-cockpit does not handle this request";

/// What the hub asks of the process.
#[derive(Debug)]
enum ProcessCommand<T> {
    Write(InputLine),
    /// The answer comes back as a [`ProcessEvent::ControlAnswer`] carrying `tag`.
    Control {
        request: ControlRequest,
        tag: T,
    },
    /// Closes stdin, and kills `claude` if it does not exit in time.
    Stop,
}

/// What the process tells the hub.
#[derive(Debug)]
pub(crate) enum ProcessEvent<T> {
    /// A line that is not an answer to the daemon's control requests.
    Line(Box<OutputLine>),
    ControlAnswer {
        tag: T,
        result: Result<Value, ControlFailure>,
    },
    /// The last event.
    Exited { notice: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlFailure {
    /// Claude Code answered with an error.
    Refused(String),
    TimedOut,
    Exited,
}

/// The hub's side of a process task. Dropping it stops the process.
#[derive(Debug)]
pub(crate) struct ProcessHandle<T> {
    commands: mpsc::UnboundedSender<ProcessCommand<T>>,
    /// `claude`'s process, which every command the agent runs descends from.
    pid: Option<ProcessId>,
    /// Another handle to the job of the task, which holds every command the agent runs.
    #[cfg(windows)]
    job: Option<Job>,
    /// Whether `pid` is `claude` itself, or `cmd.exe` running an npm `claude.cmd`.
    #[cfg(windows)]
    launch: Launch,
}

impl<T> ProcessHandle<T> {
    pub(crate) fn pid(&self) -> Option<ProcessId> {
        self.pid
    }

    #[cfg(windows)]
    pub(crate) fn launch(&self) -> Launch {
        self.launch
    }

    #[cfg(windows)]
    pub(crate) fn job(&self) -> Option<&Job> {
        self.job.as_ref()
    }

    pub(crate) fn write(&self, line: InputLine) {
        // A process that ended reports it with `Exited`.
        let _ = self.commands.send(ProcessCommand::Write(line));
    }

    /// Returns the tag when the process has ended, so its caller can answer.
    pub(crate) fn control(&self, request: ControlRequest, tag: T) -> Result<(), T> {
        match self.commands.send(ProcessCommand::Control { request, tag }) {
            Ok(()) => Ok(()),
            Err(unsent) => match unsent.0 {
                ProcessCommand::Control { tag, .. } => Err(tag),
                // The command sent is a control request, so this arm never runs.
                ProcessCommand::Write(_) | ProcessCommand::Stop => Ok(()),
            },
        }
    }

    pub(crate) fn stop(&self) {
        let _ = self.commands.send(ProcessCommand::Stop);
    }
}

/// Starts `claude` and the task that owns it. Each event goes to `events` through `wrap`.
pub(crate) fn spawn<T, M, W>(
    command: &ClaudeCommand,
    raw_log: Option<RawLog>,
    events: mpsc::Sender<M>,
    wrap: W,
) -> Result<ProcessHandle<T>, ProcessError>
where
    T: Send + 'static,
    M: Send + 'static,
    W: Fn(ProcessEvent<T>) -> M + Send + 'static,
{
    let program = program::resolve(&command.program);
    let process = command
        .to_command(&program)
        .map_err(|source| ProcessError::Spawn {
            program: program.path.clone(),
            source,
        })?;
    #[cfg(windows)]
    let launch = program.launch();
    start(
        process,
        &program.path,
        raw_log,
        events,
        wrap,
        #[cfg(windows)]
        launch,
    )
}

fn start<T, M, W>(
    mut command: Command,
    program: &Path,
    raw_log: Option<RawLog>,
    events: mpsc::Sender<M>,
    wrap: W,
    #[cfg(windows)] launch: Launch,
) -> Result<ProcessHandle<T>, ProcessError>
where
    T: Send + 'static,
    M: Send + 'static,
    W: Fn(ProcessEvent<T>) -> M + Send + 'static,
{
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

        // Replaces the flags `program::command` set. `contain` resumes the process.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    #[cfg(windows)]
    let job = contain(&mut child).map_err(|source| ProcessError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(ProcessError::Pipes {
            program: program.to_owned(),
        });
    };
    let (stdin_lines, lines_to_write) = mpsc::unbounded_channel();
    tokio::spawn(write_stdin(stdin, lines_to_write));
    let (stderr_sender, stderr_lines) = mpsc::channel(STDERR_BUFFER);
    tokio::spawn(read_stderr(stderr, stderr_sender));
    let (commands_sender, commands) = mpsc::unbounded_channel();
    let pid = child.id().map(ProcessId::new);
    #[cfg(windows)]
    let handle_job = job.as_ref().and_then(|job| {
        job.try_clone()
            .inspect_err(|error| tracing::warn!(%error, "cannot open claude's job again"))
            .ok()
    });
    let task = Task {
        child,
        #[cfg(windows)]
        job,
        stdin: Some(stdin_lines),
        raw_log,
        pending: HashMap::new(),
        next_control: 1,
        stop_deadline: None,
        events,
        wrap,
    };
    tokio::spawn(task.run(
        commands,
        LineReader::new(BufReader::new(stdout), MAX_LINE_BYTES),
        stderr_lines,
    ));
    Ok(ProcessHandle {
        commands: commands_sender,
        pid,
        #[cfg(windows)]
        job: handle_job,
        #[cfg(windows)]
        launch,
    })
}

struct PendingControl<T> {
    tag: T,
    deadline: Instant,
}

struct Task<T, M, W> {
    child: Child,
    /// Holds `claude` and every process it started. `None` when Windows could not make one.
    #[cfg(windows)]
    job: Option<Job>,
    /// `None` once stdin is closed.
    stdin: Option<mpsc::UnboundedSender<String>>,
    raw_log: Option<RawLog>,
    pending: HashMap<ControlRequestId, PendingControl<T>>,
    next_control: u64,
    stop_deadline: Option<Instant>,
    events: mpsc::Sender<M>,
    wrap: W,
}

/// The hub is gone: nothing more to report.
struct HubGone;

impl<T, M, W> Task<T, M, W> {
    /// Kills `claude` and the processes it started.
    #[cfg(unix)]
    fn kill_tree(&mut self) {
        kill_group(&mut self.child);
    }

    /// Kills every process of the job: `claude`, its MCP servers and the commands it started,
    /// even once `claude` has ended.
    #[cfg(windows)]
    fn kill_tree(&mut self) {
        if let Some(job) = &self.job {
            match job.terminate() {
                Ok(()) => return,
                Err(error) => {
                    tracing::warn!(%error, "cannot end claude's job: killing claude alone")
                }
            }
        }
        // An error means the process is already gone, which `wait` reports.
        let _ = self.child.start_kill();
    }
}

impl<T, M, W> Drop for Task<T, M, W> {
    /// A task that ends before `claude` does, as when the daemon stops, takes the processes
    /// `claude` started along.
    fn drop(&mut self) {
        self.kill_tree();
    }
}

impl<T, M, W> Task<T, M, W>
where
    T: Send + 'static,
    M: Send + 'static,
    W: Fn(ProcessEvent<T>) -> M + Send + 'static,
{
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<ProcessCommand<T>>,
        mut stdout: LineReader<BufReader<ChildStdout>>,
        mut stderr_lines: mpsc::Receiver<String>,
    ) {
        let mut commands_open = true;
        let ended = loop {
            let deadline = self.next_deadline();
            tokio::select! {
                command = commands.recv(), if commands_open => match command {
                    Some(command) => {
                        if self.handle(command).await.is_err() {
                            break Err(HubGone);
                        }
                    }
                    // The hub dropped the handle: stop as asked.
                    None => {
                        commands_open = false;
                        self.close_stdin();
                    }
                },
                line = stdout.next() => match line {
                    Ok(Some(ReadLine::Line(line))) => {
                        if self.received(&line).await.is_err() {
                            break Err(HubGone);
                        }
                    }
                    Ok(Some(ReadLine::TooLong { bytes })) => {
                        tracing::warn!(bytes, "skipped a line from claude longer than the maximum");
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => {
                        tracing::warn!(%error, "cannot read claude's output");
                        break Ok(());
                    }
                },
                Some(line) = stderr_lines.recv() => {
                    tracing::debug!(line, "claude stderr");
                    self.raw(Direction::Stderr, &line);
                }
                () = sleep_until(deadline) => {
                    if self.expire().await.is_err() {
                        break Err(HubGone);
                    }
                }
            }
        };
        if ended.is_err() {
            // Dropping the task kills `claude`'s group.
            return;
        }
        let notice = describe_exit(self.wait().await);
        let pending: Vec<PendingControl<T>> =
            self.pending.drain().map(|(_, pending)| pending).collect();
        for control in pending {
            let answer = ProcessEvent::ControlAnswer {
                tag: control.tag,
                result: Err(ControlFailure::Exited),
            };
            if self.report(answer).await.is_err() {
                return;
            }
        }
        // The hub may be gone; nothing else to do either way.
        let _ = self.report(ProcessEvent::Exited { notice }).await;
    }

    async fn handle(&mut self, command: ProcessCommand<T>) -> Result<(), HubGone> {
        match command {
            ProcessCommand::Write(line) => {
                self.send_line(&line);
                Ok(())
            }
            ProcessCommand::Control { request, tag } => {
                if self.stdin.is_none() {
                    let answer = ProcessEvent::ControlAnswer {
                        tag,
                        result: Err(ControlFailure::Exited),
                    };
                    return self.report(answer).await;
                }
                let id = ControlRequestId::new(format!("{CONTROL_ID_PREFIX}{}", self.next_control));
                self.next_control = self.next_control.saturating_add(1);
                self.send_line(&InputLine::control_request(id.clone(), request));
                let deadline = Instant::now() + CONTROL_TIMEOUT;
                self.pending.insert(id, PendingControl { tag, deadline });
                Ok(())
            }
            ProcessCommand::Stop => {
                self.close_stdin();
                Ok(())
            }
        }
    }

    async fn received(&mut self, line: &str) -> Result<(), HubGone> {
        self.raw(Direction::FromClaude, line);
        let parsed: OutputLine = match line.parse() {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(%error, "skipped a line from claude");
                if let ParseError::MalformedLine {
                    request_id: Some(id),
                    ..
                } = error
                {
                    self.refuse(id);
                }
                return Ok(());
            }
        };
        match parsed {
            OutputLine::ControlResponse(response) => self.answered(response).await,
            OutputLine::ControlRequest(request)
                if matches!(request.request, IncomingRequest::Other(_)) =>
            {
                self.refuse(request.request_id);
                Ok(())
            }
            line => self.report(ProcessEvent::Line(Box::new(line))).await,
        }
    }

    /// Answers a control request the daemon cannot handle, so Claude Code does not wait forever.
    fn refuse(&mut self, id: ControlRequestId) {
        tracing::warn!(id = id.as_str(), "refused a control request from claude");
        self.send_line(&InputLine::control_error(id, UNHANDLED_REQUEST.to_owned()));
    }

    /// Routes an answer to one of the daemon's control requests. Claude Code also echoes the
    /// answers the daemon sends to its own requests: those carry ids the daemon did not pick.
    async fn answered(&mut self, response: ControlResponse) -> Result<(), HubGone> {
        let Some(control) = response.request_id().and_then(|id| self.pending.remove(id)) else {
            return Ok(());
        };
        let result = match response {
            ControlResponse::Success(success) => Ok(success.response),
            ControlResponse::Error(error) => Err(ControlFailure::Refused(error.error)),
            ControlResponse::Unknown(body) => Err(ControlFailure::Refused(body.to_string())),
        };
        self.report(ProcessEvent::ControlAnswer {
            tag: control.tag,
            result,
        })
        .await
    }

    /// Fails the control requests past their deadline, and kills a process past its stop
    /// deadline.
    async fn expire(&mut self) -> Result<(), HubGone> {
        let now = Instant::now();
        if self.stop_deadline.is_some_and(|deadline| deadline <= now) {
            self.stop_deadline = None;
            tracing::warn!("claude did not exit after its stdin closed: killing it");
            self.kill_tree();
        }
        let expired: Vec<ControlRequestId> = self
            .pending
            .iter()
            .filter(|(_, control)| control.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(control) = self.pending.remove(&id) {
                let answer = ProcessEvent::ControlAnswer {
                    tag: control.tag,
                    result: Err(ControlFailure::TimedOut),
                };
                self.report(answer).await?;
            }
        }
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        let controls = self.pending.values().map(|control| control.deadline);
        controls.chain(self.stop_deadline).min()
    }

    fn close_stdin(&mut self) {
        if self.stdin.take().is_some() {
            self.stop_deadline = Some(Instant::now() + STOP_TIMEOUT);
        }
    }

    fn send_line(&mut self, line: &InputLine) {
        let text = match line.to_line() {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(%error, "cannot encode a line for claude");
                return;
            }
        };
        if self.stdin.is_none() {
            tracing::debug!("dropped a line for claude: its stdin is closed");
            return;
        }
        self.raw(Direction::ToClaude, &text);
        if let Some(stdin) = &self.stdin {
            // When the writer has stopped, the process is ending and reports it.
            let _ = stdin.send(text);
        }
    }

    /// Waits for the process to exit after its stdout closed, killing it if it lingers.
    async fn wait(&mut self) -> Option<ExitStatus> {
        if let Ok(status) = tokio::time::timeout(STOP_TIMEOUT, self.child.wait()).await {
            return status.ok();
        }
        tracing::warn!("claude closed its output but did not exit: killing it");
        self.kill_tree();
        self.child.wait().await.ok()
    }

    async fn report(&mut self, event: ProcessEvent<T>) -> Result<(), HubGone> {
        self.events
            .send((self.wrap)(event))
            .await
            .map_err(|_| HubGone)
    }

    fn raw(&mut self, direction: Direction, line: &str) {
        if let Some(raw_log) = &mut self.raw_log {
            raw_log.append(now(), direction, line);
        }
    }
}

/// Kills `claude` and the rest of its process group: its MCP servers and the commands it started.
/// Does nothing once `claude` is reaped: its pid, which names the group, may then name another
/// process.
#[cfg(unix)]
fn kill_group(child: &mut Child) {
    use rustix::process::{Pid, Signal};

    let group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw);
    let Some(group) = group else {
        return;
    };
    if let Err(error) = rustix::process::kill_process_group(group, Signal::KILL) {
        tracing::warn!(%error, "cannot kill claude's process group: killing claude alone");
        // An error means the process is already gone, which `wait` reports.
        let _ = child.start_kill();
    }
}

/// Puts `child`, started suspended, in a job of its own, then lets it run. On an error, the
/// child is killed.
#[cfg(windows)]
fn contain(child: &mut Child) -> std::io::Result<Option<Job>> {
    let (Some(process), Some(pid)) = (child.raw_handle(), child.id()) else {
        return Err(std::io::Error::other("claude ended before it ran"));
    };
    job::start_in_job(process, pid).inspect_err(|_| {
        // An error means the process is already gone.
        let _ = child.start_kill();
    })
}

#[cfg(unix)]
fn describe_exit(status: Option<ExitStatus>) -> String {
    use std::os::unix::process::ExitStatusExt;

    match status {
        Some(status) => match (status.code(), status.signal()) {
            (Some(0), _) => "claude exited".to_owned(),
            (Some(code), _) => format!("claude exited with status {code}"),
            (None, Some(signal)) => format!("claude was killed by signal {signal}"),
            (None, None) => "claude exited".to_owned(),
        },
        None => "claude exited, with an unknown status".to_owned(),
    }
}

/// A Windows process always ends with a code: a killed one ends with the code its killer chose.
#[cfg(windows)]
fn describe_exit(status: Option<ExitStatus>) -> String {
    match status.map(|status| status.code()) {
        Some(Some(0) | None) => "claude exited".to_owned(),
        Some(Some(code)) => format!("claude exited with status {code}"),
        None => "claude exited, with an unknown status".to_owned(),
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn write_stdin(mut stdin: ChildStdin, mut lines: mpsc::UnboundedReceiver<String>) {
    use tokio::io::AsyncWriteExt;

    while let Some(line) = lines.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
    // Dropping stdin closes it: `claude` exits after its last turn.
}

async fn read_stderr(stderr: ChildStderr, lines: mpsc::Sender<String>) {
    let mut reader = LineReader::new(BufReader::new(stderr), MAX_STDERR_LINE_BYTES);
    while let Ok(Some(line)) = reader.next().await {
        let text = match line {
            ReadLine::Line(text) => text,
            ReadLine::TooLong { bytes } => format!("[a stderr line of {bytes} bytes]"),
        };
        if lines.send(text).await.is_err() {
            return;
        }
    }
}

fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_millis(0))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;

    use super::*;

    #[test]
    fn exits_are_described() {
        assert_eq!(
            describe_exit(Some(ExitStatus::from_raw(0))),
            "claude exited"
        );
        assert_eq!(
            describe_exit(Some(ExitStatus::from_raw(1 << 8))),
            "claude exited with status 1"
        );
        assert_eq!(
            describe_exit(Some(ExitStatus::from_raw(9))),
            "claude was killed by signal 9"
        );
    }

    async fn next<T>(events: &mut mpsc::Receiver<ProcessEvent<T>>) -> ProcessEvent<T> {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 seconds")
            .expect("the task runs")
    }

    /// A `claude` that is a shell script.
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    #[tokio::test]
    async fn routes_answers_refuses_what_it_cannot_handle_and_reports_the_exit() {
        // The script answers the daemon's control request, then sends an unknown request, a
        // malformed one, an echoed answer and an init line. It exits with status 3 once it read
        // a refusal for each request, and with 4 otherwise.
        let script = r#"
            read -r first
            echo '{"type":"control_response","response":{"subtype":"success","request_id":"cockpit-1","response":{"commands":[]}}}'
            echo '{"type":"control_request","request_id":"hook-1","request":{"subtype":"hook_callback"}}'
            echo '{"type":"control_request","request_id":"bad-1","request":{"subtype":"can_use_tool"}}'
            echo '{"type":"control_response","response":{"subtype":"success","request_id":"perm-9","response":{}}}'
            echo '{"type":"system","subtype":"init","session_id":"s-1"}'
            read -r refusal_1
            read -r refusal_2
            case "$refusal_1$refusal_2" in
                *'"request_id":"hook-1"'*'"request_id":"bad-1"'*) exit 3 ;;
            esac
            exit 4
        "#;
        let (events_sender, mut events) = mpsc::channel(16);
        let handle: ProcessHandle<u8> = start(
            shell(script),
            Path::new("sh"),
            None,
            events_sender,
            |event| event,
        )
        .unwrap();

        handle.control(ControlRequest::Initialize, 7).unwrap();

        let ProcessEvent::ControlAnswer { tag, result } = next(&mut events).await else {
            panic!("expected the answer first");
        };
        assert_eq!(tag, 7);
        assert_eq!(result.unwrap()["commands"], serde_json::json!([]));
        let ProcessEvent::Line(line) = next(&mut events).await else {
            panic!("expected a line: the task handles the lines before it");
        };
        let OutputLine::System(_) = *line else {
            panic!("expected the init line: the task handles the lines before it");
        };
        let ProcessEvent::Exited { notice } = next(&mut events).await else {
            panic!("expected the exit");
        };
        assert_eq!(notice, "claude exited with status 3");
        assert_eq!(handle.control(ControlRequest::Interrupt, 8), Err(8));
    }

    #[tokio::test]
    async fn a_request_left_unanswered_fails_when_the_process_exits() {
        let (events_sender, mut events) = mpsc::channel(16);
        let handle: ProcessHandle<u8> = start(
            shell("read -r line"),
            Path::new("sh"),
            None,
            events_sender,
            |event| event,
        )
        .unwrap();

        handle.control(ControlRequest::McpStatus, 1).unwrap();

        let ProcessEvent::ControlAnswer { tag, result } = next(&mut events).await else {
            panic!("expected the failed answer");
        };
        assert_eq!((tag, result), (1, Err(ControlFailure::Exited)));
        assert!(matches!(
            next(&mut events).await,
            ProcessEvent::Exited { .. }
        ));
    }

    #[tokio::test]
    async fn stop_closes_stdin() {
        let (events_sender, mut events) = mpsc::channel(16);
        let handle: ProcessHandle<u8> = start(
            shell("while read -r line; do :; done; exit 0"),
            Path::new("sh"),
            None,
            events_sender,
            |event| event,
        )
        .unwrap();

        handle.stop();

        let ProcessEvent::Exited { notice } = next(&mut events).await else {
            panic!("expected the exit");
        };
        assert_eq!(notice, "claude exited");
    }
}
