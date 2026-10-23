//! Native session-library state. Discovery and resume validation live in core.
use jterm_core::agent_sessions::{
    AgentSession, SessionCatalog, SessionProvider, SessionQuery, SessionScanner,
    SessionSearchIndex, SessionSort, SessionStatus,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub(crate) const PAGE_SIZE: usize = 80;
use std::time::{Duration, Instant};

pub(crate) type SessionKey = (SessionProvider, String);

pub(crate) struct SessionLibrary {
    pub open: bool,
    pub catalog: SessionCatalog,
    pub scanner: SessionScanner,
    pub query: String,
    pub provider: Option<SessionProvider>,
    pub status: Option<SessionStatus>,
    pub live_only: bool,
    pub project: Option<PathBuf>,
    pub project_only: bool,
    pub sort: SessionSort,
    search_index: SessionSearchIndex,
    visible: Vec<usize>,
    page: usize,
    codex_count: usize,
    observed_count: usize,
    pub selected: Option<SessionKey>,
    pub notice: Option<String>,
    pub bindings: HashMap<SessionKey, usize>,
    last_refresh: Instant,
}

impl Default for SessionLibrary {
    fn default() -> Self {
        Self {
            open: false,
            catalog: SessionCatalog::default(),
            scanner: SessionScanner::new(),
            query: String::new(),
            provider: None,
            status: None,
            live_only: false,
            project: None,
            project_only: false,
            sort: SessionSort::default(),
            search_index: SessionSearchIndex::default(),
            visible: Vec::new(),
            page: 0,
            codex_count: 0,
            observed_count: 0,
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
    }

    pub fn poll(&mut self) -> bool {
        let old_page = self.page;
        if let Some(catalog) = self.scanner.poll() {
            self.replace_catalog(catalog);
        }
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
        self.visible = self.search_index.query(&SessionQuery {
            text: &self.query,
            provider: self.provider,
            status: self.status,
            live_only: self.live_only,
            project: self
                .project_only
                .then_some(self.project.as_deref())
                .flatten(),
            sort: self.sort,
        });
        if self.selected_session().is_none() {
            self.selected = self.visible.first().map(|index| {
                let session = &self.catalog.sessions[*index];
                (session.provider, session.id.clone())
            });
        }
        self.page = self.selected_position().unwrap_or(0) / PAGE_SIZE;
    }

    pub fn selected_position(&self) -> Option<usize> {
        let key = self.selected.as_ref()?;
        self.visible.iter().position(|index| {
            let session = &self.catalog.sessions[*index];
            session.provider == key.0 && session.id == key.1
        })
    }

    pub fn step(&mut self, delta: isize) {
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
    }

    pub fn next_sort(&self) -> SessionSort {
        match self.sort {
            SessionSort::Relevance => SessionSort::Recent,
            SessionSort::Recent => SessionSort::LiveFirst,
            SessionSort::LiveFirst => SessionSort::Relevance,
        }
    }

    pub fn clear_filters(&mut self) {
        self.query.clear();
        self.provider = None;
        self.status = None;
        self.live_only = false;
        self.project_only = false;
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
        if !self.agent_sessions.open {
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
            self.agent_sessions.open = false;
            self.pane_zoomed = false;
            self.focus_session(index);
            self.relayout();
            self.refresh_active_context();
            return;
        }
        let launch = match session
            .resume_launch()
            .and_then(|launch| launch.resolve_for_spawn())
        {
            Ok(launch) => launch,
            Err(error) => {
                self.agent_sessions.notice = Some(error);
                return;
            }
        };
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
                self.session_diagnostic = None;
                self.next_id += 1;
                let insert = (self.active + 1).min(self.sessions.len());
                self.sessions.insert(insert, terminal);
                super::apply_theme_colors(&mut self.sessions[insert].terminal, &self.theme);
                self.reindex_tabs_after_insert(insert);
                self.open_tab_with(insert);
                self.tabs[self.active_tab].title =
                    Some(format!("{} · {}", session.provider.label(), session.title));
                self.agent_sessions.open = false;
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
        let mut heading = row![
            text("Agent Sessions").size(size + 6.0),
            Space::new().width(Length::Fill)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
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
            "Search title, project, branch, recent content, session ID…",
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
        let mut statuses = row![].spacing(4);
        for (label, status) in [
            ("All activity", None),
            ("Ready", Some(SessionStatus::Idle)),
            ("Working", Some(SessionStatus::Working)),
            ("Waiting", Some(SessionStatus::Waiting)),
            ("Unknown", Some(SessionStatus::Unknown)),
            ("Archived", Some(SessionStatus::Archived)),
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
                text(session.provider.label()).size(size - 1.0),
                text(session.title.clone()).size(size),
                Space::new().width(Length::Fill),
                text(age(session.updated_at_ms)).size(size - 2.0)
            ]
            .spacing(8);
            let content = column![
                title,
                text(session.cwd.to_string_lossy().into_owned())
                    .size(size - 2.0)
                    .style(text::secondary),
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
                .style(text::secondary)
            ]
            .spacing(3);
            list = list.push(
                button(content)
                    .on_press(M::AgentSessionsSelect(key))
                    .width(Length::Fill)
                    .padding(9)
                    .style(if selected {
                        button::primary
                    } else {
                        button::secondary
                    }),
            );
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
        if let Some(session) = state.selected_session() {
            detail = detail.push(text(session.title.clone()).size(size + 2.0));
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
            let launch = session.resume_launch();
            if let Ok(launch) = &launch {
                detail = detail.push(
                    container(
                        text(launch.display_command())
                            .size(size - 1.0)
                            .font(iced::Font::MONOSPACE),
                    )
                    .padding(9)
                    .style(container::rounded_box),
                );
            }
            let bound = state
                .bindings
                .get(&(session.provider, session.id.clone()))
                .and_then(|id| self.session_index_by_id(*id))
                .is_some();
            detail = detail.push(
                button(
                    text(if bound {
                        "Switch to open tab"
                    } else {
                        "Resume in new tab"
                    })
                    .size(size),
                )
                .on_press_maybe((launch.is_ok() || bound).then_some(M::AgentSessionsResume))
                .padding([8, 14])
                .style(button::primary),
            );
            if let Err(error) = launch {
                detail = detail.push(text(error).size(size - 1.0).style(text::danger));
            } else {
                detail = detail.push(
                    button(text("Copy resume command").size(size - 1.0))
                        .on_press(M::AgentSessionsCopy)
                        .style(button::text),
                );
            }
        } else {
            detail = detail.push(text("Select a conversation to preview and resume.").size(size));
        }
        if let Some(notice) = &state.notice {
            detail = detail.push(text(notice.clone()).size(size).style(text::danger));
        }
        let wide = self.win_size.width >= 780.0;
        let content: Element<'_, M> = if wide {
            row![
                container(list)
                    .width(Length::FillPortion(5))
                    .height(Length::Fill),
                container(scrollable(detail))
                    .width(Length::FillPortion(4))
                    .height(Length::Fill)
                    .padding(8)
            ]
            .spacing(14)
            .height(Length::Fill)
            .into()
        } else {
            column![
                container(list).height(Length::FillPortion(1)),
                container(scrollable(detail)).height(Length::FillPortion(1))
            ]
            .spacing(8)
            .height(Length::Fill)
            .into()
        };
        let mut body = column![heading,
            text(format!("{} conversations   ·   {codex} Codex   ·   {claude} Claude   ·   {observed} observed live", state.catalog.sessions.len())).size(size).style(text::secondary),
            query, filters, statuses.wrap(),
            row![
                checkbox(state.live_only).label("Observed live only").on_toggle(M::AgentSessionsLive).size(size),
                checkbox(state.project_only).label("This folder")
                    .on_toggle_maybe(state.project.as_ref().map(|_| M::AgentSessionsScope as fn(bool) -> M)).size(size),
                tooltip(
                    button(text(format!("Sort: {} ↻", state.sort.label())).size(size))
                        .on_press(M::AgentSessionsSort(state.next_sort())).style(button::secondary),
                    text("Cycle Best match / Recent / Live first · Ctrl+S").size(size - 1.0),
                    tooltip::Position::Bottom,
                ),
                button(text("Clear filters").size(size - 1.0)).on_press(M::AgentSessionsClear).style(button::text),
            ].spacing(12).align_y(iced::Alignment::Center).wrap(),
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
        for warning in state.catalog.warnings.iter().take(2) {
            body = body.push(text(warning.clone()).size(size - 2.0).style(text::warning));
        }
        body = body.push(
            text("↑ ↓ select · PgUp/PgDn page · Home/End list edges · Ctrl+L search · Ctrl+S sort · Enter resume · F5 refresh · Esc close")
                .size(size - 2.0)
                .style(text::secondary),
        );
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
        let centered = container(panel)
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
    #[test]
    fn refresh_preserves_provider_and_session_identity_after_reorder() {
        let mut library = SessionLibrary::default();
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
        let mut library = SessionLibrary::default();
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
        let mut library = SessionLibrary::default();
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
        let mut library = SessionLibrary::default();
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
        let mut library = SessionLibrary::default();
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
}
