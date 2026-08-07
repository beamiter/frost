//! Agent mode state — a multi-turn LLM that proposes shell commands, watches
//! their output, and iterates. The protocol state machine, provider client,
//! transport, and redaction live in `jterm_core` (shared with anvil/2/4);
//! this module holds frost's UI-facing session driver. The iced view and
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
//!    only when it carries the locally armed one-shot approval generation and
//!    its prompt-excluded command text exactly equals the reviewed command.

use crate::config::Config;
use crate::terminal::CompletedCommand;
use jterm_core::agent::{
    AgentSession, AgentSessionEpoch, AgentSessionSnapshot, AgentSnapshotError, AgentState,
    ModelOutcome, ProposalId, ProposalStatus, Turn, MAX_AGENT_SNAPSHOT_JSON_BYTES,
};
use jterm_core::ai::{AiCancellationToken, AiClient, BlockContext, Provider};
use std::path::Path;

const MAX_AGENT_MODEL_REPLY_BYTES: usize = 128 * 1024;

fn snapshot_path() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("frost").join("agent_session.json"))
}

/// Atomically claim the persisted snapshot and consume it into a session.
///
/// A read followed by a separate delete lets two windows opening at the same
/// moment both restore the same transcript, and loses the session entirely if
/// the process dies between the two calls. Claiming first means exactly one
/// caller ever sees the file. Evidence that cannot become a session is left at
/// the claim path instead of being deleted, so a corrupt or hostile snapshot
/// stays available for inspection and is never restored by a later opener.
fn claim_snapshot_session(path: &Path) -> Option<AgentSession> {
    let claimed = crate::persistence::claim_exclusive(path).ok()?;
    let restored =
        crate::persistence::read_text_bounded(&claimed, MAX_AGENT_SNAPSHOT_JSON_BYTES as u64)
            .ok()
            .and_then(|encoded| AgentSessionSnapshot::from_json(&encoded).ok())
            .and_then(|snapshot| restore_snapshot_session(snapshot).ok());
    match restored {
        Some(session) => {
            let _ = std::fs::remove_file(&claimed);
            Some(session)
        }
        None => {
            log::warn!(
                "agent: quarantined an unusable session snapshot at {}",
                claimed.display()
            );
            None
        }
    }
}

/// Read an Agent snapshot through frost's descriptor-validated, bounded
/// persistence path. Any unsafe, corrupt, or missing entry simply falls back
/// to a fresh session. Production restores go through [`claim_snapshot_session`],
/// which claims the file before reading it.
#[cfg(test)]
fn read_snapshot_file(path: &Path) -> Option<AgentSessionSnapshot> {
    let encoded =
        crate::persistence::read_text_bounded(path, MAX_AGENT_SNAPSHOT_JSON_BYTES as u64).ok()?;
    AgentSessionSnapshot::from_json(&encoded).ok()
}

fn restore_snapshot_session(
    snapshot: AgentSessionSnapshot,
) -> Result<AgentSession, AgentSnapshotError> {
    let session = AgentSession::restore(snapshot)?;
    let mut proposal_ids = std::collections::HashSet::new();
    let mut pending = Vec::new();
    for turn in session.transcript() {
        if let Turn::AssistantProposed {
            id,
            command,
            status,
        } = turn
        {
            if crate::review_text::validate_single_line(
                command,
                crate::review_text::MAX_AGENT_COMMAND_BYTES,
            )
            .is_err()
            {
                return Err(AgentSnapshotError::Invalid(
                    "proposal command is unsafe to display or execute",
                ));
            }
            if !proposal_ids.insert(id.get()) {
                return Err(AgentSnapshotError::Invalid("duplicate proposal id"));
            }
            if *status == ProposalStatus::Pending {
                pending.push(*id);
            }
        }
    }
    match session.state() {
        AgentState::AwaitingApproval { proposal_id } if pending.as_slice() == [proposal_id] => {}
        AgentState::AwaitingApproval { .. } => {
            return Err(AgentSnapshotError::Invalid(
                "pending proposal state does not match transcript",
            ));
        }
        _ if !pending.is_empty() => {
            return Err(AgentSnapshotError::Invalid(
                "pending proposal exists outside approval state",
            ));
        }
        _ => {}
    }
    Ok(session)
}

fn proposal_command(session: &AgentSession, proposal_id: ProposalId) -> Option<&str> {
    session.transcript().iter().find_map(|turn| match turn {
        Turn::AssistantProposed { id, command, .. } if *id == proposal_id => Some(command.as_str()),
        _ => None,
    })
}

fn accept_model_reply_compat(session: &mut AgentSession, raw: &str) -> Result<(), String> {
    let checkpoint = session.snapshot();
    let outcome = session
        .accept_model_reply(raw)
        .map_err(|error| error.to_string())?;
    let ModelOutcome::Proposal { command, .. } = outcome else {
        return Ok(());
    };
    let Err(error) = crate::review_text::validate_single_line(
        &command,
        crate::review_text::MAX_AGENT_COMMAND_BYTES,
    ) else {
        return Ok(());
    };

    let message = format!("model proposal rejected before display: {error}");
    if let Some(snapshot) = checkpoint {
        match restore_snapshot_session(snapshot) {
            Ok(mut restored) => {
                let _ = restored.model_failed(message.clone());
                *session = restored;
            }
            Err(_) => session.cancel(),
        }
    } else {
        session.cancel();
    }
    Err(message)
}

/// Serialize and atomically replace an Agent snapshot under the exact jagent
/// byte budget, avoiding the pinned core's legacy predictable staging path.
fn write_snapshot_file(
    path: &Path,
    snapshot: &AgentSessionSnapshot,
) -> Result<(), AgentSnapshotError> {
    let encoded = snapshot.to_json()?;
    crate::persistence::write_snapshot_atomic(
        path,
        encoded.as_bytes(),
        MAX_AGENT_SNAPSHOT_JSON_BYTES as u64,
    )
    .map_err(|error| AgentSnapshotError::Encode(format!("write {}: {error}", path.display())))
}

pub fn client_from_config(config: &Config) -> Result<AiClient, String> {
    if !config.ai_enabled {
        return Err("AI features are disabled by configuration".to_string());
    }
    let provider = config
        .ai_provider
        .parse::<Provider>()
        .map_err(|error| error.to_string())?;
    let app_key_name = format!(
        "{}_AI_API_KEY",
        jterm_core::identity::get().app_name.to_ascii_uppercase()
    );
    let provider_key_name = match provider {
        Provider::Anthropic => "ANTHROPIC_API_KEY",
        Provider::OpenAiCompatible => "OPENAI_API_KEY",
        Provider::Ollama => "OLLAMA_API_KEY",
    };
    let nonempty_env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let api_key = match nonempty_env(&app_key_name).or_else(|| nonempty_env(provider_key_name)) {
        Some(key) => Some(key),
        None => jterm_core::ai::resolve_api_key_file(config.ai_api_key_file.as_deref())
            .as_deref()
            .map(crate::persistence::read_api_key_file)
            .transpose()
            .map_err(|error| format!("AI API key file: {error}"))?,
    };
    AiClient::new(
        provider,
        api_key,
        config.ai_model.clone(),
        config.ai_base_url.clone(),
        config.ai_max_tokens,
        config.ai_temperature,
        config.ai_redact_secrets,
    )
    .map_err(|error| error.to_string())
}

/// Everything the update loop needs to launch one model request on a
/// background task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelRequestIdentity {
    pub epoch: AgentSessionEpoch,
    pub generation: u64,
}

pub struct ModelRequest {
    pub identity: ModelRequestIdentity,
    pub client: AiClient,
    pub system: String,
    pub user: String,
    pub token: AiCancellationToken,
}

/// Human-readable fields extracted from a partial, still-streaming agent
/// reply. The protocol reply is one strict JSON object
/// (`{"action":…,"thought":…,"command"|"message":…}`), so raw deltas are
/// unreadable; this pulls out the string-field *contents* as they arrive.
/// Purely a live preview — the transcript is only ever fed the complete
/// reply through `AgentSession::accept_model_reply`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReplyPreview {
    pub thought: Option<String>,
    pub message: Option<String>,
    pub command: Option<String>,
}

impl ReplyPreview {
    pub fn is_empty(&self) -> bool {
        self.thought.is_none() && self.message.is_none() && self.command.is_none()
    }
}

/// Decode one JSON string body (opening quote already consumed). Escapes are
/// decoded; an escape truncated by the end of the fragment is dropped rather
/// than shown raw. Returns the content and whether the closing quote arrived.
fn parse_json_string(chars: &mut std::str::Chars) -> (String, bool) {
    let mut out = String::new();
    loop {
        let Some(c) = chars.next() else {
            return (out, false);
        };
        match c {
            '"' => return (out, true),
            '\\' => {
                let Some(escape) = chars.next() else {
                    return (out, false);
                };
                match escape {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let mut code = String::new();
                        for _ in 0..4 {
                            let Some(digit) = chars.next() else {
                                return (out, false);
                            };
                            code.push(digit);
                        }
                        // Lone surrogates fail `from_u32` and are dropped;
                        // this is a preview, not the recorded reply.
                        if let Some(decoded) =
                            u32::from_str_radix(&code, 16).ok().and_then(char::from_u32)
                        {
                            out.push(decoded);
                        }
                    }
                    _ => {}
                }
            }
            _ => out.push(c),
        }
    }
}

/// Best-effort extraction of `thought` / `message` / `command` values from a
/// possibly-incomplete reply. Tolerates one leading code fence (as
/// `parse_action` does) and an unterminated final string; anything that stops
/// scanning simply ends the preview — never an error, the complete reply is
/// still parsed strictly on arrival.
pub fn preview_model_reply(raw: &str) -> ReplyPreview {
    let mut preview = ReplyPreview::default();
    let trimmed = raw.trim_start();
    let body = match trimmed.strip_prefix("```") {
        // A fenced reply only becomes previewable once the fence line ends.
        Some(rest) => match rest.split_once('\n') {
            Some((_, tail)) => tail,
            None => return preview,
        },
        None => trimmed,
    };
    let Some(start) = body.find('{') else {
        return preview;
    };
    let mut chars = body[start + 1..].chars();
    loop {
        // Key: skip separators up to the next string; '}' ends the object.
        let key = loop {
            match chars.next() {
                Some('"') => {
                    let (key, closed) = parse_json_string(&mut chars);
                    if !closed {
                        return preview;
                    }
                    break key;
                }
                Some(c) if c.is_whitespace() || c == ',' => continue,
                _ => return preview,
            }
        };
        // Separator.
        loop {
            match chars.next() {
                Some(':') => break,
                Some(c) if c.is_whitespace() => continue,
                _ => return preview,
            }
        }
        // Value: only string values are previewed; the protocol has no other
        // value type, so anything else just ends the scan.
        let value = loop {
            match chars.next() {
                Some('"') => break parse_json_string(&mut chars).0,
                Some(c) if c.is_whitespace() => continue,
                _ => return preview,
            }
        };
        let slot = match key.as_str() {
            "thought" => &mut preview.thought,
            "message" => &mut preview.message,
            "command" => &mut preview.command,
            _ => continue,
        };
        if !value.is_empty() {
            *slot = Some(value);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedAgentExecution {
    pub command: String,
    pub generation: u64,
}

#[derive(Clone, Debug)]
struct PendingAgentExecution {
    proposal_id: ProposalId,
    command: String,
    generation: u64,
}

struct InFlightModelRequest {
    identity: ModelRequestIdentity,
    token: AiCancellationToken,
}

pub struct AgentUi {
    pub is_open: bool,
    pub session: Option<AgentSession>,
    /// Stable session id (not index) this agent session is bound to.
    pub bound_session_id: Option<usize>,
    /// Approved proposal currently executing in the bound session. The
    /// generation is created locally and is never sourced from PTY bytes.
    awaiting: Option<PendingAgentExecution>,
    /// Most recent command the user ran manually in the bound session while
    /// the panel was open. Attached to model requests as untrusted block
    /// context.
    pub last_manual_completed: Option<BlockContext>,
    pub input: String,
    /// Proposal being edited inline: (proposal id, buffer).
    pub edit: Option<(ProposalId, String)>,
    pub loading: bool,
    pub status: String,
    pub provider_label: String,
    /// Checked monotonic half of an in-flight model request identity. The
    /// other half is the task epoch stored in [`InFlightModelRequest`].
    generation: u64,
    /// Monotonic one-shot identity for approved shell executions.
    execution_generation: u64,
    in_flight: Option<InFlightModelRequest>,
    /// Raw streamed reply text accumulated for the current generation. Only
    /// a live preview: the transcript records the complete returned text via
    /// `accept_model_reply`, so streaming and blocking store identical
    /// conversations. Kept after a mid-stream failure so already-shown
    /// partial text stays visible next to the recorded error.
    stream_raw: String,
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
            execution_generation: 0,
            in_flight: None,
            stream_raw: String::new(),
        }
    }

    /// A snapshot persisted by the previous run is restored one-shot and
    /// rebound to the current terminal session.
    pub fn open(&mut self, config: &Config, session_id: usize) {
        self.close_session();
        self.is_open = true;
        self.bound_session_id = Some(session_id);
        self.status.clear();
        let restored = snapshot_path().and_then(|path| claim_snapshot_session(&path));
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
        if self.session.as_ref().is_some_and(|session| {
            session.transcript().iter().any(|turn| {
                matches!(
                    turn,
                    Turn::AssistantProposed { command, .. }
                        if crate::review_text::validate_single_line(
                            command,
                            crate::review_text::MAX_AGENT_COMMAND_BYTES,
                        )
                        .is_err()
                )
            })
        }) {
            log::warn!("agent: refusing to persist an unsafe proposal command");
            jterm_core::agent::remove_snapshot_file(&path);
            return;
        }
        match self.session.as_ref().and_then(|session| session.snapshot()) {
            Some(snapshot) => {
                if let Err(error) = write_snapshot_file(&path, &snapshot) {
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
        if let Some(request) = self.in_flight.take() {
            request.token.cancel();
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
        self.stream_raw.clear();
    }

    fn seal_model_request_identities(&mut self) {
        if let Some(request) = self.in_flight.take() {
            request.token.cancel();
        }
        self.loading = false;
        if let Some(session) = self.session.as_mut() {
            session.cancel();
        }
        self.status = "Agent model request identities are exhausted".to_string();
    }

    fn accepts_model_callback(&mut self, identity: ModelRequestIdentity) -> bool {
        if !self
            .in_flight
            .as_ref()
            .is_some_and(|request| request.identity == identity)
        {
            return false;
        }
        if !self
            .session
            .as_ref()
            .is_some_and(|session| session.is_current_epoch(identity.epoch))
        {
            // This is not an older callback racing a newer request: it is the
            // currently tracked request after its task epoch was replaced.
            // Retire it without feeding an error into the replacement session.
            if let Some(request) = self.in_flight.take() {
                request.token.cancel();
            }
            self.loading = false;
            self.stream_raw.clear();
            return false;
        }
        self.loading
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
    pub fn next_model_request(
        &mut self,
        config: &Config,
        cwd: Option<&str>,
    ) -> Option<ModelRequest> {
        let epoch = {
            let session = self.session.as_ref()?;
            if self.loading || session.state() != AgentState::AwaitingModel {
                return None;
            }
            session.epoch()
        };
        let Some(generation) = self.generation.checked_add(1) else {
            self.seal_model_request_identities();
            return None;
        };
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
        let session = self.session.as_ref()?;
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
        let identity = ModelRequestIdentity { epoch, generation };
        self.in_flight = Some(InFlightModelRequest {
            identity,
            token: token.clone(),
        });
        self.loading = true;
        self.status.clear();
        self.stream_raw.clear();
        self.generation = generation;
        Some(ModelRequest {
            identity,
            client,
            system: jterm_core::ai::build_agent_system_prompt(),
            user,
            token,
        })
    }

    /// Append one streamed fragment of the in-flight reply. Fragments from a
    /// stale identity (a cancelled or replaced task epoch/request generation)
    /// are dropped, as are fragments arriving after the final reply.
    pub fn model_delta(&mut self, identity: ModelRequestIdentity, fragment: &str) {
        if !self.accepts_model_callback(identity) {
            return;
        }
        if self.stream_raw.len().saturating_add(fragment.len()) > MAX_AGENT_MODEL_REPLY_BYTES {
            if let Some(request) = self.in_flight.as_ref() {
                request.token.cancel();
            }
            self.in_flight = None;
            self.loading = false;
            // Invalidate the worker immediately. Relying on a later final
            // callback after cancellation could leave the UI permanently
            // loading if a transport exits without delivering it.
            let Some(generation) = self.generation.checked_add(1) else {
                self.seal_model_request_identities();
                return;
            };
            self.generation = generation;
            let message = format!("AI reply exceeded the {MAX_AGENT_MODEL_REPLY_BYTES}-byte limit");
            if let Some(session) = self.session.as_mut() {
                if let Err(error) = session.model_failed(message.clone()) {
                    self.status = error.to_string();
                    return;
                }
            }
            self.status = format!("{message}; request cancelled");
            return;
        }
        self.stream_raw.push_str(fragment);
    }

    /// Live preview of the streaming reply, if any of it is displayable yet.
    /// Also set after a mid-stream failure, so partial text stays visible.
    pub fn reply_preview(&self) -> Option<ReplyPreview> {
        if self.stream_raw.is_empty() {
            return None;
        }
        let mut preview = preview_model_reply(&self.stream_raw);
        if let Some(command) = preview.command.as_mut() {
            *command = crate::review_text::visible_bounded(
                command,
                crate::review_text::MAX_AGENT_COMMAND_BYTES,
            );
        }
        Some(preview).filter(|preview| !preview.is_empty())
    }

    /// Feed a finished model reply back into the protocol. Stale generations
    /// (an older, cancelled request) are ignored. On success the complete
    /// returned text — the single source of truth — replaces the streamed
    /// preview; on failure the preview is kept alongside the recorded error.
    pub fn model_reply(&mut self, identity: ModelRequestIdentity, result: Result<String, String>) {
        if !self.accepts_model_callback(identity) {
            return;
        }
        self.in_flight = None;
        self.loading = false;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let outcome = match result {
            Ok(raw) if raw.len() <= MAX_AGENT_MODEL_REPLY_BYTES => {
                self.stream_raw.clear();
                accept_model_reply_compat(session, &raw)
            }
            Ok(_) => session
                .model_failed(format!(
                    "AI reply exceeded the {MAX_AGENT_MODEL_REPLY_BYTES}-byte limit"
                ))
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Err(error) => session
                .model_failed(error)
                .map(|_| ())
                .map_err(|error| error.to_string()),
        };
        if let Err(error) = outcome {
            self.status = error;
        }
    }

    /// Approve a proposal (optionally with an edited command). The returned
    /// generation must be armed in the terminal before any bytes are written;
    /// only a completion carrying that internal generation may be observed.
    pub fn approve(
        &mut self,
        id: ProposalId,
        edited: Option<String>,
    ) -> Option<ApprovedAgentExecution> {
        let session = self.session.as_mut()?;
        let candidate = edited.as_deref().or_else(|| proposal_command(session, id));
        let Some(candidate) = candidate else {
            self.status = "proposal command is unavailable".to_string();
            return None;
        };
        if let Err(error) = crate::review_text::validate_single_line(
            candidate,
            crate::review_text::MAX_AGENT_COMMAND_BYTES,
        ) {
            self.status = format!("Agent command rejected: {error}");
            return None;
        }
        let approved = match edited {
            Some(command) => session.edit_and_approve(id, command),
            None => session.approve(id),
        };
        match approved {
            Ok(approved) => {
                if let Err(error) = crate::review_text::validate_single_line(
                    &approved.command,
                    crate::review_text::MAX_AGENT_COMMAND_BYTES,
                ) {
                    session.cancel();
                    self.status = format!("Agent command rejected after approval: {error}");
                    return None;
                }
                // Checked, never wrapped: a reused generation would let a late
                // completion from an earlier execution attach its output to
                // this approval. Exhaustion needs 2^64 approvals in one
                // session, so sealing is the honest response.
                let Some(generation) = self.execution_generation.checked_add(1) else {
                    session.cancel();
                    self.awaiting = None;
                    self.status = "Agent execution identities are exhausted".to_string();
                    return None;
                };
                self.execution_generation = generation;
                let execution = ApprovedAgentExecution {
                    command: approved.command.clone(),
                    generation,
                };
                self.awaiting = Some(PendingAgentExecution {
                    proposal_id: approved.proposal_id,
                    command: approved.command,
                    generation: execution.generation,
                });
                self.status.clear();
                Some(execution)
            }
            Err(error) => {
                self.status = error.to_string();
                None
            }
        }
    }

    /// A command was approved in the pure state machine but could not be
    /// armed/written. There is no safe observation to fabricate, so stop this
    /// Agent session rather than leaving it able to accept an unrelated block.
    pub fn execution_start_failed(&mut self, generation: u64, message: impl Into<String>) {
        if self
            .awaiting
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            self.awaiting = None;
            if let Some(session) = self.session.as_mut() {
                session.cancel();
            }
            self.status = message.into();
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

    /// Feed one OSC 133 completion from session `session_id`.
    ///
    /// Command text alone is never correlation: the terminal must attach the
    /// locally armed approval generation, and the captured command must still
    /// be byte-for-byte identical. A matching generation is consumed before
    /// observation, making duplicate D events harmless.
    pub fn handle_completed(&mut self, session_id: usize, completed: &CompletedCommand) {
        const MANUAL_OUTPUT_TRUNCATION_HINT: usize = 256 * 1024;
        if !self.is_open || self.bound_session_id != Some(session_id) {
            return;
        }
        if let Some(pending) = self.awaiting.as_ref() {
            if completed.agent_generation == Some(pending.generation) {
                if completed.command != pending.command {
                    let generation = pending.generation;
                    self.execution_start_failed(
                        generation,
                        "Agent stopped: approved command completion failed strict correlation",
                    );
                    return;
                }
                let Some(exit_code) = completed.exit_code else {
                    let generation = pending.generation;
                    self.execution_start_failed(
                        generation,
                        "Agent stopped: approved command completion had no exit status",
                    );
                    return;
                };
                let Some(pending) = self.awaiting.take() else {
                    return;
                };
                if let Some(session) = self.session.as_mut() {
                    if let Err(error) =
                        session.observe(pending.proposal_id, exit_code, &completed.output)
                    {
                        self.status = error.to_string();
                    }
                }
                return;
            }
        }
        // A stale/internal Agent generation must never become model context.
        if completed.agent_generation.is_some() {
            return;
        }
        let Ok(reported) = crate::review_text::sanitize_untrusted_single_line(
            &completed.command,
            crate::review_text::MAX_HISTORY_COMMAND_BYTES,
        ) else {
            return;
        };
        self.last_manual_completed = Some(BlockContext {
            cmd: reported,
            output: completed.output.clone(),
            cwd: None,
            exit_code: completed.exit_code.unwrap_or(1),
            truncated: completed.output.len() >= MANUAL_OUTPUT_TRUNCATION_HINT,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn ai_config() -> Config {
        Config {
            ai_enabled: true,
            ai_provider: "ollama".into(),
            ai_base_url: "http://localhost:11434".into(),
            ai_model: "codellama:7b".into(),
            ..Config::default()
        }
    }

    fn snapshot_fixture() -> AgentSessionSnapshot {
        let mut session = AgentSession::new(4);
        session.submit_user("persist this session").unwrap();
        session
            .snapshot()
            .expect("non-empty session has a snapshot")
    }

    fn private_test_dir(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("frost-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    #[test]
    fn claiming_a_snapshot_has_exactly_one_winner() {
        let root = private_test_dir("agent-claim");
        let path = root.join("agent_session.json");
        write_snapshot_file(&path, &snapshot_fixture()).unwrap();

        let session = claim_snapshot_session(&path).expect("the first opener restores");
        assert!(!session.transcript().is_empty());
        // Consumed: a second opener finds nothing, and no leftover file in the
        // directory can be restored later.
        assert!(!path.exists());
        assert!(claim_snapshot_session(&path).is_none());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unusable_claim_is_quarantined_rather_than_deleted() {
        let root = private_test_dir("agent-quarantine");
        let path = root.join("agent_session.json");

        for evidence in ["not json", r#"{"version":99}"#] {
            std::fs::write(&path, evidence).unwrap();
            assert!(claim_snapshot_session(&path).is_none());
            assert!(!path.exists(), "the original name is claimed");
            let preserved: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            assert_eq!(preserved.len(), 1, "invalid evidence is kept");
            assert_eq!(std::fs::read_to_string(&preserved[0]).unwrap(), evidence);
            assert!(claim_snapshot_session(&path).is_none());
            std::fs::remove_file(&preserved[0]).unwrap();
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_plain_http_targets_with_an_authority_are_openable() {
        assert!(crate::link::is_openable_url("https://example.com/path"));
        assert!(crate::link::is_openable_url("HTTP://example.com"));
        for rejected in [
            "file:///etc/passwd",
            "ssh://host.example/path",
            "git://host.example/repo",
            "mailto:person@example.com",
            "javascript:alert(1)",
            "https:///path",
            "https://user:token@example.com/",
            "https://exam\u{200b}ple.com/",
            "https://example.com/a b",
            "https://example.com\\evil",
            "relative/path",
        ] {
            assert!(
                !crate::link::is_openable_url(rejected),
                "{rejected:?} must not be openable"
            );
        }
    }

    #[test]
    fn local_snapshot_io_round_trips_and_enforces_the_exact_budget() {
        let root =
            std::env::temp_dir().join(format!("frost-agent-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("agent_session.json");
        let snapshot = snapshot_fixture();

        write_snapshot_file(&path, &snapshot).unwrap();
        let restored = read_snapshot_file(&path).expect("snapshot should round trip");
        assert!(AgentSession::restore(restored).is_ok());

        let oversized = root.join("oversized.json");
        write_private(&oversized, vec![b'x'; MAX_AGENT_SNAPSHOT_JSON_BYTES + 1]);
        assert!(read_snapshot_file(&oversized).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_snapshot_io_rejects_unsafe_entries_and_never_uses_the_legacy_stage() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;
        use std::time::{Duration, Instant};

        let root = std::env::temp_dir().join(format!(
            "frost-agent-snapshot-unsafe-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");
        let victim = root.join("victim.json");
        let legacy_stage = root.join(format!(".agent_session.json.next.{}", std::process::id()));
        write_private(&victim, b"sentinel");
        symlink(&victim, &legacy_stage).unwrap();

        write_snapshot_file(&path, &snapshot_fixture()).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"sentinel");
        assert!(std::fs::symlink_metadata(&legacy_stage)
            .unwrap()
            .file_type()
            .is_symlink());

        let linked = root.join("linked.json");
        symlink(&path, &linked).unwrap();
        assert!(read_snapshot_file(&linked).is_none());

        let hard_linked = root.join("hard-linked.json");
        std::fs::hard_link(&path, &hard_linked).unwrap();
        assert!(read_snapshot_file(&hard_linked).is_none());

        let fifo = root.join("fifo.json");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_name is NUL-terminated and remains live for this call.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(read_snapshot_file(&fifo).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_snapshot_rejects_duplicate_proposal_id_confusion() {
        let mut session = AgentSession::new(4);
        session.submit_user("list files").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        let snapshot = session.snapshot().unwrap();
        let mut encoded: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        let transcript = encoded["transcript"].as_array_mut().unwrap();
        let duplicate = transcript
            .iter()
            .find(|turn| turn.get("AssistantProposed").is_some())
            .unwrap()
            .clone();
        transcript.insert(1, duplicate);
        let snapshot =
            AgentSessionSnapshot::from_json(&serde_json::to_string(&encoded).unwrap()).unwrap();

        assert!(matches!(
            restore_snapshot_session(snapshot),
            Err(AgentSnapshotError::Invalid(reason)) if reason.contains("proposal id")
        ));
    }

    #[test]
    fn restored_snapshot_rejects_visually_spoofed_proposals() {
        let mut session = AgentSession::new(4);
        session.submit_user("list files").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        let snapshot = session.snapshot().unwrap();
        let mut encoded: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        let transcript = encoded["transcript"].as_array_mut().unwrap();
        let proposed = transcript
            .iter_mut()
            .find(|turn| turn.get("AssistantProposed").is_some())
            .unwrap();
        proposed["AssistantProposed"]["command"] =
            serde_json::Value::String("printf safe\u{202e}; rm -rf important".into());
        let snapshot = AgentSessionSnapshot::from_json(&encoded.to_string()).unwrap();

        assert!(matches!(
            restore_snapshot_session(snapshot),
            Err(AgentSnapshotError::Invalid(reason)) if reason.contains("proposal command")
        ));
    }

    #[test]
    fn model_and_edit_proposals_fail_closed_on_visual_spoofing() {
        let mut session = AgentSession::new(4);
        session.submit_user("run safely").unwrap();
        let error = accept_model_reply_compat(
            &mut session,
            &serde_json::json!({
                "action": "run",
                "command": "printf safe\u{202e}; rm -rf important",
            })
            .to_string(),
        )
        .unwrap_err();
        assert!(error.contains("command"));
        assert!(session.transcript().iter().all(|turn| !matches!(
            turn,
            Turn::AssistantProposed { command, .. }
                if crate::review_text::contains_visual_spoofing(command)
        )));

        session.retry_model().unwrap();
        let ModelOutcome::Proposal { id, .. } = session
            .accept_model_reply(r#"{"action":"run","command":"printf safe"}"#)
            .unwrap()
        else {
            panic!("expected proposal");
        };
        let mut agent = AgentUi::new();
        agent.session = Some(session);
        assert!(agent
            .approve(id, Some("printf safe\u{2066}hidden".into()))
            .is_none());
        assert!(agent.status.contains("rejected"));
        assert!(matches!(
            agent.session.as_ref().unwrap().state(),
            AgentState::AwaitingApproval { .. }
        ));
    }

    #[test]
    fn streamed_command_preview_escapes_visual_spoofing() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "show a command".into();
        agent.submit_input();
        let identity = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start")
            .identity;
        agent.model_delta(
            identity,
            r#"{"action":"run","command":"printf safe\u202ehidden"}"#,
        );
        assert_eq!(
            agent.reply_preview().and_then(|preview| preview.command),
            Some("printf safe\\u{202E}hidden".to_string())
        );
    }

    fn completed(
        command: &str,
        exit: i32,
        output: &str,
        agent_generation: Option<u64>,
    ) -> CompletedCommand {
        CompletedCommand {
            command: command.to_string(),
            exit_code: Some(exit),
            output: output.to_string(),
            id: None,
            agent_generation,
            output_available: true,
            truncated: false,
            total_bytes: output.len(),
            duration_ms: None,
        }
    }

    #[test]
    fn approval_flow_requires_generation_and_advances_exactly_once() {
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
        let approved = agent.approve(id, None).expect("proposal approves");
        assert_eq!(approved.command, "ls -la");

        // Wrong session id: ignored entirely.
        agent.handle_completed(
            3,
            &completed("ls -la", 0, "total 0", Some(approved.generation)),
        );
        assert!(agent.awaiting.is_some());
        assert!(agent.last_manual_completed.is_none());
        // A different command in the bound session becomes manual context,
        // not an observation.
        agent.handle_completed(7, &completed("pwd", 0, "/tmp", None));
        assert!(agent.awaiting.is_some());
        assert_eq!(
            agent
                .last_manual_completed
                .as_ref()
                .map(|context| context.cmd.as_str()),
            Some("pwd")
        );
        // Matching text without the internally armed generation is still only
        // manual context and cannot advance the Agent.
        agent.handle_completed(7, &completed("ls -la", 0, "untrusted", None));
        assert!(agent.awaiting.is_some());

        agent.handle_completed(
            7,
            &completed("ls -la", 0, "total 0", Some(approved.generation)),
        );
        assert!(agent.awaiting.is_none());
        assert_eq!(
            agent.session.as_ref().unwrap().state(),
            AgentState::AwaitingModel
        );

        let observations = agent
            .session
            .as_ref()
            .unwrap()
            .transcript()
            .iter()
            .filter(|turn| matches!(turn, jterm_core::agent::Turn::Observation { .. }))
            .count();
        // A duplicate completion carrying the consumed generation is ignored.
        agent.handle_completed(
            7,
            &completed("ls -la", 0, "duplicate", Some(approved.generation)),
        );
        assert_eq!(
            agent
                .session
                .as_ref()
                .unwrap()
                .transcript()
                .iter()
                .filter(|turn| matches!(turn, jterm_core::agent::Turn::Observation { .. }))
                .count(),
            observations
        );
    }

    #[test]
    fn approved_completion_without_exit_status_fails_closed() {
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
        let approved = agent.approve(id, None).expect("proposal approves");
        let mut completion = completed("ls -la", 0, "total 0", Some(approved.generation));
        completion.exit_code = None;

        agent.handle_completed(7, &completion);

        assert!(agent.awaiting.is_none());
        assert_eq!(
            agent.session.as_ref().unwrap().state(),
            AgentState::Cancelled
        );
        assert!(agent.status.contains("no exit status"));
    }

    #[test]
    fn suffix_collision_never_observes_an_approved_command() {
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
        let approved = agent.approve(id, None).expect("proposal approves");

        agent.handle_completed(7, &completed("echo prefix; ls -la", 0, "spoof", None));

        assert!(agent.awaiting.is_some());
        assert_eq!(
            agent.session.as_ref().unwrap().state(),
            AgentState::AwaitingObservation { proposal_id: id }
        );
        assert_ne!(
            agent
                .last_manual_completed
                .as_ref()
                .map(|context| &context.cmd),
            Some(&approved.command)
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

        // Reopening installs a new session epoch; the late reply is ignored.
        agent.open(&ai_config(), 2);
        agent.model_reply(
            request.identity,
            Ok("{\"action\":\"say\",\"message\":\"hi\"}".into()),
        );
        assert_eq!(agent.session.as_ref().unwrap().transcript().len(), 0);
    }

    #[test]
    fn restored_session_epoch_rejects_an_old_in_flight_callback() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "hello".into();
        agent.submit_input();
        let request = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start");
        let cancellation = request.token.clone();
        agent.model_delta(
            request.identity,
            r#"{"action":"say","message":"old preview""#,
        );
        assert!(agent.reply_preview().is_some());
        let snapshot = agent
            .session
            .as_ref()
            .and_then(AgentSession::snapshot)
            .expect("in-flight session should snapshot");
        let restored = restore_snapshot_session(snapshot).expect("snapshot should restore");
        assert!(!restored.is_current_epoch(request.identity.epoch));
        agent.session = Some(restored);
        let transcript = agent.session.as_ref().unwrap().transcript().to_vec();

        agent.model_reply(
            request.identity,
            Ok(r#"{"action":"say","message":"stale"}"#.into()),
        );

        assert!(!agent.loading);
        assert!(agent.in_flight.is_none());
        assert!(cancellation.is_cancelled());
        assert!(agent.reply_preview().is_none());
        assert_eq!(agent.session.as_ref().unwrap().transcript(), transcript);
    }

    #[test]
    fn stale_model_callbacks_are_dropped_after_new_task() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "first task".into();
        agent.submit_input();
        let stale_identity = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("first request should start")
            .identity;
        agent.model_reply(
            stale_identity,
            Ok(r#"{"action":"done","message":"finished"}"#.into()),
        );
        assert_eq!(
            agent.session.as_ref().unwrap().state(),
            AgentState::Completed
        );

        agent.new_task();
        assert_eq!(agent.session.as_ref().unwrap().state(), AgentState::Ready);
        assert!(!agent
            .session
            .as_ref()
            .unwrap()
            .is_current_epoch(stale_identity.epoch));
        agent.input = "second task".into();
        agent.submit_input();
        let current_identity = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("second request should start")
            .identity;
        let transcript = agent.session.as_ref().unwrap().transcript().to_vec();

        agent.model_delta(stale_identity, r#"{"action":"say","message":"stale""#);
        agent.model_reply(
            stale_identity,
            Ok(r#"{"action":"say","message":"stale"}"#.into()),
        );

        assert!(agent.loading);
        assert!(agent.reply_preview().is_none());
        assert_eq!(agent.session.as_ref().unwrap().transcript(), transcript);
        agent.model_reply(
            current_identity,
            Ok(r#"{"action":"say","message":"current"}"#.into()),
        );
        assert!(!agent.loading);
    }

    #[test]
    fn model_request_generation_exhaustion_cancels_the_session() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "hello".into();
        agent.submit_input();
        agent.generation = u64::MAX;

        assert!(agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .is_none());
        assert!(!agent.loading);
        assert!(agent.in_flight.is_none());
        assert_eq!(
            agent.session.as_ref().unwrap().state(),
            AgentState::Cancelled
        );
        assert!(agent.status.contains("identities are exhausted"));
    }

    #[test]
    fn disabled_ai_reports_a_configuration_status() {
        let mut agent = AgentUi::new();
        agent.open(&Config::default(), 0);
        assert!(agent.status.contains("disabled"));
    }

    #[test]
    fn preview_extracts_fields_from_partial_replies() {
        // Nothing displayable yet.
        assert!(preview_model_reply("").is_empty());
        assert!(preview_model_reply("{\"act").is_empty());
        assert!(preview_model_reply("I will run ls for you").is_empty());

        // A message cut mid-string grows fragment by fragment.
        let preview = preview_model_reply(r#"{"action":"say","message":"Hel"#);
        assert_eq!(preview.message.as_deref(), Some("Hel"));
        let preview = preview_model_reply(r#"{"action":"say","message":"Hello there"#);
        assert_eq!(preview.message.as_deref(), Some("Hello there"));

        // Complete replies expose thought and command too.
        let preview =
            preview_model_reply(r#"{"action":"run","thought":"check first","command":"ls -la"}"#);
        assert_eq!(preview.thought.as_deref(), Some("check first"));
        assert_eq!(preview.command.as_deref(), Some("ls -la"));
        assert_eq!(preview.message, None);
    }

    #[test]
    fn preview_decodes_escapes_and_tolerates_fences() {
        let preview = preview_model_reply(r#"{"message":"a\"b\né"#);
        assert_eq!(preview.message.as_deref(), Some("a\"b\né"));

        // An escape truncated by the fragment boundary is dropped, not shown raw.
        let preview = preview_model_reply(r#"{"message":"tail\u00"#);
        assert_eq!(preview.message.as_deref(), Some("tail"));
        let preview = preview_model_reply(r#"{"message":"tail\"#);
        assert_eq!(preview.message.as_deref(), Some("tail"));

        // One leading code fence is tolerated, like parse_action; before the
        // fence line ends nothing is previewed.
        assert!(preview_model_reply("```json").is_empty());
        let preview = preview_model_reply("```json\n{\"message\":\"hi\"}\n```");
        assert_eq!(preview.message.as_deref(), Some("hi"));
    }

    #[test]
    fn streamed_deltas_preview_then_final_reply_replaces_them() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "hello".into();
        agent.submit_input();
        let identity = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start")
            .identity;

        agent.model_delta(identity, r#"{"action":"say","#);
        assert!(agent.reply_preview().is_none());
        agent.model_delta(identity, r#""message":"Hi the"#);
        assert_eq!(
            agent
                .reply_preview()
                .expect("previewable")
                .message
                .as_deref(),
            Some("Hi the")
        );
        // Stale identities never touch the preview.
        let stale_identity = ModelRequestIdentity {
            generation: identity.generation + 1,
            ..identity
        };
        agent.model_delta(stale_identity, "garbage");
        assert_eq!(
            agent
                .reply_preview()
                .expect("previewable")
                .message
                .as_deref(),
            Some("Hi the")
        );

        // The final complete text replaces the preview and is the only thing
        // recorded — the transcript matches the blocking path exactly.
        let raw = r#"{"action":"say","message":"Hi there"}"#;
        agent.model_reply(identity, Ok(raw.into()));
        assert!(agent.reply_preview().is_none());
        assert!(!agent.loading);

        let mut blocking = AgentUi::new();
        blocking.open(&ai_config(), 1);
        blocking.input = "hello".into();
        blocking.submit_input();
        let blocking_identity = blocking
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start")
            .identity;
        blocking.model_reply(blocking_identity, Ok(raw.into()));
        assert_eq!(
            agent.session.as_ref().unwrap().transcript(),
            blocking.session.as_ref().unwrap().transcript(),
        );

        let transcript = agent.session.as_ref().unwrap().transcript().to_vec();
        agent.model_reply(identity, Ok(raw.into()));
        assert_eq!(agent.session.as_ref().unwrap().transcript(), transcript);
    }

    #[test]
    fn model_reply_and_stream_preview_share_a_strict_byte_budget() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "hello".into();
        agent.submit_input();
        let identity = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start")
            .identity;

        agent.model_delta(identity, &"x".repeat(MAX_AGENT_MODEL_REPLY_BYTES + 1));
        assert!(agent.stream_raw.is_empty());
        assert!(agent.status.contains("exceeded"));
        assert!(!agent.loading);
        let transcript = agent.session.as_ref().unwrap().transcript().to_vec();

        // The cancelled worker's eventual final callback is stale and cannot
        // add a second failure or revive the request.
        agent.model_reply(identity, Ok("x".repeat(MAX_AGENT_MODEL_REPLY_BYTES + 1)));
        assert_eq!(agent.session.as_ref().unwrap().transcript(), transcript);
        assert!(matches!(
            agent.session.as_ref().unwrap().transcript().last(),
            Some(Turn::ProtocolError(error)) if error.contains("exceeded")
        ));
    }

    #[test]
    fn mid_stream_failure_keeps_the_partial_preview_visible() {
        let mut agent = AgentUi::new();
        agent.open(&ai_config(), 1);
        agent.input = "hello".into();
        agent.submit_input();
        let identity = agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .expect("request should start")
            .identity;
        agent.model_delta(identity, r#"{"action":"say","message":"partial ans"#);

        agent.model_reply(identity, Err("connection reset".into()));
        assert!(!agent.loading);
        // Partial text stays visible next to the recorded protocol error.
        assert_eq!(
            agent.reply_preview().expect("kept").message.as_deref(),
            Some("partial ans")
        );
        assert!(matches!(
            agent.session.as_ref().unwrap().transcript().last(),
            Some(jterm_core::agent::Turn::ProtocolError(error))
                if error.contains("connection reset")
        ));

        // The next request discards the stale preview.
        agent.input = "try again".into();
        agent.submit_input();
        assert!(agent
            .next_model_request(&ai_config(), Some("/tmp"))
            .is_some());
        assert!(agent.reply_preview().is_none());
    }
}
