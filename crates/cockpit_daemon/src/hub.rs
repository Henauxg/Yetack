//! The hub: one task that owns every agent, numbers the events, logs them, and sends them to every
//! connected client.

mod agents;
mod control;
mod errors;
mod history;
mod input;
mod requests;
mod restore;
mod state;

pub(crate) use input::{HubInput, Joined, Registration};
pub(crate) use restore::restore;
pub(crate) use state::Hub;
