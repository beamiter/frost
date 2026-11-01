//! Native session-library state. Discovery and resume validation live in core.
use jterm_core::agent_sessions::{
    validate_session_batch_tag, validate_session_details, AgentSession, SessionArchiveFilter,
    SessionBatchAction, SessionCatalog, SessionLaunch, SessionLibraryFilter,
    SessionLibrarySnapshot, SessionLibraryStore, SessionPreflight, SessionPreflightState,
    SessionProjectSummary, SessionProvider, SessionQuery, SessionScanner, SessionSearchIndex,
    SessionSort, SessionStatus, SESSION_ALIAS_MAX_BYTES, SESSION_BATCH_LIMIT,
    SESSION_QUERY_MAX_BYTES, SESSION_RESUME_ANYWAY_DELAY, SESSION_TAG_LIMIT, SESSION_TAG_MAX_BYTES,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub(crate) const PAGE_SIZE: usize = 80;
use std::time::{Duration, Instant};

/// Background rescans are measured from the end of the previous scan, so a
/// slow scan never starts back-to-back work or re-reads the library per tick.
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Paste guards for the name/tag editor. A draft over the shared byte limits
/// stays visible with core's explanation (and Save disabled); only an
/// implausibly large paste is refused whole, keeping the previous draft.
const ALIAS_INPUT_LIMIT: usize = SESSION_ALIAS_MAX_BYTES * 4;
const TAGS_INPUT_LIMIT: usize = (SESSION_TAG_MAX_BYTES + 2) * SESSION_TAG_LIMIT * 4;
const BULK_TAG_INPUT_LIMIT: usize = SESSION_TAG_MAX_BYTES * 4;
/// A search between core's limit and this stays visible with core's
/// explanation; only an implausible paste is refused whole.
const QUERY_INPUT_LIMIT: usize = SESSION_QUERY_MAX_BYTES * 4;

/// The batch tag's problem, checked live with the rules `apply_batch`
/// enforces. An empty field only disables Add/Remove tag.
fn batch_tag_problem(tag: &str) -> Option<String> {
    if tag.trim().is_empty() {
        None
    } else {
        validate_session_batch_tag(tag).err()
    }
}

pub(crate) type SessionKey = (SessionProvider, String);
type OpenSessionKey = (SessionProvider, String, PathBuf);

fn open_session_key(session: &AgentSession) -> OpenSessionKey {
    (
        session.provider,
        session.id.clone(),
        crate::workspace_sessions_bridge::source_profile(session),
    )
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceSessionRef {
    epoch: u64,
    session: AgentSession,
}

fn same_launch_target(left: &AgentSession, right: &AgentSession) -> bool {
    left.provider == right.provider
        && left.id == right.id
        && left.source == right.source
        && left.cwd == right.cwd
        && left.archived == right.archived
}

pub(crate) fn session_key(session: &AgentSession) -> SessionKey {
    (session.provider, session.id.clone())
}

/// The latest scan observed another CLI holding this exact conversation.
/// Shown after the first resume request; a second explicit request for the
/// same identity, at least `SESSION_RESUME_ANYWAY_DELAY` later, proceeds.
/// Selection changes, Escape, a refresh that drops the identity or no longer
/// observes the process, and an attempt that ends without a spawn all retire
/// it, so the next Resume warns again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumeWarning {
    pub(crate) key: SessionKey,
    pub(crate) text: String,
    /// When the warning was first shown; refreshing its text keeps this.
    pub(crate) shown_at: Instant,
}

/// The user's own organization change to the selected conversation that
/// may take it out of the current view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnChange {
    Archived,
    Restored,
    Unpinned,
    Organized,
    Batch,
}

impl OwnChange {
    /// Why the selected row left the view and which view finds it again.
    fn left_view(self) -> &'static str {
        match self {
            Self::Archived => "You archived this conversation, so it left this view. Choose Archived here or All conversations to find or restore it.",
            Self::Restored => "You restored this conversation to the library, so it left this view. Choose Active library or All conversations to find it.",
            Self::Unpinned => "You unpinned this conversation, so it no longer matches this view. Turn off Pinned only or clear filters to find it.",
            Self::Organized => "Its new name or tags no longer match this search or filter, so it left this view. Clear filters to find it.",
            Self::Batch => "Your batch change moved this conversation out of this view. Clear filters or choose All conversations to find it.",
        }
    }
}

/// A frozen selection and the exact opening that rendered its actions.
#[derive(Clone, Debug)]
pub(crate) struct BatchRef {
    epoch: std::sync::Arc<()>,
    keys: std::sync::Arc<[SessionKey]>,
}

/// Every organization callback belongs to one explicit editor opening, even
/// when the same session is opened again after cancelling an earlier draft.
#[derive(Clone, Debug)]
pub(crate) struct OrganizationRef {
    key: SessionKey,
    epoch: std::sync::Arc<()>,
}

struct OrganizationEditor {
    reference: OrganizationRef,
    title: String,
    alias: String,
    tags: String,
    notice: Option<String>,
    /// Live result of core's `validate_session_details` for the draft; Save
    /// stays disabled while it explains a problem.
    invalid: Option<String>,
    saving: bool,
}

impl OrganizationEditor {
    fn revalidate(&mut self) {
        self.invalid = validate_session_details(&self.alias, &self.tags).err();
    }
}

pub(crate) struct SessionLibrary {
    pub open: bool,
    pub(crate) workspace_pending: bool,
    pub(crate) workspace_task: Option<jterm_core::agent_task::TaskId>,
    catalog_epoch: u64,
    pub(crate) workspace: crate::workspace_sessions_bridge::WorkspaceSessions,
    workspace_source: Option<OpenSessionKey>,
    workspace_enabled: bool,
    metadata: SessionLibrarySnapshot,
    pub catalog: SessionCatalog,
    pub scanner: SessionScanner,
    pub query: String,
    pub provider: Option<SessionProvider>,
    pub status: Option<SessionStatus>,
    pub live_only: bool,
    pub pinned_only: bool,
    pub recent_only: bool,
    pub archive: SessionArchiveFilter,
    pub store: SessionLibraryStore,
    pub preflight: SessionPreflight,
    pending_confirm: Option<SessionKey>,
    resume_warning: Option<ResumeWarning>,
    pub project: Option<PathBuf>,
    pub project_only: bool,
    pub project_selection: Option<PathBuf>,
    pub show_projects: bool,
    pub sort: SessionSort,
    search_index: SessionSearchIndex,
    visible: Vec<usize>,
    page: usize,
    codex_count: usize,
    observed_count: usize,
    projects: Vec<SessionProjectSummary>,
    query_error: Option<String>,
    /// A refused, implausibly large search paste; the previous search stays.
    query_notice: Option<String>,
    /// The user's own change awaiting its write, with the identity it changed.
    own_change: Option<(SessionKey, OwnChange)>,
    /// Why the detached selection left the view, when the user's own change
    /// moved it; the generic explanation is shown otherwise.
    left_view: Option<&'static str>,
    editor: Option<OrganizationEditor>,
    bulk_mode: bool,
    checked: HashSet<SessionKey>,
    bulk_epoch: std::sync::Arc<()>,
    bulk_tag: String,
    bulk_notice: Option<String>,
    bulk_saving: bool,
    bulk_saved_count: usize,
    /// The user's chosen identity. It may be hidden by a refresh or filter;
    /// a background refresh never replaces it with another conversation.
    pub selected: Option<SessionKey>,
    pub notice: Option<String>,
    bindings: HashMap<OpenSessionKey, usize>,
    /// Periodic background refresh continues the explicit refresh made when
    /// the manager opens; it stops when the manager closes.
    auto_refresh: bool,
}

impl Default for SessionLibrary {
    fn default() -> Self {
        let mut state = Self::with_store(SessionLibraryStore::new());
        state.workspace_enabled = true;
        state
    }
}
impl SessionLibrary {
    pub(crate) fn owns_terminal(&self, terminal_id: usize) -> bool {
        self.bindings.values().any(|id| *id == terminal_id)
    }

    pub(crate) fn metadata(&self) -> &SessionLibrarySnapshot {
        if self.workspace_enabled && !self.workspace.rows.is_empty() {
            &self.metadata
        } else {
            self.store.snapshot()
        }
    }
    pub(crate) fn refresh_workspace_metadata(&mut self) {
        self.metadata = self
            .workspace
            .projection(&self.catalog, self.store.snapshot());
        self.reconcile();
    }
    fn shared_target(&self, key: &SessionKey) -> Result<AgentSession, String> {
        if self.workspace_source.is_some() {
            if let Some(session) = self
                .selected_session()
                .filter(|session| session_key(session) == *key)
            {
                return Ok(session.clone());
            }
        }
        let matches: Vec<_> = self
            .catalog
            .sessions
            .iter()
            .filter(|session| session_key(session) == *key)
            .collect();
        if matches.len() != 1 {
            return Err("Choose the exact source in the workspace sidebar before organizing this conversation.".into());
        }
        Ok(matches[0].clone())
    }
    pub(crate) fn shared_pin(&mut self, key: &SessionKey, pinned: bool) -> Result<(), String> {
        let target = self.shared_target(key)?;
        self.workspace.set_pinned(&target, pinned)
    }
    pub(crate) fn shared_archive(
        &mut self,
        key: &SessionKey,
        archived: bool,
    ) -> Result<(), String> {
        let target = self.shared_target(key)?;
        self.workspace.set_archived(&target, archived)
    }
    fn shared_details(&mut self, key: &SessionKey, title: &str, tags: &str) -> Result<(), String> {
        let target = self.shared_target(key)?;
        self.workspace.set_details(&target, title, tags)
    }
    fn shared_batch(
        &mut self,
        keys: &[SessionKey],
        action: &SessionBatchAction,
    ) -> Result<usize, String> {
        let sessions = keys
            .iter()
            .map(|key| self.shared_target(key))
            .collect::<Result<Vec<_>, _>>()?;
        self.workspace.apply_batch(&sessions, action)
    }
    fn organization_busy(&self) -> bool {
        if self.workspace_enabled {
            self.workspace.busy()
        } else {
            self.store.is_saving()
        }
    }
    fn organization_error(&self) -> Option<&str> {
        if self.workspace_enabled {
            self.workspace.notice.as_deref()
        } else {
            self.store.error()
        }
    }
    fn workspace_visible(&self) -> Vec<usize> {
        if SessionSearchIndex::query_error(&self.query).is_some() {
            return Vec::new();
        }
        let mut result: Vec<_> = self
            .catalog
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                let row = self.workspace.row(session);
                let archived = session.archived || row.is_some_and(|row| row.archived);
                let archive = match self.archive {
                    SessionArchiveFilter::Active => !archived,
                    SessionArchiveFilter::Archived => archived,
                    SessionArchiveFilter::All => true,
                };
                archive
                    && self
                        .provider
                        .is_none_or(|provider| provider == session.provider)
                    && self.status.is_none_or(|status| status == session.status)
                    && (!self.live_only
                        || self
                            .workspace
                            .row(session)
                            .is_some_and(|row| row.runtime.is_live())
                        || self
                            .catalog
                            .live_processes
                            .iter()
                            .any(|p| p.provider == session.provider && p.id == session.id))
                    && (!self.pinned_only || self.workspace.pinned(session))
                    && self
                        .project_selection
                        .as_deref()
                        .or({
                            if self.project_only {
                                self.project.as_deref()
                            } else {
                                None
                            }
                        })
                        .is_none_or(|project| session.cwd.starts_with(project))
                    && row.map_or_else(
                        || session.matches_query(&self.query),
                        |row| row.matches(&self.query),
                    )
            })
            .map(|(index, _)| index)
            .collect();
        result.sort_by(|&a, &b| {
            self.workspace
                .pinned(&self.catalog.sessions[b])
                .cmp(&self.workspace.pinned(&self.catalog.sessions[a]))
                .then(
                    self.catalog.sessions[b]
                        .updated_at_ms
                        .cmp(&self.catalog.sessions[a].updated_at_ms),
                )
        });
        result
    }
    pub(crate) fn workspace_reference_current(&self, reference: &WorkspaceSessionRef) -> bool {
        self.catalog_epoch == reference.epoch
            && self
                .workspace_visible()
                .iter()
                .any(|&index| same_launch_target(&self.catalog.sessions[index], &reference.session))
    }

    pub(crate) fn pin_workspace_session(&mut self, reference: &WorkspaceSessionRef) {
        if !self.workspace_reference_current(reference) {
            return;
        }
        let session = &reference.session;
        let pinned = !self.workspace.pinned(session);
        self.notice = self.workspace.set_pinned(session, pinned).err();
    }

    fn with_store(store: SessionLibraryStore) -> Self {
        Self {
            open: false,
            workspace_pending: false,
            workspace_task: None,
            catalog_epoch: 0,
            workspace: Default::default(),
            workspace_source: None,
            workspace_enabled: false,
            metadata: Default::default(),
            catalog: SessionCatalog::default(),
            scanner: SessionScanner::new(),
            query: String::new(),
            provider: None,
            status: None,
            live_only: false,
            pinned_only: false,
            recent_only: false,
            archive: SessionArchiveFilter::default(),
            store,
            preflight: SessionPreflight::new(),
            pending_confirm: None,
            resume_warning: None,
            project: None,
            project_only: false,
            project_selection: None,
            show_projects: false,
            sort: SessionSort::default(),
            search_index: SessionSearchIndex::default(),
            visible: Vec::new(),
            page: 0,
            codex_count: 0,
            observed_count: 0,
            projects: Vec::new(),
            query_error: None,
            query_notice: None,
            own_change: None,
            left_view: None,
            editor: None,
            bulk_mode: false,
            checked: HashSet::new(),
            bulk_epoch: std::sync::Arc::new(()),
            bulk_tag: String::new(),
            bulk_notice: None,
            bulk_saving: false,
            bulk_saved_count: 0,
            selected: None,
            notice: None,
            bindings: HashMap::new(),
            auto_refresh: false,
        }
    }
}

impl SessionLibrary {
    /// Opening, the Refresh button and F5 all do exactly this: rescan the
    /// provider histories, reload the shared library and re-check the
    /// selected conversation. A queued resume or live-session warning is
    /// withdrawn so it is confirmed again against the fresh evidence.
    pub fn refresh(&mut self) {
        self.cancel_confirmation();
        self.auto_refresh = true;
        self.scanner.refresh();
        self.store.refresh();
        // A newly selected target starts its own check; only an unchanged
        // one needs a fresh check, so opening never checks twice.
        if !self.sync_preflight() {
            self.preflight.refresh();
        }
    }

    /// Opening is the user's own action. The selection kept from an earlier
    /// opening stays only while the current filters still list it (the
    /// captured folder may differ, or a live-only CLI may have exited);
    /// otherwise the first match is selected instead of opening on a
    /// detached conversation. Then the same full refresh as Refresh and F5.
    pub(crate) fn open_view(&mut self) {
        self.adopt_listed_selection();
        self.refresh();
    }

    fn adopt_listed_selection(&mut self) {
        self.project_query();
        if self.selected_position().is_none() {
            self.selected = self.first_visible_key();
        }
        self.left_view = None;
        self.sync_page();
    }

    /// The user's own search edit. The complete text goes to the shared
    /// parser: truncating could drop a trailing `-tag:` exclusion and widen
    /// the results, so text over core's limit stays visible with core's
    /// explanation and no matches. Only an implausible paste is refused,
    /// keeping the previous search. Returns whether the search changed.
    pub(crate) fn set_query(&mut self, query: String) -> bool {
        if query.len() > QUERY_INPUT_LIMIT {
            self.query_notice = Some(format!(
                "That search is far over the {SESSION_QUERY_MAX_BYTES}-byte limit. Your previous search was kept."
            ));
            return false;
        }
        self.query_notice = None;
        if self.query == query {
            return false;
        }
        self.query = query;
        self.apply_filters();
        true
    }

    /// Remember the user's own change to `key` until its write settles; if
    /// it then took the selected conversation out of the view, the detached
    /// selection says why instead of blaming another window.
    pub(crate) fn note_own_change(&mut self, key: SessionKey, change: OwnChange) {
        self.own_change = Some((key, change));
    }

    pub fn poll(&mut self) -> bool {
        let old_page = self.page;
        if let Some(catalog) = self.scanner.poll() {
            self.replace_catalog(catalog);
        }
        if self.store.poll() {
            self.refresh_workspace_metadata();
        }
        // The user's own change settled. The selection is never retargeted;
        // if the change took it out of the view, say so and where it went.
        let organization_busy = self.organization_busy();
        if let Some((key, change)) = self.own_change.take_if(|_| !organization_busy) {
            if self.organization_error().is_none()
                && self.selected.as_ref() == Some(&key)
                && self.selected_position().is_none()
            {
                self.left_view = Some(change.left_view());
            }
        }
        // `is_saving` covers only this window's own writes until `poll`
        // acknowledges them; the periodic read-only reload never counts.
        if self.bulk_saving && !self.organization_busy() {
            self.bulk_saving = false;
            self.bulk_notice = Some(self.organization_error().map(str::to_owned).unwrap_or_else(
                || format!("Saved changes for {} conversations.", self.bulk_saved_count),
            ));
            self.retire_bulk();
        }
        if self.editor.as_ref().is_some_and(|editor| editor.saving) && !self.organization_busy() {
            if let Some(error) = self.organization_error().map(str::to_owned) {
                if let Some(editor) = &mut self.editor {
                    editor.saving = false;
                    editor.notice = Some(error.to_owned());
                }
            } else {
                self.editor = None;
            }
        }
        self.sync_preflight();
        self.preflight.poll();
        // Measured from the end of the previous scan: a scan slower than the
        // interval never triggers back-to-back rescans, and the library file
        // is reloaded only alongside a scan that actually starts.
        if self.auto_refresh && self.scanner.refresh_if_idle_for(AUTO_REFRESH_INTERVAL) {
            self.store.refresh();
        }
        old_page != self.page
    }

    pub fn replace_catalog(&mut self, catalog: SessionCatalog) -> bool {
        if self.catalog == catalog {
            return false;
        }
        self.catalog_epoch = self.catalog_epoch.wrapping_add(1);
        self.catalog = catalog;
        self.search_index = SessionSearchIndex::new(&self.catalog);
        self.codex_count = self
            .catalog
            .sessions
            .iter()
            .filter(|s| s.provider == SessionProvider::Codex)
            .count();
        let live: HashSet<_> = self
            .catalog
            .live_processes
            .iter()
            .map(|p| (p.provider, p.id.as_str()))
            .collect();
        self.observed_count = self
            .catalog
            .sessions
            .iter()
            .filter(|s| live.contains(&(s.provider, s.id.as_str())))
            .count();
        self.reconcile();
        true
    }

    #[cfg(test)]
    pub fn filtered(&self) -> Vec<&AgentSession> {
        self.visible_sessions(0..self.visible.len()).collect()
    }

    /// Listed conversations in display order, without collecting the list.
    fn visible_sessions(
        &self,
        range: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &AgentSession> + '_ {
        self.visible[range]
            .iter()
            .filter_map(|index| self.catalog.sessions.get(*index))
    }

    fn first_visible_key(&self) -> Option<SessionKey> {
        let first = self.visible_sessions(0..self.visible.len()).next();
        first.map(session_key)
    }

    /// The display positions of the current page.
    fn page_range(&self) -> std::ops::Range<usize> {
        let start = (self.page * PAGE_SIZE).min(self.visible.len());
        start..(start + PAGE_SIZE).min(self.visible.len())
    }

    /// The selected conversation while it is listed. A hidden selection
    /// returns `None`, which disables resume, copy and organization.
    pub fn selected_session(&self) -> Option<&AgentSession> {
        if let Some(source) = &self.workspace_source {
            if self
                .selected
                .as_ref()
                .is_some_and(|key| key.0 == source.0 && key.1 == source.1)
            {
                return self
                    .workspace_visible()
                    .into_iter()
                    .map(|index| &self.catalog.sessions[index])
                    .find(|session| open_session_key(session) == *source);
            }
        }
        let key = self.selected.as_ref()?;
        self.visible_sessions(0..self.visible.len())
            .find(|session| session.provider == key.0 && session.id == key.1)
    }

    /// The listed selection's identity, for keyboard-initiated actions.
    pub(crate) fn selected_key(&self) -> Option<SessionKey> {
        self.selected_session().map(session_key)
    }

    /// A selection that a refresh or filter removed from the list. It stays
    /// selected (never silently replaced) until the user chooses again.
    pub(crate) fn selection_hidden(&self) -> bool {
        self.selected.is_some() && self.selected_position().is_none()
    }

    /// Re-project after a catalog, library or background change. A refresh
    /// never retargets the selection: a selected identity that left the list
    /// stays selected but hidden, and its armed resume is withdrawn by
    /// `sync_preflight`. Only an empty selection adopts the first row.
    pub fn reconcile(&mut self) {
        self.project_query();
        if self.selected.is_none() {
            self.selected = self.first_visible_key();
        }
        self.sync_page();
        self.sync_preflight();
    }

    /// The user's own search, filter, scope, sort or Clear filters. The best
    /// match is selected, as in the other terminals: the user just chose a
    /// different list, so type-then-Enter resumes its top hit. Only a
    /// background refresh keeps a selection (see `reconcile`).
    pub(crate) fn apply_filters(&mut self) {
        self.cancel_confirmation();
        self.project_query();
        self.selected = self.first_visible_key();
        self.sync_page();
        self.sync_preflight();
    }

    fn sync_page(&mut self) {
        let last = self.visible.len().saturating_sub(1) / PAGE_SIZE;
        self.page = match self.selected_position() {
            Some(position) => position / PAGE_SIZE,
            // A hidden selection keeps the page the user was reading.
            None => self.page.min(last),
        };
    }

    fn project_query(&mut self) {
        self.query_error = SessionSearchIndex::query_error(&self.query);
        self.projects = self.search_index.projects(self.metadata());
        self.visible = self.search_index.query_with_library(
            &SessionQuery {
                text: &self.query,
                provider: self.provider,
                status: self.status,
                live_only: self.live_only,
                project: self.project_selection.as_deref().or_else(|| {
                    self.project_only
                        .then_some(self.project.as_deref())
                        .flatten()
                }),
                sort: self.sort,
            },
            self.metadata(),
            SessionLibraryFilter {
                pinned_only: self.pinned_only,
                recent_only: self.recent_only,
                pinned_first: true,
                archive: self.archive,
            },
        );
    }

    /// Withdraw an armed resume and any live-session warning. Every explicit
    /// navigation, filter, refresh or organization action goes through here.
    pub fn cancel_confirmation(&mut self) {
        self.pending_confirm = None;
        self.resume_warning = None;
    }

    /// Escape first withdraws a live-session warning (and the resume it
    /// guarded) before it closes the manager. Returns whether it did.
    pub(crate) fn cancel_resume_warning(&mut self) -> bool {
        if self.resume_warning.take().is_none() {
            return false;
        }
        self.pending_confirm = None;
        self.notice = Some("Resume cancelled.".into());
        true
    }

    /// The live-session warning for the listed selection, if one is showing.
    pub(crate) fn resume_warning(&self) -> Option<&ResumeWarning> {
        let warning = self.resume_warning.as_ref()?;
        self.selected_session()
            .is_some_and(|session| session.provider == warning.key.0 && session.id == warning.key.1)
            .then_some(warning)
    }

    /// Gate one resume request. A conversation that the latest scan observed
    /// running in another CLI first only shows the warning; a second explicit
    /// request for that exact identity (Resume anyway) is admitted. Returns
    /// whether the resume may be armed.
    pub(crate) fn admit_resume(&mut self, session: &AgentSession) -> bool {
        let key = session_key(session);
        if jterm_core::agent_workspace::managed_session_running(session) {
            return true;
        }
        let Some(text) = self.catalog.resume_conflict(session) else {
            self.resume_warning = None;
            return true;
        };
        if let Some(warning) = self
            .resume_warning
            .as_mut()
            .filter(|warning| warning.key == key)
        {
            warning.text = text;
            // A repeat this soon is the rest of a double-click or a quick
            // double Enter, not a decision about the warning it just raised.
            return warning.shown_at.elapsed() >= SESSION_RESUME_ANYWAY_DELAY;
        }
        self.pending_confirm = None;
        self.notice = None;
        self.resume_warning = Some(ResumeWarning {
            key,
            text,
            shown_at: Instant::now(),
        });
        false
    }

    /// The pending attempt ended without a spawn (its check failed, the
    /// launch failed, or its conversation is no longer the listed
    /// selection). A "Resume anyway" covers only the attempt it started, so
    /// both are withdrawn and the next Resume warns again.
    pub(crate) fn abandon_resume(&mut self, notice: Option<String>) {
        self.pending_confirm = None;
        self.resume_warning = None;
        if notice.is_some() {
            self.notice = notice;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.auto_refresh = false;
        self.cancel_confirmation();
        self.editor = None;
        self.bulk_mode = false;
        self.retire_bulk();
        self.preflight.select(None);
    }

    pub(crate) fn editing(&self) -> bool {
        self.editor.is_some() || self.bulk_mode
    }

    fn retire_bulk(&mut self) {
        self.bulk_epoch = std::sync::Arc::new(());
    }

    fn accepts_bulk(&self, reference: &BatchRef) -> bool {
        self.open
            && self.bulk_mode
            && !self.bulk_saving
            && self.editor.is_none()
            && std::sync::Arc::ptr_eq(&self.bulk_epoch, &reference.epoch)
    }

    fn batch_reference(&self) -> BatchRef {
        let mut keys: Vec<_> = self.checked.iter().cloned().collect();
        keys.sort_by(|a, b| (a.0.label(), &a.1).cmp(&(b.0.label(), &b.1)));
        BatchRef {
            epoch: self.bulk_epoch.clone(),
            keys: keys.into(),
        }
    }

    fn page_keys(&self) -> Vec<SessionKey> {
        self.visible_sessions(self.page_range())
            .map(session_key)
            .collect()
    }

    #[cfg(test)]
    fn page_reference(&self) -> BatchRef {
        self.page_reference_for(self.page_keys())
    }

    fn page_reference_for(&self, keys: Vec<SessionKey>) -> BatchRef {
        BatchRef {
            epoch: self.bulk_epoch.clone(),
            keys: keys.into(),
        }
    }

    pub(crate) fn toggle_bulk(&mut self) {
        if !self.open || self.editor.is_some() || self.bulk_saving {
            return;
        }
        self.bulk_mode = !self.bulk_mode;
        self.cancel_confirmation();
        self.retire_bulk();
    }

    pub(crate) fn check_session(&mut self, reference: &BatchRef, key: SessionKey, checked: bool) {
        if !self.accepts_bulk(reference) || !self.page_keys().contains(&key) {
            return;
        }
        if checked && !self.checked.contains(&key) && self.checked.len() >= SESSION_BATCH_LIMIT {
            self.bulk_notice = Some(format!(
                "Select at most {SESSION_BATCH_LIMIT} conversations per batch."
            ));
            return;
        }
        if checked {
            self.checked.insert(key);
        } else {
            self.checked.remove(&key);
        }
        self.bulk_notice = None;
        self.retire_bulk();
    }

    pub(crate) fn select_visible(&mut self, reference: &BatchRef) {
        if !self.accepts_bulk(reference) || self.page_keys().as_slice() != reference.keys.as_ref() {
            return;
        }
        let additional = reference
            .keys
            .iter()
            .filter(|key| !self.checked.contains(*key))
            .count();
        if self.checked.len() + additional > SESSION_BATCH_LIMIT {
            self.bulk_notice = Some(format!("This page would exceed the {SESSION_BATCH_LIMIT}-conversation limit. Clear the selection or choose individual rows."));
            return;
        }
        self.checked.extend(reference.keys.iter().cloned());
        self.bulk_notice = None;
        self.retire_bulk();
    }

    pub(crate) fn clear_checked(&mut self, reference: &BatchRef) {
        if !self.accepts_bulk(reference) {
            return;
        }
        self.checked.clear();
        self.bulk_notice = None;
        self.retire_bulk();
    }

    pub(crate) fn edit_bulk_tag(&mut self, reference: &BatchRef, value: String) {
        if !self.accepts_bulk(reference) {
            return;
        }
        if value.len() > BULK_TAG_INPUT_LIMIT {
            self.bulk_notice =
                Some("The tag exceeds the input limit. Your existing draft was kept.".into());
            return;
        }
        self.bulk_tag = value;
        self.bulk_notice = None;
    }

    pub(crate) fn apply_batch(&mut self, reference: &BatchRef, action: SessionBatchAction) {
        if !self.accepts_bulk(reference)
            || reference.keys.is_empty()
            || reference.keys.len() != self.checked.len()
            || !reference.keys.iter().all(|key| self.checked.contains(key))
        {
            return;
        }
        if matches!(&action, SessionBatchAction::AddTag(tag) | SessionBatchAction::RemoveTag(tag) if tag != &self.bulk_tag)
        {
            return;
        }
        let result = if self.workspace_enabled {
            self.shared_batch(&reference.keys, &action)
        } else {
            self.store.apply_batch(&reference.keys, action)
        };
        match result {
            Ok(count) => {
                if let Some(selected) = self
                    .selected
                    .clone()
                    .filter(|selected| reference.keys.contains(selected))
                {
                    self.note_own_change(selected, OwnChange::Batch);
                }
                self.bulk_saved_count = count;
                self.bulk_saving = true;
                self.bulk_notice = Some(format!("Saving {count} conversations…"));
                self.retire_bulk();
            }
            Err(error) => self.bulk_notice = Some(error),
        }
    }

    pub(crate) fn begin_organization(&mut self, key: &SessionKey) {
        if !self.open || self.editing() {
            return;
        }
        let Some(session) = self
            .selected_session()
            .filter(|session| session.provider == key.0 && session.id == key.1)
        else {
            return;
        };
        let title = session.title.clone();
        let facts = self.metadata().facts(key.0, &key.1);
        let alias = facts.alias.unwrap_or_default().to_owned();
        let tags = facts.tags.join(", ");
        self.cancel_confirmation();
        let mut editor = OrganizationEditor {
            reference: OrganizationRef {
                key: key.clone(),
                epoch: std::sync::Arc::new(()),
            },
            title,
            alias,
            tags,
            notice: None,
            invalid: None,
            saving: false,
        };
        editor.revalidate();
        self.editor = Some(editor);
    }

    fn editor_for(&mut self, reference: &OrganizationRef) -> Option<&mut OrganizationEditor> {
        self.open.then_some(())?;
        self.editor.as_mut().filter(|editor| {
            editor.reference.key == reference.key
                && std::sync::Arc::ptr_eq(&editor.reference.epoch, &reference.epoch)
                && !editor.saving
        })
    }

    pub(crate) fn organization_alias(&mut self, reference: &OrganizationRef, value: String) {
        if let Some(editor) = self.editor_for(reference) {
            // Keep the actual input for validation; never silently rewrite a
            // pasted control character into a different stored title, nor
            // cut a draft at a character count that disagrees with the bytes
            // the shared library admits.
            if value.len() > ALIAS_INPUT_LIMIT {
                editor.notice =
                    Some("That name is far over the limit. Your existing draft was kept.".into());
                return;
            }
            editor.alias = value;
            editor.notice = None;
            editor.revalidate();
        }
    }

    pub(crate) fn organization_tags(&mut self, reference: &OrganizationRef, value: String) {
        if let Some(editor) = self.editor_for(reference) {
            if value.len() > TAGS_INPUT_LIMIT {
                editor.notice =
                    Some("Those tags are far over the limit. Your existing draft was kept.".into());
                return;
            }
            editor.tags = value;
            editor.notice = None;
            editor.revalidate();
        }
    }

    pub(crate) fn save_organization(&mut self, reference: &OrganizationRef) {
        let Some(editor) = self.editor_for(reference) else {
            return;
        };
        if let Some(invalid) = editor.invalid.clone() {
            editor.notice = Some(invalid);
            return;
        }
        let alias = editor.alias.clone();
        let tags = editor.tags.clone();
        let result = if self.workspace_enabled {
            self.shared_details(&reference.key, &alias, &tags)
        } else {
            self.store
                .set_details(reference.key.0, &reference.key.1, &alias, &tags)
        };
        match result {
            Ok(()) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.saving = true;
                    editor.notice = None;
                }
                self.note_own_change(reference.key.clone(), OwnChange::Organized);
            }
            Err(error) => {
                if let Some(editor) = self.editor_for(reference) {
                    editor.notice = Some(error);
                }
            }
        }
    }

    pub(crate) fn cancel_organization(&mut self, reference: &OrganizationRef) {
        if self.editor_for(reference).is_some() {
            self.editor = None;
        }
    }

    pub(crate) fn cancel_current_organization(&mut self) {
        if self.bulk_mode && !self.bulk_saving {
            self.bulk_mode = false;
            self.retire_bulk();
        }
        if let Some(reference) = self.editor.as_ref().map(|editor| editor.reference.clone()) {
            self.cancel_organization(&reference);
        }
    }

    /// Bind the installation check to the listed selection. Returns whether
    /// its target changed (which starts a new check).
    pub fn sync_preflight(&mut self) -> bool {
        let selected = self.selected_session().cloned();
        let changed = self.preflight.select(selected.as_ref());
        if changed {
            self.pending_confirm = None;
            self.notice = None;
            if selected.is_some() {
                // A listed selection: any earlier "left this view" is over.
                self.left_view = None;
            }
        }
        // A live-session warning belongs to one listed identity and to the
        // scan evidence behind it. Selecting elsewhere, losing the row, or a
        // scan that no longer observes the process retires it, so a later
        // conflict needs a new explicit confirmation. (Losing the row also
        // changed the preflight target above, which withdrew an armed resume.)
        if let Some(mut warning) = self.resume_warning.take() {
            let text = selected
                .filter(|session| session_key(session) == warning.key)
                .and_then(|session| self.catalog.resume_conflict(&session));
            self.resume_warning = text.map(|text| {
                warning.text = text;
                warning
            });
        }
        changed
    }

    pub fn selected_position(&self) -> Option<usize> {
        let key = self.selected.as_ref()?;
        self.visible.iter().position(|index| {
            let session = &self.catalog.sessions[*index];
            session.provider == key.0 && session.id == key.1
        })
    }

    pub fn step(&mut self, delta: isize) {
        self.cancel_confirmation();
        if self.visible.is_empty() {
            self.selected = None;
            self.page = 0;
            return;
        }
        let last = self.visible.len() as isize - 1;
        let next = match self.selected_position() {
            Some(index) => (index as isize).saturating_add(delta),
            // A hidden selection has no row. A single step enters the page
            // being read at the edge it points to; page and edge moves count
            // from that page instead of from an arbitrary first row.
            None => {
                let page_start = (self.page * PAGE_SIZE) as isize;
                let page_end = page_start.saturating_add(PAGE_SIZE as isize - 1).min(last);
                match delta {
                    0 | 1 => page_start,
                    -1 => page_end,
                    delta if delta > 0 => page_start.saturating_add(delta),
                    delta => page_end.saturating_add(delta),
                }
            }
        }
        .clamp(0, last) as usize;
        let session = &self.catalog.sessions[self.visible[next]];
        self.selected = Some(session_key(session));
        self.page = next / PAGE_SIZE;
        self.sync_preflight();
    }

    pub fn next_sort(&self) -> SessionSort {
        let current = SessionSort::ALL
            .iter()
            .position(|sort| *sort == self.sort)
            .unwrap_or(0);
        SessionSort::ALL[(current + 1) % SessionSort::ALL.len()]
    }

    pub fn clear_filters(&mut self) {
        self.cancel_confirmation();
        self.query.clear();
        self.provider = None;
        self.status = None;
        self.live_only = false;
        self.pinned_only = false;
        self.recent_only = false;
        self.archive = SessionArchiveFilter::default();
        self.project_only = false;
        self.project_selection = None;
        self.show_projects = false;
        self.sort = SessionSort::Relevance;
        self.notice = None;
        self.query_notice = None;
        // Clearing is the user's own change: like any filter edit it selects
        // the first match of the full library.
        self.apply_filters();
    }
}

pub(crate) fn age(timestamp_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let seconds = now.saturating_sub(timestamp_ms) / 1000;
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

fn hashtags(tags: &[String]) -> String {
    tags.iter()
        .map(|tag| format!("#{tag}"))
        .collect::<Vec<_>>()
        .join("  ")
}

impl super::Frost {
    pub(crate) fn activate_workspace_session(&mut self, reference: WorkspaceSessionRef) {
        let state = &mut self.agent_sessions;
        if !state.workspace_reference_current(&reference) {
            return;
        }
        state.cancel_confirmation();
        state.selected = Some(session_key(&reference.session));
        state.workspace_source = Some(open_session_key(&reference.session));
        if !state
            .selected_session()
            .is_some_and(|session| same_launch_target(session, &reference.session))
        {
            state.notice = Some(
                "This source changed. Refresh the conversation list before opening it.".into(),
            );
            return;
        }
        state.workspace_pending = true;
        self.resume_agent_session(&session_key(&reference.session));
    }

    pub(crate) fn workspace_navigation(&self) -> iced::Element<'_, super::Message> {
        use super::Message as M;
        use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
        use iced::Length;
        let state = &self.agent_sessions;
        let size = self.chrome_font_size();
        let mut list = column![
            row![
                text("Agent Workspace").size(size),
                Space::new().width(Length::Fill),
                button(text("＋").size(size)).on_press(M::AiChatsNewChat)
            ]
            .spacing(4),
            row![
                button(text("Terminal").size(size - 1.0)).on_press(M::WorkspaceNewTerminal),
                button(text("Task").size(size - 1.0)).on_press(M::TaskComposerOpen),
                button(text("⋯").size(size)).on_press(M::AgentSessionsToggle)
            ]
            .spacing(4),
            text_input("Search conversations…", &state.query)
                .on_input(M::AgentSessionsQuery)
                .size(size),
        ]
        .spacing(8);
        let project = state
            .project_selection
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "All projects".into());
        list = list.push(
            button(text(format!("{project} ▾")).size(size))
                .on_press(M::AgentSessionsProjectsToggle)
                .width(Length::Fill),
        );
        if state.show_projects {
            list = list.push(
                button(text("All projects").size(size))
                    .on_press(M::WorkspaceProject(None))
                    .style(button::text),
            );
            for project in &state.projects {
                list = list.push(
                    button(text(project.root.to_string_lossy().into_owned()).size(size - 1.0))
                        .on_press(M::WorkspaceProject(Some(project.root.clone())))
                        .style(button::text),
                );
            }
        }
        list = list.push(
            row![
                button(text("All").size(size - 1.0))
                    .on_press(M::AgentSessionsClear)
                    .style(button::text),
                button(text("Running").size(size - 1.0))
                    .on_press(M::AgentSessionsLive(!state.live_only))
                    .style(if state.live_only {
                        button::primary
                    } else {
                        button::text
                    }),
                button(text("Needs you").size(size - 1.0))
                    .on_press(M::AgentSessionsStatus(
                        if state.status == Some(SessionStatus::Waiting) {
                            None
                        } else {
                            Some(SessionStatus::Waiting)
                        }
                    ))
                    .style(if state.status == Some(SessionStatus::Waiting) {
                        button::primary
                    } else {
                        button::text
                    }),
            ]
            .spacing(2),
        );
        if let Some(notice) = state
            .notice
            .as_ref()
            .or(state.workspace.notice.as_ref())
            .or(state.query_error.as_ref())
        {
            list = list.push(text(notice).size(size - 1.0).style(text::danger));
        }
        if state.pending_confirm.is_some() {
            list = list.push(text("Checking conversation…").size(size - 1.0));
        }
        let mut group = None;
        let visible = state.workspace_visible();
        for &index in &visible {
            let session = &state.catalog.sessions[index];
            let pinned = state.workspace.pinned(session);
            if group != Some(pinned) {
                group = Some(pinned);
                list = list.push(
                    text(if pinned {
                        "PINNED"
                    } else {
                        "RECENT CONVERSATIONS"
                    })
                    .size(size - 2.0)
                    .style(text::secondary),
                );
            }
            let binding = state.bindings.get(&open_session_key(session)).copied();
            let selected = !self.ai_chats.is_open
                && state.workspace_task.is_none()
                && binding.is_some_and(|id| {
                    self.sessions
                        .get(self.active)
                        .is_some_and(|session| session.id == id)
                });
            let title = state.workspace.title(session);
            let reference = WorkspaceSessionRef {
                epoch: state.catalog_epoch,
                session: session.clone(),
            };
            let row = row![
                button(text(if pinned { "★" } else { "☆" }).size(size))
                    .on_press(M::WorkspacePin(reference.clone()))
                    .style(button::text),
                button(
                    column![
                        text(crate::review_text::visible_bounded(title, 90)).size(size),
                        text(format!(
                            "{} · {}",
                            session.provider.label(),
                            if binding.is_some() {
                                "Open terminal"
                            } else {
                                "Ready to resume"
                            }
                        ))
                        .size(size - 2.0)
                        .style(text::secondary)
                    ]
                    .spacing(3)
                )
                .on_press(M::WorkspaceOpenSession(reference))
                .width(Length::Fill)
                .style(if selected {
                    button::primary
                } else {
                    button::text
                }),
            ]
            .spacing(2);
            list = list.push(row);
        }
        if visible.is_empty() {
            list = list.push(
                text(if state.scanner.is_scanning() {
                    "Discovering local sessions…"
                } else {
                    "No CLI conversations match"
                })
                .size(size - 1.0)
                .style(text::secondary),
            );
        }
        list = list.push(
            text("API CONVERSATIONS")
                .size(size - 2.0)
                .style(text::secondary),
        );
        let query = state.query.trim().to_lowercase();
        for chat in self.ai_chats.workspace.rows.iter().filter(|chat| {
            !chat.archived
                && (!state.live_only || chat.runtime.is_live())
                && (state.status != Some(SessionStatus::Waiting)
                    || matches!(
                        chat.runtime,
                        jterm_core::agent_workspace::RuntimeState::Failed
                            | jterm_core::agent_workspace::RuntimeState::Conflict
                    ))
                && state
                    .project_selection
                    .as_ref()
                    .is_none_or(|project| std::path::Path::new(&chat.cwd).starts_with(project))
                && (query.is_empty()
                    || format!("{} {}", chat.title, chat.preview)
                        .to_lowercase()
                        .contains(&query))
        }) {
            let selected = self.ai_chats.is_open
                && self
                    .ai_chats
                    .workspace
                    .id_for(self.ai_chats.store.active_id())
                    == Some(&chat.id);
            list = list.push(
                row![
                    button(text(if chat.pinned { "★" } else { "☆" }).size(size))
                        .on_press(M::WorkspaceApiPin(chat.id.clone(), !chat.pinned))
                        .style(button::text),
                    button(
                        column![
                            text(&chat.title).size(size),
                            text("API conversation")
                                .size(size - 2.0)
                                .style(text::secondary)
                        ]
                        .spacing(3),
                    )
                    .on_press(M::WorkspaceApi(chat.id.clone()))
                    .width(Length::Fill)
                    .style(if selected {
                        button::primary
                    } else {
                        button::text
                    })
                ]
                .spacing(2),
            );
        }
        if self.config.experimental_task_sidebar {
            list = list.push(
                text("TASK CONVERSATIONS")
                    .size(size - 2.0)
                    .style(text::secondary),
            );
            for task in self.task_manager.tasks().iter().filter(|task| {
                use jterm_core::agent_task::TaskStatus;
                task.status != TaskStatus::Archived
                    && (!state.live_only
                        || matches!(
                            task.status,
                            TaskStatus::Starting
                                | TaskStatus::Working
                                | TaskStatus::WaitingForApproval
                        ))
                    && (state.status != Some(SessionStatus::Waiting)
                        || matches!(
                            task.status,
                            TaskStatus::WaitingForApproval
                                | TaskStatus::WaitingForHuman
                                | TaskStatus::ReadyForReview
                                | TaskStatus::Failed
                        ))
            }) {
                if !query.is_empty() && !task.title.to_lowercase().contains(&query) {
                    continue;
                }
                if state
                    .project_selection
                    .as_ref()
                    .is_some_and(|project| project != &task.repo_root)
                {
                    continue;
                }
                list = list.push(
                    button(
                        column![
                            text(crate::review_text::visible_bounded(&task.title, 90)).size(size),
                            text(format!(
                                "{} · {}",
                                task.provider.display_name(),
                                task.status.label()
                            ))
                            .size(size - 2.0)
                            .style(text::secondary)
                        ]
                        .spacing(3),
                    )
                    .on_press(M::WorkspaceTask(task.id))
                    .width(Length::Fill)
                    .style(if state.workspace_task == Some(task.id) {
                        button::primary
                    } else {
                        button::text
                    }),
                );
            }
        }
        list = list.push(text("TERMINALS").size(size - 2.0).style(text::secondary));
        for (index, session) in self.sessions.iter().enumerate() {
            if state.bindings.values().any(|&id| id == session.id) {
                continue;
            }
            let title = self
                .tabs
                .iter()
                .position(|tab| tab.contains(index))
                .map(|tab| self.tab_label(tab))
                .unwrap_or_else(|| "Terminal".into());
            let selected =
                !self.ai_chats.is_open && state.workspace_task.is_none() && self.active == index;
            list = list.push(
                button(text(title).size(size))
                    .on_press(M::WorkspaceTerminal(session.id))
                    .width(Length::Fill)
                    .style(if selected {
                        button::primary
                    } else {
                        button::text
                    }),
            );
        }
        container(
            scrollable(list)
                .id(iced::widget::Id::new("workspace-session-navigation"))
                .height(Length::Fill),
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// The live tab this window already resumed for one conversation.
    fn agent_session_tab(&self, key: &SessionKey) -> Option<usize> {
        let session = self
            .agent_sessions
            .selected_session()
            .filter(|session| session_key(session) == *key)?;
        self.agent_sessions
            .bindings
            .get(&open_session_key(session))
            .copied()
            .and_then(|id| self.session_index_by_id(id))
            .filter(|index| !self.sessions[*index].hold_after_exit)
    }

    /// Resume (or switch to) the conversation an action was rendered for.
    /// A stale request for another or hidden identity does nothing.
    pub(crate) fn resume_agent_session(&mut self, key: &SessionKey) {
        if (!self.agent_sessions.open && !self.agent_sessions.workspace_pending)
            || self.agent_sessions.editing()
        {
            return;
        }
        let Some(session) = self
            .agent_sessions
            .selected_session()
            .filter(|session| session_key(session) == *key)
            .cloned()
        else {
            return;
        };
        let key = session_key(&session);
        self.native_enter_ownership.lock().claim_pressed();
        if let Some(index) = self.agent_session_tab(&key) {
            self.pane_zoomed = false;
            if !self.focus_session(index) {
                self.agent_sessions.open = true;
                self.agent_sessions.notice =
                    Some("The open terminal is no longer available".into());
                return;
            }
            self.record_resumed_agent_session(session.provider, &session.id);
            self.agent_sessions.close();
            self.agent_sessions.workspace_pending = false;
            self.agent_sessions.workspace_task = None;
            self.ai_chats.close(self.config.ai_redact_secrets);
            self.relayout();
            self.refresh_active_context();
            return;
        }
        // Not open here: a CLI observed holding this conversation first only
        // explains the conflict; the next explicit request (Resume anyway)
        // for this exact identity proceeds.
        if !self.agent_sessions.admit_resume(&session) {
            self.agent_sessions.open = true;
            self.agent_sessions.workspace_pending = false;
            return;
        }
        self.agent_sessions.sync_preflight();
        self.agent_sessions.pending_confirm = Some(key);
        self.agent_sessions.notice = None;
        self.agent_sessions.preflight.refresh();
    }

    pub(crate) fn finish_pending_agent_resume(&mut self) {
        if (!self.agent_sessions.open && !self.agent_sessions.workspace_pending)
            || self.agent_sessions.editing()
        {
            self.agent_sessions.pending_confirm = None;
            return;
        }
        let Some(key) = self.agent_sessions.pending_confirm.clone() else {
            return;
        };
        let Some(session) = self
            .agent_sessions
            .selected_session()
            .cloned()
            .filter(|session| session_key(session) == key)
        else {
            self.agent_sessions.abandon_resume(None);
            return;
        };
        if let SessionPreflightState::Unavailable(error) = self.agent_sessions.preflight.state() {
            // The attempt ends here: a later Resume must warn again.
            let error = error.clone();
            self.agent_sessions.abandon_resume(Some(error));
            return;
        }
        let Some(launch) = self
            .agent_sessions
            .preflight
            .ready_launch(&session)
            .cloned()
        else {
            return;
        };
        // A scan that finished while the check ran may have just observed
        // another CLI on this conversation: that needs the same explicit
        // confirmation as a conflict known before arming.
        if !self.agent_sessions.admit_resume(&session) {
            self.agent_sessions.open = true;
            self.agent_sessions.workspace_pending = false;
            return;
        }
        self.agent_sessions.pending_confirm = None;
        self.native_enter_ownership.lock().claim_held();
        self.launch_checked_agent_session(session, key, launch);
    }

    /// Copy the exact user-facing resume command for the conversation the
    /// button was rendered for, including an `env CODEX_HOME=…` prefix when
    /// the history lives in another provider home. It is never the resolved
    /// spawn argv with an absolute PATH. The background check computed it, so
    /// copying never touches the project directory or history file here.
    pub(crate) fn copy_agent_session_command(
        &mut self,
        key: &SessionKey,
    ) -> iced::Task<super::Message> {
        if !self.agent_sessions.open || self.agent_sessions.editing() {
            return iced::Task::none();
        }
        let state = &self.agent_sessions;
        let Some(session) = state
            .selected_session()
            .filter(|session| session_key(session) == *key)
        else {
            return iced::Task::none();
        };
        let command = state.preflight.command(session).map(str::to_owned);
        match command {
            Some(command) => {
                self.push_toast("Copied resume command", super::ToastKind::Success);
                iced::clipboard::write(command)
            }
            None => {
                let notice = match state.preflight.state() {
                    SessionPreflightState::Unavailable(error) => error.clone(),
                    _ => "The resume command is still being checked.".to_owned(),
                };
                self.agent_sessions.notice = Some(notice);
                iced::Task::none()
            }
        }
    }

    fn record_resumed_agent_session(&mut self, _provider: SessionProvider, _id: &str) {
        self.agent_sessions.workspace.refresh();
    }

    fn launch_checked_agent_session(
        &mut self,
        session: AgentSession,
        _key: SessionKey,
        _launch: SessionLaunch,
    ) {
        let launch = match jterm_core::agent_workspace::managed_launch(&session) {
            Ok(launch) => launch,
            Err(error) => {
                self.agent_sessions.abandon_resume(Some(error));
                return;
            }
        };
        let Some(cwd) = launch.cwd.to_str() else {
            self.agent_sessions
                .abandon_resume(Some("The session directory is not valid UTF-8".into()));
            return;
        };
        let argv: Vec<_> = std::iter::once(launch.program).chain(launch.args).collect();
        match super::Session::spawn_argv(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            Some(cwd),
            Some(&argv),
        ) {
            Ok(terminal) => {
                self.agent_sessions
                    .bindings
                    .insert(open_session_key(&session), terminal.id);
                self.record_resumed_agent_session(session.provider, &session.id);
                self.session_diagnostic = None;
                self.next_id += 1;
                let insert = (self.active + 1).min(self.sessions.len());
                self.sessions.insert(insert, terminal);
                super::apply_theme_colors(&mut self.sessions[insert].terminal, &self.theme);
                self.reindex_tabs_after_insert(insert);
                self.open_tab_with(insert);
                self.tabs[self.active_tab].title = Some(format!(
                    "{} · {}",
                    session.provider.label(),
                    self.agent_sessions.metadata().display_title(
                        session.provider,
                        &session.id,
                        &session.title,
                    ),
                ));
                self.agent_sessions.close();
                self.agent_sessions.workspace_pending = false;
                self.agent_sessions.workspace_task = None;
                self.ai_chats.close(self.config.ai_redact_secrets);
                self.agent_sessions.notice = None;
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
                self.push_toast(
                    format!("Resumed {} in a new tab", session.provider.label()),
                    super::ToastKind::Success,
                );
            }
            // The attempt ends without a CLI: a confirmed live-session
            // warning does not carry over to the next Resume.
            Err(error) => self.agent_sessions.abandon_resume(Some(format!(
                "Could not resume {}: {error}",
                session.provider.label()
            ))),
        }
    }

    pub(crate) fn agent_sessions_scroll_task(&self) -> iced::Task<super::Message> {
        let state = &self.agent_sessions;
        let local = state.selected_position().unwrap_or(0) % PAGE_SIZE;
        let count = state
            .visible
            .len()
            .saturating_sub(state.page * PAGE_SIZE)
            .min(PAGE_SIZE);
        let y = if count <= 1 {
            0.0
        } else {
            local as f32 / (count - 1) as f32
        };
        iced::widget::operation::snap_to(
            super::AGENT_SESSIONS_LIST_ID.clone(),
            iced::widget::operation::RelativeOffset { x: 0.0, y },
        )
    }

    pub(crate) fn agent_sessions_view(&self) -> iced::Element<'_, super::Message> {
        use super::Message as M;
        use iced::widget::{
            button, checkbox, column, container, mouse_area, row, scrollable, stack, text,
            text_input, tooltip, Space,
        };
        use iced::{Element, Length};
        let state = &self.agent_sessions;
        let size = self.chrome_font_size();
        if let Some(editor) = &state.editor {
            let reference = editor.reference.clone();
            let mut alias = text_input("Optional name for this conversation", &editor.alias)
                .id(super::AGENT_SESSIONS_ALIAS_ID.clone())
                .size(size)
                .padding(9);
            let mut tags = text_input("e.g. release, renderer, review", &editor.tags)
                .size(size)
                .padding(9);
            if !editor.saving {
                let alias_ref = reference.clone();
                alias =
                    alias.on_input(move |value| M::AgentSessionsAlias(alias_ref.clone(), value));
                let tags_ref = reference.clone();
                tags = tags.on_input(move |value| M::AgentSessionsTags(tags_ref.clone(), value));
            }
            // Only this window's own pending writes disable Save; the periodic
            // read-only reload of other terminals' changes never flickers it.
            let writable = state.store.is_loaded()
                && !state.store.is_saving()
                && state.store.error().is_none()
                && !editor.saving
                && editor.invalid.is_none();
            let mut form = column![
                text("Organize conversation").size(size + 5.0),
                text(format!("{} · {}", reference.key.0.label(), editor.title))
                    .size(size).style(text::secondary),
                text("Display name").size(size),
                alias,
                text(format!(
                    "Leave blank to use the provider's title. {} / {SESSION_ALIAS_MAX_BYTES} bytes.",
                    editor.alias.len()
                ))
                .size(size - 2.0).style(text::secondary),
                text("Tags").size(size),
                tags,
                text(format!(
                    "Separate tags with commas. Up to {SESSION_TAG_LIMIT} tags, {SESSION_TAG_MAX_BYTES} bytes each."
                ))
                .size(size - 2.0).style(text::secondary),
                text("Names and tags are shared across the four terminals. The original conversation stays unchanged.")
                    .size(size - 1.0).style(text::secondary),
            ].spacing(10);
            if let Some(error) = editor
                .notice
                .as_deref()
                .or(editor.invalid.as_deref())
                .or_else(|| state.store.error())
            {
                form = form.push(text(error).size(size).style(text::danger));
            }
            form = form.push(
                row![
                    button(
                        text(if editor.saving {
                            "Saving…"
                        } else {
                            "Save details"
                        })
                        .size(size)
                    )
                    .on_press_maybe(
                        writable.then_some(M::AgentSessionsSaveOrganization(reference.clone()))
                    )
                    .style(button::primary),
                    button(text("Cancel").size(size))
                        .on_press_maybe(
                            (!editor.saving)
                                .then_some(M::AgentSessionsCancelOrganization(reference.clone()))
                        )
                        .style(button::secondary),
                ]
                .spacing(8),
            );
            let panel = container(crate::widget_identity_scope::scope(
                reference.epoch.clone(),
                scrollable(form),
            ))
            .padding(22)
            .width(Length::Fixed(
                (self.win_size.width - 32.0).clamp(1.0, 640.0),
            ))
            .max_height((self.win_size.height - 40.0).max(1.0))
            .style(self.popup_style());
            // Consume clicks outside the editor without closing it or touching
            // the terminal behind the session manager.
            let backdrop = mouse_area(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(M::AgentSessionsOrganize(reference.key));
            return stack![
                Element::from(backdrop),
                Element::from(
                    container(panel)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                )
            ]
            .into();
        }
        let mut heading = row![
            text("Agent Sessions").size(size + 6.0),
            Space::new().width(Length::Fill)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        if self.config.experimental_task_sidebar {
            let badge = self.task_attention_badge();
            let tasks = button(
                text(match &badge {
                    Some((total, _)) => format!("Tasks · {total}"),
                    None => "Tasks".to_owned(),
                })
                .size(size),
            )
            .on_press(M::AgentSessionsShowTasks)
            .style(button::secondary);
            heading = heading.push(match badge {
                Some((_, summary)) => Element::from(tooltip(
                    tasks,
                    text(summary).size(size - 1.0),
                    tooltip::Position::Bottom,
                )),
                None => tasks.into(),
            });
        }
        heading = heading.push(
            button(
                text(if state.scanner.is_scanning() {
                    "Refreshing…"
                } else {
                    "Refresh"
                })
                .size(size),
            )
            .on_press_maybe((!state.scanner.is_scanning()).then_some(M::AgentSessionsRefresh))
            .style(button::secondary),
        );
        heading = heading.push(
            button(text("Close").size(size))
                .on_press(M::AgentSessionsClose)
                .style(button::text),
        );
        let codex = state.codex_count;
        let claude = state.catalog.sessions.len() - codex;
        let observed = state.observed_count;
        let query = text_input(
            "Search names, tags, projects…  provider:codex tag:review -is:live",
            &state.query,
        )
        .id(super::AGENT_SESSIONS_INPUT_ID.clone())
        .on_input(M::AgentSessionsQuery)
        .size(size)
        .padding(9);
        let providers = [
            ("All agents", None),
            ("Codex", Some(SessionProvider::Codex)),
            ("Claude", Some(SessionProvider::Claude)),
        ];
        let mut filters = row![].spacing(5);
        for (label, provider) in providers {
            filters = filters.push(
                button(text(label).size(size))
                    .on_press(M::AgentSessionsProvider(provider))
                    .style(if state.provider == provider {
                        button::primary
                    } else {
                        button::secondary
                    }),
            );
        }
        for archive in SessionArchiveFilter::ALL {
            filters = filters.push(
                button(text(archive.label()).size(size - 1.0))
                    .on_press(M::AgentSessionsArchiveFilter(archive))
                    .style(if state.archive == archive {
                        button::primary
                    } else {
                        button::secondary
                    }),
            );
        }
        let mut statuses = row![].spacing(4);
        for (label, status) in [
            ("All activity", None),
            ("Ready", Some(SessionStatus::Idle)),
            ("Working", Some(SessionStatus::Working)),
            ("Waiting", Some(SessionStatus::Waiting)),
            ("Unknown", Some(SessionStatus::Unknown)),
            ("Provider archived", Some(SessionStatus::Archived)),
        ] {
            statuses = statuses.push(
                button(text(label).size(size - 1.0))
                    .width(Length::Fixed(
                        label.chars().count() as f32 * size * 0.6 + 24.0,
                    ))
                    .on_press(M::AgentSessionsStatus(status))
                    .style(if state.status == status {
                        button::primary
                    } else {
                        button::secondary
                    }),
            );
        }
        let library = state.metadata();
        let matching = state.visible.len();
        let batch_reference = state.batch_reference();
        let mut list = column![].spacing(5);
        let page = state.page_range();
        let start = page.start;
        for session in state.visible_sessions(page) {
            let key = session_key(session);
            let selected = state.selected.as_ref() == Some(&key);
            let here = state
                .bindings
                .get(&open_session_key(session))
                .and_then(|id| self.session_index_by_id(*id))
                .is_some();
            // One identity lookup for every personal fact on this row.
            let facts = library.facts(session.provider, &session.id);
            let title = row![
                text(if facts.pinned {
                    format!("★ {}", session.provider.label())
                } else {
                    session.provider.label().into()
                })
                .size(size - 1.0),
                text(facts.display_title(&session.title)).size(size),
                Space::new().width(Length::Fill),
                text(age(session.updated_at_ms)).size(size - 2.0)
            ]
            .spacing(8);
            let mut content = column![
                title,
                text(session.cwd.to_string_lossy().into_owned()).size(size - 2.0),
                text(if here {
                    "Open in this window".to_owned()
                } else {
                    let pids = state.catalog.live_pids(session);
                    if pids.is_empty() {
                        session.status.label().to_owned()
                    } else {
                        format!("Process observed · {}", session.status.label())
                    }
                })
                .size(size - 2.0)
            ]
            .spacing(3);
            if !facts.tags.is_empty() {
                content = content.push(text(hashtags(facts.tags)).size(size - 2.0));
            }
            if facts.archived {
                content = content.push(text("Archived in library").size(size - 2.0));
            }
            let session_button = button(content)
                .on_press(M::AgentSessionsSelect(key.clone()))
                .width(Length::Fill)
                .padding(9)
                .style(if selected {
                    button::primary
                } else {
                    button::secondary
                });
            let row: Element<'_, M> = if state.bulk_mode {
                let reference = batch_reference.clone();
                let checked_key = key.clone();
                row![
                    checkbox(state.checked.contains(&key)).on_toggle_maybe(
                        (!state.bulk_saving).then_some(move |checked| M::AgentSessionsChecked(
                            reference.clone(),
                            checked_key.clone(),
                            checked
                        ))
                    ),
                    session_button
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                session_button.into()
            };
            list = list.push(crate::widget_identity_scope::scope_key(
                (key, state.bulk_mode),
                row,
            ));
        }
        if matching == 0 {
            list = list.push(
                text(if state.scanner.is_scanning() {
                    "Looking for your Codex and Claude sessions…"
                } else if state.catalog.sessions.is_empty() {
                    "No saved sessions found. Start a Codex or Claude conversation, then refresh."
                } else {
                    "No sessions match these filters."
                })
                .size(size),
            );
        }
        let end = (start + PAGE_SIZE).min(matching);
        let pages = matching.div_ceil(PAGE_SIZE).max(1);
        let page_controls = row![
            button(text("Previous").size(size - 1.0))
                .on_press_maybe(
                    (state.page > 0).then_some(M::AgentSessionsStep(-(PAGE_SIZE as isize)))
                )
                .style(button::secondary),
            text(format!(
                "{}–{} of {} · page {}/{}",
                if matching == 0 { 0 } else { start + 1 },
                end,
                matching,
                state.page + 1,
                pages
            ))
            .size(size - 1.0),
            button(text("Next").size(size - 1.0))
                .on_press_maybe(
                    (end < matching).then_some(M::AgentSessionsStep(PAGE_SIZE as isize))
                )
                .style(button::secondary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let list: Element<'_, M> = column![
            text(format!("{matching} matching · {}", state.sort.label()))
                .size(size - 1.0)
                .style(text::secondary),
            scrollable(list)
                .id(super::AGENT_SESSIONS_LIST_ID.clone())
                .height(Length::Fill),
            page_controls,
        ]
        .spacing(7)
        .height(Length::Fill)
        .into();
        let mut detail = column![].spacing(10);
        let mut actions = column![].spacing(6);
        let mut action_identity = None;
        if let Some(session) = state.selected_session() {
            let facts = library.facts(session.provider, &session.id);
            detail = detail.push(text(facts.display_title(&session.title)).size(size + 2.0));
            if facts.alias.is_some() {
                detail = detail.push(
                    text(format!("Original title: {}", session.title))
                        .size(size - 2.0)
                        .style(text::secondary),
                );
            }
            if !facts.tags.is_empty() {
                detail = detail.push(text(hashtags(facts.tags)).size(size));
            }
            detail = detail.push(
                text(format!(
                    "{}  ·  {}",
                    session.provider.label(),
                    age(session.updated_at_ms)
                ))
                .size(size)
                .style(text::secondary),
            );
            detail = detail.push(text(session.status.label()).size(size));
            let pids = state.catalog.live_pids(session);
            if !pids.is_empty() {
                detail = detail.push(
                    text(format!(
                        "Live process observed · PID {}",
                        pids.iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .size(size),
                );
            }
            detail = detail.push(text(session.cwd.to_string_lossy().into_owned()).size(size));
            if let Some(branch) = &session.branch {
                detail = detail.push(text(format!("Branch  {branch}")).size(size));
            }
            detail = detail.push(
                text(format!("Session  {}", session.id))
                    .size(size - 1.0)
                    .style(text::secondary),
            );
            // Computed by the background check; never derived here, where it
            // would touch the project directory and history file.
            let command = state.preflight.command(session);
            if let Some(command) = command {
                detail = detail.push(
                    text(format!("Resume command  {command}"))
                        .size(size - 1.0)
                        .style(text::secondary),
                );
            }
            detail = detail.push(text("Recent conversation").size(size));
            detail = detail.push(
                text(if session.preview.is_empty() {
                    "No text preview available".into()
                } else {
                    session.preview.clone()
                })
                .size(size),
            );
            let key = session_key(session);
            let pinned = facts.pinned;
            if let Some(timestamp) = facts.last_resumed_ms {
                detail = detail.push(
                    text(format!("Last resumed {}", age(timestamp)))
                        .size(size - 1.0)
                        .style(text::secondary),
                );
            }
            let bound = self.agent_session_tab(&key).is_some();
            let ready = state.preflight.ready_launch(session).is_some();
            let checking = state.pending_confirm.is_some();
            // Switching to this window's own tab never starts a second CLI.
            let warning = state.resume_warning().filter(|_| !bound);
            if let Some(warning) = warning {
                actions = actions.push(
                    text(format!(
                        "⚠ {} Choose Resume anyway to start another CLI, or press Esc to cancel.",
                        warning.text
                    ))
                    .size(size - 1.0)
                    .style(text::warning),
                );
            }
            if bound {
                actions = actions.push(
                    text("Open in this window")
                        .size(size - 1.0)
                        .style(text::secondary),
                );
            } else {
                let (message, unavailable) = match state.preflight.state() {
                    SessionPreflightState::Idle => ("Select a session to check".to_owned(), false),
                    SessionPreflightState::Checking => {
                        ("Checking project and agent installation…".to_owned(), false)
                    }
                    SessionPreflightState::Ready(_) => ("Ready to resume".to_owned(), false),
                    SessionPreflightState::Unavailable(error) => (error.clone(), true),
                };
                let status = text(message).size(size - 1.0);
                actions = actions.push(if unavailable {
                    status.style(text::danger)
                } else {
                    status.style(text::secondary)
                });
                if unavailable {
                    actions = actions.push(
                        button(text("Check again").size(size - 1.0))
                            .on_press(M::AgentSessionsCheck)
                            .style(button::text),
                    );
                }
            }
            actions = actions.push(
                button(
                    text(if bound {
                        "Switch to open tab"
                    } else if checking {
                        "Checking before opening…"
                    } else if warning.is_some() {
                        "Resume anyway"
                    } else {
                        "Resume in new tab"
                    })
                    .size(size),
                )
                .on_press_maybe(
                    ((bound || ready) && !checking).then(|| M::AgentSessionsResume(key.clone())),
                )
                .padding([8, 14])
                .width(Length::Fill)
                .style(if warning.is_some() {
                    button::danger
                } else {
                    button::primary
                }),
            );
            // Only this window's own pending writes disable organization; the
            // periodic reload of other terminals' changes never does.
            let writable = state.store.is_loaded()
                && !state.store.is_saving()
                && state.store.error().is_none();
            actions = actions.push(
                row![
                    button(
                        text(if pinned {
                            "★ Unpin"
                        } else {
                            "☆ Pin session"
                        })
                        .size(size - 1.0)
                    )
                    .on_press_maybe(writable.then(|| M::AgentSessionsPin(key.clone())))
                    .style(button::secondary),
                    button(text("Copy resume command").size(size - 1.0))
                        .on_press_maybe(
                            command.is_some().then(|| M::AgentSessionsCopy(key.clone())),
                        )
                        .style(button::text),
                ]
                .spacing(8)
                .wrap(),
            );
            let archived = facts.archived;
            // Iced buttons retain a press until release. A refresh may replace
            // the selected conversation or change an action while held, so the
            // complete identity must retire the old subtree's pressed state.
            action_identity = Some((
                key.clone(),
                pinned,
                archived,
                bound,
                ready,
                checking,
                warning.is_some(),
                command.is_some(),
            ));
            actions = actions.push(
                row![
                    button(text("Name and tags…").size(size - 1.0))
                        .on_press_maybe(writable.then_some(M::AgentSessionsOrganize(key.clone())))
                        .style(button::secondary),
                    button(
                        text(if archived {
                            "Restore to library"
                        } else {
                            "Archive in library"
                        })
                        .size(size - 1.0)
                    )
                    .on_press_maybe(writable.then_some(M::AgentSessionsArchive(key)))
                    .style(button::text),
                ]
                .spacing(8)
                .wrap(),
            );
        } else if state.selection_hidden() {
            // A refresh or filter removed the chosen conversation. It is never
            // replaced by whichever row moved into its place. When the user's
            // own change moved it, say so and which view finds it.
            detail = detail.push(
                text(state.left_view.unwrap_or(
                    "The selected conversation is no longer listed here. Its actions are paused until you select a conversation.",
                ))
                .size(size)
                .style(text::warning),
            );
        } else {
            detail = detail.push(text("Select a conversation to preview and resume.").size(size));
        }
        if let Some(notice) = &state.notice {
            let duplicate = matches!(state.preflight.state(), SessionPreflightState::Unavailable(error) if error == notice);
            if !duplicate {
                actions = actions.push(text(notice.clone()).size(size - 1.0).style(text::danger));
            }
        }
        if state.bulk_mode {
            actions = column![text(
                "Bulk selection is active. Finish organizing to resume a conversation."
            )
            .size(size - 1.0)
            .style(text::secondary)];
        }
        let actions =
            crate::widget_identity_scope::scope_key((action_identity, state.bulk_mode), actions);
        let details: Element<'_, M> = column![scrollable(detail).height(Length::Fill), actions]
            .spacing(10)
            .height(Length::Fill)
            .into();
        let wide = self.win_size.width >= 780.0;
        // At compact sizes the filter controls and action bar can exceed the
        // available height. Give the results a useful minimum and let the
        // whole manager scroll, keeping every action reachable.
        let compact = !wide || self.win_size.height < 720.0;
        let content_height = if compact {
            Length::Fixed(if wide { 380.0 } else { 540.0 })
        } else {
            Length::Fill
        };
        let content: Element<'_, M> = if wide {
            row![
                container(list)
                    .width(Length::FillPortion(5))
                    .height(Length::Fill),
                container(details)
                    .width(Length::FillPortion(4))
                    .height(Length::Fill)
                    .padding(8)
            ]
            .spacing(14)
            .height(content_height)
            .into()
        } else {
            column![
                container(list).height(Length::FillPortion(1)),
                container(details).height(Length::FillPortion(1))
            ]
            .spacing(8)
            .height(content_height)
            .into()
        };
        let mut project_controls = column![row![
            button(text(format!("Projects · {}", state.projects.len())).size(size - 1.0))
                .on_press(M::AgentSessionsProjectsToggle)
                .style(if state.show_projects {
                    button::primary
                } else {
                    button::secondary
                }),
            text(
                state
                    .project_selection
                    .as_deref()
                    .map(|path| jterm_core::review_input::safe_inline_display(
                        &path.to_string_lossy(),
                        1024
                    ))
                    .unwrap_or_else(|| "Choose a project to focus its conversations".into())
            )
            .size(size - 2.0)
            .style(text::secondary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .wrap()]
        .spacing(6);
        if state.show_projects {
            let mut projects = column![].spacing(4);
            for project in &state.projects {
                projects = projects.push(
                    button(
                        row![
                            text(jterm_core::review_input::safe_inline_display(
                                &project.root.to_string_lossy(),
                                1024
                            ))
                            .size(size - 1.0),
                            Space::new().width(Length::Fill),
                            text(format!(
                                "{} active · {} live · {} waiting · {} pinned · {} archived",
                                project.total,
                                project.live,
                                project.waiting,
                                project.pinned,
                                project.archived
                            ))
                            .size(size - 2.0),
                        ]
                        .spacing(8)
                        .wrap(),
                    )
                    .width(Length::Fill)
                    .padding(7)
                    .on_press(M::AgentSessionsProject(project.root.clone()))
                    .style(
                        if state.project_selection.as_ref() == Some(&project.root) {
                            button::primary
                        } else {
                            button::secondary
                        },
                    ),
                );
            }
            project_controls =
                project_controls.push(scrollable(projects).height(Length::Fixed(124.0)));
        }
        let search_problem = state
            .query_notice
            .as_deref()
            .or(state.query_error.as_deref());
        let search_help = text(search_problem.unwrap_or(
            "Search: tag:review provider:codex project:\"my project\" is:pinned -is:live",
        ))
        .size(size - 2.0);
        let search_help = if search_problem.is_some() {
            search_help.style(text::danger)
        } else {
            search_help.style(text::secondary)
        };
        let heading: Element<'_, M> = if self.win_size.width < 600.0 {
            heading.wrap().into()
        } else {
            heading.into()
        };
        let bulk_controls: Element<'_, M> = if state.bulk_mode {
            let checked_keys: HashSet<_> = state
                .checked
                .iter()
                .map(|(provider, id)| (*provider, id.as_str()))
                .collect();
            let visible_selected = state
                .visible_sessions(0..matching)
                .filter(|session| checked_keys.contains(&(session.provider, session.id.as_str())))
                .count();
            let hidden = state.checked.len().saturating_sub(visible_selected);
            let writable = state.store.is_loaded()
                && !state.store.is_saving()
                && !state.bulk_saving
                && !state.checked.is_empty();
            // One projection of the page serves both the widget identity and
            // the frozen reference its "Select visible page" action carries.
            let page_keys = state.page_keys();
            let page_reference = state.page_reference_for(page_keys.clone());
            let mut controls = column![
                row![
                    text(format!(
                        "{} selected · {} hidden by filters",
                        state.checked.len(),
                        hidden
                    ))
                    .size(size),
                    button(text("Finish organizing").size(size - 1.0))
                        .style(button::secondary)
                        .on_press_maybe((!state.bulk_saving).then_some(M::AgentSessionsBulkToggle)),
                ]
                .spacing(8)
                .wrap(),
                crate::widget_identity_scope::scope(
                    state.bulk_epoch.clone(),
                    crate::widget_identity_scope::scope_key(
                        page_keys,
                        row![
                            button(text("Select visible page").size(size - 1.0))
                                .style(button::secondary)
                                .on_press_maybe(
                                    (!state.bulk_saving)
                                        .then_some(M::AgentSessionsSelectVisible(page_reference))
                                ),
                            button(text("Clear selection").size(size - 1.0))
                                .style(button::text)
                                .on_press_maybe((!state.bulk_saving).then(|| {
                                    M::AgentSessionsClearChecked(batch_reference.clone())
                                })),
                        ]
                        .spacing(8)
                        .wrap()
                    )
                ),
            ]
            .spacing(6);
            let mut actions = row![].spacing(6);
            for (label, action) in [
                ("Pin", SessionBatchAction::Pin(true)),
                ("Unpin", SessionBatchAction::Pin(false)),
                ("Archive", SessionBatchAction::Archive(true)),
                ("Restore", SessionBatchAction::Archive(false)),
            ] {
                actions = actions.push(
                    button(text(label).size(size - 1.0))
                        .style(button::secondary)
                        .on_press_maybe(
                            writable
                                .then(|| M::AgentSessionsBatch(batch_reference.clone(), action)),
                        ),
                );
            }
            let reference = batch_reference.clone();
            let tag_problem = batch_tag_problem(&state.bulk_tag);
            controls = controls.push(
                text_input("One tag to add or remove", &state.bulk_tag)
                    .size(size)
                    .on_input_maybe((!state.bulk_saving).then_some({
                        move |value| M::AgentSessionsBulkTag(reference.clone(), value)
                    })),
            );
            if let Some(problem) = &tag_problem {
                controls =
                    controls.push(text(problem.clone()).size(size - 1.0).style(text::danger));
            }
            for (label, action) in [
                (
                    "Add tag",
                    SessionBatchAction::AddTag(state.bulk_tag.clone()),
                ),
                (
                    "Remove tag",
                    SessionBatchAction::RemoveTag(state.bulk_tag.clone()),
                ),
            ] {
                actions = actions.push(
                    button(text(label).size(size - 1.0))
                        .style(button::secondary)
                        .on_press_maybe(
                            (writable
                                && !state.bulk_tag.trim().is_empty()
                                && tag_problem.is_none())
                            .then(|| M::AgentSessionsBatch(batch_reference.clone(), action)),
                        ),
                );
            }
            controls = controls.push(crate::widget_identity_scope::scope(
                state.bulk_epoch.clone(),
                crate::widget_identity_scope::scope_key(state.bulk_tag.clone(), actions.wrap()),
            ));
            if let Some(notice) = &state.bulk_notice {
                controls = controls.push(text(notice).size(size - 1.0));
            }
            controls = controls.push(text("Actions apply to the complete checked selection, including hidden conversations. Provider transcripts stay intact.")
                .size(size - 2.0).style(text::secondary));
            container(controls)
                .padding(8)
                .style(container::bordered_box)
                .into()
        } else {
            button(
                text(if state.checked.is_empty() {
                    "Bulk organize…".to_owned()
                } else {
                    format!("Bulk organize… ({} checked)", state.checked.len())
                })
                .size(size - 1.0),
            )
            .on_press(M::AgentSessionsBulkToggle)
            .style(button::secondary)
            .into()
        };
        let mut body = column![heading,
            text(format!("{} conversations   ·   {codex} Codex   ·   {claude} Claude   ·   {observed} observed live", state.catalog.sessions.len())).size(size).style(text::secondary),
            query, search_help, filters.wrap(), statuses.wrap(),
            row![
                checkbox(state.live_only).label("Observed live only").on_toggle(M::AgentSessionsLive).size(size),
                checkbox(state.project_only).label("This folder")
                    .on_toggle_maybe(state.project.as_ref().map(|_| M::AgentSessionsScope as fn(bool) -> M)).size(size),
                tooltip(
                    button(text(format!("Sort: {} ↻", state.sort.label())).size(size))
                        .on_press(M::AgentSessionsSort(state.next_sort())).style(button::secondary),
                    text(format!(
                        "Cycle {} · Ctrl+S",
                        SessionSort::ALL.map(SessionSort::label).join(" / ")
                    ))
                    .size(size - 1.0),
                    tooltip::Position::Bottom,
                ),
                button(text("Clear filters").size(size - 1.0)).on_press(M::AgentSessionsClear).style(button::text),
            ].spacing(12).align_y(iced::Alignment::Center).wrap(),
            row![
                checkbox(state.pinned_only).label("Pinned only").on_toggle(M::AgentSessionsPinned).size(size),
                checkbox(state.recent_only).label("Previously resumed").on_toggle(M::AgentSessionsRecent).size(size),
                text(if !state.store.is_loaded() { "Loading saved library…" } else if state.store.is_saving() { "Saving library…" } else { "Names, tags, pins and archive are shared across terminals" }).size(size - 2.0).style(text::secondary),
            ].spacing(12).align_y(iced::Alignment::Center).wrap(),
            project_controls,
            bulk_controls,
            text("Activity reflects the last saved event. Live badges require an observed matching process.").size(size - 2.0).style(text::secondary),
            content,
        ].spacing(9);
        if state.project_only {
            if let Some(project) = &state.project {
                body = body.push(
                    text(format!(
                        "Folder scope: {} (including subfolders)",
                        project.display()
                    ))
                    .size(size - 2.0)
                    .style(text::secondary),
                );
            }
        }
        if let Some(error) = state.store.error() {
            body = body.push(
                text(format!("Saved library: {error}"))
                    .size(size - 2.0)
                    .style(text::danger),
            );
        }
        for warning in state.catalog.warnings.iter().take(2) {
            body = body.push(text(warning.clone()).size(size - 2.0).style(text::warning));
        }
        body = body.push(
            text(if state.bulk_mode { "Bulk mode · checked conversations stay selected across filters · Esc finishes organizing" } else { "↑ ↓ select · PgUp/PgDn page · Home/End list edges · Ctrl+L search · Ctrl+S sort · Enter resume · F5 refresh · Esc close" })
                .size(size - 2.0)
                .style(text::secondary),
        );
        let body: Element<'_, M> = if compact {
            scrollable(body).height(Length::Fill).into()
        } else {
            body.into()
        };
        let panel = container(body)
            .padding(18)
            .width(Length::Fixed(
                (self.win_size.width - 32.0).clamp(1.0, 1120.0),
            ))
            .height(Length::Fixed((self.win_size.height - 40.0).max(1.0)))
            .style(self.popup_style());
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(M::AgentSessionsClose);
        let centered = container(mouse_area(panel).on_press(M::AgentSessionsSurface))
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_click_rejects_replaced_source_and_hidden_rows() {
        let first = session(SessionProvider::Codex, "exact-id", "First source");
        let mut state = library();
        state.replace_catalog(catalog(vec![first.clone()]));
        let reference = WorkspaceSessionRef {
            epoch: state.catalog_epoch,
            session: first.clone(),
        };
        assert!(state.workspace_reference_current(&reference));
        state.set_query("no matching conversation".into());
        assert!(!state.workspace_reference_current(&reference));
        state.set_query(String::new());
        let mut replacement = first;
        replacement.source = PathBuf::from("/another/profile/session.jsonl");
        state.replace_catalog(catalog(vec![replacement]));
        assert!(!state.workspace_reference_current(&reference));
    }

    #[test]
    fn workspace_live_binding_includes_provider_source() {
        let first = session(SessionProvider::Codex, "same-native-id", "First source");
        let mut second = first.clone();
        second.source = PathBuf::from("/another/profile/session.jsonl");
        assert_ne!(open_session_key(&first), open_session_key(&second));
        assert!(!same_launch_target(&first, &second));
    }

    use super::*;
    use std::time::Instant;
    fn session(provider: SessionProvider, id: &str, title: &str) -> AgentSession {
        AgentSession {
            provider,
            id: id.into(),
            title: title.into(),
            cwd: "/tmp/project".into(),
            updated_at_ms: 0,
            status: SessionStatus::Idle,
            preview: "recent message".into(),
            branch: Some("main".into()),
            source: "/tmp/session.jsonl".into(),
            archived: false,
        }
    }
    fn catalog(sessions: Vec<AgentSession>) -> SessionCatalog {
        SessionCatalog {
            sessions,
            ..Default::default()
        }
    }
    fn library() -> SessionLibrary {
        SessionLibrary::with_store(SessionLibraryStore::with_path(
            std::env::temp_dir()
                .join(format!(
                    "frost-session-library-test-{}",
                    uuid::Uuid::new_v4()
                ))
                .join("library.json"),
        ))
    }
    fn batch_library() -> SessionLibrary {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(
                SessionProvider::Codex,
                "11111111-1111-4111-8111-111111111111",
                "First task",
            ),
            session(
                SessionProvider::Claude,
                "22222222-2222-4222-8222-222222222222",
                "Second task",
            ),
        ]));
        library.open = true;
        library.toggle_bulk();
        settle_library(&mut library);
        library
    }
    fn settle_library(library: &mut SessionLibrary) {
        let until = Instant::now() + Duration::from_secs(3);
        while !library.store.is_loaded() || library.store.is_busy() || library.bulk_saving {
            library.poll();
            assert!(Instant::now() < until, "library worker did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn bulk_checked_identities_survive_filters_and_archive_exactly_that_selection() {
        let mut library = batch_library();
        library.select_visible(&library.page_reference());
        let selected = library.checked.clone();
        library.query = "Second".into();
        library.reconcile();
        assert_eq!(library.filtered().len(), 1);
        assert_eq!(library.checked, selected);
        let reference = library.batch_reference();
        library.apply_batch(&reference, SessionBatchAction::Archive(true));
        assert!(library.bulk_saving);
        library.clear_checked(&library.batch_reference());
        assert_eq!(
            library.checked, selected,
            "saving cannot relabel its selection"
        );
        settle_library(&mut library);
        for (provider, id) in &selected {
            assert!(library.store.snapshot().is_archived(*provider, id));
        }
        assert_eq!(library.checked, selected);
        assert!(library.filtered().is_empty());
    }

    #[test]
    fn retained_batch_callback_cannot_apply_after_selection_or_opening_changes() {
        let mut library = batch_library();
        let keys = library.page_keys();
        library.check_session(&library.batch_reference(), keys[0].clone(), true);
        let old = library.batch_reference();
        library.check_session(&library.batch_reference(), keys[1].clone(), true);
        library.apply_batch(&old, SessionBatchAction::Pin(true));
        assert!(!library.store.is_busy());
        let old = library.batch_reference();
        library.close();
        library.open = true;
        library.toggle_bulk();
        library.apply_batch(&old, SessionBatchAction::Pin(true));
        assert!(!library.store.is_busy());
        assert_eq!(library.store.snapshot().pinned_count(), 0);
        assert_eq!(library.checked.len(), 2);
    }

    #[test]
    fn batch_tag_is_checked_live_with_the_shared_batch_rules() {
        assert_eq!(batch_tag_problem(""), None);
        assert_eq!(batch_tag_problem("   "), None);
        assert_eq!(batch_tag_problem(" Review "), None);
        assert_eq!(
            batch_tag_problem("review,release").as_deref(),
            Some("A batch adds or removes one tag; remove the comma.")
        );
        assert!(batch_tag_problem(&"t".repeat(SESSION_TAG_MAX_BYTES + 1)).is_some());
        assert!(batch_tag_problem("bad\u{202e}tag").is_some());
        let at_limit = "t".repeat(SESSION_TAG_MAX_BYTES);
        assert_eq!(batch_tag_problem(&at_limit), None);
    }

    #[test]
    fn rejected_batch_keeps_selection_and_tag_draft_and_cannot_use_an_old_tag() {
        let mut library = batch_library();
        library.select_visible(&library.page_reference());
        library.edit_bulk_tag(&library.batch_reference(), "old".into());
        let reference = library.batch_reference();
        library.edit_bulk_tag(&reference, "new".into());
        library.apply_batch(&reference, SessionBatchAction::AddTag("old".into()));
        assert!(!library.store.is_busy());
        library.edit_bulk_tag(&library.batch_reference(), "bad,tag".into());
        library.apply_batch(
            &library.batch_reference(),
            SessionBatchAction::AddTag("bad,tag".into()),
        );
        assert!(!library.store.is_busy());
        assert_eq!(library.checked.len(), 2);
        assert_eq!(library.bulk_tag, "bad,tag");
        assert!(library.bulk_notice.is_some());
        assert!(
            library.editing(),
            "bulk tag input must own Enter rather than resume"
        );
        library.cancel_current_organization();
        assert!(!library.editing());
        assert_eq!(library.bulk_tag, "bad,tag");
    }

    #[test]
    fn select_visible_page_is_atomic_at_the_batch_limit_and_rejects_a_changed_page() {
        let mut library = batch_library();
        let sessions: Vec<_> = (0..300)
            .map(|index| {
                session(
                    SessionProvider::Codex,
                    &uuid::Uuid::from_u128(index + 1).to_string(),
                    &format!("Task {index:03}"),
                )
            })
            .collect();
        library.replace_catalog(catalog(sessions));
        library.checked = library
            .catalog
            .sessions
            .iter()
            .take(250)
            .map(|session| (session.provider, session.id.clone()))
            .collect();
        library.page = 3;
        let page = library.page_reference();
        library.select_visible(&page);
        assert_eq!(library.checked.len(), 250);
        assert!(library.bulk_notice.is_some());
        library.checked.clear();
        library.page = 0;
        library.select_visible(&page);
        assert!(library.checked.is_empty());
    }
    #[test]
    fn refresh_preserves_provider_and_session_identity_after_reorder() {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "one", "First"),
            session(SessionProvider::Claude, "two", "Second"),
        ]));
        library.step(1);
        let mut next = library.catalog.clone();
        next.sessions.reverse();
        library.replace_catalog(next);
        assert_eq!(library.selected_session().unwrap().id, "two");
        // The user's own filter change moves to the first match when the
        // chosen conversation is no longer listed.
        library.provider = Some(SessionProvider::Codex);
        library.apply_filters();
        assert_eq!(library.selected_session().unwrap().id, "one");
    }
    #[test]
    fn hidden_selection_cannot_resume_and_filters_reconcile() {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "one", "Repair parser"),
            session(SessionProvider::Claude, "two", "Fix renderer"),
        ]));
        library.query = "renderer claude".into();
        library.apply_filters();
        assert_eq!(library.selected_session().unwrap().id, "two");
        library.query = "no matches".into();
        library.apply_filters();
        assert!(library.selected_session().is_none());
        library.step(1);
        assert!(library.selected.is_none());
    }
    #[test]
    fn live_filter_uses_process_evidence_independent_of_historical_activity() {
        let mut library = library();
        let mut snapshot = catalog(vec![
            session(SessionProvider::Codex, "one", "Done"),
            session(SessionProvider::Claude, "two", "Old working event"),
        ]);
        snapshot.sessions[1].status = SessionStatus::Working;
        snapshot
            .live_processes
            .push(jterm_core::agent_sessions::LiveSessionProcess {
                provider: SessionProvider::Codex,
                id: "one".into(),
                pid: 42,
            });
        library.replace_catalog(snapshot);
        library.live_only = true;
        library.apply_filters();
        assert_eq!(library.filtered().len(), 1);
        assert_eq!(library.selected_session().unwrap().id, "one");
        let mut next = library.catalog.clone();
        next.live_processes.clear();
        library.replace_catalog(next);
        // The refresh hid the selection; it is kept, not replaced.
        assert!(library.selected_session().is_none());
        assert_eq!(library.selected.as_ref().unwrap().1, "one");
        assert!(library.selection_hidden());
    }
    #[test]
    fn pages_reach_every_session_and_unchanged_refresh_preserves_navigation() {
        let mut library = library();
        let sessions = (0..205)
            .map(|i| {
                let mut s = session(SessionProvider::Codex, &format!("session-{i:03}"), "Task");
                s.updated_at_ms = i;
                s
            })
            .collect();
        library.replace_catalog(catalog(sessions));
        assert_eq!(library.filtered().len(), 205);
        library.step(PAGE_SIZE as isize);
        assert_eq!(library.page, 1);
        assert_eq!(library.selected_position(), Some(80));
        let key = library.selected.clone();
        assert!(!library.replace_catalog(library.catalog.clone()));
        assert_eq!(library.selected, key);
        assert_eq!(library.page, 1);
        library.step(isize::MAX);
        assert_eq!(library.selected_position(), Some(204));
        assert_eq!(library.page, 2);
        library.step(isize::MIN);
        assert_eq!(library.selected_position(), Some(0));
        assert_eq!(library.page, 0);
    }
    #[test]
    fn ranked_search_and_directory_scope_compose_and_reset() {
        let mut library = library();
        let mut title = session(SessionProvider::Codex, "title", "Fix renderer");
        title.cwd = "/work/repo/src".into();
        let mut incidental = session(SessionProvider::Codex, "preview", "Other task");
        incidental.preview = "renderer".into();
        incidental.cwd = "/work/repo-other".into();
        incidental.updated_at_ms = 999;
        library.replace_catalog(catalog(vec![incidental, title]));
        library.query = "renderer".into();
        library.selected = None;
        library.apply_filters();
        assert_eq!(library.selected_session().unwrap().id, "title");
        library.project = Some("/work/repo".into());
        library.project_only = true;
        library.apply_filters();
        assert_eq!(library.filtered().len(), 1);
        library.clear_filters();
        assert_eq!(library.filtered().len(), 2);
        // Clearing is the user's own edit: the top hit of the full library
        // ("preview" is now the most recent) is selected, as everywhere.
        assert_eq!(library.filtered()[0].id, "preview");
        assert_eq!(library.selected_session().unwrap().id, "preview");
    }
    fn until_library(library: &mut SessionLibrary, ready: impl Fn(&SessionLibrary) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            library.poll();
            if ready(library) {
                return;
            }
            assert!(Instant::now() < deadline, "library worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn durable_metadata_reprojects_pins_recent_receipts_and_reload() {
        let root =
            std::env::temp_dir().join(format!("frost-library-projection-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("library.json");
        let mut library = SessionLibrary::with_store(SessionLibraryStore::with_path(path.clone()));
        let first = "11111111-1111-4111-8111-111111111111";
        let second = "22222222-2222-4222-8222-222222222222";
        let mut a = session(SessionProvider::Codex, first, "Pinned task");
        a.updated_at_ms = 1;
        let mut b = session(SessionProvider::Claude, second, "Newest task");
        b.updated_at_ms = 999;
        library.replace_catalog(catalog(vec![a, b]));
        until_library(&mut library, |s| s.store.is_loaded() && !s.store.is_busy());
        assert_eq!(library.filtered()[0].id, second);
        library
            .store
            .set_pinned(SessionProvider::Codex, first, true)
            .unwrap();
        until_library(&mut library, |s| {
            s.store.snapshot().is_pinned(SessionProvider::Codex, first)
        });
        assert_eq!(library.filtered()[0].id, first);
        library.pinned_only = true;
        library.reconcile();
        assert_eq!(library.filtered().len(), 1);
        library
            .store
            .record_resumed(SessionProvider::Claude, second)
            .unwrap();
        until_library(&mut library, |s| {
            s.store
                .snapshot()
                .last_resumed(SessionProvider::Claude, second)
                .is_some()
        });
        library
            .store
            .set_pinned(SessionProvider::Codex, first, false)
            .unwrap();
        until_library(&mut library, |s| {
            !s.store.snapshot().is_pinned(SessionProvider::Codex, first)
        });
        assert!(library.filtered().is_empty());
        assert!(library.selected_session().is_none());
        library.pinned_only = false;
        library.recent_only = true;
        library.sort = SessionSort::RecentlyResumed;
        library.reconcile();
        assert_eq!(library.filtered()[0].id, second);
        let mut reloaded = SessionLibrary::with_store(SessionLibraryStore::with_path(path));
        until_library(&mut reloaded, |s| s.store.is_loaded() && !s.store.is_busy());
        assert!(!reloaded
            .store
            .snapshot()
            .is_pinned(SessionProvider::Codex, first));
        assert!(reloaded
            .store
            .snapshot()
            .last_resumed(SessionProvider::Claude, second)
            .is_some());
        drop(reloaded);
        drop(library);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn explicit_navigation_and_close_cancel_queued_confirmation() {
        let mut library = library();
        library.replace_catalog(catalog(vec![session(
            SessionProvider::Codex,
            "one",
            "First",
        )]));
        library.pending_confirm = library.selected.clone();
        library.step(0);
        assert!(library.pending_confirm.is_none());
        library.pending_confirm = library.selected.clone();
        library.close();
        library.open = true;
        library.reconcile();
        assert!(library.pending_confirm.is_none());
    }
    #[test]
    fn manager_shortcut_and_palette_use_the_same_command() {
        assert_eq!(
            "agent:sessions"
                .parse::<crate::keybindings::Command>()
                .unwrap(),
            crate::keybindings::Command::AgentSessions
        );
        assert_eq!(
            crate::keybindings::KeyBindings::default_bindings().get_command("ctrl+shift+alt+g"),
            Some(crate::keybindings::Command::AgentSessions)
        );
    }

    #[test]
    fn organization_callbacks_belong_to_the_explicit_editor_opening() {
        let mut library = library();
        let id = "11111111-1111-4111-8111-111111111111";
        let key = (SessionProvider::Codex, id.to_owned());
        library.replace_catalog(catalog(vec![session(
            SessionProvider::Codex,
            id,
            "Original",
        )]));
        // A callback retained after close cannot open an editor or queue a save.
        library.begin_organization(&key);
        assert!(!library.editing());
        library.open = true;
        library.pending_confirm = Some(key.clone());
        library.begin_organization(&key);
        assert!(library.pending_confirm.is_none());
        let old = library.editor.as_ref().unwrap().reference.clone();
        library.organization_alias(&old, "First draft".into());
        library.cancel_organization(&old);
        library.begin_organization(&key);
        let current = library.editor.as_ref().unwrap().reference.clone();
        library.organization_alias(&current, "Current draft".into());
        library.organization_alias(&old, "Stale replacement".into());
        library.organization_tags(&old, "stale".into());
        library.cancel_organization(&old);
        library.save_organization(&old);
        let editor = library.editor.as_ref().unwrap();
        assert_eq!(editor.alias, "Current draft");
        assert!(editor.tags.is_empty());
        assert!(!editor.saving);
        library.close();
        library.save_organization(&current);
        assert!(!library.editing());
    }

    #[test]
    fn organizer_save_archive_search_and_project_views_share_durable_identity() {
        let root = std::env::temp_dir().join(format!("frost-organizer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("library.json");
        let mut library = SessionLibrary::with_store(SessionLibraryStore::with_path(path.clone()));
        let first = "11111111-1111-4111-8111-111111111111";
        let second = "22222222-2222-4222-8222-222222222222";
        let mut a = session(SessionProvider::Codex, first, "Untitled conversation");
        a.cwd = "/work/repo/src".into();
        a.updated_at_ms = 100;
        let mut b = session(SessionProvider::Claude, second, "Another conversation");
        b.cwd = "/work/repo-other".into();
        library.replace_catalog(catalog(vec![a, b]));
        until_library(&mut library, |state| {
            state.store.is_loaded() && !state.store.is_busy()
        });
        library.open = true;
        library.begin_organization(&(SessionProvider::Codex, first.into()));
        let reference = library.editor.as_ref().unwrap().reference.clone();
        library.organization_alias(&reference, "\u{202e}hidden".into());
        library.save_organization(&reference);
        assert!(library.editor.as_ref().unwrap().notice.is_some());
        assert!(!library.editor.as_ref().unwrap().saving);
        assert!(library
            .store
            .snapshot()
            .alias(SessionProvider::Codex, first)
            .is_none());
        library.organization_alias(&reference, "Renderer release".into());
        library.organization_tags(&reference, "release, review".into());
        library.save_organization(&reference);
        assert!(library.editor.as_ref().unwrap().saving);
        // An edit racing the accepted save cannot relabel the committed transaction.
        library.organization_alias(&reference, "Late mutation".into());
        until_library(&mut library, |state| {
            !state.editing() && !state.store.is_busy()
        });
        assert_eq!(
            library
                .store
                .snapshot()
                .alias(SessionProvider::Codex, first),
            Some("Renderer release")
        );
        library.query = "tag:review provider:codex \"Renderer release\"".into();
        library.reconcile();
        assert_eq!(
            library
                .filtered()
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec![first]
        );
        assert_eq!(library.filtered()[0].title, "Untitled conversation");
        library
            .store
            .set_archived(SessionProvider::Codex, first, true)
            .unwrap();
        until_library(&mut library, |state| {
            state
                .store
                .snapshot()
                .is_archived(SessionProvider::Codex, first)
        });
        assert!(library.filtered().is_empty());
        assert!(library.selected_session().is_none());
        library.archive = SessionArchiveFilter::Archived;
        library.reconcile();
        assert_eq!(library.filtered()[0].id, first);
        library.clear_filters();
        library.archive = SessionArchiveFilter::All;
        library.project_selection = Some("/work/repo".into());
        library.reconcile();
        assert_eq!(library.filtered().len(), 1);
        assert_eq!(library.filtered()[0].id, first);
        library.query = "is:unknown".into();
        library.reconcile();
        assert!(library.query_error.is_some());
        assert!(library.filtered().is_empty());
        library.close();
        drop(library);
        let mut reloaded = SessionLibrary::with_store(SessionLibraryStore::with_path(path));
        until_library(&mut reloaded, |state| {
            state.store.is_loaded() && !state.store.is_busy()
        });
        assert_eq!(
            reloaded
                .store
                .snapshot()
                .alias(SessionProvider::Codex, first),
            Some("Renderer release")
        );
        assert_eq!(
            reloaded
                .store
                .snapshot()
                .tags(SessionProvider::Codex, first),
            &["release", "review"]
        );
        assert!(reloaded
            .store
            .snapshot()
            .is_archived(SessionProvider::Codex, first));
        drop(reloaded);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn live(
        provider: SessionProvider,
        id: &str,
        pid: u32,
    ) -> jterm_core::agent_sessions::LiveSessionProcess {
        jterm_core::agent_sessions::LiveSessionProcess {
            provider,
            id: id.into(),
            pid,
        }
    }

    /// Let the shown warning reach the age at which "Resume anyway" counts.
    fn age_resume_warning(library: &mut SessionLibrary) {
        let warning = library.resume_warning.as_mut().expect("a warning is shown");
        warning.shown_at = Instant::now()
            .checked_sub(SESSION_RESUME_ANYWAY_DELAY)
            .expect("the monotonic clock is past the delay");
    }

    #[test]
    fn a_resume_anyway_covers_only_the_attempt_it_started() {
        let mut library = library();
        library.open = true;
        let mut snapshot = catalog(vec![session(SessionProvider::Codex, "busy", "Shared")]);
        snapshot
            .live_processes
            .push(live(SessionProvider::Codex, "busy", 4242));
        library.replace_catalog(snapshot);
        library.selected = Some((SessionProvider::Codex, "busy".into()));
        library.sync_preflight();
        let busy = library.selected_session().unwrap().clone();
        assert!(!library.admit_resume(&busy), "the first request warns");
        age_resume_warning(&mut library);
        assert!(library.admit_resume(&busy), "Resume anyway");
        library.pending_confirm = Some(session_key(&busy));
        // Its readiness check fails, for example because the CLI is missing.
        library.abandon_resume(Some("codex was not found".into()));
        assert!(library.pending_confirm.is_none());
        assert_eq!(library.notice.as_deref(), Some("codex was not found"));
        assert!(library.resume_warning().is_none());
        assert!(
            !library.admit_resume(&busy),
            "after fixing it, the next Resume warns again instead of launching"
        );
        // Every way an armed attempt ends without a CLI withdraws it.
        let source = include_str!("agent_sessions.rs");
        let body = |start: &str| {
            source
                .split(start)
                .nth(1)
                .unwrap()
                .split("\n    }\n")
                .next()
                .unwrap()
                .to_owned()
        };
        let finish = body("pub(crate) fn finish_pending_agent_resume(");
        assert!(finish.contains("self.agent_sessions.abandon_resume(None);"));
        assert!(finish.contains("self.agent_sessions.abandon_resume(Some(error));"));
        let launch = body("fn launch_checked_agent_session(");
        assert_eq!(
            launch.matches("abandon_resume(Some(").count(),
            3,
            "the managed-launch validation has its own guarded failure exit"
        );
    }

    #[test]
    fn opening_replaces_a_selection_the_current_filters_no_longer_list() {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "one", "First"),
            session(SessionProvider::Claude, "two", "Second"),
        ]));
        library.selected = Some((SessionProvider::Claude, "two".into()));
        library.sync_preflight();
        // A filter kept from the previous opening no longer lists it.
        library.provider = Some(SessionProvider::Codex);
        library.open = true;
        library.adopt_listed_selection();
        assert_eq!(library.selected_session().unwrap().id, "one");
        assert!(!library.selection_hidden());
        // A selection the filters still list survives reopening.
        library.provider = None;
        library.selected = Some((SessionProvider::Claude, "two".into()));
        library.adopt_listed_selection();
        assert_eq!(library.selected_session().unwrap().id, "two");
        // A background refresh still never replaces a selection.
        library.provider = Some(SessionProvider::Codex);
        library.reconcile();
        assert!(library.selection_hidden());
        // Opening runs one installation check, not a second refresh of it.
        let source = include_str!("agent_sessions.rs");
        let refresh = source
            .split("    pub fn refresh(&mut self) {")
            .nth(1)
            .unwrap()
            .split("\n    }\n")
            .next()
            .unwrap();
        assert!(refresh.contains("if !self.sync_preflight() {"));
        let main = include_str!("main.rs");
        let toggle = main
            .split("Message::AgentSessionsToggle => {")
            .nth(1)
            .unwrap()
            .split("Message::AgentSessionsSurface")
            .next()
            .unwrap();
        assert!(toggle.contains("self.agent_sessions.open_view();"));
        assert!(!toggle.contains("reconcile()"));
    }

    #[test]
    fn the_users_own_search_and_filter_edits_select_the_best_match() {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "one", "Renderer review"),
            session(SessionProvider::Claude, "two", "Renderer fix"),
        ]));
        assert!(library.set_query("renderer".into()));
        let order: Vec<_> = library
            .filtered()
            .iter()
            .map(|session| session_key(session))
            .collect();
        assert_eq!(order.len(), 2);
        assert_eq!(library.selected.as_ref(), Some(&order[0]));
        // The second hit is chosen, then the user edits the filters while it
        // is still listed: type-then-Enter must resume the new top hit.
        library.selected = Some(order[1].clone());
        library.sync_preflight();
        library.sort = SessionSort::Relevance;
        library.apply_filters();
        assert_eq!(library.selected.as_ref(), Some(&order[0]));
        library.selected = Some(order[1].clone());
        library.clear_filters();
        assert_eq!(
            library.selected_session().map(session_key),
            library
                .filtered()
                .first()
                .map(|session| session_key(session))
        );

        // An implausible paste is refused whole and the search is kept.
        let before = library.query.clone();
        assert!(!library.set_query("x".repeat(QUERY_INPUT_LIMIT + 1)));
        assert_eq!(library.query, before);
        assert!(library
            .query_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("4096-byte limit")));
        // Over core's limit but plausible: kept whole, explained, no matches.
        let long = format!("{} -tag:done", "x".repeat(SESSION_QUERY_MAX_BYTES));
        assert!(library.set_query(long));
        assert!(library.query.ends_with("-tag:done"));
        assert!(library.query_notice.is_none());
        assert!(library.query_error.is_some());
        assert!(library.filtered().is_empty());
        let main = include_str!("main.rs");
        let query = main
            .split("Message::AgentSessionsQuery(query) => {")
            .nth(1)
            .unwrap()
            .split("Message::AgentSessionsProvider(")
            .next()
            .unwrap();
        assert!(query.contains("self.agent_sessions.set_query(query);"));
    }

    #[test]
    fn the_users_own_archive_explains_why_the_selection_left_the_view() {
        const ONE: &str = "11111111-1111-4111-8111-111111111111";
        const TWO: &str = "22222222-2222-4222-8222-222222222222";
        let mut library = library();
        library.open = true;
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, ONE, "First"),
            session(SessionProvider::Codex, TWO, "Second"),
        ]));
        settle_library(&mut library);
        library.selected = Some((SessionProvider::Codex, ONE.into()));
        library.sync_preflight();
        let key = (SessionProvider::Codex, ONE.to_string());
        library
            .store
            .set_archived(SessionProvider::Codex, ONE, true)
            .unwrap();
        library.note_own_change(key.clone(), OwnChange::Archived);
        settle_library(&mut library);
        // Never retargeted: the archived conversation stays selected but
        // detached, and the manager says the user's own change moved it.
        assert_eq!(library.selected.as_ref(), Some(&key));
        assert!(library.selection_hidden());
        assert_eq!(library.left_view, Some(OwnChange::Archived.left_view()));
        assert!(library.own_change.is_none());
        // Choosing a conversation ends the explanation.
        library.step(1);
        assert!(library.selected_session().is_some());
        assert!(library.left_view.is_none());

        // A change that keeps the row listed explains nothing.
        library.archive = SessionArchiveFilter::All;
        library.apply_filters();
        let listed = library.selected.clone().unwrap();
        library.store.set_pinned(listed.0, &listed.1, true).unwrap();
        library.note_own_change(listed, OwnChange::Unpinned);
        settle_library(&mut library);
        assert!(library.own_change.is_none());
        assert!(library.left_view.is_none());
        let main = include_str!("main.rs");
        for (handler, change) in [
            (
                "Message::AgentSessionsPin((provider, id)) => {",
                "OwnChange::Unpinned",
            ),
            (
                "Message::AgentSessionsArchive((provider, id)) => {",
                "OwnChange::Archived",
            ),
        ] {
            let body = main
                .split(handler)
                .nth(1)
                .unwrap()
                .split("            Message::")
                .next()
                .unwrap();
            assert!(body.contains(change), "{handler}");
        }
    }

    #[test]
    fn background_refresh_keeps_a_hidden_selection_and_never_auto_launches_another() {
        let mut library = library();
        library.open = true;
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "one", "First"),
            session(SessionProvider::Claude, "two", "Second"),
            session(SessionProvider::Codex, "three", "Third"),
        ]));
        library.selected = Some((SessionProvider::Claude, "two".into()));
        library.sync_preflight();
        // An armed resume waits for its own identity's readiness check.
        library.pending_confirm = library.selected.clone();
        let mut refreshed = library.catalog.clone();
        refreshed.sessions.retain(|session| session.id != "two");
        assert!(library.replace_catalog(refreshed));
        assert_eq!(
            library.selected,
            Some((SessionProvider::Claude, "two".into())),
            "a refresh must not retarget the selection to the row that moved up"
        );
        assert!(library.selection_hidden());
        assert!(library.selected_session().is_none());
        assert_eq!(library.selected_key(), None, "resume and organize pause");
        assert!(
            library.pending_confirm.is_none(),
            "the armed resume is withdrawn"
        );
        assert!(matches!(
            library.preflight.state(),
            SessionPreflightState::Idle
        ));
        // When the identity returns it is selected again, but nothing launches
        // until the user confirms anew.
        let mut restored = library.catalog.clone();
        restored
            .sessions
            .push(session(SessionProvider::Claude, "two", "Second"));
        assert!(library.replace_catalog(restored));
        assert_eq!(library.selected_session().unwrap().id, "two");
        assert!(library.pending_confirm.is_none());
        // From a hidden selection, explicit navigation enters the list at the
        // edge it points to instead of skipping the first match.
        library.selected = Some((SessionProvider::Codex, "gone".into()));
        library.step(1);
        assert_eq!(library.selected_position(), Some(0));
        library.selected = Some((SessionProvider::Codex, "gone".into()));
        library.step(-1);
        assert_eq!(library.selected_position(), Some(library.visible.len() - 1));
    }

    #[test]
    fn user_filter_changes_select_the_first_match() {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "alpha", "Alpha renderer"),
            session(SessionProvider::Claude, "beta", "Beta renderer"),
            session(SessionProvider::Codex, "gamma", "Gamma parser"),
        ]));
        library.selected = Some((SessionProvider::Claude, "beta".into()));
        library.query = "renderer".into();
        library.apply_filters();
        let top = |library: &SessionLibrary| library.filtered()[0].id.clone();
        assert_eq!(library.selected_session().unwrap().id, top(&library));
        library.selected = Some((SessionProvider::Claude, "beta".into()));
        library.sort = library.next_sort();
        library.apply_filters();
        assert_eq!(
            library.selected_session().unwrap().id,
            top(&library),
            "a still-listed selection also yields to the new top hit"
        );
        library.query = "parser".into();
        library.apply_filters();
        assert_eq!(library.selected_session().unwrap().id, "gamma");
    }

    #[test]
    fn live_conflict_requires_a_second_request_for_that_exact_identity() {
        let mut library = library();
        library.open = true;
        let mut snapshot = catalog(vec![
            session(SessionProvider::Codex, "busy", "Running elsewhere"),
            session(SessionProvider::Codex, "free", "Quiet"),
        ]);
        snapshot
            .live_processes
            .push(live(SessionProvider::Codex, "busy", 4242));
        library.replace_catalog(snapshot);
        library.selected = Some((SessionProvider::Codex, "busy".into()));
        library.sync_preflight();
        let busy = library.selected_session().unwrap().clone();
        assert!(!library.admit_resume(&busy), "the first request only warns");
        let warning = library.resume_warning().unwrap().clone();
        assert_eq!(warning.key, (SessionProvider::Codex, "busy".to_string()));
        assert!(warning.text.contains("PID 4242"), "{}", warning.text);
        assert!(library.pending_confirm.is_none());
        // The second click of a double-click (or a quick second Enter) lands
        // on "Resume anyway" before anyone could read the warning.
        assert!(
            !library.admit_resume(&busy),
            "an immediate repeat cannot confirm the warning it raised"
        );
        assert_eq!(
            library.resume_warning().unwrap().shown_at,
            warning.shown_at,
            "an early repeat neither confirms nor restarts the delay"
        );
        age_resume_warning(&mut library);
        assert!(library.admit_resume(&busy), "Resume anyway admits it");

        // Escape withdraws the warning first and keeps the manager open.
        assert!(library.cancel_resume_warning());
        assert_eq!(library.notice.as_deref(), Some("Resume cancelled."));
        assert!(!library.cancel_resume_warning());
        assert!(!library.admit_resume(&busy), "a new request warns again");

        // Selecting another conversation retires the warning, and a
        // conversation without a live CLI is admitted at once.
        library.selected = Some((SessionProvider::Codex, "free".into()));
        library.sync_preflight();
        assert_eq!(library.selected_session().unwrap().id, "free");
        assert!(library.resume_warning.is_none());
        let free = library.selected_session().unwrap().clone();
        assert!(library.admit_resume(&free));

        // A refresh that no longer observes the process retires a warning,
        // and one that drops the identity hides it without confirming.
        library.selected = Some((SessionProvider::Codex, "busy".into()));
        library.sync_preflight();
        assert!(!library.admit_resume(&busy));
        let mut quiet = library.catalog.clone();
        quiet.live_processes.clear();
        library.replace_catalog(quiet);
        assert!(library.resume_warning.is_none());
        assert!(library.admit_resume(&busy));
        let mut busy_again = library.catalog.clone();
        busy_again
            .live_processes
            .push(live(SessionProvider::Codex, "busy", 7));
        library.replace_catalog(busy_again);
        assert!(!library.admit_resume(&busy));
        let mut removed = library.catalog.clone();
        removed.sessions.retain(|session| session.id != "busy");
        library.replace_catalog(removed);
        assert!(library.resume_warning.is_none());
        assert!(library.resume_warning().is_none());
    }

    #[test]
    fn resume_and_copy_actions_are_bound_to_the_rendered_identity() {
        let main = include_str!("main.rs");
        assert!(main.contains("AgentSessionsResume(agent_sessions::SessionKey),"));
        assert!(main.contains("AgentSessionsCopy(agent_sessions::SessionKey),"));
        assert!(
            main.contains("Message::AgentSessionsResume(key) => self.resume_agent_session(&key),")
        );
        assert!(main.contains(
            "Message::AgentSessionsCopy(key) => return self.copy_agent_session_command(&key),"
        ));
        let source = include_str!("agent_sessions.rs");
        for start in [
            "pub(crate) fn resume_agent_session(",
            "pub(crate) fn copy_agent_session_command(",
        ] {
            let body = source
                .split(start)
                .nth(1)
                .unwrap()
                .split("\n    }\n")
                .next()
                .unwrap();
            assert!(
                body.contains(".filter(|session| session_key(session) == *key)"),
                "{start} must refuse a stale identity"
            );
        }
        let copy = source
            .split("pub(crate) fn copy_agent_session_command(")
            .nth(1)
            .unwrap()
            .split("\n    }\n")
            .next()
            .unwrap();
        // The background check computed the command: copying never checks
        // the project directory or history file on the UI thread.
        assert!(copy.contains("state.preflight.command(session)"));
        assert!(!copy.contains("resume_launch"));
        assert!(copy.contains("\"Copied resume command\""));
        assert!(
            !copy.contains("ready_launch"),
            "never the resolved spawn argv"
        );
        // The search keeps the user's complete text.
        let query = main
            .split("Message::AgentSessionsQuery(query) => {")
            .nth(1)
            .unwrap()
            .split("Message::AgentSessionsProvider(")
            .next()
            .unwrap();
        assert!(!query.contains("visible_bounded"));
    }

    #[test]
    fn hidden_or_moved_selection_has_no_actionable_key() {
        let mut library = library();
        library.replace_catalog(catalog(vec![
            session(SessionProvider::Codex, "one", "First"),
            session(SessionProvider::Claude, "two", "Second"),
        ]));
        library.selected = Some((SessionProvider::Claude, "two".into()));
        assert_eq!(
            library.selected_key(),
            Some((SessionProvider::Claude, "two".into()))
        );
        library.query = "First".into();
        library.reconcile();
        assert_eq!(library.selected_key(), None);
        assert!(library.selection_hidden());
    }

    #[test]
    fn long_queries_are_kept_whole_and_explained_by_core() {
        let mut library = library();
        library.replace_catalog(catalog(vec![session(
            SessionProvider::Codex,
            "one",
            "Renderer",
        )]));
        // A long query keeps its trailing exclusion instead of being cut.
        let long = format!("title:\"{}\" -tag:done", "renderer ".repeat(70).trim_end());
        assert!(long.len() > 512);
        library.query = long.clone();
        library.apply_filters();
        assert_eq!(library.query, long);
        assert!(library.query_error.is_none());
        library.query = "x".repeat(4097);
        library.apply_filters();
        assert!(library.query_error.is_some());
        assert!(library.filtered().is_empty());
    }

    #[test]
    fn editor_validates_live_against_core_byte_limits() {
        let mut library = library();
        let id = "11111111-1111-4111-8111-111111111111";
        let key = (SessionProvider::Codex, id.to_owned());
        library.replace_catalog(catalog(vec![session(SessionProvider::Codex, id, "Title")]));
        library.open = true;
        library.begin_organization(&key);
        let reference = library.editor.as_ref().unwrap().reference.clone();
        assert!(library.editor.as_ref().unwrap().invalid.is_none());
        // 54 three-byte characters exceed the 160-byte name limit while
        // staying far below the old 512-character cut.
        let over = "名".repeat(SESSION_ALIAS_MAX_BYTES / 3 + 1);
        library.organization_alias(&reference, over.clone());
        let editor = library.editor.as_ref().unwrap();
        assert_eq!(editor.alias, over, "the draft is kept, not truncated");
        assert_eq!(
            editor.invalid,
            validate_session_details(&over, "").err(),
            "the editor shows core's own explanation"
        );
        library.save_organization(&reference);
        assert!(!library.editor.as_ref().unwrap().saving);
        assert!(!library.store.is_saving(), "an invalid draft never queues");
        let tags = (0..=SESSION_TAG_LIMIT)
            .map(|index| format!("t{index}"))
            .collect::<Vec<_>>()
            .join(",");
        library.organization_alias(&reference, "Renderer".into());
        library.organization_tags(&reference, tags);
        assert!(library.editor.as_ref().unwrap().invalid.is_some());
        library.organization_tags(&reference, "release, review".into());
        assert!(library.editor.as_ref().unwrap().invalid.is_none());
        // Only an implausible paste is refused whole, keeping the draft.
        library.organization_alias(&reference, "x".repeat(ALIAS_INPUT_LIMIT + 1));
        let editor = library.editor.as_ref().unwrap();
        assert_eq!(editor.alias, "Renderer");
        assert!(editor.notice.is_some());
    }

    #[test]
    fn periodic_refresh_follows_an_explicit_open_and_sort_cycles_core_order() {
        let mut library = library();
        library.open = true;
        library.poll();
        assert!(
            !library.scanner.is_scanning(),
            "only the explicit opening refresh arms periodic rescans"
        );
        let mut seen = Vec::new();
        for _ in 0..SessionSort::ALL.len() {
            seen.push(library.sort);
            library.sort = library.next_sort();
        }
        assert_eq!(seen, SessionSort::ALL);
        assert_eq!(library.sort, SessionSort::ALL[0]);
    }
}
