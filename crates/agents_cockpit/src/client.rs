//! The client commands: each sends one request to the daemon, except `tail`, which follows its
//! events.

use std::io;
use std::path::PathBuf;

use anyhow::bail;
#[cfg(unix)]
use cockpit_client::BRIDGE_SUBCOMMAND;
use cockpit_client::{Backoff, Connection, Session, SessionUpdate, Transport};
use cockpit_protocol::{
    AccountId, AgentId, AgentParams, Answer, AnswerParams, ApprovalAnswer, ApprovalDecision,
    Catchup, Effort, Event, MoveParams, PermissionMode, PlanAnswer, PlanDecision, Prompt,
    PromptParams, QuestionAnswer, RenameParams, Request, ResponseBody, StartParams,
};
use serde::Serialize;
use serde_json::Value;

#[cfg(unix)]
use crate::cli::SERVE_SUBCOMMAND;
use crate::cli::{AnswerCommand, Command, PROGRAM, SSH_FLAG, Target};

/// The argument that makes a command read its text from stdin.
const STDIN_ARG: &str = "-";
/// The width of `list`'s title column; a longer title is shortened.
const TITLE_COLUMN: usize = 32;
/// What `list` prints in place of a control character.
const CONTROL_REPLACEMENT: char = '\u{fffd}';

/// Whether stdout still takes lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stdout {
    Open,
    /// Its reader left, as after `| head`: not an error, but nothing more to print.
    Closed,
}

pub(crate) async fn run(target: &Target, command: Command) -> anyhow::Result<()> {
    let transport = target.transport()?;
    let request = match command {
        Command::List => return list(&transport).await,
        Command::Tail { agent } => return tail(transport, agent).await,
        #[cfg(unix)]
        Command::Serve | Command::Bridge => {
            bail!("{SERVE_SUBCOMMAND} and {BRIDGE_SUBCOMMAND} run on their own")
        }
        Command::Start {
            folder,
            prompt,
            title,
            model,
            mode,
            effort,
        } => Request::AgentStart(StartParams {
            folder: agent_folder(target, folder)?,
            title,
            model,
            effort: effort.map(Effort::from),
            mode: mode.map(PermissionMode::from),
            prompt: prompt
                .map(|text| read_text(text).map(text_prompt))
                .transpose()?,
            account: AccountId::DEFAULT,
        }),
        Command::Prompt { agent, text } => Request::AgentPrompt(PromptParams {
            agent,
            prompt: text_prompt(read_text(text)?),
        }),
        Command::Answer {
            agent,
            pending,
            answer,
        } => Request::AgentAnswer(AnswerParams {
            agent,
            pending,
            answer: answer_of(answer),
        }),
        Command::Interrupt { agent } => Request::AgentInterrupt(AgentParams { agent }),
        Command::Stop { agent } => Request::AgentStop(AgentParams { agent }),
        Command::Close { agent } => Request::AgentClose(AgentParams { agent }),
        Command::Rename { agent, title } => Request::AgentRename(RenameParams { agent, title }),
        Command::Move { agent, before } => Request::AgentMove(MoveParams { agent, before }),
    };
    let (connection, _) = Connection::connect(&transport, None).await?;
    match connection.request(request).await? {
        ResponseBody::AgentStarted { agent } => {
            print_line(&agent.get().to_string())?;
        }
        ResponseBody::Done => {}
        other => {
            print_line(&serde_json::to_string(&other)?)?;
        }
    }
    Ok(())
}

async fn list(transport: &Transport) -> anyhow::Result<()> {
    let (_, catchup) = Connection::connect(transport, None).await?;
    let Catchup::Snapshot(snapshot) = catchup else {
        bail!("the daemon sent no snapshot");
    };
    let header = format!(
        "{:<4} {:<9} {:<4} {:<5} {:<TITLE_COLUMN$} FOLDER",
        "ID", "STATUS", "WAIT", "TURNS", "TITLE"
    );
    if print_line(&header)? == Stdout::Closed {
        return Ok(());
    }
    let mut agents = snapshot.agents;
    agents.sort_by(|left, right| cockpit_protocol::list_order(left.place(), right.place()));
    for agent in agents {
        let row = format!(
            "{:<4} {:<9} {:<4} {:<5} {:<TITLE_COLUMN$} {}",
            agent.id.get(),
            word(agent.status)?,
            agent.pending_count,
            agent.turns,
            shorten(&printable(&agent.title), TITLE_COLUMN),
            printable(&agent.folder.display().to_string())
        );
        if print_line(&row)? == Stdout::Closed {
            return Ok(());
        }
    }
    Ok(())
}

/// Prints events as JSON lines until interrupted or stdout closes, reconnecting after a drop.
async fn tail(transport: Transport, agent: Option<AgentId>) -> anyhow::Result<()> {
    let mut session = Session::start(transport, Backoff::default());
    while let Some(update) = session.next_update().await {
        match update {
            SessionUpdate::Event(record) => {
                if (agent.is_none() || event_agent(&record.event) == agent)
                    && print_line(&serde_json::to_string(&record)?)? == Stdout::Closed
                {
                    return Ok(());
                }
            }
            SessionUpdate::Disconnected { error, retry_in } => {
                eprintln!("{PROGRAM}: disconnected ({error}); retrying in {retry_in:?}");
            }
            SessionUpdate::Connecting { .. } | SessionUpdate::Connected(_) => {}
        }
    }
    Ok(())
}

fn event_agent(event: &Event) -> Option<AgentId> {
    match event {
        Event::AgentAdded(summary) | Event::AgentUpdated(summary) => Some(summary.id),
        Event::AgentRemoved { agent }
        | Event::TranscriptAppended { agent, .. }
        | Event::TranscriptChanged { agent, .. }
        | Event::DetailsChanged { agent, .. } => Some(*agent),
        Event::PlanUsageChanged(_)
        | Event::SettingsChanged(_)
        | Event::AccountPlanUsageChanged(_)
        | Event::AccountsChanged { .. } => None,
    }
}

fn answer_of(answer: AnswerCommand) -> Answer {
    match answer {
        AnswerCommand::Allow => Answer::Approval(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        }),
        AnswerCommand::Deny { message } => Answer::Approval(ApprovalAnswer {
            decision: ApprovalDecision::Deny,
            message,
        }),
        AnswerCommand::Question { answers } => Answer::Question {
            answers: answers
                .into_iter()
                .map(|answer| QuestionAnswer {
                    selected: vec![answer],
                    other: None,
                })
                .collect(),
        },
        AnswerCommand::Plan { decision, message } => Answer::Plan(PlanAnswer {
            decision: PlanDecision::from(decision),
            message,
        }),
    }
}

/// The daemon needs an absolute folder. A relative one is resolved here only for a local daemon:
/// this machine's working folder means nothing on an SSH host.
fn agent_folder(target: &Target, folder: PathBuf) -> anyhow::Result<PathBuf> {
    use anyhow::Context;

    if folder.is_absolute() {
        return Ok(folder);
    }
    if target.is_remote() {
        bail!(
            "give an absolute folder with --{SSH_FLAG}: {}",
            folder.display()
        );
    }
    std::path::absolute(&folder).with_context(|| format!("cannot resolve {}", folder.display()))
}

fn read_text(text: String) -> anyhow::Result<String> {
    use std::io::Read;

    use anyhow::Context;

    if text != STDIN_ARG {
        return Ok(text);
    }
    let mut read = String::new();
    io::stdin()
        .read_to_string(&mut read)
        .context("cannot read the prompt from stdin")?;
    Ok(read)
}

fn text_prompt(text: String) -> Prompt {
    Prompt {
        text,
        images: Vec::new(),
    }
}

/// Text from the daemon with its control characters replaced, since a terminal acts on them.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                CONTROL_REPLACEMENT
            } else {
                c
            }
        })
        .collect()
}

fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// A protocol word, such as a status, as the daemon writes it.
fn word<T: Serialize>(value: T) -> anyhow::Result<String> {
    match serde_json::to_value(value)? {
        Value::String(word) => Ok(word),
        other => Ok(other.to_string()),
    }
}

/// Prints a line. A closed stdout, as after `| head`, is not an error.
fn print_line(line: &str) -> anyhow::Result<Stdout> {
    use std::io::Write;

    let mut stdout = io::stdout().lock();
    match writeln!(stdout, "{line}") {
        Ok(()) => Ok(Stdout::Open),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(Stdout::Closed),
        Err(error) => Err(error.into()),
    }
}
