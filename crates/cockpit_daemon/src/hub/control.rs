//! The daemon's control requests to Claude Code, and their answers.

use claude_protocol::{
    ContextUsage as ClaudeContextUsage, ControlRequest, InitializeResponse, McpStatus, ParseError,
    SettingsAnswer,
};
use cockpit_protocol::{
    AgentId, ErrorBody, ErrorCode, ResponseBody, SetModeParams, SetModelParams,
};
use serde_json::Value;

use crate::hub::input::{ControlPurpose, Reply};
use crate::hub::state::Hub;
use crate::hub::{agents, errors};
use crate::process::ControlFailure;
use crate::transcript::words;

impl ControlPurpose {
    /// Answers the client waiting for this request, if any, with `error`.
    pub(super) fn fail(self, error: ErrorBody) {
        match self {
            Self::Done(reply)
            | Self::Interrupt(reply)
            | Self::SetModel { reply, .. }
            | Self::ContextUsage(reply)
            | Self::McpChanged(reply)
            | Self::SetMode {
                reply: Some(reply), ..
            }
            | Self::McpStatus(Some(reply)) => agents::respond(reply, Err(error)),
            Self::Initialize
            | Self::ContextRefresh
            | Self::DefaultEffort
            | Self::Closing
            | Self::SetMode { reply: None, .. }
            | Self::McpStatus(None) => {}
        }
    }
}

impl Hub {
    /// Sends a control request to an agent's `claude`. The answer comes back to
    /// [`Hub::control_answered`].
    pub(super) fn control(
        &mut self,
        id: AgentId,
        request: ControlRequest,
        purpose: ControlPurpose,
    ) {
        let Some(agent) = self.agents.get(&id) else {
            purpose.fail(errors::agent_not_found(id));
            return;
        };
        let Some(process) = &agent.process else {
            purpose.fail(errors::agent_exited(id));
            return;
        };
        if let Err(purpose) = process.control(request, purpose) {
            purpose.fail(errors::agent_exited(id));
        }
    }

    pub(super) fn control_answered(
        &mut self,
        id: AgentId,
        purpose: ControlPurpose,
        result: Result<Value, ControlFailure>,
    ) {
        let Some(agent) = self.agents.get_mut(&id) else {
            purpose.fail(errors::agent_not_found(id));
            return;
        };
        let view = &mut agent.view;
        let mut record_changed = false;
        let mut refresh_mcp = false;
        let mut refresh_context = false;
        let mut rename_session = false;
        let mut refresh_effort = false;
        match purpose {
            ControlPurpose::Initialize => {
                rename_session = view.initialized();
                refresh_effort = agent.record.effort.is_none();
                match parse::<InitializeResponse>(result) {
                    Ok(response) => view.set_commands(&response),
                    Err(error) => tracing::warn!(agent = id.get(), error, "initialize failed"),
                }
                // Claude Code answers both before the first prompt.
                refresh_mcp = true;
                refresh_context = true;
            }
            ControlPurpose::ContextRefresh => match parse::<ClaudeContextUsage>(result) {
                Ok(usage) => {
                    view.set_context(&usage);
                }
                Err(error) => tracing::debug!(agent = id.get(), error, "no context usage"),
            },
            // An effort given to the agent meanwhile stays.
            ControlPurpose::DefaultEffort if agent.record.effort.is_some() => {}
            ControlPurpose::DefaultEffort => match parse::<SettingsAnswer>(result) {
                Ok(answer) => {
                    let effort = answer.applied.effort.as_deref();
                    view.set_default_effort(effort.and_then(words::effort_from_word));
                }
                Err(error) => {
                    tracing::debug!(agent = id.get(), error, "no settings");
                    view.set_default_effort(None);
                }
            },
            ControlPurpose::Done(reply) => {
                let result = result
                    .map(|_| ResponseBody::Done)
                    .map_err(|failure| errors::control_failed(id, failure));
                agents::respond(reply, result);
            }
            ControlPurpose::Interrupt(reply) => {
                // A sent prompt Claude Code does not hold any more keeps no turn coming: without
                // this, a prompt the daemon missed in an echo would keep the agent working.
                let holds_none = result.as_ref().is_ok_and(|body| {
                    body.get("still_queued")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                });
                if holds_none {
                    view.release_unheld_prompts();
                }
                let result = result
                    .map(|_| ResponseBody::Done)
                    .map_err(|failure| errors::control_failed(id, failure));
                agents::respond(reply, result);
            }
            ControlPurpose::SetModel { model, reply } => match result {
                Ok(_) => {
                    view.set_model(model.clone());
                    agent.record.model = Some(model);
                    record_changed = true;
                    // The model has its own default effort.
                    refresh_effort = agent.record.effort.is_none();
                    agents::respond(reply, Ok(ResponseBody::Done));
                }
                Err(failure) => agents::respond(reply, Err(errors::control_failed(id, failure))),
            },
            ControlPurpose::SetMode { mode, reply } => {
                let result = match result {
                    Ok(_) => {
                        view.set_mode(mode);
                        agent.record.mode = Some(mode);
                        record_changed = true;
                        Ok(ResponseBody::Done)
                    }
                    Err(failure) => Err(errors::control_failed(id, failure)),
                };
                match reply {
                    Some(reply) => agents::respond(reply, result),
                    None => {
                        if let Err(error) = result {
                            tracing::warn!(agent = id.get(), error.message, "mode change failed");
                        }
                    }
                }
            }
            ControlPurpose::ContextUsage(reply) => {
                let result = parse_for_client::<ClaudeContextUsage>(id, result)
                    .map(|usage| ResponseBody::ContextUsage(view.set_context(&usage)));
                agents::respond(reply, result);
            }
            ControlPurpose::McpStatus(reply) => {
                let result = parse_for_client::<McpStatus>(id, result).map(|status| {
                    ResponseBody::McpServers {
                        servers: view.set_mcp_status(&status),
                    }
                });
                match reply {
                    Some(reply) => agents::respond(reply, result),
                    None => {
                        if let Err(error) = result {
                            tracing::debug!(agent = id.get(), error.message, "no MCP status");
                        }
                    }
                }
            }
            ControlPurpose::McpChanged(reply) => {
                refresh_mcp = result.is_ok();
                let result = result
                    .map(|_| ResponseBody::Done)
                    .map_err(|failure| errors::control_failed(id, failure));
                agents::respond(reply, result);
            }
            // A closed agent leaves the hub at once: its answers go to `fail`.
            ControlPurpose::Closing => {}
        }
        if record_changed {
            self.save_agents();
        }
        if rename_session {
            self.rename_session(id);
        }
        if refresh_effort {
            self.control(
                id,
                ControlRequest::GetSettings,
                ControlPurpose::DefaultEffort,
            );
        }
        if refresh_mcp {
            self.control(
                id,
                ControlRequest::McpStatus,
                ControlPurpose::McpStatus(None),
            );
        }
        if refresh_context {
            self.control(
                id,
                ControlRequest::GetContextUsage,
                ControlPurpose::ContextRefresh,
            );
        }
        self.publish(id);
    }

    /// Asks the running `claude`, or records the model for the next start.
    pub(super) fn set_model(&mut self, params: SetModelParams, reply: Reply) {
        if let Err(error) = agents::check_model(&params.model) {
            agents::respond(reply, Err(error));
            return;
        }
        let id = params.agent;
        let Some(agent) = self.agents.get_mut(&id) else {
            agents::respond(reply, Err(errors::agent_not_found(id)));
            return;
        };
        if agent.process.is_some() {
            let request = ControlRequest::SetModel {
                model: Some(params.model.clone()),
            };
            let purpose = ControlPurpose::SetModel {
                model: params.model,
                reply,
            };
            self.control(id, request, purpose);
            return;
        }
        agent.view.set_model(params.model.clone());
        agent.record.model = Some(params.model);
        if agent.record.effort.is_none() {
            // The new model's default is known once the agent starts.
            agent.view.set_default_effort(None);
        }
        self.save_agents();
        self.publish(id);
        agents::respond(reply, Ok(ResponseBody::Done));
    }

    /// Asks the running `claude`, or records the mode for the next start.
    pub(super) fn set_mode(&mut self, params: SetModeParams, reply: Reply) {
        let id = params.agent;
        let Some(agent) = self.agents.get_mut(&id) else {
            agents::respond(reply, Err(errors::agent_not_found(id)));
            return;
        };
        if agent.process.is_some() {
            let request = ControlRequest::SetPermissionMode {
                mode: words::mode_to_claude(params.mode),
            };
            let purpose = ControlPurpose::SetMode {
                mode: params.mode,
                reply: Some(reply),
            };
            self.control(id, request, purpose);
            return;
        }
        agent.view.set_mode(params.mode);
        agent.record.mode = Some(params.mode);
        self.save_agents();
        self.publish(id);
        agents::respond(reply, Ok(ResponseBody::Done));
    }
}

/// Reads an answer's body, or says why there is none.
fn parse<T>(result: Result<Value, ControlFailure>) -> Result<T, String>
where
    T: for<'body> TryFrom<&'body Value, Error = ParseError>,
{
    let body = result.map_err(|failure| format!("{failure:?}"))?;
    T::try_from(&body).map_err(|error| error.to_string())
}

fn parse_for_client<T>(id: AgentId, result: Result<Value, ControlFailure>) -> Result<T, ErrorBody>
where
    T: for<'body> TryFrom<&'body Value, Error = ParseError>,
{
    let body = result.map_err(|failure| errors::control_failed(id, failure))?;
    T::try_from(&body).map_err(|error| ErrorBody::new(ErrorCode::ClaudeFailed, error.to_string()))
}
