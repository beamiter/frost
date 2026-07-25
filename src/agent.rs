//! Agent mode state — a multi-turn LLM that proposes shell commands, watches
//! their output, and iterates. The protocol state machine, provider client,
//! transport, and redaction live in `jterm_core` (shared with jterm1/2/4);
//! this module holds jterm3's UI-facing session driver. The iced view and
//! message routing live in `main.rs`, mirroring `command_palette.rs`.
//!
//! ## Safety model (immutable, by design)
//!
//! 1. **Per-command approval.** Every proposed command renders as an
//!    Approve/Edit/Reject card; nothing reaches the PTY without a click.
//! 2. **Dangerous-command flagging** via `jterm_core::agent::is_dangerous`.
//! 3. **Single session, single binding.** At most one agent session, bound
//!    to the terminal session (stable id) it was opened on.
//! 4. **Turn cap** from `agent_max_turns`.
//! 5. **Strict correlation.** An OSC 133 completion becomes an observation
//!    only when its reconstructed command line ends with the approved
//!    command (the row text includes the shell prompt prefix).

use crate::config::Config;
use crate::terminal::CompletedCommand;
use jterm_core::agent::{AgentSession, AgentState, ProposalId};
use jterm_core::ai::{AiCancellationToken, AiClient, AiSettings, BlockContext};

fn snapshot_path() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("jterm3").join("agent_session.json"))
}

pub fn client_from_config(config: &Config) -> Result<AiClient, String> {
    AiClient::from_settings(&AiSettings {
        enabled: config.ai_enabled,
        provider: config.ai_provider.clone(),
        api_key_file: config.ai_api_key_file.clone(),
        model: config.ai_model.clone(),
        base_url: config.ai_base_url.clone(),
        max_tokens: config.ai_max_tokens,
        redact_secrets: config.ai_redact_secrets,
    })
    .map_err(|error| error.to_string())
}

/// Everything the update loop needs to launch one model request on a
/// background task.
pub struct ModelRequest {
    pub generation: u64,
    pub client: AiClient,
    pub system: String,
    pub user: String,
    pub token: AiCancellationToken,
}

pub struct AgentUi {
    pub is_open: bool,
    pub session: Option<AgentSession>,
    /// Stable session id (not index) this agent session is bound to.
    pub bound_session_id: Option<usize>,
    /// Approved proposal currently executing in the bound session.
    pub awaiting: Option<(ProposalId, String)>,
    /// Most recent command the user ran manually in the bound session while
    /// the panel was open (command text still carries the prompt prefix).
    /// Attached to model requests as untrusted block context.
    pub last_manual_completed: Option<BlockContext>,
    pub input: String,
    /// Proposal being edited inline: (proposal id, buffer).
    pub edit: Option<(ProposalId, String)>,
    pub loading: bool,
    pub status: String,
    pub provider_label: String,
    /// Monotonic id for in-flight model requests; replies carrying a stale
    /// generation (from a closed/replaced session) are dropped.
    generation: u64,
    in_flight: Option<AiCancellationToken>,
}

impl AgentUi {
    pub fn new() -> Self {
        Self {
            is_open: false,
            session: None,
            bound_session_id: None,
            awaiting: None,
            last_manual_completed: None,
            input: String::new(),
            edit: None,
            loading: false,
            status: String::new(),
            provider_label: String::new(),
            generation: 0,
            in_flight: None,
        }
    }

    /// A snapshot persisted by the previous run is restored one-shot and
    /// rebound to the current terminal session.
    pub fn open(&mut self, config: &Config, session_id: usize) {
        self.close_session();
        self.is_open = true;
        self.bound_session_id = Some(session_id);
        self.status.clear();
        let restored = snapshot_path().and_then(|path| {
            let snapshot = jterm_core::agent::read_snapshot_file(&path)?;
            jterm_core::agent::remove_snapshot_file(&path);
            AgentSession::restore(snapshot).ok()
        });
        match restored {
            Some(session) => {
                self.session = Some(session);
                self.status = "restored the previous agent session".to_string();
            }
            None => self.session = Some(AgentSession::new(config.agent_max_turns)),
        }
        match client_from_config(config) {
            Ok(client) => self.provider_label = client.display_name(),
            Err(error) => {
                self.provider_label.clear();
                self.status = error;
            }
        }
    }

    /// Persist the live session (if any) for the next run. Called on app
    /// exit, before the session is dropped.
    pub fn persist(&self) {
        let Some(path) = snapshot_path() else {
            return;
        };
        match self.session.as_ref().and_then(|session| session.snapshot()) {
            Some(snapshot) => {
                if let Err(error) = jterm_core::agent::write_snapshot_file(&path, &snapshot) {
                    log::warn!("agent: could not persist session: {error}");
                }
            }
            None => jterm_core::agent::remove_snapshot_file(&path),
        }
    }

    pub fn close(&mut self) {
        self.close_session();
        self.is_open = false;
    }

    fn close_session(&mut self) {
        if let Some(token) = self.in_flight.take() {
            token.cancel();
        }
        if let Some(session) = self.session.as_mut() {
            session.cancel();
        }
        self.session = None;
        self.bound_session_id = None;
        self.awaiting = None;
        self.last_manual_completed = None;
        self.loading = false;
        self.edit = None;
        // Invalidate any reply still in flight.
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn submit_input(&mut self) {
        let message = self.input.trim().to_string();
        if message.is_empty() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.submit_user(message) {
            Ok(()) => {
                self.input.clear();
                self.status.clear();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    /// When the protocol is waiting on the model and nothing is in flight,
    /// produce the request the update loop should run on a blocking task.
    pub fn next_model_request(&mut self, config: &Config, cwd: Option<&str>) -> Option<ModelRequest> {
        let session = self.session.as_ref()?;
        if self.loading || session.state() != AgentState::AwaitingModel {
            return None;
        }
        let client = match client_from_config(config) {
            Ok(client) => client,
            Err(error) => {
                self.status = error.clone();
                if let Some(session) = self.session.as_mut() {
                    let _ = session.model_failed(error);
                }
                return None;
            }
        };
        self.provider_label = client.display_name();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        // Cached repo probe with a bounded UI wait; None outside a repo.
        let git = cwd.and_then(|cwd| jterm_core::git_meta::read(std::path::Path::new(cwd)));
        let user = jterm_core::ai::agent_user_prompt(
            &session.build_user_prompt(),
            cwd.unwrap_or("."),
            &shell,
            std::env::consts::OS,
            git.as_ref(),
            self.last_manual_completed.as_ref(),
        );
        let token = AiCancellationToken::new();
        self.in_flight = Some(token.clone());
        self.loading = true;
        self.status.clear();
        self.generation = self.generation.wrapping_add(1);
        Some(ModelRequest {
            generation: self.generation,
            client,
            system: jterm_core::ai::build_agent_system_prompt(),
            user,
            token,
        })
    }

    /// Feed a finished model reply back into the protocol. Stale generations
    /// (an older, cancelled request) are ignored.
    pub fn model_reply(&mut self, generation: u64, result: Result<String, String>) {
        if generation != self.generation {
            return;
        }
        self.in_flight = None;
        self.loading = false;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let outcome = match result {
            Ok(raw) => session.accept_model_reply(&raw).map(|_| ()),
            Err(error) => session.model_failed(error).map(|_| ()),
        };
        if let Err(error) = outcome {
            self.status = error.to_string();
        }
    }

    /// Approve a proposal (optionally with an edited command). Returns the
    /// command line to type into the bound session's PTY.
    pub fn approve(&mut self, id: ProposalId, edited: Option<String>) -> Option<String> {
        let session = self.session.as_mut()?;
        let approved = match edited {
            Some(command) => session.edit_and_approve(id, command),
            None => session.approve(id),
        };
        match approved {
            Ok(approved) => {
                self.awaiting = Some((approved.proposal_id, approved.command.clone()));
                self.status.clear();
                Some(approved.command)
            }
            Err(error) => {
                self.status = error.to_string();
                None
            }
        }
    }

    pub fn reject(&mut self, id: ProposalId) {
        if let Some(session) = self.session.as_mut() {
            if let Err(error) = session.reject(id) {
                self.status = error.to_string();
            }
        }
    }

    /// Follow up on a completed task in the same transcript (budget allowing).
    pub fn continue_task(&mut self) {
        self.edit = None;
        self.awaiting = None;
        if let Some(session) = self.session.as_mut() {
            match session.continue_after_completion() {
                Ok(()) => self.status.clear(),
                Err(error) => self.status = error.to_string(),
            }
        }
    }

    /// Drop the finished transcript and start fresh in the same binding.
    pub fn new_task(&mut self) {
        self.edit = None;
        self.awaiting = None;
        if let Some(session) = self.session.as_mut() {
            match session.start_new_task() {
                Ok(()) => self.status.clear(),
                Err(error) => self.status = error.to_string(),
            }
        }
    }

    /// Feed one OSC 133 completion from session `session_id`. The
    /// reconstructed command row includes the prompt prefix, so the approved
    /// command is matched as a suffix of the reported line. A completion that
    /// is not the approved proposal is remembered as the user's most recent
    /// manual command and attached to later model requests as untrusted
    /// block context.
    pub fn handle_completed(&mut self, session_id: usize, completed: &CompletedCommand) {
        const MANUAL_OUTPUT_TRUNCATION_HINT: usize = 256 * 1024;
        if !self.is_open || self.bound_session_id != Some(session_id) {
            return;
        }
        let reported = completed.command.trim();
        if let Some((proposal_id, approved_command)) = self.awaiting.clone() {
            let approved = approved_command.trim();
            if reported == approved || reported.ends_with(approved) {
                if let Some(session) = self.session.as_mut() {
                    match session.observe(
                        proposal_id,
                        completed.exit_code.unwrap_or(0),
                        &completed.output,
                    ) {
                        Ok(()) => self.awaiting = None,
                        Err(error) => self.status = error.to_string(),
                    }
                }
                return;
            }
        }
        if reported.is_empty() {
            return;
        }
        self.last_manual_completed = Some(BlockContext {
            cmd: reported.to_string(),
            output: completed.output.clone(),
            cwd: None,
            exit_code: completed.exit_code.unwrap_or(0),
            truncated: completed.output.len() >= MANUAL_OUTPUT_TRUNCATION_HINT,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ai_config() -> Config {
        let mut config = Config::default();
        config.ai_enabled = true;
        config.ai_provider = "ollama".into();
        config.ai_base_url = "http://localhost:11434".into();
        config.ai_model = "codellama:7b".into();
        config
    }

    fn completed(command: &str, exit: i32, output: &str) -> CompletedCommand {
        CompletedCommand {
            command: command.to_string(),
            exit_code: Some(exit),
            output: output.to_string(),
            id: None,
            output_available: true,
            truncated: false,
            total_bytes: output.len(),
        }
    }

    #[test]
    fn approval_flow_correlates_prompt_prefixed_completions() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 7);
        let session = agent.session.as_mut().unwrap();
        session.submit_user("list files").unwrap();
        let outcome = session
            .accept_model_reply(r#"{"action":"run","command":"ls -la"}"#)
            .unwrap();
        let jterm_core::agent::ModelOutcome::Proposal { id, .. } = outcome else {
            panic!("expected proposal");
        };
        assert_eq!(agent.approve(id, None).as_deref(), Some("ls -la"));

        // Wrong session id: ignored entirely.
        agent.handle_completed(3, &completed("❯ ls -la", 0, "total 0"));
        assert!(agent.awaiting.is_some());
        assert!(agent.last_manual_completed.is_none());
        // A different command in the bound session becomes manual context,
        // not an observation.
        agent.handle_completed(7, &completed("❯ pwd", 0, "/tmp"));
        assert!(agent.awaiting.is_some());
        assert_eq!(
            agent
                .last_manual_completed
                .as_ref()
                .map(|context| context.cmd.as_str()),
            Some("❯ pwd")
        );
        // Prompt-prefixed match observes.
        agent.handle_completed(7, &completed("user@host ❯ ls -la", 0, "total 0"));
        assert!(agent.awaiting.is_none());
        assert_eq!(
            agent.session.as_ref().unwrap().state(),
            AgentState::AwaitingModel
        );
    }

    #[test]
    fn stale_model_replies_are_dropped_after_reopen() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "hello".into();
        agent.submit_input();
        let request = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start");
        assert!(agent.loading);

        // Reopening invalidates the generation; the late reply is ignored.
        agent.open(&ai_config(), 2);
        agent.model_reply(request.generation, Ok("{\"action\":\"say\",\"message\":\"hi\"}".into()));
        assert_eq!(agent.session.as_ref().unwrap().transcript().len(), 0);
    }

    #[test]
    fn disabled_ai_reports_a_configuration_status() {
        let mut agent = AgentUi::new();
        agent.open(&Config::default(), 0);
        assert!(agent.status.contains("disabled"));
    }
}
