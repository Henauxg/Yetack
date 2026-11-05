//! The client commands: each sends one request to the daemon, except `tail`, which follows its
//! events.

use std::io;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, bail};
use cockpit_client::{Backoff, Connection, Session, SessionUpdate, Transport};
use cockpit_protocol::{
    AgentId, AgentParams, Answer, AnswerParams, ApprovalAnswer, ApprovalDecision, Catchup, Event,
    PendingId, PlanAnswer, Prompt, PromptParams, QuestionAnswer, Request, ResponseBody,
    StartParams,
};
use serde::Serialize;

use crate::cli::{AnswerCommand, Command, Target};

/// The argument that makes a command read its text from stdin.
const STDIN_ARG: &str = "-";
/// The width of `list`'s title column; a longer title is shortened.
const TITLE_COLUMN: usize = 32;

pub(crate) async fn run(target: &Target, command: Command) -> anyhow::Result<()> {
    let transport = target.transport()?;
    let request = match command {
        Command::List => return list(&transport).await,
        Command::Tail { agent } => return tail(transport, agent.map(AgentId::new)).await,
        Command::Serve | Command::Bridge => bail!("serve and bridge run on their own"),
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
            effort: effort.map(|effort| effort.to_protocol()),
            mode: mode.map(|mode| mode.to_protocol()),
            prompt: prompt
                .map(|text| read_text(text).map(text_prompt))
                .transpose()?,
        }),
        Command::Prompt { agent, text } => Request::AgentPrompt(PromptParams {
            agent: AgentId::new(agent),
            prompt: text_prompt(read_text(text)?),
        }),
        Command::Answer {
            agent,
            pending,
            answer,
        } => Request::AgentAnswer(AnswerParams {
            agent: AgentId::new(agent),
            pending: PendingId::new(pending),
            answer: answer_of(answer),
        }),
        Command::Interrupt { agent } => Request::AgentInterrupt(AgentParams {
            agent: AgentId::new(agent),
        }),
        Command::Stop { agent } => Request::AgentStop(AgentParams {
            agent: AgentId::new(agent),
        }),
    };
    let (connection, _) = Connection::connect(&transport, None).await?;
    match connection.request(request).await? {
        ResponseBody::AgentStarted { agent } => print_line(&agent.get().to_string())?,
        ResponseBody::Done => {}
        other => print_line(&serde_json::to_string(&other)?)?,
    }
    Ok(())
}

async fn list(transport: &Transport) -> anyhow::Result<()> {
    let (_, catchup) = Connection::connect(transport, None).await?;
    let Catchup::Snapshot(snapshot) = catchup else {
        bail!("the daemon sent no snapshot");
    };
    print_line(&format!(
        "{:<4} {:<9} {:<4} {:<5} {:<TITLE_COLUMN$} FOLDER",
        "ID", "STATUS", "WAIT", "TURNS", "TITLE"
    ))?;
    for agent in snapshot.agents {
        print_line(&format!(
            "{:<4} {:<9} {:<4} {:<5} {:<TITLE_COLUMN$} {}",
            agent.id.get(),
            word(agent.status)?,
            agent.pending_count,
            agent.turns,
            shorten(&agent.title, TITLE_COLUMN),
            agent.folder.display()
        ))?;
    }
    Ok(())
}

/// Prints events as JSON lines until interrupted, reconnecting after a drop.
async fn tail(transport: Transport, agent: Option<AgentId>) -> anyhow::Result<()> {
    let mut session = Session::start(transport, Backoff::default());
    while let Some(update) = session.next_update().await {
        match update {
            SessionUpdate::Event(record) => {
                if agent.is_none() || event_agent(&record.event) == agent {
                    print_line(&serde_json::to_string(&record)?)?;
                }
            }
            SessionUpdate::Disconnected { error, retry_in } => {
                eprintln!("agents-cockpit: disconnected ({error}); retrying in {retry_in:?}");
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
        Event::PlanUsageChanged(_) | Event::SettingsChanged(_) => None,
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
            decision: decision.to_protocol(),
            message,
        }),
    }
}

/// The daemon needs an absolute folder. A relative one is resolved here only for a local daemon:
/// this machine's working folder means nothing on an SSH host.
fn agent_folder(target: &Target, folder: PathBuf) -> anyhow::Result<PathBuf> {
    if folder.is_absolute() {
        return Ok(folder);
    }
    if target.is_remote() {
        bail!("give an absolute folder with --ssh: {}", folder.display());
    }
    std::path::absolute(&folder).with_context(|| format!("cannot resolve {}", folder.display()))
}

fn read_text(text: String) -> anyhow::Result<String> {
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
        serde_json::Value::String(word) => Ok(word),
        other => Ok(other.to_string()),
    }
}

/// Prints a line. A closed stdout, as after `| head`, is not an error.
fn print_line(line: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    match writeln!(stdout, "{line}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}
