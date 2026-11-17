//! Terminals: shells on a PTY in an agent's folder, run as the daemon's user, as the agents are.
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
    AgentId, ErrorBody, ErrorCode, Frame, ResponseBody, ServerMessage, TerminalChange,
    TerminalExit, TerminalId, TerminalInfo, TerminalInputParams, TerminalOpenParams,
    TerminalParams, TerminalResizeParams, TerminalSize, TerminalUpdate,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::hub::input::HubInput;
use crate::hub::state::{CLIENT_QUEUE, Hub};

/// The output a terminal keeps for a client that shows it again: a few screens of a busy build.
const RECENT_BYTES: usize = 512 * 1024;

/// What one read of a shell's output takes at most.
const READ_BYTES: usize = 64 * 1024;

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
    /// `Some` once the shell ended.
    exited: Option<TerminalExit>,
}

impl Terminals {
    /// Starts the user's shell in `folder` on a PTY of `size`. Returns its id and title.
    pub(super) fn open(
        &mut self,
        agent: AgentId,
        folder: &Path,
        size: TerminalSize,
        inputs: &mpsc::Sender<HubInput>,
    ) -> Result<(TerminalId, String), String> {
        let terminal = match self.last {
            Some(last) => last.next().ok_or("no terminal id left")?,
            None => TerminalId::new(1),
        };
        let pair = native_pty_system()
            .openpty(pty_size(size))
            .map_err(|error| format!("cannot open a terminal: {error}"))?;
        let (program, title) = shell();
        let mut command = CommandBuilder::new(&program);
        command.cwd(folder);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // The shell is the user's, not one Claude Code runs.
        command.env_remove("CLAUDECODE");
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("cannot start {program}: {error}"))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("cannot read the terminal: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("cannot write to the terminal: {error}"))?;
        let (input, typed) = std_mpsc::channel();
        spawn_thread(
            "terminal-read",
            read_output(terminal, reader, inputs.clone()),
        )?;
        spawn_thread("terminal-write", write_input(writer, typed))?;
        let exits = inputs.clone();
        spawn_thread("terminal-wait", move || {
            let code = child
                .wait()
                .ok()
                .filter(|status| status.signal().is_none())
                .and_then(|status| i32::try_from(status.exit_code()).ok());
            // The hub may have stopped meanwhile.
            let _ = exits.blocking_send(HubInput::TerminalExited { terminal, code });
        })?;
        self.last = Some(terminal);
        self.open.insert(
            terminal,
            Terminal {
                agent,
                title: title.clone(),
                master: pair.master,
                input,
                killer,
                recent: VecDeque::new(),
                written: 0,
                exited: None,
            },
        );
        Ok((terminal, title))
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
            .map_err(|error| format!("cannot resize the terminal: {error}"))
    }

    /// The output kept, base64, and where it starts in everything the shell wrote.
    pub(super) fn screen(
        &mut self,
        agent: AgentId,
        terminal: TerminalId,
    ) -> Result<(u64, String), String> {
        let open = self.of_agent(agent, terminal)?;
        let (front, back) = open.recent.as_slices();
        let mut bytes = Vec::with_capacity(open.recent.len());
        bytes.extend_from_slice(front);
        bytes.extend_from_slice(back);
        let start = open.written - bytes.len() as u64;
        Ok((start, STANDARD.encode(bytes)))
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

fn pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The user's shell and its name.
fn shell() -> (String, String) {
    #[cfg(unix)]
    let program = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned());
    #[cfg(windows)]
    let program = std::env::var("COMSPEC")
        .ok()
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "cmd.exe".to_owned());
    let title = Path::new(&program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("shell")
        .to_owned();
    (program, title)
}

fn spawn_thread(name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map(drop)
        .map_err(|error| format!("cannot start a thread for the terminal: {error}"))
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
    pub(super) fn open_terminal(
        &mut self,
        params: TerminalOpenParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let folder = self.agent(params.agent)?.record.folder.clone();
        let (terminal, title) = self
            .terminals
            .open(params.agent, &folder, params.size, &self.inputs)
            .map_err(|message| ErrorBody::new(ErrorCode::Unsupported, message))?;
        self.send_terminal(TerminalUpdate {
            agent: params.agent,
            terminal,
            change: TerminalChange::Opened { title },
        });
        Ok(ResponseBody::TerminalOpened { terminal })
    }

    pub(super) fn list_terminals(&self, agent: AgentId) -> Result<ResponseBody, ErrorBody> {
        self.agent(agent)?;
        Ok(ResponseBody::Terminals {
            terminals: self.terminals.list(agent),
        })
    }

    pub(super) fn attach_terminal(
        &mut self,
        params: TerminalParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let (offset, data) = self
            .terminals
            .screen(params.agent, params.terminal)
            .map_err(not_found)?;
        Ok(ResponseBody::TerminalScreen { offset, data })
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let _ = child.wait();
        Terminal {
            agent: AgentId::new(3),
            title: "sh".to_owned(),
            master: pair.master,
            input,
            killer,
            recent: VecDeque::new(),
            written: 0,
            exited: Some(TerminalExit { code: Some(0) }),
        }
    }

    #[test]
    fn the_output_kept_stops_at_its_limit_and_knows_where_it_starts() {
        let mut terminals = Terminals::default();
        terminals.open.insert(TerminalId::new(1), ended_terminal());
        let chunk = vec![b'a'; RECENT_BYTES / 2 + 1];

        let first = terminals.output(TerminalId::new(1), &chunk);
        terminals.output(TerminalId::new(1), &chunk);
        let (start, screen) = terminals
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
