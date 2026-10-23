//! Native session-library state. Discovery and resume validation live in core.
use jterm_core::agent_sessions::{
    AgentSession, SessionArchiveFilter, SessionBatchAction, SessionCatalog, SessionLaunch,
    SessionLibraryFilter, SessionLibraryStore, SessionPreflight, SessionPreflightState,
    SessionProjectSummary, SessionProvider, SessionQuery, SessionScanner, SessionSearchIndex,
    SessionSort, SessionStatus, SESSION_BATCH_LIMIT,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub(crate) const PAGE_SIZE: usize = 80;
use std::time::{Duration, Instant};

pub(crate) type SessionKey = (SessionProvider, String);

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
    saving: bool,
}

pub(crate) struct SessionLibrary {
    pub open: bool,
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
    editor: Option<OrganizationEditor>,
    bulk_mode: bool,
    checked: HashSet<SessionKey>,
    bulk_epoch: std::sync::Arc<()>,
    bulk_tag: String,
    bulk_notice: Option<String>,
    bulk_saving: bool,
    bulk_saved_count: usize,
    pub selected: Option<SessionKey>,
    pub notice: Option<String>,
    pub bindings: HashMap<SessionKey, usize>,
    last_refresh: Instant,
}

impl Default for SessionLibrary {
    fn default() -> Self {
        Self::with_store(SessionLibraryStore::new())
    }
}
impl SessionLibrary {
    fn with_store(store: SessionLibraryStore) -> Self {
        Self {
            open: false,
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
            last_refresh: Instant::now(),
        }
    }
}

impl SessionLibrary {
    pub fn refresh(&mut self) {
        if self.scanner.refresh() {
            self.last_refresh = Instant::now();
        }
        self.store.refresh();
    }

    pub fn poll(&mut self) -> bool {
        let old_page = self.page;
        if let Some(catalog) = self.scanner.poll() {
            self.replace_catalog(catalog);
        }
        if self.store.poll() {
            self.reconcile();
        }
        if self.bulk_saving && !self.store.is_busy() {
            self.bulk_saving = false;
            self.bulk_notice = Some(self.store.error().map(str::to_owned).unwrap_or_else(|| {
                format!("Saved changes for {} conversations.", self.bulk_saved_count)
            }));
            self.retire_bulk();
        }
        if self.editor.as_ref().is_some_and(|editor| editor.saving) && !self.store.is_busy() {
            if let Some(error) = self.store.error() {
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
        if self.open && self.last_refresh.elapsed() >= Duration::from_secs(5) {
            self.refresh();
        }
        old_page != self.page
    }

    pub fn replace_catalog(&mut self, catalog: SessionCatalog) -> bool {
        if self.catalog == catalog {
            return false;
        }
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

    pub fn filtered(&self) -> Vec<&AgentSession> {
        self.visible
            .iter()
            .filter_map(|index| self.catalog.sessions.get(*index))
            .collect()
    }

    pub fn selected_session(&self) -> Option<&AgentSession> {
        let key = self.selected.as_ref()?;
        self.visible
            .iter()
            .filter_map(|index| self.catalog.sessions.get(*index))
            .find(|session| session.provider == key.0 && session.id == key.1)
    }

    pub fn reconcile(&mut self) {
        self.query_error = SessionSearchIndex::query_error(&self.query);
        self.projects = self.search_index.projects(self.store.snapshot());
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
            self.store.snapshot(),
            SessionLibraryFilter {
                pinned_only: self.pinned_only,
                recent_only: self.recent_only,
                pinned_first: true,
                archive: self.archive,
            },
        );
        if self.selected_session().is_none() {
            self.selected = self.visible.first().map(|index| {
                let session = &self.catalog.sessions[*index];
                (session.provider, session.id.clone())
            });
        }
        self.page = self.selected_position().unwrap_or(0) / PAGE_SIZE;
        self.sync_preflight();
    }

    pub fn cancel_confirmation(&mut self) {
        self.pending_confirm = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.pending_confirm = None;
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
        self.filtered()
            .into_iter()
            .skip(self.page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .map(|session| (session.provider, session.id.clone()))
            .collect()
    }

    fn page_reference(&self) -> BatchRef {
        BatchRef {
            epoch: self.bulk_epoch.clone(),
            keys: self.page_keys().into(),
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
        if value.len() > 128 {
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
        match self.store.apply_batch(&reference.keys, action) {
            Ok(count) => {
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
        let snapshot = self.store.snapshot();
        let alias = snapshot.alias(key.0, &key.1).unwrap_or_default().to_owned();
        let tags = snapshot.tags(key.0, &key.1).join(", ");
        self.cancel_confirmation();
        self.editor = Some(OrganizationEditor {
            reference: OrganizationRef {
                key: key.clone(),
                epoch: std::sync::Arc::new(()),
            },
            title,
            alias,
            tags,
            notice: None,
            saving: false,
        });
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
            // pasted control character into a different stored title.
            editor.alias = value.chars().take(512).collect();
            editor.notice = None;
        }
    }

    pub(crate) fn organization_tags(&mut self, reference: &OrganizationRef, value: String) {
        if let Some(editor) = self.editor_for(reference) {
            editor.tags = value.chars().take(1024).collect();
            editor.notice = None;
        }
    }

    pub(crate) fn save_organization(&mut self, reference: &OrganizationRef) {
        let Some(editor) = self.editor_for(reference) else {
            return;
        };
        let alias = editor.alias.clone();
        let tags = editor.tags.clone();
        match self
            .store
            .set_details(reference.key.0, &reference.key.1, &alias, &tags)
        {
            Ok(()) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.saving = true;
                    editor.notice = None;
                }
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

    pub fn sync_preflight(&mut self) {
        let selected = self.selected_session().cloned();
        if self.preflight.select(selected.as_ref()) {
            self.pending_confirm = None;
            self.notice = None;
        }
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
        let index = self.selected_position().unwrap_or(0);
        let next = (index as isize)
            .saturating_add(delta)
            .clamp(0, self.visible.len() as isize - 1) as usize;
        let session = &self.catalog.sessions[self.visible[next]];
        self.selected = Some((session.provider, session.id.clone()));
        self.page = next / PAGE_SIZE;
        self.sync_preflight();
    }

    pub fn next_sort(&self) -> SessionSort {
        match self.sort {
            SessionSort::Relevance => SessionSort::Recent,
            SessionSort::Recent => SessionSort::LiveFirst,
            SessionSort::LiveFirst => SessionSort::RecentlyResumed,
            SessionSort::RecentlyResumed => SessionSort::Relevance,
        }
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
        self.selected = None;
        self.notice = None;
        self.reconcile();
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

impl super::Frost {
    pub(crate) fn resume_agent_session(&mut self) {
        if !self.agent_sessions.open || self.agent_sessions.editing() {
            return;
        }
        let Some(session) = self.agent_sessions.selected_session().cloned() else {
            return;
        };
        let key = (session.provider, session.id.clone());
        self.native_enter_ownership.lock().claim_pressed();
        if let Some(index) = self
            .agent_sessions
            .bindings
            .get(&key)
            .copied()
            .and_then(|id| self.session_index_by_id(id))
            .filter(|index| !self.sessions[*index].hold_after_exit)
        {
            self.pane_zoomed = false;
            if !self.focus_session(index) {
                self.agent_sessions.open = true;
                self.agent_sessions.notice =
                    Some("The open terminal is no longer available".into());
                return;
            }
            self.record_resumed_agent_session(session.provider, &session.id);
            self.agent_sessions.close();
            self.relayout();
            self.refresh_active_context();
            return;
        }
        self.agent_sessions.sync_preflight();
        self.agent_sessions.pending_confirm = Some(key);
        self.agent_sessions.notice = None;
        self.agent_sessions.preflight.refresh();
    }

    pub(crate) fn finish_pending_agent_resume(&mut self) {
        if !self.agent_sessions.open || self.agent_sessions.editing() {
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
            .filter(|session| (session.provider, session.id.clone()) == key)
        else {
            self.agent_sessions.pending_confirm = None;
            return;
        };
        if let SessionPreflightState::Unavailable(error) = self.agent_sessions.preflight.state() {
            self.agent_sessions.notice = Some(error.clone());
            self.agent_sessions.pending_confirm = None;
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
        self.agent_sessions.pending_confirm = None;
        self.native_enter_ownership.lock().claim_held();
        self.launch_checked_agent_session(session, key, launch);
    }

    fn record_resumed_agent_session(&mut self, provider: SessionProvider, id: &str) {
        if let Err(error) = self.agent_sessions.store.record_resumed(provider, id) {
            self.push_toast(
                format!("Could not save recent-session history: {error}"),
                super::ToastKind::Warning,
            );
        }
    }

    fn launch_checked_agent_session(
        &mut self,
        session: AgentSession,
        key: SessionKey,
        launch: SessionLaunch,
    ) {
        let Some(cwd) = launch.cwd.to_str() else {
            self.agent_sessions.notice = Some("The session directory is not valid UTF-8".into());
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
                self.agent_sessions.bindings.insert(key, terminal.id);
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
                    self.agent_sessions.store.snapshot().display_title(
                        session.provider,
                        &session.id,
                        &session.title,
                    ),
                ));
                self.agent_sessions.close();
                self.agent_sessions.notice = None;
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
                self.push_toast(
                    format!("Resumed {} in a new tab", session.provider.label()),
                    super::ToastKind::Success,
                );
            }
            Err(error) => {
                self.agent_sessions.notice = Some(format!(
                    "Could not resume {}: {error}",
                    session.provider.label()
                ))
            }
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
            let writable = state.store.is_loaded()
                && !state.store.is_busy()
                && state.store.error().is_none()
                && !editor.saving;
            let mut form = column![
                text("Organize conversation").size(size + 5.0),
                text(format!("{} · {}", reference.key.0.label(), editor.title))
                    .size(size).style(text::secondary),
                text("Display name").size(size),
                alias,
                text("Leave blank to use the provider's title. Up to 160 bytes.")
                    .size(size - 2.0).style(text::secondary),
                text("Tags").size(size),
                tags,
                text("Separate tags with commas. Up to 12 tags, 32 bytes each.")
                    .size(size - 2.0).style(text::secondary),
                text("Names and tags are shared across the four terminals. The original conversation stays unchanged.")
                    .size(size - 1.0).style(text::secondary),
            ].spacing(10);
            if let Some(error) = editor.notice.as_deref().or_else(|| state.store.error()) {
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
            heading = heading.push(
                button(text("Tasks").size(size))
                    .on_press(M::AgentSessionsShowTasks)
                    .style(button::secondary),
            );
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
        let filtered = state.filtered();
        let batch_reference = state.batch_reference();
        let mut list = column![].spacing(5);
        let start = state.page * PAGE_SIZE;
        for session in filtered.iter().skip(start).take(PAGE_SIZE) {
            let key = (session.provider, session.id.clone());
            let selected = state.selected.as_ref() == Some(&key);
            let here = state
                .bindings
                .get(&key)
                .and_then(|id| self.session_index_by_id(*id))
                .is_some();
            let title = row![
                text(
                    if state
                        .store
                        .snapshot()
                        .is_pinned(session.provider, &session.id)
                    {
                        format!("★ {}", session.provider.label())
                    } else {
                        session.provider.label().into()
                    }
                )
                .size(size - 1.0),
                text(state.store.snapshot().display_title(
                    session.provider,
                    &session.id,
                    &session.title
                ))
                .size(size),
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
            let tags = state.store.snapshot().tags(session.provider, &session.id);
            if !tags.is_empty() {
                content = content.push(
                    text(
                        tags.iter()
                            .map(|tag| format!("#{tag}"))
                            .collect::<Vec<_>>()
                            .join("  "),
                    )
                    .size(size - 2.0),
                );
            }
            if state
                .store
                .snapshot()
                .is_archived(session.provider, &session.id)
            {
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
        if filtered.is_empty() {
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
        let end = (start + PAGE_SIZE).min(filtered.len());
        let pages = filtered.len().div_ceil(PAGE_SIZE).max(1);
        let page_controls = row![
            button(text("Previous").size(size - 1.0))
                .on_press_maybe(
                    (state.page > 0).then_some(M::AgentSessionsStep(-(PAGE_SIZE as isize)))
                )
                .style(button::secondary),
            text(format!(
                "{}–{} of {} · page {}/{}",
                if filtered.is_empty() { 0 } else { start + 1 },
                end,
                filtered.len(),
                state.page + 1,
                pages
            ))
            .size(size - 1.0),
            button(text("Next").size(size - 1.0))
                .on_press_maybe(
                    (end < filtered.len()).then_some(M::AgentSessionsStep(PAGE_SIZE as isize))
                )
                .style(button::secondary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let list: Element<'_, M> = column![
            text(format!(
                "{} matching · {}",
                filtered.len(),
                state.sort.label()
            ))
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
            detail = detail.push(
                text(state.store.snapshot().display_title(
                    session.provider,
                    &session.id,
                    &session.title,
                ))
                .size(size + 2.0),
            );
            if state
                .store
                .snapshot()
                .alias(session.provider, &session.id)
                .is_some()
            {
                detail = detail.push(
                    text(format!("Original title: {}", session.title))
                        .size(size - 2.0)
                        .style(text::secondary),
                );
            }
            let tags = state.store.snapshot().tags(session.provider, &session.id);
            if !tags.is_empty() {
                detail = detail.push(
                    text(
                        tags.iter()
                            .map(|tag| format!("#{tag}"))
                            .collect::<Vec<_>>()
                            .join("  "),
                    )
                    .size(size),
                );
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
            detail = detail.push(text("Recent conversation").size(size));
            detail = detail.push(
                text(if session.preview.is_empty() {
                    "No text preview available".into()
                } else {
                    session.preview.clone()
                })
                .size(size),
            );
            let key = (session.provider, session.id.clone());
            let pinned = state
                .store
                .snapshot()
                .is_pinned(session.provider, &session.id);
            if let Some(timestamp) = state
                .store
                .snapshot()
                .last_resumed(session.provider, &session.id)
            {
                detail = detail.push(
                    text(format!("Last resumed {}", age(timestamp)))
                        .size(size - 1.0)
                        .style(text::secondary),
                );
            }
            let bound = state
                .bindings
                .get(&(session.provider, session.id.clone()))
                .and_then(|id| self.session_index_by_id(*id))
                .filter(|index| !self.sessions[*index].hold_after_exit)
                .is_some();
            let ready = state.preflight.ready_launch(session).is_some();
            let checking = state.pending_confirm.is_some();
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
                    } else {
                        "Resume in new tab"
                    })
                    .size(size),
                )
                .on_press_maybe(((bound || ready) && !checking).then_some(M::AgentSessionsResume))
                .padding([8, 14])
                .width(Length::Fill)
                .style(button::primary),
            );
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
                    .on_press_maybe(
                        (state.store.is_loaded()
                            && !state.store.is_busy()
                            && state.store.error().is_none())
                        .then_some(M::AgentSessionsPin(key.clone()))
                    )
                    .style(button::secondary),
                    button(text("Copy resume command").size(size - 1.0))
                        .on_press_maybe(ready.then_some(M::AgentSessionsCopy))
                        .style(button::text),
                ]
                .spacing(8)
                .wrap(),
            );
            let writable =
                state.store.is_loaded() && !state.store.is_busy() && state.store.error().is_none();
            let archived = state
                .store
                .snapshot()
                .is_archived(session.provider, &session.id);
            // Iced buttons retain a press until release. A refresh may replace
            // the selected conversation or change an action while held, so the
            // complete identity must retire the old subtree's pressed state.
            action_identity = Some((key.clone(), pinned, archived, bound, ready, checking));
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
        let search_help = text(state.query_error.as_deref().unwrap_or(
            "Search: tag:review provider:codex project:\"my project\" is:pinned -is:live",
        ))
        .size(size - 2.0);
        let search_help = if state.query_error.is_some() {
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
            let visible_selected = filtered
                .iter()
                .filter(|session| checked_keys.contains(&(session.provider, session.id.as_str())))
                .count();
            let hidden = state.checked.len().saturating_sub(visible_selected);
            let writable = state.store.is_loaded()
                && !state.store.is_busy()
                && !state.bulk_saving
                && !state.checked.is_empty();
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
                        state.page_keys(),
                        row![
                            button(text("Select visible page").size(size - 1.0))
                                .style(button::secondary)
                                .on_press_maybe((!state.bulk_saving).then(|| {
                                    M::AgentSessionsSelectVisible(state.page_reference())
                                })),
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
            controls = controls.push(
                text_input("One tag to add or remove", &state.bulk_tag)
                    .size(size)
                    .on_input_maybe((!state.bulk_saving).then_some({
                        move |value| M::AgentSessionsBulkTag(reference.clone(), value)
                    })),
            );
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
                            (writable && !state.bulk_tag.trim().is_empty())
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
                    text("Cycle Best match / Recent / Live first / Recently resumed · Ctrl+S").size(size - 1.0),
                    tooltip::Position::Bottom,
                ),
                button(text("Clear filters").size(size - 1.0)).on_press(M::AgentSessionsClear).style(button::text),
            ].spacing(12).align_y(iced::Alignment::Center).wrap(),
            row![
                checkbox(state.pinned_only).label("Pinned only").on_toggle(M::AgentSessionsPinned).size(size),
                checkbox(state.recent_only).label("Previously resumed").on_toggle(M::AgentSessionsRecent).size(size),
                text(if !state.store.is_loaded() { "Loading saved library…" } else if state.store.is_busy() { "Saving library…" } else { "Names, tags, pins and archive are shared across terminals" }).size(size - 2.0).style(text::secondary),
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
    use super::*;
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
        library.provider = Some(SessionProvider::Codex);
        library.reconcile();
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
        library.reconcile();
        assert_eq!(library.selected_session().unwrap().id, "two");
        library.query = "no matches".into();
        library.reconcile();
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
        library.reconcile();
        assert_eq!(library.filtered().len(), 1);
        assert_eq!(library.selected_session().unwrap().id, "one");
        let mut next = library.catalog.clone();
        next.live_processes.clear();
        library.replace_catalog(next);
        assert!(library.selected.is_none());
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
        library.reconcile();
        assert_eq!(library.selected_session().unwrap().id, "title");
        library.project = Some("/work/repo".into());
        library.project_only = true;
        library.reconcile();
        assert_eq!(library.filtered().len(), 1);
        library.clear_filters();
        assert_eq!(library.filtered().len(), 2);
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
}
