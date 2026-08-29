//! Persistent AI chats library — frost's iced-facing driver over the shared
//! [`jterm_core::ai::ChatStore`] (see `crate::ai_chat_store` for the policy
//! frost pins it with).
//!
//! Ported from anvil `src/dialogs/ai_panel.rs` (with forge
//! `src/ui/ai_panel.rs` as the convergent twin). The store owns every chat and
//! keys each in-flight reply by `(chat_id, epoch)`; this module holds the
//! panel state iced renders, request construction, and persistence. Storage
//! adapts the sources' layout to frost's paths: anvil/forge embed the
//! snapshot in their window-state file, frost keeps a standalone
//! `~/.config/frost/ai_chats.json` beside `agent_session.json`, written
//! through the app's descriptor-validated atomic persistence.
//!
//! That file is a single path shared by every frost process (the Agent
//! panel's neighbouring snapshot says the same of its own), and every write
//! replaces the whole of it — so persistence is fenced twice: nothing is
//! written before this run has read what is on disk ([`PersistState`]), and
//! only the instance holding the single-instance lock republishes it at all.
//!
//! frost deliberately has no "ask about selected block" entry into this
//! panel (the Agent panel already owns that surface), so the store's Block
//! context stays schema-compatible but is always `None` for turns begun here.
//! Recent shell context is consent-gated on `ai_share_command_context`,
//! frost's stricter standing rule for automatic command-context sharing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jterm_core::ai::{AiCancellationToken, AiClient, ConversationSnapshot, Turn};
use jterm_core::command_history;

use crate::agent;
use crate::ai_chat_store::{self, ChatStatus, ChatStore, ChatStoreError, RequestToken};
use crate::config::Config;

pub(crate) const STOPPED_STATUS: &str = "Response stopped. You can retry when ready.";
pub(crate) const MAX_SEARCH_CHARS: usize = 1_024;
/// anvil compacts to 1 MiB because the snapshot embeds in its 4 MiB
/// window-state envelope; frost's standalone file answers only to the shared
/// schema cap, so the compaction target is the core limit itself.
const CHATS_FILE_BUDGET: usize = jterm_core::ai::MAX_CONVERSATION_SNAPSHOT_JSON_BYTES;
/// The "include recent shell context" slice, same as the sources.
const RECENT_CONTEXT_ENTRIES: usize = 5;

pub(crate) fn chats_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("frost").join("ai_chats.json"))
}

/// Whether this panel may write `~/.config/frost/ai_chats.json`.
///
/// The library file is one fixed path shared by every frost process and by
/// every run, and [`AiChatsUi::persist`] is a blind whole-file replace — so a
/// write is only ever safe once *this* run has actually read what is on disk.
/// Ported from ember's `PersistState`, whose comments name both hazards: a
/// window that never opened the panel must not clobber the file, and "a
/// corrupt or unreadable existing file must never be replaced by a fresh
/// empty library". frost's own Agent panel already refuses the neighbouring
/// write for the first reason (`agent::AgentUi::persist`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistState {
    /// Nothing has been read yet, so the store is still the empty default:
    /// writing now would replace every saved chat with one blank "New chat".
    Unloaded,
    /// The file was read, or confirmed absent; the live library descends from
    /// what is on disk, so replacing it only loses what this run deleted.
    Loaded,
    /// The file is there but could not be read or decoded (truncated by a
    /// crash, over the read bound, wrong permissions, a schema version from a
    /// future sibling). Fail closed: those bytes may still be recoverable by
    /// hand, and a fresh empty library must never overwrite them.
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatsPage {
    Chat,
    Library,
}

/// Everything the update loop needs to launch one chat request on a
/// background task (mirrors `agent::ModelRequest`).
pub(crate) struct ChatRequest {
    pub(crate) token: RequestToken,
    pub(crate) client: AiClient,
    pub(crate) system: String,
    pub(crate) history: Vec<Turn>,
    pub(crate) cancellation: AiCancellationToken,
}

pub(crate) struct AiChatsUi {
    pub(crate) is_open: bool,
    pub(crate) store: ChatStore,
    /// Where this panel's library lives. Resolved once at construction so
    /// every read and write answers to the same path (and so tests can point
    /// a panel at a fixture instead of the user's real config directory).
    path: Option<PathBuf>,
    persist_state: PersistState,
    /// Only the instance holding frost's single-instance lock republishes the
    /// shared library. A second window still restores and uses its chats, but
    /// its writes would be a last-writer-wins overwrite of whatever the lock
    /// holder has since saved (ember gates the same file the same way, and
    /// frost already gates its session snapshot on `is_first_instance`).
    owns_persistence: bool,
    pub(crate) page: ChatsPage,
    pub(crate) search: String,
    /// The rename editor's raw buffer; the store keeps the normalized title.
    pub(crate) title_draft: String,
    /// Panel-level notice (restore failure, chat limit, provider error). The
    /// store's per-chat status covers request lifecycle text.
    pub(crate) notice: String,
    pub(crate) provider_label: String,
    include_recent: HashMap<u64, bool>,
    in_flight: HashMap<RequestToken, AiCancellationToken>,
    /// Per-chat text of the last failed or stopped composer turn, for Retry.
    retry_payloads: HashMap<u64, String>,
    /// One stable system prompt per chat, assigned on its first request.
    conversation_systems: HashMap<u64, String>,
    /// Streamed fragments of a failed reply kept visible next to the recorded
    /// error (the Agent panel's `stream_raw` discipline), keyed by chat.
    interrupted_partial: Option<(u64, String)>,
    confirm_delete: bool,
    dirty: bool,
}

impl AiChatsUi {
    /// `owns_persistence` is the process-wide single-instance lock: pass the
    /// app's `is_first_instance`.
    pub(crate) fn new(owns_persistence: bool) -> Self {
        Self::with_path(owns_persistence, chats_path())
    }

    /// Construct against an explicit library path; `None` means "no durable
    /// library at all" (no config directory, or a test panel).
    fn with_path(owns_persistence: bool, path: Option<PathBuf>) -> Self {
        Self {
            is_open: false,
            store: ai_chat_store::new_store(),
            path,
            persist_state: PersistState::Unloaded,
            owns_persistence,
            page: ChatsPage::Chat,
            search: String::new(),
            title_draft: String::new(),
            notice: String::new(),
            provider_label: String::new(),
            include_recent: HashMap::new(),
            in_flight: HashMap::new(),
            retry_payloads: HashMap::new(),
            conversation_systems: HashMap::new(),
            interrupted_partial: None,
            confirm_delete: false,
            dirty: false,
        }
    }

    /// Open the panel: restore the persisted library once per process and
    /// resolve the provider label. The caller gates `ai_enabled` with a toast,
    /// so a broken provider still opens the panel and explains itself in
    /// place (anvil's restored-panel behavior).
    pub(crate) fn open(&mut self, config: &Config) {
        self.is_open = true;
        self.page = ChatsPage::Chat;
        self.confirm_delete = false;
        if self.persist_state == PersistState::Unloaded {
            if let Some(path) = self.path.clone() {
                self.restore_from_path(&path);
            }
        }
        // Say so rather than letting a window quietly drop what is typed in
        // it: a second instance reads the shared library but never
        // republishes it, because its write would be a whole-file overwrite
        // of whatever the lock holder has saved since.
        if !self.owns_persistence && self.notice.is_empty() {
            self.notice =
                "Another frost window owns the saved chat library; chats started here are not saved."
                    .to_string();
        }
        self.sync_title_draft();
        match agent::client_from_config(config) {
            Ok(client) => {
                self.provider_label = client.display_name();
            }
            Err(error) => {
                self.provider_label.clear();
                self.notice = error;
            }
        }
    }

    /// Close the panel. In-flight requests keep running into the store (and
    /// persist on completion) so closing never loses a reply.
    pub(crate) fn close(&mut self, redact: bool) {
        self.is_open = false;
        self.confirm_delete = false;
        self.persist(redact);
    }

    /// Load the persisted library from an explicit path (tests inject their
    /// own). A missing file means this run legitimately owns a fresh library.
    /// Unreadable or undecodable content keeps the fresh library *and blocks
    /// every later write*: the panel is usable, but the damaged file stays
    /// exactly as it is instead of being replaced by one empty chat.
    fn restore_from_path(&mut self, path: &Path) {
        let encoded = match crate::persistence::read_text_bounded(path, CHATS_FILE_BUDGET as u64) {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.persist_state = PersistState::Loaded;
                return;
            }
            Err(error) => {
                self.block_persistence(&error.to_string());
                return;
            }
        };
        match ConversationSnapshot::from_json(&encoded) {
            Ok(snapshot) => {
                self.store = ai_chat_store::restore_store(snapshot);
                self.persist_state = PersistState::Loaded;
            }
            Err(error) => {
                self.block_persistence(&error.to_string());
            }
        }
    }

    /// Refuse every later write and say so, once, in the panel's notice line.
    fn block_persistence(&mut self, reason: &str) {
        self.persist_state = PersistState::Blocked;
        self.notice = format!(
            "Saved AI chats were not restored: {reason}. Saving is disabled this run so the \
             existing file is not overwritten."
        );
    }

    /// The two questions every write has to answer before it replaces the
    /// whole shared file: did this run read what is already there, and is this
    /// the instance that owns the file?
    fn can_persist(&self) -> bool {
        self.owns_persistence && self.persist_state == PersistState::Loaded
    }

    /// Persist the durable library through frost's atomic write. Retry
    /// payloads are materialized into a clone first so the live composer keeps
    /// an unrelated draft; the clone is thrown away, which is why it uses the
    /// core's *detaching* recovery — severing an in-flight request costs
    /// nothing there, and the message still survives as a draft.
    pub(crate) fn persist(&mut self, redact: bool) {
        // Fail closed. Nothing below is a read-modify-write: it replaces the
        // whole file, so a run that never opened the panel, a run whose
        // restore failed, and a window that does not hold the instance lock
        // all have to stop here rather than publish their own view.
        if !self.can_persist() {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let mut durable = self.store.clone();
        for (chat_id, text) in &self.retry_payloads {
            durable.recover_retry_payload_detaching(*chat_id, text, None);
        }
        // The core compacts live state *before* serialising it, which is what
        // keeps a grown library from reaching a size the shared schema refuses
        // outright — at which point nothing could be saved at all.
        let Ok((mut snapshot, _)) = durable.snapshot_for_persistence(redact) else {
            log::warn!("ai chats: could not build a durable snapshot");
            return;
        };
        if snapshot
            .compact_to_measured_limit(CHATS_FILE_BUDGET, |candidate| {
                candidate.to_json().ok().map(|encoded| encoded.len())
            })
            .is_none()
        {
            log::warn!("ai chats: snapshot exceeds the schema budget after compaction");
            return;
        }
        let encoded = match snapshot.to_json() {
            Ok(encoded) => encoded,
            Err(error) => {
                log::warn!("ai chats: could not encode snapshot: {error}");
                return;
            }
        };
        if let Err(error) = crate::persistence::write_snapshot_atomic(
            &path,
            encoded.as_bytes(),
            CHATS_FILE_BUDGET as u64,
        ) {
            log::warn!("ai chats: could not persist {}: {error}", path.display());
            return;
        }
        // Both compaction passes ran on the clone. Carry the truncation
        // markers they applied back into the live library so its rows admit
        // what the saved copy dropped.
        self.store.sync_truncation_markers(&snapshot);
        self.dirty = false;
    }

    /// Debounced-persistence hook driven by the app's config tick: draft
    /// keystrokes mark the library dirty and flush here instead of writing on
    /// every keystroke.
    pub(crate) fn flush_if_dirty(&mut self, redact: bool) {
        if self.dirty {
            self.persist(redact);
        }
    }

    pub(crate) fn set_draft(&mut self, draft: String) {
        if self.store.set_active_draft(draft) {
            self.dirty = true;
        }
    }

    pub(crate) fn set_search(&mut self, query: String) {
        self.search = query.chars().take(MAX_SEARCH_CHARS).collect();
    }

    pub(crate) fn include_recent(&self, chat_id: u64) -> bool {
        self.include_recent.get(&chat_id).copied().unwrap_or(true)
    }

    pub(crate) fn set_include_recent(&mut self, enabled: bool) {
        self.include_recent.insert(self.store.active_id(), enabled);
    }

    /// The "include recent shell context" slice for one request. Consent-gated
    /// on `ai_share_command_context`: without it no command history leaves the
    /// machine through this panel at all.
    fn recent_shell_context(&self, config: &Config, chat_id: u64) -> Option<String> {
        if !config.ai_share_command_context || !self.include_recent(chat_id) {
            return None;
        }
        let path = config.resolved_command_history_path()?;
        let items = command_history::read_recent(&path, RECENT_CONTEXT_ENTRIES).ok()?;
        if items.is_empty() {
            return None;
        }
        Some(
            items
                .iter()
                .rev()
                .map(|item| format!("$ {} (exit {})", item.command, item.exit_code))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Begin one provider turn for the given text. Shared by the composer
    /// (which clears the draft on success) and Retry (which does not).
    /// Returns the request the update loop should run, or None after setting
    /// the notice/status that explains the refusal.
    fn begin_request(
        &mut self,
        config: &Config,
        text: String,
        clear_draft: bool,
    ) -> Option<ChatRequest> {
        let text = text.trim().to_string();
        // Provider preflight precedes `begin_turn` (anvil's order): a failed
        // client must not consume the message into history.
        let client = match agent::client_from_config(config) {
            Ok(client) => client,
            Err(error) => {
                self.notice = error;
                return None;
            }
        };
        self.provider_label = client.display_name();
        let provider = jterm_core::review_input::safe_inline_display(&client.display_name(), 256);
        let start =
            match self
                .store
                .begin_turn(text.clone(), None, format!("Thinking… ({provider})"), true)
            {
                Ok(start) => start,
                Err(ChatStoreError::Archived) => {
                    self.notice = "Unarchive this chat before sending.".to_string();
                    return None;
                }
                Err(ChatStoreError::EmptyMessage) => {
                    self.notice = "Message is empty.".to_string();
                    return None;
                }
                Err(ChatStoreError::MessageTooLarge) => {
                    self.notice = "Message is too large (64 KiB limit).".to_string();
                    return None;
                }
                Err(
                    ChatStoreError::Busy
                    | ChatStoreError::LimitReached
                    | ChatStoreError::SnapshotInvalid,
                ) => {
                    return None;
                }
            };
        let chat_id = start.token.chat_id;
        let recent = self.recent_shell_context(config, chat_id);
        let (new_system, api_user) = jterm_core::ai::build_session_prompt(&text, recent.as_deref());
        let mut history = start.history;
        if let Some(last) = history
            .iter_mut()
            .rev()
            .find(|turn| turn.role == jterm_core::ai::Role::User)
        {
            last.text = api_user;
        }
        let system = self
            .conversation_systems
            .entry(chat_id)
            .or_insert(new_system)
            .clone();
        let cancellation = AiCancellationToken::new();
        self.in_flight.insert(start.token, cancellation.clone());
        self.retry_payloads.insert(chat_id, text);
        self.interrupted_partial = None;
        if clear_draft {
            self.store.set_active_draft(String::new());
        }
        // `begin_turn` derives the title from the first message; reflect it in
        // the rename editor (anvil's render_all after start_request).
        self.sync_title_draft();
        self.notice.clear();
        self.dirty = true;
        Some(ChatRequest {
            token: start.token,
            client,
            system,
            history,
            cancellation,
        })
    }

    /// Send the composer draft as the active chat's next turn.
    pub(crate) fn submit(&mut self, config: &Config) -> Option<ChatRequest> {
        if self.store.active_request_token().is_some() {
            return None;
        }
        let text = self.store.active_draft().trim().to_string();
        self.begin_request(config, text, true)
    }

    /// Stop the active chat's in-flight request, restoring its message into
    /// the composer draft (the store's pending-as-draft rollback).
    pub(crate) fn stop(&mut self, redact: bool) {
        let Some(token) = self.store.active_request_token() else {
            return;
        };
        if let Some(cancellation) = self.in_flight.remove(&token) {
            cancellation.cancel();
        }
        if self
            .store
            .cancel_request(token, STOPPED_STATUS.to_string())
            .is_some()
        {
            self.dirty = true;
            self.persist(redact);
        }
    }

    /// Re-send the active chat's last failed or stopped message. The recovered
    /// copy comes out of the draft first so a reply cannot duplicate it.
    pub(crate) fn retry(&mut self, config: &Config) -> Option<ChatRequest> {
        let chat_id = self.store.active_id();
        let text = self.retry_payloads.get(&chat_id)?.clone();
        let remaining = draft_without_retry_message(&text, self.store.active_draft());
        let original = self.store.active_draft().to_string();
        self.store.set_active_draft(remaining);
        match self.begin_request(config, text, false) {
            Some(request) => Some(request),
            None => {
                self.store.set_active_draft(original);
                None
            }
        }
    }

    pub(crate) fn has_retry(&self) -> bool {
        self.retry_payloads.contains_key(&self.store.active_id())
    }

    /// Streamed fragment for an in-flight turn (stale tokens dropped by the
    /// store). Returns whether the fragment landed in the viewed chat.
    pub(crate) fn push_delta(&mut self, token: RequestToken, text: &str) -> bool {
        if !self.in_flight.contains_key(&token) {
            return false;
        }
        self.store.push_delta(token, text) == Some(true)
    }

    /// A worker finished. Stale tokens (stop/replace raced the reply) are
    /// dropped; the complete text is the single source of truth and replaces
    /// the streamed preview, exactly like the Agent panel's `model_reply`.
    pub(crate) fn complete(&mut self, token: RequestToken, result: Result<String, String>) {
        if self.in_flight.remove(&token).is_none() {
            return;
        }
        match result {
            Ok(answer) => {
                if self
                    .store
                    .complete_success(token, answer.trim().to_string())
                    .is_some()
                {
                    self.retry_payloads.remove(&token.chat_id);
                    self.interrupted_partial = None;
                }
            }
            Err(error) => {
                let error = jterm_core::review_input::safe_inline_display(&error, 2 * 1024);
                // Keep the streamed evidence visible beside the recorded
                // error; the store has already rolled the message back into
                // the composer draft.
                let partial = self
                    .store
                    .active_request_token()
                    .filter(|active| *active == token)
                    .map(|_| self.store.active_partial().to_string())
                    .filter(|partial| !partial.is_empty());
                if self
                    .store
                    .complete_error(token, format!("AI error: {error}"))
                    .is_some()
                {
                    self.interrupted_partial = partial.map(|text| (token.chat_id, text));
                }
            }
        }
        self.dirty = true;
    }

    /// Streamed text preserved after a mid-stream failure, for the viewed chat.
    pub(crate) fn interrupted_partial(&self) -> Option<&str> {
        self.interrupted_partial
            .as_ref()
            .filter(|(chat_id, _)| *chat_id == self.store.active_id())
            .map(|(_, text)| text.as_str())
    }

    pub(crate) fn new_chat(&mut self, redact: bool) {
        match self.store.new_chat() {
            Ok(_) => {
                self.page = ChatsPage::Chat;
                self.confirm_delete = false;
                self.interrupted_partial = None;
                self.sync_title_draft();
                self.dirty = true;
                self.persist(redact);
            }
            Err(ChatStoreError::LimitReached) => {
                self.notice =
                    "50 chats are already saved. Delete one before creating another.".to_string();
            }
            Err(_) => {}
        }
    }

    pub(crate) fn select_chat(&mut self, id: u64, redact: bool) {
        if self.store.select_chat(id) {
            self.confirm_delete = false;
            self.interrupted_partial = None;
            self.sync_title_draft();
            self.notice.clear();
            self.dirty = true;
            self.persist(redact);
        }
        self.page = ChatsPage::Chat;
    }

    /// The rename editor types into `title_draft`; the store normalizes every
    /// change into its own title (whitespace collapse, spoof replacement,
    /// 80-char/256-byte bounds).
    pub(crate) fn rename(&mut self, title: String) {
        self.title_draft = title;
        if self.store.rename_active(&self.title_draft) {
            self.dirty = true;
        }
    }

    pub(crate) fn toggle_archive(&mut self, redact: bool) {
        match self.store.toggle_archive_active() {
            Ok(_) => {
                self.confirm_delete = false;
                self.interrupted_partial = None;
                self.sync_title_draft();
                self.notice.clear();
                self.dirty = true;
                self.persist(redact);
            }
            Err(ChatStoreError::Busy) => {
                self.notice = "Stop this response before archiving the chat.".to_string();
            }
            // Archiving the last writable chat has to allocate its
            // replacement; the core refuses before mutating rather than
            // leaving a library with nothing writable in it.
            Err(ChatStoreError::LimitReached) => {
                self.notice = "50 chats are already saved. Delete one before archiving this chat."
                    .to_string();
            }
            Err(_) => {}
        }
    }

    /// Delete is a two-step action in the panel: the first click arms, the
    /// second confirms (frost has no alert-dialog idiom).
    pub(crate) fn delete_armed(&self) -> bool {
        self.confirm_delete
    }

    pub(crate) fn delete(&mut self, redact: bool) {
        if !self.confirm_delete {
            self.confirm_delete = true;
            return;
        }
        self.confirm_delete = false;
        match self.store.delete_active() {
            Ok(outcome) => {
                let id = outcome.deleted_chat_id;
                self.retry_payloads.remove(&id);
                self.conversation_systems.remove(&id);
                self.include_recent.remove(&id);
                self.interrupted_partial = None;
                self.sync_title_draft();
                self.notice.clear();
                self.dirty = true;
                self.persist(redact);
            }
            Err(ChatStoreError::Busy) => {
                self.notice = "Stop this response before deleting the chat.".to_string();
            }
            Err(_) => {}
        }
    }

    /// The one status line under the transcript: a panel notice wins, then the
    /// store's per-chat lifecycle status.
    pub(crate) fn status_line(&self) -> &str {
        if !self.notice.is_empty() {
            return &self.notice;
        }
        match self.store.active_status() {
            ChatStatus::Idle => "",
            ChatStatus::Thinking(text) | ChatStatus::Info(text) | ChatStatus::Error(text) => text,
        }
    }

    /// Panel notices are refusals or restore failures; lifecycle text
    /// (thinking/stopped) is not an error style.
    pub(crate) fn status_is_error(&self) -> bool {
        !self.notice.is_empty() || matches!(self.store.active_status(), ChatStatus::Error(_))
    }

    fn sync_title_draft(&mut self) {
        self.title_draft = self.store.active_title().to_string();
    }
}

/// anvil's `draft_without_retry_message`: a retry's recovered text is stripped
/// from the draft before re-sending so the reply cannot duplicate it.
fn draft_without_retry_message(retry: &str, draft: &str) -> String {
    if draft == retry {
        return String::new();
    }
    draft
        .strip_prefix(retry)
        .and_then(|rest| rest.strip_prefix("\n\n"))
        .map_or_else(|| draft.to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("frost-ai-chats-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
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

    /// A panel that can never touch the user's real library: no path, and not
    /// the persistence owner.
    fn fresh_panel() -> AiChatsUi {
        AiChatsUi::with_path(false, None)
    }

    /// A panel that owns persistence at a fixture path, as the first instance
    /// with the panel opened would be.
    fn owner_panel(path: &Path) -> AiChatsUi {
        AiChatsUi::with_path(true, Some(path.to_path_buf()))
    }

    fn completed_store() -> ChatStore {
        let mut store = ai_chat_store::new_store();
        let token = store
            .begin_turn("hello".into(), None, "Thinking…".into(), true)
            .unwrap()
            .token;
        store.complete_success(token, "world".into());
        store.rename_active("Renamed");
        store
    }

    #[test]
    fn persist_restore_round_trips_through_the_file() {
        let scratch = ScratchDir::new("round-trip");
        let path = scratch.0.join("ai_chats.json");
        let mut panel = owner_panel(&path);
        // Restoring an absent file is the "this run owns a fresh library"
        // case, which is what unlocks writing.
        panel.restore_from_path(&path);
        panel.store = completed_store();
        panel.new_chat(false);
        panel.set_draft("unfinished".into());
        panel.persist(false);

        let mut restored_panel = owner_panel(&path);
        restored_panel.restore_from_path(&path);
        assert!(restored_panel.notice.is_empty());
        assert_eq!(restored_panel.store.summaries_filtered("").len(), 2);
        assert_eq!(restored_panel.store.active_draft(), "unfinished");
        assert!(restored_panel
            .store
            .summaries_filtered("renamed")
            .iter()
            .any(|chat| chat.title == "Renamed"));
    }

    /// A run that never opened the panel — or one whose restore failed, or a
    /// window that does not own the shared file — must not replace the saved
    /// library. Every one of those wrote a one-chat "New chat" library over
    /// the user's whole history before this guard existed.
    #[test]
    fn persist_never_overwrites_a_library_this_run_did_not_read() {
        let scratch = ScratchDir::new("guard");
        let path = scratch.0.join("ai_chats.json");
        let saved = {
            let mut store = completed_store();
            store.new_chat().unwrap();
            store.rename_active("Second");
            let (snapshot, _) = store.snapshot_for_persistence(false).unwrap();
            snapshot.to_json().unwrap()
        };
        let write_fixture = || {
            crate::persistence::write_snapshot_atomic(
                &path,
                saved.as_bytes(),
                CHATS_FILE_BUDGET as u64,
            )
            .unwrap();
        };

        // 1. The quit path of a run that never opened the panel: the store is
        //    still the empty default, and the default snapshot is *valid*, so
        //    only the load state can stop the write.
        write_fixture();
        let mut never_opened = owner_panel(&path);
        assert_eq!(never_opened.store.summaries_filtered("").len(), 1);
        never_opened.persist(false);
        never_opened.close(false);
        never_opened.flush_if_dirty(false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);

        // 2. A second window restores the library but never republishes it:
        //    its write would be a last-writer-wins overwrite of whatever the
        //    lock holder has saved since.
        let mut second_window = AiChatsUi::with_path(false, Some(path.clone()));
        second_window.open(&ai_config());
        assert!(
            second_window.notice.contains("not saved"),
            "{}",
            second_window.notice
        );
        assert_eq!(second_window.store.summaries_filtered("").len(), 2);
        second_window.new_chat(false);
        second_window.set_draft("only in the second window".into());
        second_window.flush_if_dirty(false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);

        // 3. The owner does republish, once it has read the file.
        let mut owner = owner_panel(&path);
        owner.restore_from_path(&path);
        owner.new_chat(false);
        assert_ne!(std::fs::read_to_string(&path).unwrap(), saved);
        let mut reloaded = owner_panel(&path);
        reloaded.restore_from_path(&path);
        assert_eq!(reloaded.store.summaries_filtered("").len(), 3);
    }

    /// A library that could not be read or decoded is evidence, not garbage:
    /// the panel keeps working on a fresh library but never writes over the
    /// damaged bytes (ember's `PersistState::Blocked`).
    #[test]
    fn a_failed_restore_blocks_every_later_write() {
        let scratch = ScratchDir::new("blocked");
        for (label, contents) in [
            (
                "truncated.json",
                "{\"version\":2,\"active_chat_id\":1,\"cha",
            ),
            // A future sibling's schema version decodes as an error too.
            (
                "future.json",
                "{\"version\":3,\"active_chat_id\":1,\"chats\":[]}",
            ),
        ] {
            let path = scratch.0.join(label);
            std::fs::write(&path, contents).unwrap();
            let mut panel = owner_panel(&path);
            panel.restore_from_path(&path);
            assert!(
                panel.notice.contains("Saving is disabled"),
                "{}",
                panel.notice
            );
            // Everything the user can do next still refuses to write.
            panel.set_draft("typed after the failed restore".into());
            panel.flush_if_dirty(false);
            panel.new_chat(false);
            panel.close(false);
            panel.persist(false);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents, "{label}");
        }
    }

    #[test]
    fn restore_of_invalid_or_oversized_files_keeps_a_fresh_library() {
        let scratch = ScratchDir::new("invalid");
        let path = scratch.0.join("ai_chats.json");
        std::fs::write(&path, "not a snapshot").unwrap();
        let mut panel = fresh_panel();
        panel.restore_from_path(&path);
        assert_eq!(panel.store.summaries_filtered("").len(), 1);
        assert!(panel.notice.contains("not restored"), "{}", panel.notice);

        let oversized = scratch.0.join("oversized.json");
        std::fs::write(&oversized, "x".repeat(CHATS_FILE_BUDGET + 1)).unwrap();
        let mut panel = fresh_panel();
        panel.restore_from_path(&oversized);
        assert!(panel.notice.contains("not restored"), "{}", panel.notice);

        let missing = scratch.0.join("missing.json");
        let mut panel = fresh_panel();
        panel.restore_from_path(&missing);
        assert!(panel.notice.is_empty());
    }

    #[test]
    fn recent_shell_context_is_consent_gated_and_checkbox_scoped() {
        let scratch = ScratchDir::new("recent");
        let history = scratch.0.join("history.jsonl");
        std::fs::write(
            &history,
            "{\"command\":\"cargo test\",\"exit_code\":0}\n\
             {\"command\":\"git status\",\"exit_code\":1}\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&history, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut config = ai_config();
        config.command_history_path = Some(history.clone());
        let panel = fresh_panel();
        let chat_id = panel.store.active_id();

        // Default off: no command context leaves the machine.
        assert!(panel.recent_shell_context(&config, chat_id).is_none());
        config.ai_share_command_context = true;
        let recent = panel
            .recent_shell_context(&config, chat_id)
            .expect("consent + history yields context");
        // Newest-first on disk comes back oldest-first, like the sources.
        assert_eq!(recent, "$ cargo test (exit 0)\n$ git status (exit 1)");

        let mut panel = panel;
        panel.set_include_recent(false);
        assert!(panel.recent_shell_context(&config, chat_id).is_none());

        // History disabled resolves no path at all.
        config.command_history_enabled = false;
        panel.set_include_recent(true);
        assert!(panel.recent_shell_context(&config, chat_id).is_none());
    }

    #[test]
    fn submit_preflight_failure_never_consumes_the_draft() {
        let mut panel = fresh_panel();
        panel.set_draft("why is chmod recursive".into());
        // ai_enabled=false makes client construction fail before begin_turn.
        assert!(panel.submit(&Config::default()).is_none());
        assert!(!panel.notice.is_empty());
        assert_eq!(panel.store.active_draft(), "why is chmod recursive");
        assert!(panel.store.active_history().is_empty());

        let request = panel
            .submit(&ai_config())
            .expect("keyless ollama client builds");
        assert_eq!(request.token, panel.store.active_request_token().unwrap());
        assert_eq!(panel.store.active_draft(), "");
        assert_eq!(panel.store.active_history().len(), 1);
        // The session prompt embeds the question; the system prompt is stable.
        assert!(request
            .history
            .last()
            .unwrap()
            .text
            .contains("why is chmod recursive"));
        assert!(panel.has_retry());
    }

    #[test]
    fn stop_and_retry_mirror_the_sources_draft_discipline() {
        let mut panel = fresh_panel();
        panel.set_draft("retry me".into());
        let request = panel.submit(&ai_config()).unwrap();
        panel.stop(false);
        assert_eq!(panel.store.active_draft(), "retry me");
        assert!(panel.store.active_history().is_empty());
        assert!(panel.has_retry());

        // Retry re-sends the payload; the draft's recovered copy is consumed.
        let retried = panel.retry(&ai_config()).unwrap();
        assert_ne!(retried.token, request.token);
        assert_eq!(panel.store.active_draft(), "");
        assert!(panel
            .store
            .cancel_request(retried.token, "stop".into())
            .is_some());
    }

    #[test]
    fn completion_and_stale_tokens_update_the_store_once() {
        let mut panel = fresh_panel();
        panel.set_draft("hello".into());
        let request = panel.submit(&ai_config()).unwrap();
        let token = request.token;
        // A reply nobody owns (already stopped/replaced) is dropped.
        panel.complete(
            RequestToken {
                chat_id: token.chat_id,
                epoch: token.epoch.wrapping_add(1),
            },
            Ok("stale".into()),
        );
        assert_eq!(panel.store.active_history().len(), 1);
        assert!(panel.push_delta(token, "wor"));
        panel.complete(token, Ok("world".into()));
        assert_eq!(panel.store.active_history()[1].text, "world");
        assert!(!panel.has_retry());
        assert!(panel.dirty);
    }

    #[test]
    fn failed_stream_keeps_the_partial_visible_for_the_viewed_chat() {
        let mut panel = fresh_panel();
        panel.set_draft("hello".into());
        let token = panel.submit(&ai_config()).unwrap().token;
        assert!(panel.push_delta(token, "partial evidence"));
        panel.complete(token, Err("offline".into()));
        assert_eq!(panel.interrupted_partial(), Some("partial evidence"));
        // The failed message is back in the composer draft for editing.
        assert_eq!(panel.store.active_draft(), "hello");
        // Switching chats clears the transient row.
        panel.new_chat(false);
        assert_eq!(panel.interrupted_partial(), None);
    }

    /// A failed request rolls its message back into the composer, and the
    /// merge can cross the store's 64 KiB live-message budget — dropping the
    /// tail of what the user typed. The shared store reports that (forge's
    /// behaviour, which the app-local copies lacked); this pins that frost
    /// actually puts the report in front of the user.
    #[test]
    fn a_failed_request_that_trims_the_recovered_draft_says_so() {
        let mut panel = fresh_panel();
        panel.set_draft("a".repeat(40 * 1024));
        let token = panel.submit(&ai_config()).unwrap().token;
        // The pending 40 KiB question, plus 40 KiB typed while it ran, cannot
        // both survive the merge.
        panel.set_draft("b".repeat(40 * 1024));
        panel.complete(token, Err("offline".into()));
        assert!(
            panel.status_line().contains("omitted at the 64 KiB limit"),
            "{}",
            panel.status_line()
        );
        assert!(panel.store.active_history_truncated());
    }

    #[test]
    fn delete_is_two_step_and_archive_blocks_while_busy() {
        let mut panel = fresh_panel();
        panel.set_draft("busy chat".into());
        let _request = panel.submit(&ai_config()).unwrap();
        panel.toggle_archive(false);
        assert!(
            panel.notice.contains("Stop this response"),
            "{}",
            panel.notice
        );
        panel.stop(false);
        panel.toggle_archive(false);
        // Archiving the only chat yields the active slot to a fresh writable
        // chat while the original stays archived.
        assert!(!panel.store.active_archived());
        assert_eq!(panel.store.summaries_filtered("").len(), 2);
        panel.delete(false);
        assert!(panel.delete_armed());
        assert_eq!(panel.store.summaries_filtered("").len(), 2);
        panel.delete(false);
        assert!(!panel.delete_armed());
        // Deleting the fresh chat leaves the archived one untouched and a
        // writable replacement active (the store's invariant).
        assert!(!panel.store.active_archived());
        assert!(panel
            .store
            .summaries_filtered("")
            .iter()
            .any(|chat| chat.archived));
    }

    #[test]
    fn archiving_the_last_writable_chat_at_capacity_is_refused_with_a_notice() {
        let mut panel = fresh_panel();
        // Fill the library, then archive everything but the active chat
        // through the store so no fixture write reaches the real config path.
        while panel.store.new_chat().is_ok() {}
        for _ in 0..jterm_core::ai::MAX_PERSISTED_CHATS - 1 {
            panel.store.toggle_archive_active().unwrap();
        }
        assert!(!panel.store.active_archived());

        // The last writable chat cannot be archived: its replacement would
        // have to be allocated past the 50-chat cap. The core refuses before
        // mutating, so the library still has a writable chat to type into.
        panel.toggle_archive(false);
        assert!(
            panel.notice.contains("50 chats are already saved"),
            "{}",
            panel.notice
        );
        assert!(!panel.store.active_archived());
    }

    #[test]
    fn retry_payload_text_never_duplicates_in_the_draft() {
        assert_eq!(draft_without_retry_message("failed", "failed"), "");
        assert_eq!(
            draft_without_retry_message("failed", "failed\n\nfollow-up"),
            "follow-up"
        );
        assert_eq!(
            draft_without_retry_message("failed", "edited failed"),
            "edited failed"
        );
    }
}
