//! What changed in the daemon. Every connected client gets every event.

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::agent::AgentSummary;
use crate::details::{AgentDetails, SlashCommand};
use crate::details_change::DetailsChange;
use crate::ids::{AgentId, Seq};
use crate::settings::Settings;
use crate::transcript::TranscriptItem;
use crate::usage::{AccountPlanUsage, PlanUsage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AgentAdded(AgentSummary),
    /// Replaces the agent's summary.
    AgentUpdated(AgentSummary),
    AgentRemoved {
        agent: AgentId,
    },
    TranscriptAppended {
        agent: AgentId,
        item: TranscriptItem,
    },
    /// Replaces the item with the same id, such as a tool call that finished.
    TranscriptChanged {
        agent: AgentId,
        item: TranscriptItem,
    },
    /// Replaces the agent's details whole, its subagents included. The daemon sends it as the
    /// agent's first details event, and a replay sends the latest details this way, in place of
    /// the events that changed them.
    DetailsChanged {
        agent: AgentId,
        details: AgentDetails,
    },
    /// Changes the parts of the agent's details that changed since its last details event.
    DetailsUpdated {
        agent: AgentId,
        changes: Vec<DetailsChange>,
    },
    /// Replaces the agent's slash commands and skills, for the palette.
    CommandsChanged {
        agent: AgentId,
        commands: Vec<SlashCommand>,
    },
    /// The default account's plan usage changed. An app that predates accounts reads this one
    /// alone.
    PlanUsageChanged(PlanUsage),
    SettingsChanged(Settings),
    /// The plan usage of an account other than the default one changed.
    AccountPlanUsageChanged(AccountPlanUsage),
    /// Replaces the accounts. The daemon also sends it when an account signs in or out, so that
    /// clients ask `accounts.list` again.
    AccountsChanged {
        accounts: Vec<Account>,
    },
}

/// An event with its sequence number, as the daemon sends it and logs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: Seq,
    pub event: Event,
}
