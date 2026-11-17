//! The hub: one task that owns every agent, numbers the events, logs them, and sends them to every
//! connected client.

mod accounts;
mod agents;
mod control;
mod errors;
mod folders;
mod history;
mod input;
mod order;
mod requests;
mod restore;
mod state;
mod terminals;

pub(crate) use input::{HubInput, Joined, Registration};
pub(crate) use restore::{Restored, restore};
pub(crate) use state::Hub;
