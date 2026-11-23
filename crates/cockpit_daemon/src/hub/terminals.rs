//! Terminals: shells on a PTY in an agent's folder, run as the daemon's user, as the agents are.
//!
//! A shell starts in the agent's folder, which a mount that never answers would hold, so it
//! starts in a task with a time limit, and the hub takes the terminal once the start comes back
//! as a [`HubInput`]. A shell that no terminal takes ends at once.
//!
//! Three threads serve each shell, since the PTY's reads, writes and wait block: one reads its
//! output, one writes what the user types, and one waits for it to end. The first and the last
//! report to the hub, which keeps the recent output and sends every client what comes.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc as std_mpsc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cockpit_protocol::{
    AgentId, ErrorBody, ErrorCode, Frame, ResponseBody, ServerMessage, ShellChoice, TerminalChange,
    TerminalExit, TerminalId, TerminalInfo, TerminalInputParams, TerminalOpenParams,
    TerminalParams, TerminalResizeParams, TerminalSize, TerminalSizeAt, TerminalUpdate,
};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::time;

use crate::blocking::{self, Scope};
use crate::hub::agents::respond;
use crate::hub::input::{HubInput, Reply};
use crate::hub::shells::{self, Shell};
use crate::hub::state::{CLIENT_QUEUE, Hub};
use crate::hub::{errors, start};

/// The output a terminal keeps for a client that shows it again: a few screens of a busy build.
const RECENT_BYTES: usize = 512 * 1024;

/// The most sizes a terminal keeps over its kept output. A pane resizes the PTY once it stops
/// moving, so the kept output rarely spans more.
const MAX_SIZES: usize = 256;

/// What one read of a shell's output takes at most.
const READ_BYTES: usize = 64 * 1024;

/// The output a terminal keeps, as `terminal.attach` answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Screen {
    /// Where the kept output starts in everything the shell wrote.
    pub(super) offset: u64,
    /// The kept output, base64.
    pub(super) data: String,
    /// The sizes the PTY took over it, oldest first.
    pub(super) sizes: Vec<TerminalSizeAt>,
}

#[derive(Default)]
pub(super) struct Terminals {
    last: Option<TerminalId>,
    open: BTreeMap<TerminalId, Terminal>,
}

struct Terminal {
    agent: AgentId,
    title: String,
    master: Box<dyn MasterPty + Send>,
    /// To the thread that writes to the shell.
    input: std_mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// The last output, at most [`RECENT_BYTES`].
    recent: VecDeque<u8>,
    /// How many bytes the shell wrote in all.
    written: u64,
    /// The sizes the PTY took over `recent`, oldest first, at most [`MAX_SIZES`]: the first is the
    /// size where `recent` starts, unless it was dropped among too many.
    sizes: VecDeque<TerminalSizeAt>,
    /// `Some` once the shell ended.
    exited: Option<TerminalExit>,
}

impl Terminal {
    /// Notes that the PTY took `size` after the output so far. A size taken before any more
    /// output replaces the one before it.
    fn note_size(&mut self, size: TerminalSize) {
        let size = taken(size);
        if self
            .sizes
            .back()
            .is_some_and(|last| last.offset == self.written)
        {
            self.sizes.pop_back();
        }
        if self.sizes.back().map(|last| last.size) != Some(size) {
            self.sizes.push_back(TerminalSizeAt {
                offset: self.written,
                size,
            });
        }
        self.trim_sizes();
    }

    /// Drops the sizes that no kept output was written at: those before the one where `recent`
    /// starts, and the oldest past [`MAX_SIZES`].
    fn trim_sizes(&mut self) {
        let start = self.written - self.recent.len() as u64;
        while self.sizes.get(1).is_some_and(|next| next.offset <= start) {
            self.sizes.pop_front();
        }
        let excess = self.sizes.len().saturating_sub(MAX_SIZES);
        self.sizes.drain(..excess);
    }
}

/// A shell that runs on its PTY and waits for [`Terminals::add`]: the thread that waits for it
/// reports nothing before then. Dropping it ends the shell.
pub(crate) struct SpawnedShell {
    title: String,
    /// The PTY's size as the shell starts.
    size: TerminalSize,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Tells the thread that waits for the shell that a terminal owns it. Dropped unsent, it
    /// makes the thread end the shell.
    owned: std_mpsc::Sender<()>,
}

impl Terminals {
    /// Takes the id of a terminal that opens. `None` when no id is left.
    pub(super) fn next_id(&mut self) -> Option<TerminalId> {
        let terminal = match self.last {
            Some(last) => last.next()?,
            None => TerminalId::new(1),
        };
        self.last = Some(terminal);
        Some(terminal)
    }

    /// Makes `shell`, started as `terminal`, one of the agent's terminals, and starts the threads
    /// that read and write it. Returns its title.
    pub(super) fn add(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
        shell: SpawnedShell,
        inputs: &mpsc::Sender<HubInput>,
    ) -> Result<String, String> {
        let SpawnedShell {
            title,
            size,
            master,
            killer,
            owned,
        } = shell;
        let reader = master
            .try_clone_reader()
            .map_err(|error| format!("cannot read the terminal: {error}"))?;
        let writer = master
            .take_writer()
            .map_err(|error| format!("cannot write to the terminal: {error}"))?;
        let (input, typed) = std_mpsc::channel();
        spawn_thread(
            "terminal-read",
            read_output(terminal, reader, inputs.clone()),
        )?;
        spawn_thread("terminal-write", write_input(writer, typed))?;
        // The thread that waits for the shell is gone only if it panicked.
        let _ = owned.send(());
        self.open.insert(
            terminal,
            Terminal {
                agent,
                title: title.clone(),
                master,
                input,
                killer,
                recent: VecDeque::new(),
                written: 0,
                sizes: VecDeque::from([TerminalSizeAt {
                    offset: 0,
                    size: taken(size),
                }]),
                exited: None,
            },
        );
        Ok(title)
    }

    /// The agent's terminals, oldest first.
    pub(super) fn list(&self, agent: AgentId) -> Vec<TerminalInfo> {
        self.open
            .iter()
            .filter(|(_, open)| open.agent == agent)
            .map(|(terminal, open)| TerminalInfo {
                terminal: *terminal,
                title: open.title.clone(),
                exited: open.exited,
            })
            .collect()
    }

    /// The terminal, when it is the agent's.
    fn of_agent(&mut self, agent: AgentId, terminal: TerminalId) -> Result<&mut Terminal, String> {
        self.open
            .get_mut(&terminal)
            .filter(|open| open.agent == agent)
            .ok_or_else(|| "This terminal is closed.".to_owned())
    }

    pub(super) fn input(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
        data: String,
    ) -> Result<(), String> {
        let open = self.of_agent(agent, terminal)?;
        open.input
            .send(data.into_bytes())
            .map_err(|_| "The terminal's shell ended.".to_owned())
    }

    pub(super) fn resize(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
        size: TerminalSize,
    ) -> Result<(), String> {
        let open = self.of_agent(agent, terminal)?;
        open.master
            .resize(pty_size(size))
            .map_err(|error| format!("cannot resize the terminal: {error}"))?;
        open.note_size(size);
        Ok(())
    }

    /// The output kept, base64, where it starts in everything the shell wrote, and the sizes the
    /// PTY took over it.
    pub(super) fn screen(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
    ) -> Result<Screen, String> {
        let open = self.of_agent(agent, terminal)?;
        let (front, back) = open.recent.as_slices();
        let mut bytes = Vec::with_capacity(open.recent.len());
        bytes.extend_from_slice(front);
        bytes.extend_from_slice(back);
        Ok(Screen {
            offset: open.written - bytes.len() as u64,
            data: STANDARD.encode(bytes),
            sizes: open.sizes.iter().copied().collect(),
        })
    }

    /// Ends the shell and forgets the terminal.
    pub(super) fn close(&mut self, agent: AgentId, terminal: TerminalId) -> Result<(), String> {
        self.of_agent(agent, terminal)?;
        if let Some(mut open) = self.open.remove(&terminal) {
            end(&mut open);
        }
        Ok(())
    }

    /// Closes every terminal of an agent, as when it is removed. Returns the closed ones.
    pub(super) fn close_agent(&mut self, agent: AgentId) -> Vec<TerminalId> {
        let closed: Vec<TerminalId> = self
            .open
            .iter()
            .filter(|(_, open)| open.agent == agent)
            .map(|(terminal, _)| *terminal)
            .collect();
        for terminal in &closed {
            if let Some(mut open) = self.open.remove(terminal) {
                end(&mut open);
            }
        }
        closed
    }

    /// Keeps output the shell wrote, and returns the update every client gets.
    pub(super) fn output(&mut self, terminal: TerminalId, data: &[u8]) -> Option<TerminalUpdate> {
        let open = self.open.get_mut(&terminal)?;
        let offset = open.written;
        open.written += data.len() as u64;
        open.recent.extend(data);
        let excess = open.recent.len().saturating_sub(RECENT_BYTES);
        open.recent.drain(..excess);
        open.trim_sizes();
        Some(TerminalUpdate {
            agent: open.agent,
            terminal,
            change: TerminalChange::Output {
                offset,
                data: STANDARD.encode(data),
            },
        })
    }

    /// Marks the shell ended, and returns the update every client gets.
    pub(super) fn exited(
        &mut self,
        terminal: TerminalId,
        code: Option<i32>,
    ) -> Option<TerminalUpdate> {
        let open = self.open.get_mut(&terminal)?;
        open.exited = Some(TerminalExit { code });
        Some(TerminalUpdate {
            agent: open.agent,
            terminal,
            change: TerminalChange::Exited { code },
        })
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        for open in self.open.values_mut() {
            end(open);
        }
    }
}

fn end(open: &mut Terminal) {
    if open.exited.is_none()
        && let Err(error) = open.killer.kill()
    {
        tracing::warn!(%error, "cannot end a terminal's shell");
    }
}

/// The size a PTY takes when asked for `size`: at least one row and one column.
fn taken(size: TerminalSize) -> TerminalSize {
    TerminalSize {
        cols: size.cols.max(1),
        rows: size.rows.max(1),
    }
}

fn pty_size(size: TerminalSize) -> PtySize {
    let TerminalSize { cols, rows } = taken(size);
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn spawn_thread(name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map(drop)
        .map_err(|error| format!("cannot start a thread for the terminal: {error}"))
}

/// Starts the shell whose program is `program`, or the default one, in `folder` on a PTY of
/// `size`, as terminal `terminal`. It waits until the shell runs in the folder, so a folder on a
/// stuck mount holds it.
fn spawn_shell(
    terminal: TerminalId,
    folder: &Path,
    size: TerminalSize,
    program: Option<&Path>,
    inputs: mpsc::Sender<HubInput>,
) -> Result<SpawnedShell, String> {
    let shell = shells::pick(shells::find(), program)?;
    spawn(&shell, terminal, folder, size, inputs)
}

/// Starts `shell` in `folder` on a PTY of `size`, with the thread that waits for it.
fn spawn(
    shell: &Shell,
    terminal: TerminalId,
    folder: &Path,
    size: TerminalSize,
    inputs: mpsc::Sender<HubInput>,
) -> Result<SpawnedShell, String> {
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(|error| format!("cannot open a terminal: {error}"))?;
    let program = &shell.program;
    let mut command = CommandBuilder::new(program);
    command.args(shell.args);
    command.cwd(folder);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    // As login(1) does: what the terminal starts, as tmux or vim's `:sh`, runs the same shell.
    #[cfg(unix)]
    command.env(shells::USER_SHELL_ENV, program);
    // The shell is the user's, not one Claude Code runs.
    command.env_remove("CLAUDECODE");
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("cannot start {}: {error}", program.display()))?;
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let (owned, owner) = std_mpsc::channel();
    let waiting = spawn_thread(
        "terminal-wait",
        wait_for_shell(terminal, child, owner, inputs),
    );
    if let Err(error) = waiting {
        // The thread's work held the shell, and never runs.
        if let Err(kill) = killer.kill() {
            tracing::warn!(error = %kill, "cannot end a terminal's shell");
        }
        return Err(error);
    }
    Ok(SpawnedShell {
        title: shell.title().to_owned(),
        size,
        master: pair.master,
        killer,
        owned,
    })
}

/// Waits for the shell to end and tells the hub, once a terminal owns the shell. When none does,
/// it ends the shell and reaps it.
fn wait_for_shell(
    terminal: TerminalId,
    mut child: Box<dyn Child + Send + Sync>,
    owner: std_mpsc::Receiver<()>,
    inputs: mpsc::Sender<HubInput>,
) -> impl FnOnce() + Send + 'static {
    move || {
        if owner.recv().is_err() {
            // On Unix, the kill sends SIGHUP, waits a moment for the shell to end, then sends
            // SIGKILL.
            if let Err(error) = child.kill() {
                tracing::warn!(%error, "cannot end a shell that no terminal took");
            }
            let _ = child.wait();
            return;
        }
        let code = child
            .wait()
            .ok()
            .filter(|status| status.signal().is_none())
            .and_then(|status| i32::try_from(status.exit_code()).ok());
        // The hub may have stopped meanwhile.
        let _ = inputs.blocking_send(HubInput::TerminalExited { terminal, code });
    }
}

/// Reads the shell's output until the PTY closes, and hands each read to the hub.
fn read_output(
    terminal: TerminalId,
    mut reader: Box<dyn Read + Send>,
    inputs: mpsc::Sender<HubInput>,
) -> impl FnOnce() + Send + 'static {
    move || {
        let mut buffer = vec![0; READ_BYTES];
        loop {
            let data = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => buffer.get(..read).unwrap_or_default().to_vec(),
            };
            if inputs
                .blocking_send(HubInput::TerminalOutput { terminal, data })
                .is_err()
            {
                return;
            }
        }
    }
}

/// Writes what the user types until the terminal is closed.
fn write_input(
    mut writer: Box<dyn Write + Send>,
    typed: std_mpsc::Receiver<Vec<u8>>,
) -> impl FnOnce() + Send + 'static {
    move || {
        for data in typed {
            if writer
                .write_all(&data)
                .and_then(|()| writer.flush())
                .is_err()
            {
                return;
            }
        }
    }
}

impl Hub {
    /// Starts the shell asked for, or the default one, in the agent's folder in a task, with the
    /// time limit of an agent's start. The hub takes the terminal and answers in
    /// [`Hub::terminal_spawned`].
    pub(super) fn open_terminal(&mut self, params: TerminalOpenParams, reply: Reply) {
        let TerminalOpenParams { agent, size, shell } = params;
        let folder = match self.agent(agent) {
            Ok(found) => found.record.folder.clone(),
            Err(error) => return respond(reply, Err(error)),
        };
        let Some(terminal) = self.terminals.next_id() else {
            return respond(reply, Err(unsupported("no terminal id left".to_owned())));
        };
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let (shell_folder, exits) = (folder.clone(), inputs.clone());
            let deadline = time::Instant::now() + start::FOLDER_TIME;
            // A shell that starts past the limit drops with no terminal, which ends it.
            let spawned = blocking::run(deadline, Scope::Folder(&folder), move || {
                spawn_shell(terminal, &shell_folder, size, shell.as_deref(), exits)
                    .map_err(unsupported)
            })
            .await;
            let spawned = spawned.unwrap_or_else(|error| Err(start::not_answered(&folder, &error)));
            let input = HubInput::TerminalSpawned {
                agent,
                terminal,
                spawned,
                reply,
            };
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::TerminalSpawned { reply, .. } = unsent.0
            {
                respond(reply, Err(errors::stopping()));
            }
        });
    }

    /// A task started a terminal's shell, or could not.
    pub(super) fn terminal_spawned(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
        spawned: Result<SpawnedShell, ErrorBody>,
        reply: Reply,
    ) {
        let opened = spawned.and_then(|shell| self.add_terminal(agent, terminal, shell));
        respond(reply, opened);
    }

    fn add_terminal(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
        shell: SpawnedShell,
    ) -> Result<ResponseBody, ErrorBody> {
        // An agent closed meanwhile drops the shell, which ends it.
        self.agent(agent)?;
        let title = self
            .terminals
            .add(agent, terminal, shell, &self.inputs)
            .map_err(unsupported)?;
        self.send_terminal(TerminalUpdate {
            agent,
            terminal,
            change: TerminalChange::Opened { title },
        });
        Ok(ResponseBody::TerminalOpened { terminal })
    }

    /// Lists the agent's terminals and the shells a terminal can run. A task finds the shells,
    /// since it reads files, and the hub answers in [`Hub::shells_found`], with the terminals open
    /// then.
    pub(super) fn list_terminals(&self, agent: AgentId, reply: Reply) {
        if let Err(error) = self.agent(agent) {
            return respond(reply, Err(error));
        }
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let deadline = time::Instant::now() + start::FOLDER_TIME;
            let found = blocking::run(deadline, Scope::Shells, shells::find).await;
            // Without the shells, the app still shows the terminals, with no menu.
            let shells = found.unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot find the shells a terminal can run");
                Vec::new()
            });
            let shells: Vec<ShellChoice> = shells.into_iter().map(Shell::into_choice).collect();
            let input = HubInput::ShellsFound {
                agent,
                shells,
                reply,
            };
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::ShellsFound { reply, .. } = unsent.0
            {
                respond(reply, Err(errors::stopping()));
            }
        });
    }

    /// A task found the shells that `agent.terminals` lists.
    pub(super) fn shells_found(&self, agent: AgentId, shells: Vec<ShellChoice>, reply: Reply) {
        let listed = self.agent(agent).map(|_| ResponseBody::Terminals {
            terminals: self.terminals.list(agent),
            shells,
        });
        respond(reply, listed);
    }

    pub(super) fn attach_terminal(
        &mut self,
        params: TerminalParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let Screen {
            offset,
            data,
            sizes,
        } = self
            .terminals
            .screen(params.agent, params.terminal)
            .map_err(not_found)?;
        Ok(ResponseBody::TerminalScreen {
            offset,
            data,
            sizes,
        })
    }

    pub(super) fn type_in_terminal(
        &mut self,
        params: TerminalInputParams,
    ) -> Result<ResponseBody, ErrorBody> {
        self.terminals
            .input(params.agent, params.terminal, params.data)
            .map_err(not_found)?;
        Ok(ResponseBody::Done)
    }

    pub(super) fn resize_terminal(
        &mut self,
        params: TerminalResizeParams,
    ) -> Result<ResponseBody, ErrorBody> {
        self.terminals
            .resize(params.agent, params.terminal, params.size)
            .map_err(not_found)?;
        Ok(ResponseBody::Done)
    }

    pub(super) fn close_terminal(
        &mut self,
        params: TerminalParams,
    ) -> Result<ResponseBody, ErrorBody> {
        self.terminals
            .close(params.agent, params.terminal)
            .map_err(not_found)?;
        self.send_terminal(TerminalUpdate {
            agent: params.agent,
            terminal: params.terminal,
            change: TerminalChange::Closed,
        });
        Ok(ResponseBody::Done)
    }

    /// Closes an agent's terminals as it leaves.
    pub(super) fn close_agent_terminals(&mut self, agent: AgentId) {
        for terminal in self.terminals.close_agent(agent) {
            self.send_terminal(TerminalUpdate {
                agent,
                terminal,
                change: TerminalChange::Closed,
            });
        }
    }

    pub(super) fn terminal_output(&mut self, terminal: TerminalId, data: &[u8]) {
        if let Some(update) = self.terminals.output(terminal, data) {
            self.send_terminal(update);
        }
    }

    pub(super) fn terminal_exited(&mut self, terminal: TerminalId, code: Option<i32>) {
        if let Some(update) = self.terminals.exited(terminal, code) {
            self.send_terminal(update);
        }
    }

    /// Sends a terminal update to every client. Unlike an event, it has no number and no log, and
    /// a client whose queue is full misses it rather than being dropped: it asks
    /// `terminal.attach` for the output it missed. Output also skips a client whose queue is half
    /// full, since an event that finds the queue full drops the client.
    fn send_terminal(&mut self, update: TerminalUpdate) {
        let spare = match update.change {
            TerminalChange::Output { .. } => CLIENT_QUEUE / 2,
            TerminalChange::Opened { .. }
            | TerminalChange::Exited { .. }
            | TerminalChange::Closed => 0,
        };
        let frame = match Frame::encode(&ServerMessage::Terminal(update)) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(%error, "cannot encode a terminal update: it is lost");
                return;
            }
        };
        self.clients.retain(|client| {
            if client.capacity() <= spare {
                return !client.is_closed();
            }
            match client.try_send(frame.clone()) {
                Ok(()) | Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Closed(_)) => false,
            }
        });
    }
}

fn not_found(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, message)
}

fn unsupported(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::Unsupported, message)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::time::Duration;

    use cockpit_protocol::{AccountId, AgentParams, Mark, Position, Request, Timestamp};

    use crate::store::{AgentRecord, AgentsFile, ResumeAs, StateDir};
    #[cfg(target_os = "linux")]
    use crate::test_script;

    use super::*;

    /// A hub with one exited agent, 1, in `<temp>/app`, and the inputs its tasks send it.
    fn hub_with_agent(temp: &Path) -> (Hub, mpsc::Receiver<HubInput>, AgentId) {
        let state_dir = temp.join("state");
        let folder = temp.join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = AgentId::new(1);
        let state = StateDir::create(&state_dir).unwrap();
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![AgentRecord {
                id: agent,
                title: "Fix the cart".to_owned(),
                folder,
                session: None,
                model: None,
                effort: None,
                mode: None,
                running: false,
                started_at: Timestamp::from_unix_millis(0),
                past: None,
                position: Position::default(),
                process: None,
                account: AccountId::DEFAULT,
                mark: Mark::Unmarked,
                resume_as: ResumeAs::Same,
            }],
        }
        .save(&state.agents_file())
        .unwrap();
        let (hub, inputs) = Hub::for_tests_with_inputs(&state_dir);
        (hub, inputs, agent)
    }

    fn open_request(agent: AgentId) -> Request {
        Request::AgentTerminalOpen(TerminalOpenParams {
            agent,
            size: TerminalSize { cols: 80, rows: 24 },
            shell: None,
        })
    }

    /// The shell starts off the hub: the terminal is listed, and the request answered, once the
    /// start comes back.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_terminal_opens_once_its_shell_runs() {
        use tokio::sync::oneshot;

        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path());
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);
        let (reply, mut answered) = oneshot::channel();

        hub.request(open_request(agent), reply);
        let at_once = answered.try_recv().is_ok();
        let listed_at_once = hub.terminals.list(agent).len();
        let opened = hub.await_answer(&mut inputs, answered).await;

        assert!(!at_once, "the shell starts off the hub");
        assert_eq!(listed_at_once, 0);
        let Ok(ResponseBody::TerminalOpened { terminal }) = opened else {
            panic!("expected a terminal, got {opened:?}");
        };
        let listed: Vec<TerminalId> = hub
            .terminals
            .list(agent)
            .iter()
            .map(|info| info.terminal)
            .collect();
        assert_eq!(listed, [terminal]);
        let frame = frames.try_recv().unwrap();
        let sent = String::from_utf8_lossy(frame.as_bytes()).into_owned();
        assert!(sent.contains(r#""type":"opened""#), "{sent}");
    }

    /// The agent's folder is on a mount that never answers: opening a terminal there fails in
    /// time, and the hub goes on.
    #[tokio::test]
    async fn a_terminal_in_a_folder_that_does_not_answer_times_out() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path());
        let folder = hub.agents[&agent].record.folder.clone();
        let releases = blocking::hold_folder(&folder).await;

        let refused = hub.ask(&mut inputs, open_request(agent)).await;
        // Lets the held threads end, so that the test's runtime can shut down.
        drop(releases);

        let refused = refused.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Timeout);
        assert_eq!(
            refused.message,
            format!("the folder {} did not answer", folder.display())
        );
        assert!(hub.terminals.list(agent).is_empty());
    }

    #[tokio::test]
    async fn the_terminal_list_names_the_shells_with_one_default() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path());

        let listed = hub
            .ask(&mut inputs, Request::AgentTerminals(AgentParams { agent }))
            .await;

        let Ok(ResponseBody::Terminals { terminals, shells }) = listed else {
            panic!("expected the terminals, got {listed:?}");
        };
        assert!(terminals.is_empty());
        let defaults = shells.iter().filter(|shell| shell.default).count();
        assert_eq!(defaults, 1, "{shells:?}");
    }

    /// The app names a shell from the list: any other program is refused, and nothing runs.
    #[tokio::test]
    async fn a_shell_the_machine_does_not_offer_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path());
        let program = temp.path().join("not-a-shell");
        let request = Request::AgentTerminalOpen(TerminalOpenParams {
            agent,
            size: TerminalSize { cols: 80, rows: 24 },
            shell: Some(program.clone()),
        });

        let refused = hub.ask(&mut inputs, request).await;

        let refused = refused.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Unsupported);
        assert_eq!(
            refused.message,
            format!("{} is not a shell of this machine", program.display())
        );
        assert!(hub.terminals.list(agent).is_empty());
    }

    /// A shell that writes its pid in its folder, then runs until it is killed: it ignores the
    /// SIGHUP of its PTY's end. `write_script` runs it once without a terminal, and it then ends
    /// at once.
    #[cfg(target_os = "linux")]
    fn pid_shell(temp: &Path) -> Shell {
        let program = temp.join("shell");
        test_script::write_script(
            &program,
            "#!/bin/sh\n[ -t 0 ] || exit 0\ntrap '' HUP\necho $$ > shell.pid\nexec sleep 600\n",
        );
        Shell {
            program,
            args: &[],
            name: "shell".to_owned(),
            default: true,
        }
    }

    /// The pid the shell of [`pid_shell`] wrote in `folder`, once it runs.
    #[cfg(target_os = "linux")]
    async fn shell_pid(folder: &Path) -> u32 {
        let file = folder.join("shell.pid");
        for _ in 0..1000 {
            let written = std::fs::read_to_string(&file).ok();
            if let Some(pid) = written.and_then(|text| text.trim().parse().ok()) {
                return pid;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the shell did not start");
    }

    /// Whether the process `pid` ends within 10 seconds.
    #[cfg(target_os = "linux")]
    async fn ends(pid: u32) -> bool {
        for _ in 0..1000 {
            if !test_script::runs(pid) {
                return true;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// A shell that starts once its start stopped waiting, as in a folder that answers late, ends:
    /// no terminal owns it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_shell_that_starts_past_the_limit_ends() {
        use crate::blocking::BlockingError;

        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let shell = pid_shell(temp.path());
        let (inputs, _received) = mpsc::channel(16);
        let (answer, answered) = std_mpsc::channel::<()>();
        let deadline = time::Instant::now() + Duration::from_millis(50);

        let shell_folder = folder.clone();
        let started = blocking::run(deadline, Scope::Folder(&folder), move || {
            let _ = answered.recv();
            let size = TerminalSize { cols: 80, rows: 24 };
            let spawned = spawn(&shell, TerminalId::new(1), &shell_folder, size, inputs);
            // The work returns once the shell runs, so that the test sees its pid first.
            let pid_file = shell_folder.join("shell.pid");
            for _ in 0..1000 {
                if pid_file.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            spawned
        })
        .await;
        answer.send(()).unwrap();
        let pid = shell_pid(&folder).await;

        assert!(matches!(started, Err(BlockingError::TimedOut)));
        assert!(ends(pid).await, "the late shell ended");
    }

    /// The agent closed while its terminal's shell started: the shell ends, and the request
    /// finds no agent.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_shell_whose_agent_closed_meanwhile_ends() {
        use cockpit_protocol::ResponseResult;
        use tokio::sync::oneshot;

        let temp = tempfile::tempdir().unwrap();
        let (mut hub, _inputs, agent) = hub_with_agent(temp.path());
        let folder = hub.agents[&agent].record.folder.clone();
        let shell = pid_shell(temp.path());
        let terminal = hub.terminals.next_id().unwrap();
        let size = TerminalSize { cols: 80, rows: 24 };
        let spawned = spawn(&shell, terminal, &folder, size, hub.inputs.clone());
        let pid = shell_pid(&folder).await;
        hub.close(agent).unwrap();
        let (reply, mut answered) = oneshot::channel();

        hub.terminal_spawned(agent, terminal, spawned.map_err(unsupported), reply);

        let Ok(ResponseResult::Error(gone)) = answered.try_recv() else {
            panic!("expected the terminal to find no agent");
        };
        assert_eq!(gone.code, ErrorCode::NotFound);
        assert!(hub.terminals.open.is_empty());
        assert!(ends(pid).await, "the shell ended");
    }

    /// A terminal of agent 3 whose shell already ended.
    fn ended_terminal() -> Terminal {
        let (input, _typed) = std_mpsc::channel();
        let pair = native_pty_system()
            .openpty(pty_size(TerminalSize { cols: 80, rows: 24 }))
            .unwrap();
        let mut child = pair
            .slave
            .spawn_command(CommandBuilder::new(if cfg!(windows) {
                "cmd.exe"
            } else {
                "true"
            }))
            .unwrap();
        let killer = child.clone_killer();
        // Windows' pseudo console holds the shell until a terminal answers its cursor query, which
        // no test does: the shell is ended, not waited for.
        let _ = child.kill();
        let _ = child.wait();
        Terminal {
            agent: AgentId::new(3),
            title: "sh".to_owned(),
            master: pair.master,
            input,
            killer,
            recent: VecDeque::new(),
            written: 0,
            sizes: VecDeque::from([TerminalSizeAt {
                offset: 0,
                size: SMALL,
            }]),
            exited: Some(TerminalExit { code: Some(0) }),
        }
    }

    const SMALL: TerminalSize = TerminalSize { cols: 80, rows: 24 };
    const WIDE: TerminalSize = TerminalSize {
        cols: 200,
        rows: 24,
    };
    const TALL: TerminalSize = TerminalSize { cols: 80, rows: 60 };

    fn at(offset: u64, size: TerminalSize) -> TerminalSizeAt {
        TerminalSizeAt { offset, size }
    }

    /// Notes that terminal 1's PTY took `size`, as `terminal.resize` does once the PTY resized.
    fn resize(terminals: &mut Terminals, size: TerminalSize) {
        if let Some(open) = terminals.open.get_mut(&TerminalId::new(1)) {
            open.note_size(size);
        }
    }

    fn sizes(terminals: &mut Terminals) -> Vec<TerminalSizeAt> {
        terminals
            .screen(AgentId::new(3), TerminalId::new(1))
            .unwrap()
            .sizes
    }

    #[test]
    fn the_output_kept_stops_at_its_limit_and_knows_where_it_starts() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());
        let chunk = vec![b'a'; RECENT_BYTES / 2 + 1];

        let first = terminals.output(TerminalId::new(1), &chunk);
        terminals.output(TerminalId::new(1), &chunk);
        let Screen {
            offset: start,
            data: screen,
            ..
        } = terminals
            .screen(AgentId::new(3), TerminalId::new(1))
            .unwrap();

        assert!(matches!(
            first.map(|update| update.change),
            Some(TerminalChange::Output { offset: 0, .. })
        ));
        assert_eq!(start, 2);
        assert_eq!(STANDARD.decode(screen).unwrap().len(), RECENT_BYTES);
        assert!(
            terminals
                .screen(AgentId::new(4), TerminalId::new(1))
                .is_err()
        );
    }

    /// The kept output says at which byte the PTY took each size, so a client writes each part at
    /// the size the shell wrote it for.
    #[test]
    fn the_screen_carries_each_size_from_the_byte_it_was_taken_at() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());

        terminals.output(TerminalId::new(1), b"apt");
        resize(&mut terminals, WIDE);
        terminals.output(TerminalId::new(1), b" install");

        assert_eq!(sizes(&mut terminals), [at(0, SMALL), at(3, WIDE)]);
    }

    /// No output was written at a size the PTY left before writing any, and a size the PTY
    /// already has is no change.
    #[test]
    fn a_size_without_output_since_the_last_replaces_it_and_a_repeat_adds_none() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());

        resize(&mut terminals, WIDE);
        resize(&mut terminals, TALL);
        resize(&mut terminals, TALL);
        terminals.output(TerminalId::new(1), b"ls\r\n");
        resize(&mut terminals, SMALL);
        resize(&mut terminals, SMALL);

        assert_eq!(sizes(&mut terminals), [at(0, TALL), at(4, SMALL)]);
    }

    /// Once the kept output starts past a size's change, the sizes before it go, but for the one
    /// the first kept byte was written at.
    #[test]
    fn the_sizes_go_with_the_output_they_were_taken_over() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());
        let chunk = vec![b'a'; RECENT_BYTES / 2];

        terminals.output(TerminalId::new(1), &chunk);
        resize(&mut terminals, WIDE);
        terminals.output(TerminalId::new(1), &chunk);
        resize(&mut terminals, TALL);
        // The kept output now starts one byte past the change to WIDE.
        terminals.output(TerminalId::new(1), &vec![b'b'; RECENT_BYTES / 2 + 1]);

        let half = (RECENT_BYTES / 2) as u64;
        assert_eq!(sizes(&mut terminals), [at(half, WIDE), at(2 * half, TALL)]);
    }

    #[test]
    fn a_terminal_keeps_at_most_its_limit_of_sizes() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());

        for cols in 1..=MAX_SIZES as u16 + 10 {
            terminals.output(TerminalId::new(1), b"a");
            resize(&mut terminals, TerminalSize { cols, rows: 24 });
        }

        let kept = sizes(&mut terminals);
        assert_eq!(kept.len(), MAX_SIZES);
        assert_eq!(
            kept.last().map(|last| last.size.cols),
            Some(MAX_SIZES as u16 + 10)
        );
    }

    /// A PTY has at least one row and one column, so a client that replays the output writes it
    /// at the size the shell had, not at one that no terminal can take.
    #[test]
    fn a_size_without_rows_or_columns_is_kept_as_the_one_the_pty_took() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());

        terminals.output(TerminalId::new(1), b"ls\r\n");
        terminals
            .resize(
                AgentId::new(3),
                TerminalId::new(1),
                TerminalSize { cols: 0, rows: 0 },
            )
            .unwrap();

        let one = TerminalSize { cols: 1, rows: 1 };
        assert_eq!(sizes(&mut terminals), [at(0, SMALL), at(4, one)]);
    }

    #[test]
    fn output_leaves_half_of_a_slow_clients_queue_to_events() {
        let folder = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(folder.path());
        let (client, mut frames) = mpsc::channel(CLIENT_QUEUE);
        hub.clients.push(client);
        hub.terminals
            .open
            .insert(TerminalId::new(1), ended_terminal());

        for _ in 0..CLIENT_QUEUE {
            hub.terminal_output(TerminalId::new(1), b"a");
        }
        hub.terminal_exited(TerminalId::new(1), Some(0));
        let mut received = 0;
        while frames.try_recv().is_ok() {
            received += 1;
        }

        assert_eq!(received, CLIENT_QUEUE / 2 + 1);
        assert_eq!(hub.clients.len(), 1);
    }
}
