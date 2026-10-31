//! One window-owned shared Eva. No automatic terminal reads or direct model/audio calls.
//! Only an explicitly previewed and approved selection may join a dialogue request.
//! NativePresented means successful native submission, NOT GPU completion/scanout.
use crate::{Frost, Message};
use companion_host::{self as shared, atlas, Placement, Rect, Request};
use iced::widget::{
    button, canvas, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Color, Element, Length, Point, Rectangle};
use std::hash::{Hash, Hasher};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Wake,
    ForceClose(u64),
    KeepOpen(u64),
    Geometry(Rectangle),
    PreviewPose(String),
    PreviewMood(String),
    Bind(iced::window::Id),
    Animate(u64),
    InputIdle(u64),
    Skin(String),
    Enabled(bool),
    Contrast(bool),
    Intent(Intent),
    Quiet(bool),
    Night(bool),
    InputFocus(bool),
    Draft(String),
    Submit(u64),
    CancelDialogue(String),
    ReplySeen(String),
    DialogueSave(String, u64),
    HistoryScope(String),
    HistoryLoad(bool, u64),
    SelectionCapture(u64),
    SelectionReview(u64),
    SelectionApprove(String, u64),
    SelectionCancel,
    ProjectDraft(String),
    ProjectAssign(u64),
    MemoryProject(bool),
    ScopeDiscover(Option<String>),
    StoredScopeDiscover(Option<String>),
    ScopeReview(shared::MemoryScope, u64),
    ScopeApprove(String, String, u64),
    ScopeCurrent,
    ArchiveDraft(String),
    ArchiveProject(bool),
    MemoryList,
    MemoryMore,
    MemoryGlobal(bool),
    MemoryKey(String),
    MemoryValue(String),
    MemorySave(u64),
    MemoryDelete(i64, u64, u64),
    MemoryClear(u64),
    MemoryApprove(u64, String, u64),
    MemoryCancel,
    MemoryPause(bool),
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Intent {
    Follow,
    Stay,
    Hide,
    Recall,
}
#[derive(Clone)]
struct Wake {
    id: u64,
    signal: tokio::sync::watch::Sender<u64>,
}
impl Hash for Wake {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.id.hash(h)
    }
}
impl PartialEq for Wake {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id
    }
}
impl Eq for Wake {}
#[derive(Default)]
struct Submission {
    /// Written only while drawing a body-free root tree, never by message receipt.
    cleared_tree: AtomicU64,
    after: AtomicU64,
    pending: AtomicBool,
    submitted: AtomicU64,
    body_tree: AtomicU64,
    body_after: AtomicU64,
    body_pending: AtomicBool,
    body_submitted: AtomicU64,
}
impl Submission {
    fn begin_draw(&self) {
        self.cleared_tree.store(0, Ordering::Release);
        self.body_tree.store(0, Ordering::Release);
    }
    fn snapshot(&self) -> (u64, u64) {
        (
            self.cleared_tree.load(Ordering::Acquire),
            self.body_tree.load(Ordering::Acquire),
        )
    }
    fn needs_feedback(&self) -> bool {
        (self.pending.load(Ordering::Acquire)
            && self.cleared_tree.load(Ordering::Acquire) > self.after.load(Ordering::Acquire))
            || (self.body_pending.load(Ordering::Acquire)
                && self.body_tree.load(Ordering::Acquire) > self.body_after.load(Ordering::Acquire))
    }
    fn presented_snapshot(&self, (serial, body): (u64, u64)) -> bool {
        let retired = serial > self.after.load(Ordering::Acquire)
            && self.pending.swap(false, Ordering::AcqRel);
        if retired {
            self.submitted.store(serial, Ordering::Release);
        }
        let appeared = body > self.body_after.load(Ordering::Acquire)
            && self.body_pending.swap(false, Ordering::AcqRel);
        if appeared {
            self.body_submitted.store(body, Ordering::Release);
        }
        retired || appeared
    }
    #[cfg(test)]
    fn presented(&self) -> bool {
        self.presented_snapshot(self.snapshot())
    }
}
fn task_reference(host: &str, pane: usize, task: u64) -> String {
    format!("{host}:pane-{pane}:task-{task}")
}
fn review_matches(
    current: Option<&shared::ContextRef>,
    pending: &(shared::ContextRef, shared::MemoryReview),
    id: u64,
    digest: &str,
    revision: u64,
) -> bool {
    current == Some(&pending.0)
        && pending.0.revision == revision
        && pending.1.id == id
        && pending.1.digest == digest
}
fn scope_approval_matches(
    current: Option<&shared::ContextRef>,
    pending: &(shared::ContextRef, shared::MemoryScope, String, String),
    id: &str,
    digest: &str,
    revision: u64,
) -> bool {
    current == Some(&pending.0)
        && pending.0.revision == revision
        && pending.2 == id
        && pending.3 == digest
}
fn selection_matches(
    current: Option<&shared::ContextRef>,
    preview: &(shared::ContextRef, String, String),
    evidence: &shared::SelectionEvidence,
) -> bool {
    current == Some(&evidence.context)
        && preview.0 == evidence.context
        && preview.1 == evidence.text
        && preview.2 == evidence.source_event_id
}
pub(crate) struct Eva {
    host: shared::SharedHost,
    wake: Wake,
    submission: Arc<Submission>,
    registration: Option<iced_winit::presentation_observer::Registration>,
    retirement_armed: Option<(shared::Token, u64)>,
    generation: u64,
    admission_generation: u64,
    drawn_token: Option<shared::Token>,
    action_started: Instant,
    last_pose: String,
    comfort: (bool, bool),
    cached: Option<atlas::Frame>,
    cache: canvas::Cache,
    placement: Placement,
    geometry: Option<Rect>,
    slot_size: Option<(f32, f32)>,
    metadata: Option<(bool, bool, bool, bool, Rect)>,
    input_until: Option<Instant>,
    input_generation: u64,
    deadline: Option<(u64, Instant)>,
    animation_generation: u64,
    closed: bool,
    pub closing: bool,
    close_guarded: bool,
    close_paused_for_work: bool,
    close_ui_revision: u64,
    cancel_key: shared::CancelKeyLease,
    force_close: bool,
    task_revision: u64,
    running_task: Option<(usize, bool)>,
    task_ids: std::collections::BTreeMap<usize, u64>,
    task_active: std::collections::BTreeSet<usize>,
    context: Option<shared::ContextRef>,
    context_revision: u64,
    input_focused: bool,
    draft: String,
    dialogue: Option<(String, shared::ContextRef)>,
    dialogue_save: shared::DialogueSave,
    /// Local request error, shown until the history view changes.
    history_error: Option<(String, u64)>,
    /// Rendered page text keyed by view revision; view() stays allocation-free.
    history_text: (u64, String),
    dialogue_status: String,
    dialogue_reply: String,
    projects: std::collections::BTreeMap<String, String>,
    project_draft: String,
    memory_project: bool,
    selection_captured_at: u64,
    selection_preview: Option<(shared::ContextRef, String, String)>,
    selection_review: Option<shared::SelectionEvidence>,
    selection_approved: Option<shared::SelectionEvidence>,
    stored_scopes: Vec<shared::MemoryScope>,
    stored_scope_next: Option<String>,
    task_scopes: Vec<shared::TaskScopeInfo>,
    scope_next: Option<String>,
    scope_review: Option<(shared::ContextRef, shared::MemoryScope, String, String)>,
    managed_scope: Option<shared::MemoryScope>,
    archive_draft: String,
    archive_project: bool,
    memory_global: bool,
    memory_key: String,
    memory_value: String,
    memory_context: Option<shared::ContextRef>,
    memory_entries: Vec<shared::MemoryEntry>,
    memory_review: Option<(shared::ContextRef, shared::MemoryReview)>,
    memory_status: String,
    memory_paused: bool,
    memory_next: Option<i64>,
    memory_append: bool,
}
impl Eva {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let (signal, _) = tokio::sync::watch::channel(0u64);
        let wake = Wake {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            signal,
        };
        let tx = wake.signal.clone();
        let host = shared::SharedHost::from_env(
            "frost",
            shared::RetirementCapability::NativePresented,
            move || {
                tx.send_modify(|n| *n = n.wrapping_add(1));
            },
        );
        Self {
            host,
            wake,
            submission: Arc::new(Submission::default()),
            registration: None,
            retirement_armed: None,
            generation: 1,
            admission_generation: 0,
            drawn_token: None,
            action_started: Instant::now(),
            last_pose: String::new(),
            comfort: (false, false),
            cached: None,
            cache: canvas::Cache::new(),
            placement: Placement::Hidden,
            geometry: None,
            slot_size: None,
            metadata: None,
            input_until: None,
            input_generation: 0,
            deadline: None,
            animation_generation: 0,
            closed: false,
            closing: false,
            close_guarded: false,
            close_paused_for_work: false,
            close_ui_revision: 0,
            cancel_key: Default::default(),
            force_close: false,
            task_revision: 0,
            running_task: None,
            task_ids: Default::default(),
            task_active: Default::default(),
            context: None,
            context_revision: 0,
            input_focused: false,
            draft: String::new(),
            dialogue: None,
            dialogue_save: Default::default(),
            history_error: None,
            history_text: (0, String::new()),
            dialogue_status: "Offline dialogue; no terminal text attached".into(),
            dialogue_reply: String::new(),
            projects: Default::default(),
            project_draft: String::new(),
            memory_project: false,
            selection_captured_at: 0,
            selection_preview: None,
            selection_review: None,
            selection_approved: None,
            stored_scopes: vec![],
            stored_scope_next: None,
            task_scopes: vec![],
            scope_next: None,
            scope_review: None,
            managed_scope: None,
            archive_draft: String::new(),
            archive_project: false,
            memory_global: false,
            memory_key: String::new(),
            memory_value: String::new(),
            memory_context: None,
            memory_entries: vec![],
            memory_review: None,
            memory_status: "No memory write receipt".into(),
            memory_paused: false,
            memory_next: None,
            memory_append: false,
        }
    }
    pub fn bind(&mut self, id: iced::window::Id) {
        if self.registration.is_some() || self.closed {
            return;
        }
        let submission = self.submission.clone();
        let signal = self.wake.signal.clone();
        let before = self.submission.clone();
        let capture = self.submission.clone();
        self.registration = Some(iced_winit::presentation_observer::register_with_receipt(
            id,
            move || before.begin_draw(),
            move || capture.needs_feedback().then(|| capture.snapshot()),
            move |native_serial, snapshot| {
                let accepted = submission.presented_snapshot(snapshot);
                log::debug!(target: "eva_native_host", "native host receipt native_pass={native_serial} cleared_frame={} body_frame={} accepted={accepted}", snapshot.0, snapshot.1);
                if accepted {
                    signal.send_modify(|n| *n = n.wrapping_add(1));
                }
            },
        ));
    }
    pub fn input(&mut self) {
        self.input_generation = self.input_generation.wrapping_add(1);
        self.input_until = Some(Instant::now() + Duration::from_millis(900));
    }
    fn advance(&mut self) {
        self.host.begin_frame(self.generation);
        let submitted = self.submission.submitted.swap(0, Ordering::AcqRel);
        if submitted != 0 {
            self.host.frame_presented(submitted);
        }
        let body = self.submission.body_submitted.swap(0, Ordering::AcqRel);
        if body != 0 {
            self.host.body_submitted(body);
            self.host.frame_presented(body);
        }
        self.host.poll();
        let comfort = (self.host.gate.quiet(), self.host.gate.night());
        if comfort != self.comfort {
            self.comfort = comfort;
            self.cache.clear();
            self.deadline = None;
        }
        if self.host.gate.token() != self.drawn_token.as_ref() {
            self.drawn_token = self.host.gate.token().cloned();
            self.submission
                .body_after
                .store(self.generation, Ordering::Release);
            self.submission
                .body_pending
                .store(self.drawn_token.is_some(), Ordering::Release);
        }
        let connection = self.host.gate.admission_generation();
        if connection != self.admission_generation {
            self.admission_generation = connection;
            self.metadata = None;
            self.running_task = None;
            self.context = None;
            self.clear_consents();
            self.memory_review = None;
            self.retirement_armed = None;
            if self.closing {
                self.send(Request::Withdraw);
            }
        }
        let retirement = self
            .host
            .gate
            .pending_retirement_boundary()
            .map(|(token, after)| (token.clone(), after));
        if self.retirement_armed != retirement {
            self.retirement_armed = retirement;
            if self.retirement_armed.is_some() {
                self.submission
                    .after
                    .store(self.generation, Ordering::Release);
                self.submission.pending.store(true, Ordering::Release);
            }
        }
        self.host.history.context_changed(self.context.as_ref());
        if self.host.history.revision() != self.history_text.0 {
            self.history_text = (
                self.host.history.revision(),
                self.host.history.text(self.context.as_ref()),
            );
        }
        while let Some(reply) = self.host.take_dialogue() {
            if let shared::Reply::Dialogue {
                turn_id,
                context,
                status,
                reply,
            } = reply
            {
                self.dialogue_save
                    .observe(&turn_id, &context, &status, reply.as_deref());
                if self.dialogue.as_ref() == Some(&(turn_id.clone(), context.clone())) {
                    self.dialogue_status = format!("{} · source {}", status, context.pane_id);
                    if let Some(reply) = reply {
                        self.dialogue_reply = reply.chars().take(4096).collect();
                    }
                }
            }
        }
        while let Some(shared::Reply::ContextAccessReview { evidence }) =
            self.host.take_context_access()
        {
            if self
                .selection_preview
                .as_ref()
                .is_some_and(|preview| selection_matches(self.context.as_ref(), preview, &evidence))
            {
                self.selection_review = Some(evidence);
            }
        }
        while let Some(reply) = self.host.take_memory() {
            match reply {
                shared::Reply::MemoryScopes { scopes, next_after } => {
                    self.stored_scopes = scopes;
                    self.stored_scope_next = next_after;
                }
                shared::Reply::TaskScopes { scopes, next_after } => {
                    self.task_scopes = scopes;
                    self.scope_next = next_after;
                }
                shared::Reply::MemoryScopeReview {
                    context,
                    scope,
                    review_id,
                    digest,
                } if self.context.as_ref() == Some(&context) => {
                    self.scope_review = Some((context, scope, review_id, digest));
                }
                shared::Reply::MemoryList {
                    context,
                    entries,
                    paused,
                    next_before_id,
                    scope,
                } if self.memory_context.as_ref() == Some(&context)
                    && self.memory_scope().as_ref() == Some(&scope) =>
                {
                    self.memory_entries = entries;
                    self.memory_next = next_before_id;
                    self.memory_append = false;
                    self.memory_paused = paused;
                    if self.memory_status == "No memory write receipt" {
                        self.memory_status = "Memory list received; no write receipt".into();
                    }
                }
                shared::Reply::MemoryReview { context, review }
                    if self.memory_context.as_ref() == Some(&context) =>
                {
                    self.memory_review = Some((context, review));
                    self.memory_status = "Review required — not saved yet".into();
                }
                shared::Reply::MemoryResult { status, .. } => {
                    self.memory_status = status;
                    self.memory_review = None;
                    if let (Some(context), Some(scope)) =
                        (self.context.clone(), self.memory_scope())
                    {
                        self.memory_append = false;
                        self.memory_context = Some(context.clone());
                        self.send(Request::MemoryList {
                            context,
                            scope,
                            query: String::new(),
                            before_id: None,
                        });
                    }
                }
                _ => {}
            }
        }
        self.generation = self.generation.saturating_add(1);
    }
    fn send(&mut self, r: Request) {
        self.host.send(r)
    }
    pub fn appearance(&mut self, skin: String, motion: String) {
        self.send(Request::Appearance { skin, motion });
        self.deadline = None;
        self.cache.clear();
    }
    pub fn task_result(&mut self, session: usize, exit: Option<i32>) {
        self.task_revision = self.task_revision.saturating_add(1);
        let task = *self.task_ids.entry(session).or_insert(self.task_revision);
        self.task_active.remove(&session);
        self.send(Request::TaskObservation {
            task_id: task_reference(self.host.gate.host_id(), session, task),
            context_revision: self.task_revision,
            status: match exit {
                Some(0) => shared::TaskStatus::Succeeded,
                Some(_) => shared::TaskStatus::Failed,
                None => shared::TaskStatus::Unknown,
            },
        });
    }
    fn sync_context(&mut self, pane: Option<usize>) {
        let Some(pane) = pane else {
            self.context = None;
            self.clear_consents();
            self.memory_review = None;
            return;
        };
        let pane_id = format!("pane-{pane}");
        let task_id = self
            .task_ids
            .get(&pane)
            .map(|id| task_reference(self.host.gate.host_id(), pane, *id));
        let project_id = self.projects.get(&pane_id).cloned();
        if self.context.as_ref().is_none_or(|c| {
            c.pane_id != pane_id || c.task_id != task_id || c.project_id != project_id
        }) {
            self.context_revision = self.context_revision.saturating_add(1);
            let context = shared::ContextRef {
                host_id: self.host.gate.host_id().into(),
                pane_id,
                task_id,
                project_id,
                revision: self.context_revision,
            };
            self.context = Some(context.clone());
            self.clear_consents();
            self.memory_review = None;
            self.memory_context = None;
            self.memory_next = None;
            self.memory_entries.clear();
            self.send(Request::ContextObserved { context });
        }
    }
    fn clear_consents(&mut self) {
        self.dialogue_save.clear();
        self.selection_preview = None;
        self.selection_review = None;
        self.selection_approved = None;
        self.scope_review = None;
        self.managed_scope = None;
        self.memory_review = None;
        self.memory_entries.clear();
        self.memory_context = None;
    }
    fn memory_scope(&self) -> Option<shared::MemoryScope> {
        if let Some(scope) = &self.managed_scope {
            return Some(scope.clone());
        }
        if self.memory_project {
            return self
                .context
                .as_ref()?
                .project_id
                .clone()
                .map(shared::MemoryScope::Project);
        }
        if self.memory_global {
            Some(shared::MemoryScope::Global)
        } else {
            self.context
                .as_ref()?
                .task_id
                .clone()
                .map(shared::MemoryScope::Task)
        }
    }
    fn propose(&mut self, change: shared::MemoryChange) {
        if let Some(context) = self.context.clone() {
            self.memory_context = Some(context.clone());
            self.memory_review = None;
            self.send(Request::MemoryPropose { context, change });
            self.memory_status = "Proposal sent; not saved".into();
        }
    }
    fn interaction(&mut self, event: Event) {
        match event {
            Event::SelectionCancel => {
                self.selection_preview = None;
                self.selection_review = None;
                self.selection_approved = None;
            }
            Event::SelectionReview(rev) => {
                if let Some((context, text, source)) = self
                    .selection_preview
                    .clone()
                    .filter(|(c, _, _)| c.revision == rev && self.context.as_ref() == Some(c))
                {
                    self.send(Request::ContextAccessRequested {
                        context,
                        text,
                        source_event_id: source,
                        captured_at_ms: self.selection_captured_at,
                    });
                }
            }
            Event::SelectionApprove(id, rev) => {
                if let Some(evidence) = self.selection_review.clone().filter(|e| {
                    e.consent_id == id
                        && e.context.revision == rev
                        && self.context.as_ref() == Some(&e.context)
                }) {
                    self.send(Request::ContextAccessGranted {
                        context: evidence.context.clone(),
                        consent_id: id,
                    });
                    self.selection_approved = Some(evidence);
                    self.selection_review = None;
                    self.selection_preview = None;
                }
            }
            Event::ProjectDraft(v) => self.project_draft = v.chars().take(96).collect(),
            Event::ProjectAssign(rev) => {
                if let Some(c) = self.context.clone().filter(|c| c.revision == rev) {
                    let name = self.project_draft.trim().to_owned();
                    if name.is_empty() {
                        self.projects.remove(&c.pane_id);
                    } else if shared::valid_scope(&shared::MemoryScope::Project(name.clone())) {
                        self.projects.insert(c.pane_id.clone(), name);
                    } else {
                        self.memory_status = "Invalid project ID".into();
                        return;
                    }
                    // A named-project change creates a fresh local task scope. Never
                    // rebind an archived task's immutable project ownership.
                    if c.project_id != self.projects.get(&c.pane_id).cloned() {
                        if let Some(pane) = c
                            .pane_id
                            .strip_prefix("pane-")
                            .and_then(|id| id.parse::<usize>().ok())
                        {
                            if self.task_ids.contains_key(&pane) {
                                self.task_revision = self.task_revision.saturating_add(1);
                                self.task_ids.insert(pane, self.task_revision);
                                self.running_task = None;
                            }
                        }
                    }
                    self.clear_consents();
                    self.context = None;
                }
            }
            Event::MemoryProject(v) => {
                self.memory_project = v;
                self.memory_global = false;
                self.managed_scope = None;
                self.memory_entries.clear();
                self.memory_review = None;
            }
            Event::StoredScopeDiscover(after) => self.send(Request::MemoryScopes { after }),
            Event::ScopeDiscover(after) => self.send(Request::TaskScopes { after }),
            Event::ArchiveDraft(v) => self.archive_draft = v.chars().take(128).collect(),
            Event::ArchiveProject(v) => self.archive_project = v,
            Event::ScopeCurrent => {
                self.managed_scope = None;
                self.scope_review = None;
                self.memory_entries.clear();
                self.memory_review = None;
            }
            Event::ScopeReview(scope, rev) => {
                if let Some(context) = self.context.clone().filter(|c| c.revision == rev) {
                    self.send(Request::MemoryScopeReviewRequested { context, scope });
                }
            }
            Event::ScopeApprove(id, digest, rev) => {
                if let Some((context, scope, review_id, exact)) =
                    self.scope_review.clone().filter(|pending| {
                        scope_approval_matches(self.context.as_ref(), pending, &id, &digest, rev)
                    })
                {
                    self.send(Request::MemoryScopeApprove {
                        context: context.clone(),
                        review_id,
                        digest: exact,
                    });
                    self.managed_scope = Some(scope.clone());
                    self.scope_review = None;
                    self.memory_review = None;
                    self.memory_entries.clear();
                    self.memory_context = Some(context.clone());
                    self.send(Request::MemoryList {
                        context,
                        scope,
                        query: String::new(),
                        before_id: None,
                    });
                }
            }
            Event::ForceClose(revision) => {
                if self.closing && !self.close_paused_for_work && revision == self.close_ui_revision
                {
                    self.force_close = true;
                }
            }
            Event::KeepOpen(revision) => {
                if self.closing && self.close_guarded && revision == self.close_ui_revision {
                    self.keep_window_open();
                }
            }
            Event::Quiet(quiet) => self.send(Request::Comfort {
                quiet,
                night: self.host.gate.night(),
            }),
            Event::Night(night) => self.send(Request::Comfort {
                quiet: self.host.gate.quiet(),
                night,
            }),
            Event::InputFocus(v) => self.input_focused = v,
            Event::Draft(v) => self.draft = v.chars().take(2048).collect(),
            Event::Submit(expected) => {
                if let Some(context) = self.context.clone().filter(|c| c.revision == expected) {
                    if !self.draft.trim().is_empty() {
                        let turn_id = format!("frost-{}", uuid::Uuid::new_v4());
                        self.dialogue = Some((turn_id.clone(), context.clone()));
                        self.dialogue_reply.clear();
                        self.dialogue_status = "Submitted; not yet answered".into();
                        let text = std::mem::take(&mut self.draft);
                        self.dialogue_save.begin(&turn_id, &context, &text);
                        let approved_selection = self
                            .selection_approved
                            .take()
                            .filter(|e| self.context.as_ref() == Some(&e.context));
                        self.send(Request::DialogueStart {
                            turn_id,
                            context,
                            text,
                            approved_selection,
                        });
                    }
                }
            }
            Event::HistoryScope(label) => {
                if let Some(scope) = shared::history::SCOPES
                    .into_iter()
                    .find(|s| shared::history::scope_label(*s) == label)
                {
                    self.host.history.set_scope(scope);
                }
            }
            Event::HistoryLoad(older, expected) => {
                let result = match self.context.clone().filter(|c| c.revision == expected) {
                    Some(context) => {
                        self.send(Request::ContextObserved {
                            context: context.clone(),
                        });
                        self.host.request_history(&context, older)
                    }
                    None => Err("Focus a terminal pane first."),
                };
                self.history_error = result
                    .err()
                    .map(|message| (message.to_owned(), self.host.history.revision()));
            }
            Event::DialogueSave(turn, revision) => {
                if let Some(context) = self.context.as_ref().filter(|c| c.revision == revision) {
                    if let Some(scope) = self.memory_scope() {
                        match self.dialogue_save.proposal(&turn, context, scope) {
                            Ok(change) => self.propose(change),
                            Err(reason) => self.memory_status = reason.into(),
                        }
                    } else {
                        self.memory_status =
                            "Choose the current task/project or explicitly choose Global.".into();
                    }
                }
            }
            Event::ReplySeen(expected) => {
                if !self.dialogue_reply.is_empty() {
                    if let Some((turn_id, _)) =
                        self.dialogue.as_ref().filter(|(id, _)| id == &expected)
                    {
                        self.send(Request::DialogueDisplayed {
                            turn_id: turn_id.clone(),
                        });
                    }
                }
            }
            Event::CancelDialogue(expected) => {
                if let Some((turn_id, _)) = self.dialogue.as_ref().filter(|(id, _)| id == &expected)
                {
                    self.send(Request::DialogueCancel {
                        turn_id: turn_id.clone(),
                    });
                }
            }
            Event::MemoryGlobal(v) => {
                self.memory_global = v;
                self.memory_project = false;
                self.managed_scope = None;
                self.memory_entries.clear();
                self.memory_review = None;
            }
            Event::MemoryKey(v) => self.memory_key = v.chars().take(128).collect(),
            Event::MemoryValue(v) => self.memory_value = v.chars().take(1024).collect(),
            Event::MemoryMore => {
                if let (Some(context), Some(scope), Some(before)) =
                    (self.context.clone(), self.memory_scope(), self.memory_next)
                {
                    self.memory_append = true;
                    self.send(Request::MemoryList {
                        context,
                        scope,
                        query: String::new(),
                        before_id: Some(before),
                    });
                }
            }
            Event::MemoryList => {
                self.memory_append = false;
                if let (Some(context), Some(scope)) = (self.context.clone(), self.memory_scope()) {
                    self.memory_context = Some(context.clone());
                    self.send(Request::MemoryList {
                        context,
                        scope,
                        query: String::new(),
                        before_id: None,
                    });
                }
            }
            Event::MemorySave(expected) => {
                if self.context.as_ref().is_none_or(|c| c.revision != expected) {
                    return;
                }
                if let Some(scope) = self.memory_scope() {
                    if !self.memory_key.trim().is_empty() && !self.memory_value.trim().is_empty() {
                        let revision = self
                            .memory_entries
                            .iter()
                            .find(|entry| entry.key == self.memory_key && entry.scope == scope)
                            .map(|e| e.revision);
                        self.propose(shared::MemoryChange::Put {
                            scope,
                            key: self.memory_key.clone(),
                            value: self.memory_value.clone(),
                            source_event_id: format!("frost-memory-{}", uuid::Uuid::new_v4()),
                            expected_revision: revision,
                            expires_at: None,
                        });
                    }
                }
            }
            Event::MemoryDelete(id, expected_revision, expected) => {
                if self.context.as_ref().is_none_or(|c| c.revision != expected) {
                    return;
                }
                self.propose(shared::MemoryChange::Delete {
                    id,
                    expected_revision,
                })
            }
            Event::MemoryClear(expected) => {
                if self.context.as_ref().is_none_or(|c| c.revision != expected) {
                    return;
                }
                if let Some(scope) = self.memory_scope() {
                    self.propose(shared::MemoryChange::Clear { scope });
                }
            }
            Event::MemoryApprove(id, digest, expected) => {
                if !self.memory_review.as_ref().is_some_and(|pending| {
                    review_matches(self.context.as_ref(), pending, id, &digest, expected)
                }) {
                    return;
                }
                if let Some((context, review)) = self.memory_review.take() {
                    if self.context.as_ref() == Some(&context) {
                        self.send(Request::MemoryApprove {
                            context,
                            review_id: review.id,
                            digest: review.digest,
                        });
                        self.memory_status =
                            "Approval submitted; awaiting actual write receipt".into();
                    } else {
                        self.memory_status = "Context changed; review again".into();
                    }
                }
            }
            Event::MemoryCancel => {
                if let Some((_, review)) = self.memory_review.take() {
                    self.send(Request::MemoryCancel {
                        review_id: review.id,
                    });
                }
            }
            Event::MemoryPause(paused) => self.send(Request::MemoryPause { paused }),
            _ => {}
        }
    }
    pub fn request_close(&mut self) {
        if !self.closing {
            self.close_ui_revision = self.close_ui_revision.saturating_add(1);
            self.force_close = false;
            self.closing = true;
            self.cached = None;
            self.deadline = None;
            self.input_until = None;
            self.send(Request::Withdraw);
        }
    }
    pub fn request_guarded_close(&mut self) {
        if self.close_paused_for_work {
            self.close_ui_revision = self.close_ui_revision.saturating_add(1);
        }
        self.close_guarded = true;
        self.close_paused_for_work = false;
        self.request_close();
    }
    pub fn guarded_close_waiting(&self) -> bool {
        self.closing && self.close_guarded && !self.close_paused_for_work
    }
    pub fn pause_close_for_work(&mut self) {
        if self.closing && self.close_guarded && !self.close_paused_for_work {
            // Keep the exact in-flight withdrawal/proof. Ending the new work
            // must not itself authorize a GUI exit.
            self.close_paused_for_work = true;
            self.close_ui_revision = self.close_ui_revision.saturating_add(1);
        }
    }
    pub fn allow_final_session_close(&mut self) {
        self.close_ui_revision = self.close_ui_revision.saturating_add(1);
        // The final pane has its own process confirmation/completion path.
        self.close_guarded = false;
        self.close_paused_for_work = false;
    }
    pub fn keep_window_open(&mut self) {
        self.close_ui_revision = self.close_ui_revision.saturating_add(1);
        self.closing = false;
        self.close_guarded = false;
        self.close_paused_for_work = false;
        self.force_close = false;
        self.cached = None;
        self.deadline = None;
        // Preserve the actual in-flight Submission and retirement token.
        self.host.cancel_withdrawal();
    }
    pub fn route_close_escape(&mut self, event: &iced::keyboard::Event, focused: bool) -> bool {
        use iced::keyboard::{key::Named, Event, Key};
        match event {
            Event::KeyPressed {
                key: Key::Named(Named::Escape),
                repeat,
                ..
            } => match self.cancel_key.press(self.guarded_close_waiting(), *repeat) {
                shared::CancelKeyAction::Cancel => {
                    self.keep_window_open();
                    return true;
                }
                shared::CancelKeyAction::Suppress => return true,
                shared::CancelKeyAction::Pass => {}
            },
            Event::KeyReleased {
                key: Key::Named(Named::Escape),
                ..
            } => return self.cancel_key.release(focused),
            Event::KeyPressed { .. } => self.cancel_key.other_input(),
            _ => {}
        }
        false
    }
    pub fn cancel_key_focus_lost(&mut self) {
        self.cancel_key.focus_lost();
    }
    pub fn ready_to_close(&self) -> bool {
        self.closing
            && !self.close_paused_for_work
            && (self.force_close || self.host.gate.withdrawal_accepted())
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.registration = None;
        self.cached = None;
        self.deadline = None;
        self.submission.pending.store(false, Ordering::Release);
        // Never manufacture surface destruction proof from CloseRequested.
        self.send(Request::Focus { focused: false });
    }
    pub fn subscription(&self) -> iced::Subscription<Message> {
        if self.closed {
            return iced::Subscription::none();
        }
        let mut subscriptions = vec![iced::Subscription::run_with(self.wake.clone(), |key| {
            let receiver = key.signal.subscribe();
            iced::futures::stream::unfold((receiver, true), |(mut receiver, first)| async move {
                if first || receiver.changed().await.is_ok() {
                    Some((Message::Eva(Event::Wake), (receiver, false)))
                } else {
                    None
                }
            })
        })];
        if let Some((generation, at)) = self.deadline {
            subscriptions.push(iced::Subscription::run_with(
                (self.wake.id, generation, at),
                |key| {
                    let (_, generation, at) = *key;
                    iced::futures::stream::once(async move {
                        tokio::time::sleep_until(at.into()).await;
                        Message::Eva(Event::Animate(generation))
                    })
                },
            ));
        }
        if let Some(at) = self.input_until {
            subscriptions.push(iced::Subscription::run_with(
                (self.wake.id, self.input_generation, at),
                |key| {
                    let (_, generation, at) = *key;
                    iced::futures::stream::once(async move {
                        tokio::time::sleep_until(at.into()).await;
                        Message::Eva(Event::InputIdle(generation))
                    })
                },
            ));
        }
        iced::Subscription::batch(subscriptions)
    }
}
impl Drop for Eva {
    fn drop(&mut self) {
        self.close();
    }
}
impl Frost {
    pub(crate) fn eva_closing(&self) -> bool {
        self.eva
            .as_ref()
            .is_some_and(|e| e.closing && !e.close_paused_for_work)
    }
    pub(crate) fn eva_input_owns_keys(&self) -> bool {
        self.eva.as_ref().is_some_and(|e| e.input_focused)
            && self.reference_inspector_visible()
            && self.reference_inspector_tab == crate::native_shell::InspectorTab::Agent
            && !self.config_panel_open
    }
    pub(crate) fn legacy_organism_enabled(&self) -> bool {
        self.eva.is_none() && self.config.ascii_organism_enabled
    }
    pub(crate) fn eva_motion_changed(&mut self) {
        let motion = match self.organism_effective_motion() {
            Some(crate::organism::Motion::Full) => "full",
            Some(crate::organism::Motion::Static) => "static",
            _ => "calm",
        };
        if let Some(eva) = self.eva.as_mut() {
            eva.appearance(eva.host.gate.skin().to_string(), motion.to_string());
        }
    }
    pub(crate) fn eva_event(&mut self, event: Event) {
        if matches!(event, Event::KeepOpen(_)) && self.eva_closing() {
            self.native_enter_ownership.lock().claim_held();
        }
        match event {
            Event::SelectionCapture(rev) => {
                let selection = self
                    .sessions
                    .get(self.active)
                    .filter(|s| {
                        !s.managed_remote
                            && !s.transcript_read_only()
                            && !s.terminal.is_alt_buffer_active()
                    })
                    .and_then(|s| {
                        s.terminal.selection.and_then(|range| {
                            s.terminal.copy_selection().map(|text| (s.id, range, text))
                        })
                    });
                if let (Some(eva), Some((pane, range, mut text))) = (self.eva.as_mut(), selection) {
                    if let Some(context) = eva
                        .context
                        .clone()
                        .filter(|c| c.revision == rev && c.pane_id == format!("pane-{pane}"))
                    {
                        while text.len() > 4096 {
                            text.pop();
                        }
                        if !text.is_empty() {
                            eva.selection_review = None;
                            eva.selection_approved = None;
                            eva.selection_captured_at = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis()
                                as u64;
                            eva.selection_preview = Some((
                                context,
                                text,
                                format!(
                                    "selection-{}-{}:{}-{}:{}",
                                    uuid::Uuid::new_v4(),
                                    range.anchor.0,
                                    range.anchor.1,
                                    range.active.0,
                                    range.active.1
                                ),
                            ));
                        }
                    }
                }
            }
            Event::PreviewPose(v) => {
                if let Some(pose) = atlas::Pose::from_key(&v) {
                    self.eva_preview_pose = pose;
                }
            }
            Event::PreviewMood(v) => {
                if let Some(mood) = atlas::Mood::ALL.into_iter().find(|m| m.key() == v) {
                    self.eva_preview_mood = mood;
                }
            }
            Event::Enabled(v) => {
                self.config.eva_companion_enabled = v;
                self.config_dirty = true;
            }
            Event::Skin(v) => {
                if atlas::Skin::from_key(&v).is_some() {
                    if let Some(eva) = self.eva.as_mut() {
                        let motion = eva.host.gate.motion().to_string();
                        eva.appearance(v.clone(), motion);
                    }
                    self.config.eva_companion_skin = v;
                    self.config_dirty = true;
                }
            }
            Event::Contrast(v) => {
                if let Some(eva) = self.eva.as_ref() {
                    eva.cache.clear();
                }
                self.config.eva_companion_high_contrast = v;
                self.config_dirty = true;
            }
            _ => {
                if let Some(eva) = self.eva.as_mut() {
                    match event {
                        Event::Bind(id) => eva.bind(id),
                        Event::Geometry(r) => {
                            eva.geometry = Some(Rect {
                                x: r.x as f64,
                                y: r.y as f64,
                                width: r.width as f64,
                                height: r.height as f64,
                            })
                        }
                        Event::InputIdle(g) if g == eva.input_generation => eva.input_until = None,
                        Event::Animate(g)
                            if eva.deadline.is_some_and(|(current, _)| current == g) =>
                        {
                            eva.deadline = None
                        }
                        Event::Intent(intent) => eva.send(match intent {
                            Intent::Recall => Request::Recall,
                            Intent::Follow => Request::Presence {
                                mode: shared::Presence::FollowMe,
                            },
                            Intent::Stay => Request::Presence {
                                mode: shared::Presence::StayHere,
                            },
                            Intent::Hide => Request::Presence {
                                mode: shared::Presence::Hidden,
                            },
                        }),
                        other => eva.interaction(other),
                    }
                }
            }
        }
    }
    /// Safe slot is in the existing Agent inspector, not the document/PTY. It
    /// never covers composer, IME or approval UI; uncertain geometry hides.
    pub(crate) fn sync_eva(&mut self) {
        if self.eva.is_none() {
            return;
        }
        let inspector = self.reference_layout().inspector;
        let session = self.sessions.get(self.active);
        let selecting = session.is_some_and(|s| s.terminal.has_text_selection());
        let unsafe_input = session.is_none_or(|s| {
            s.managed_remote
                || s.transcript_read_only()
                || s.terminal.is_alt_buffer_active()
                || s.terminal.is_mouse_enabled()
                || !s.terminal.preedit_text.is_empty()
        });
        let pointer = self.terminal_mouse_gestures.iter().any(Option::is_some)
            || self.dragging_tab.is_some()
            || self.pane_drag.is_some()
            || self.dragging_divider.is_some()
            || self.sidebar_resize.is_some();
        let eligible = self.focused
            && self.terminal_input_active()
            && !unsafe_input
            && !selecting
            && !pointer
            && !self.agent.is_open
            && !self.ai_chats.is_open
            && self.agent.session.is_none()
            && self.reference_inspector_tab == crate::native_shell::InspectorTab::Agent
            && !self.config.experimental_task_sidebar
            && !jterm_core::reference_conversation::fixture_enabled();

        let reduced = self.organism_motion_control.system_reduced();
        let header = 72.0_f32.min(self.reference_layout().header.height);
        let viewport = Rect {
            x: 0.,
            y: 0.,
            width: self.win_size.width.max(1.) as f64,
            height: self.win_size.height.max(1.) as f64,
        };
        // Dedicated inspector slot: measure its actual native Canvas bounds below.
        // Reserve no terminal/document rows. Tiny/narrow inspectors have no slot.
        let slot_size = inspector.and_then(|r| {
            let height = r.height - header - 400.;
            let width = r.width - 24.;
            if width >= 240. && height >= 242. {
                Some((240., 242.))
            } else if width >= 120. && height >= 121. {
                Some((120., 121.))
            } else {
                None
            }
        });
        let running = session
            .filter(|s| {
                !s.managed_remote
                    && !s.transcript_read_only()
                    && !matches!(s.fg_proc_cache.as_deref(), Some("ssh" | "mosh" | "telnet"))
            })
            .map(|s| (s.id, s.terminal.is_command_running()));
        let eva = self.eva.as_mut().unwrap();
        if eva.closed {
            return;
        }
        eva.advance();
        if eva.closing {
            eva.cached = None;
            eva.deadline = None;
            eva.slot_size = None;
            return;
        }
        self.config.eva_companion_skin = eva.host.gate.skin().to_string();
        let skin = atlas::Skin::from_key(eva.host.gate.skin()).unwrap_or(atlas::Skin::Cat);
        self.config.ascii_organism_motion = Some(match eva.host.gate.motion() {
            "full" => crate::organism::Motion::Full,
            "static" => crate::organism::Motion::Static,
            _ => crate::organism::Motion::Calm,
        });
        let mut motion = match eva.host.gate.motion() {
            "full" => atlas::Motion::Full,
            "static" => atlas::Motion::Static,
            _ => atlas::Motion::Calm,
        };
        if (eva.host.gate.quiet() || eva.host.gate.night()) && motion == atlas::Motion::Full {
            motion = atlas::Motion::Calm;
        }
        let mut task_observation = None;
        if running != eva.running_task {
            if let Some((id, true)) = running {
                eva.task_revision = eva.task_revision.saturating_add(1);
                if eva.task_active.insert(id) {
                    eva.task_ids.insert(id, eva.task_revision);
                }
                let task = *eva.task_ids.entry(id).or_insert(eva.task_revision);
                task_observation = Some(Request::TaskObservation {
                    task_id: task_reference(eva.host.gate.host_id(), id, task),
                    context_revision: eva.task_revision,
                    status: shared::TaskStatus::Running,
                });
            }
            eva.running_task = running;
        }
        eva.sync_context(running.map(|(id, _)| id));
        // Re-admission clears the daemon's context. The new task context must
        // reach it before the first fact; otherwise that fact is rejected and
        // the unchanged local running-task cache would prevent a retry.
        if let Some(observation) = task_observation {
            eva.send(observation);
        }
        let input = eva.input_until.is_some_and(|until| Instant::now() < until) || unsafe_input;
        let safe = eligible && !input && slot_size.is_some();
        // Reserve layout while an inspector field owns input. Painting and
        // offered geometry remain gated by `safe`; typing must not remove the
        // canvas and replace the focused field's retained widget state.
        eva.slot_size = slot_size;
        let slot = eva.geometry.filter(|r| {
            slot_size.is_some_and(|(w, h)| {
                (r.width - w as f64).abs() < 1. && (r.height - h as f64).abs() < 1.
            })
        });
        let candidates = if safe {
            slot.into_iter().collect::<Vec<_>>()
        } else {
            vec![]
        };
        let metadata = (
            self.focused,
            input,
            selecting,
            safe,
            slot.unwrap_or(viewport),
        );
        // No content enters IPC: only geometry, focus and booleans.
        if eva.metadata != Some(metadata) {
            eva.send(Request::Focus {
                focused: self.focused,
            });
            eva.send(Request::Input {
                active: input || !eligible,
                selecting,
            });
            eva.send(Request::Geometry {
                revision: eva.generation,
                viewport,
                candidates: candidates.clone(),
                exclusions: vec![],
            });
            eva.metadata = Some(metadata);
        }
        eva.placement = shared::choose(viewport, &candidates, &[], (240., 242.), (120., 121.));
        let visible = eva.registration.is_some()
            && eva.host.gate.token().is_some()
            && !matches!(eva.placement, Placement::Hidden);
        let pose = atlas::Pose::from_key(eva.host.gate.pose()).unwrap_or(atlas::Pose::Idle);
        if eva.last_pose != pose.key() {
            eva.last_pose = pose.key().into();
            eva.action_started = Instant::now();
        }
        let mut request = atlas::Request::new(skin, pose);
        request.visible = visible;
        request.motion = motion;
        request.reduced_motion = reduced;
        request.elapsed_ms = eva
            .action_started
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let frame = atlas::sample(request);
        if eva.cached.as_ref().map(|f| (&f.cells, &f.accessible_label))
            != frame.as_ref().map(|f| (&f.cells, &f.accessible_label))
        {
            eva.cache.clear();
        }
        if eva.deadline.is_none() || !visible || motion == atlas::Motion::Static || reduced {
            eva.animation_generation = eva.animation_generation.wrapping_add(1);
            eva.deadline = frame.as_ref().and_then(|f| f.next_frame_in_ms).map(|ms| {
                (
                    eva.animation_generation,
                    Instant::now() + Duration::from_millis(ms.max(1)),
                )
            });
        }
        eva.cached = frame;
    }
    pub(crate) fn eva_card(&self) -> Element<'_, Message> {
        let Some(eva) = self.eva.as_ref() else {
            return iced::widget::Space::new().height(0).into();
        };
        let context_revision = eva.context.as_ref().map_or(0, |c| c.revision);
        let turn = eva
            .dialogue
            .as_ref()
            .map_or(String::new(), |(id, _)| id.clone());
        let mut card = column![
            text("Eva · shared companion").size(13),
            text(eva.host.gate.status()).size(11)
        ]
        .spacing(6);
        let mut avatar = column![].spacing(6);
        if let Some((width, height)) = eva.slot_size {
            // The unpainted slot can establish geometry before residence is granted.
            // Only the authorized cached frame paints; it is never an offline fallback.
            avatar = avatar.push(
                canvas::Canvas::new(Glyphs {
                    frame: eva.cached.as_ref(),
                    submission: Some(&eva.submission),
                    serial: eva.generation,
                    cache: &eva.cache,
                    skin: atlas::Skin::from_key(&self.config.eva_companion_skin)
                        .unwrap_or(atlas::Skin::Cat),
                    contrast: self.config.eva_companion_high_contrast,
                    dim: eva.host.gate.quiet() || eva.host.gate.night(),
                })
                .width(width)
                .height(height),
            );
            avatar = avatar.push(
                text(
                    eva.cached
                        .as_ref()
                        .map_or(" ", |frame| frame.accessible_label.as_str()),
                )
                .size(11),
            );
        }
        // Transient glyph/error children stay inside fixed parent positions;
        // neither an admission change nor a hidden body may reset Ask Eva.
        card = card.push(avatar);
        let mut controls = column![].spacing(6);
        if let Some(error) = eva.host.gate.operation_error() {
            controls = controls.push(text(error.to_string()).size(11));
        }
        card = card.push(controls.push(self.eva_controls()));
        card = card
            .push(
                text_input("Ask Eva — never sent to the shell", &eva.draft)
                    .on_input(|v| Message::Eva(Event::Draft(v)))
                    .on_submit(Message::Eva(Event::Submit(context_revision))),
            )
            .push(
                row![
                    button(text("Send to Eva"))
                        .on_press(Message::Eva(Event::Submit(context_revision))),
                    button(text("Cancel reply"))
                        .on_press(Message::Eva(Event::CancelDialogue(turn.clone())))
                ]
                .spacing(4),
            )
            .push(text(eva.dialogue_status.clone()).size(11));
        let can_select = self.sessions.get(self.active).is_some_and(|s| {
            !s.managed_remote
                && !s.transcript_read_only()
                && !s.terminal.is_alt_buffer_active()
                && s.terminal.has_text_selection()
        });
        card = card.push(
            button(text("Preview current selection (local only)")).on_press_maybe(
                can_select.then_some(Message::Eva(Event::SelectionCapture(context_revision))),
            ),
        );
        if let Some((origin, selected, source)) = &eva.selection_preview {
            card = card
                .push(
                    text(format!(
                        "Local snapshot · {} · range/source {} · {} bytes",
                        origin.pane_id,
                        source,
                        selected.len()
                    ))
                    .size(11),
                )
                .push(scrollable(text(selected.clone()).size(12)).height(90))
                .push(
                    button(text("Request review of this exact snapshot"))
                        .on_press(Message::Eva(Event::SelectionReview(origin.revision))),
                );
        }
        if let Some(evidence) = &eva.selection_review {
            card = card
                .push(text("Approve once: attach only to the next explicit Send to Eva").size(11))
                .push(
                    button(text("Approve exact selection once")).on_press(Message::Eva(
                        Event::SelectionApprove(
                            evidence.consent_id.clone(),
                            evidence.context.revision,
                        ),
                    )),
                );
        }
        if eva.selection_approved.is_some() {
            card = card.push(
                text("One selected snapshot approved for next Send; not sent to model yet")
                    .size(11),
            );
        }
        if eva.selection_preview.is_some()
            || eva.selection_review.is_some()
            || eva.selection_approved.is_some()
        {
            card = card.push(
                button(text("Discard selected context"))
                    .on_press(Message::Eva(Event::SelectionCancel)),
            );
        }
        if !eva.dialogue_reply.is_empty() {
            card = card.push(scrollable(text(eva.dialogue_reply.clone()).size(12)).height(80));
        }
        if !eva.dialogue_reply.is_empty() {
            card = card.push(
                button(text("Acknowledge displayed reply"))
                    .on_press(Message::Eva(Event::ReplySeen(turn.clone()))),
            );
        }
        card = card.push(button(text("Eva settings / memory")).on_press(
            Message::ReferenceCommand(crate::keybindings::Command::ConfigOpen),
        ));
        crate::agent_task_ui::report_input_focus(
            container(card).padding(12).width(Length::Fill),
            eva.input_focused,
            |v| Message::Eva(Event::InputFocus(v)),
        )
    }
    fn eva_controls(&self) -> Element<'_, Message> {
        let mut controls = row![
            button(text("Follow")).on_press(Message::Eva(Event::Intent(Intent::Follow))),
            button(text("Stay")).on_press(Message::Eva(Event::Intent(Intent::Stay))),
            button(text("Hide")).on_press(Message::Eva(Event::Intent(Intent::Hide))),
            button(text("Recall")).on_press(Message::Eva(Event::Intent(Intent::Recall)))
        ];
        if let Some(eva) = self
            .eva
            .as_ref()
            .filter(|eva| eva.closing && eva.close_guarded)
        {
            controls = controls.push(crate::widget_identity_scope::scope_key(
                eva.close_ui_revision,
                button(text(if eva.close_paused_for_work {
                    "Keep window open"
                } else {
                    "Keep window open · Esc"
                }))
                .on_press(Message::Eva(Event::KeepOpen(eva.close_ui_revision))),
            ));
        }
        controls.spacing(4).wrap().into()
    }
    pub(crate) fn eva_settings(&self) -> Element<'_, Message> {
        let skin =
            atlas::Skin::from_key(&self.config.eva_companion_skin).unwrap_or(atlas::Skin::Cat);
        let mut preview = atlas::Request::new(skin, self.eva_preview_pose);
        preview.motion = atlas::Motion::Static;
        preview.mood = self.eva_preview_mood;
        let preview = atlas::sample(preview).expect("static visible preview");
        let mut controls=column![
            text("Eva · one identity across terminals"),
            checkbox(self.config.eva_companion_enabled).label("Shared Eva (next window launch)").on_toggle(|v|Message::Eva(Event::Enabled(v))),
            text("Opt-in; restart this window after saving. Shared mode never falls back to a local pet on disconnect. Disable older terminal pets before using shared mode.").size(12),
            text("Skin — static source sketch preview"),
            pick_list(atlas::Skin::ALL.map(|s|s.key().to_string()).to_vec(),Some(skin.key().to_string()),|s|Message::Eva(Event::Skin(s))),
            text(skin.name()),text(skin.original_sketch()).font(self.mono).size(13),
            text("Implementation-derived static pose / artistic mood preview (not task facts)").size(12),
            pick_list(atlas::Pose::ALL.map(|p|p.key().to_string()).to_vec(),Some(self.eva_preview_pose.key().to_string()),|p|Message::Eva(Event::PreviewPose(p))),
            pick_list(atlas::Mood::ALL.map(|m|m.key().to_string()).to_vec(),Some(self.eva_preview_mood.key().to_string()),|m|Message::Eva(Event::PreviewMood(m))),
            text(preview.lines().join("\n")).font(self.mono).size(8),
            text(preview.accessible_label).size(12),
            checkbox(self.config.eva_companion_high_contrast).label("High contrast Eva").on_toggle(|v|Message::Eva(Event::Contrast(v))),
            text("Motion uses Full / Calm / Static below; reduced motion also stops Eva frame timers. Skin previews are static, never a second live body.").size(12),
            text("Native presentation submission is supported; GPU completion, scanout and compositor destruction proof are not. Strict physical mode requires JWM/compositor proof.").size(12),
        ].spacing(8);
        if let Some(eva) = self.eva.as_ref() {
            controls = controls
                .push(text(eva.host.gate.status()))
                .push(self.eva_controls())
                .push(
                    text(format!(
                        "Task: metadata only · Memory: {} · Audio: owned by Eva",
                        eva.memory_status
                    ))
                    .size(12),
                );
        }
        if let Some(eva) = self.eva.as_ref() {
            controls = controls
                .push(
                    checkbox(eva.host.gate.quiet())
                        .label("Quiet mode — less motion / lower brightness")
                        .on_toggle(|v| Message::Eva(Event::Quiet(v))),
                )
                .push(
                    checkbox(eva.host.gate.night())
                        .label("Night mode — shared night-cat skin / softer motion")
                        .on_toggle(|v| Message::Eva(Event::Night(v))),
                );
            if let Some(error) = eva.host.gate.operation_error() {
                controls = controls.push(text(error.to_string()).size(12));
            }
            let context_revision = eva.context.as_ref().map_or(0, |c| c.revision);
            controls = controls
                .push(text(
                    "Project scope — explicit named assignment; no automatic directory/file access",
                ))
                .push(
                    text(format!(
                        "Current pane project: {}",
                        eva.context
                            .as_ref()
                            .and_then(|c| c.project_id.as_deref())
                            .unwrap_or("none")
                    ))
                    .size(12),
                )
                .push(
                    text_input(
                        "Project ID: letters, digits, _-.: (empty clears)",
                        &eva.project_draft,
                    )
                    .on_input(|v| Message::Eva(Event::ProjectDraft(v))),
                )
                .push(
                    button(text("Assign project / new task scope / clear approvals"))
                        .on_press(Message::Eva(Event::ProjectAssign(context_revision))),
                )
                .push(
                    checkbox(eva.memory_project)
                        .label("Manage current PROJECT scope (not model-memory consent)")
                        .on_toggle(|v| Message::Eva(Event::MemoryProject(v))),
                )
                .push(text(
                    "Archived memory management — never adopts tasks or enables model retrieval",
                ))
                .push(row![
                    button(text("Discover archived scopes"))
                        .on_press(Message::Eva(Event::ScopeDiscover(None))),
                    button(text("More scopes")).on_press_maybe(
                        eva.scope_next
                            .clone()
                            .map(|n| Message::Eva(Event::ScopeDiscover(Some(n))))
                    )
                ]);
            controls = controls.push(row![
                button(text("Discover stored memory scopes"))
                    .on_press(Message::Eva(Event::StoredScopeDiscover(None))),
                button(text("More stored scopes")).on_press_maybe(
                    eva.stored_scope_next
                        .clone()
                        .map(|after| Message::Eva(Event::StoredScopeDiscover(Some(after))))
                )
            ]);
            for scope in &eva.stored_scopes {
                controls = controls.push(
                    button(text(format!("Review stored scope {scope:?}"))).on_press(Message::Eva(
                        Event::ScopeReview(scope.clone(), context_revision),
                    )),
                );
            }
            for item in &eva.task_scopes {
                controls =
                    controls
                        .push(
                            text(format!(
                                "Task {} · owner {}",
                                item.task_id, item.owner_host_id
                            ))
                            .size(11),
                        )
                        .push(button(text("Review task management access")).on_press(
                            Message::Eva(Event::ScopeReview(
                                shared::MemoryScope::Task(item.task_id.clone()),
                                context_revision,
                            )),
                        ));
                if let Some(project) = &item.project_id {
                    controls =
                        controls.push(button(text(format!("Review project: {project}"))).on_press(
                            Message::Eva(Event::ScopeReview(
                                shared::MemoryScope::Project(project.clone()),
                                context_revision,
                            )),
                        ));
                }
            }
            controls = controls
                .push(
                    text_input("Exact archived task/project ID", &eva.archive_draft)
                        .on_input(|v| Message::Eva(Event::ArchiveDraft(v))),
                )
                .push(
                    checkbox(eva.archive_project)
                        .label("Archived ID is a project (otherwise task)")
                        .on_toggle(|v| Message::Eva(Event::ArchiveProject(v))),
                )
                .push(
                    button(text("Review exact archived scope")).on_press(Message::Eva(
                        Event::ScopeReview(
                            if eva.archive_project {
                                shared::MemoryScope::Project(eva.archive_draft.trim().into())
                            } else {
                                shared::MemoryScope::Task(eva.archive_draft.trim().into())
                            },
                            context_revision,
                        ),
                    )),
                );
            if let Some((context, scope, id, digest)) = &eva.scope_review {
                controls = controls
                    .push(
                        text(format!(
                            "Management-only scope {scope:?} · source {} · digest {digest}",
                            context.pane_id
                        ))
                        .size(12),
                    )
                    .push(
                        button(text("Approve exact management scope")).on_press(Message::Eva(
                            Event::ScopeApprove(id.clone(), digest.clone(), context.revision),
                        )),
                    );
            }
            if eva.managed_scope.is_some() || eva.scope_review.is_some() {
                controls = controls
                    .push(
                        text(format!("Selected archived scope: {:?}", eva.managed_scope)).size(12),
                    )
                    .push(
                        button(text("Dismiss / return to current scope"))
                            .on_press(Message::Eva(Event::ScopeCurrent)),
                    );
            }
            let history = &eva.host.history;
            let status = match eva
                .history_error
                .as_ref()
                .filter(|(_, at)| *at == history.revision())
            {
                Some((message, _)) => message.as_str(),
                None if history.status().is_empty() => {
                    "Nothing is loaded until you choose Load history."
                }
                None => history.status(),
            };
            controls = controls
                .push(text("Conversation history — read-only, loaded only on request"))
                .push(
                    text("Eva dialogue only. Terminal output and voice transcripts are never included.")
                        .size(12),
                )
                .push(pick_list(
                    shared::history::SCOPES
                        .map(|s| shared::history::scope_label(s).to_string())
                        .to_vec(),
                    Some(shared::history::scope_label(history.scope()).to_string()),
                    |label| Message::Eva(Event::HistoryScope(label)),
                ))
                .push(
                    row![
                        button(text("Load history")).on_press_maybe(
                            (!history.pending())
                                .then_some(Message::Eva(Event::HistoryLoad(false, context_revision)))
                        ),
                        button(text("Older")).on_press_maybe(
                            history
                                .can_load_older()
                                .then_some(Message::Eva(Event::HistoryLoad(true, context_revision)))
                        ),
                    ]
                    .spacing(4),
                )
                .push(text(status).size(12));
            if !eva.history_text.1.is_empty() {
                controls = controls
                    .push(scrollable(text(eva.history_text.1.as_str()).size(12)).height(240));
            }
            controls = controls
                .push(text("Scoped memory — explicit review before every change"))
                .push(button(text("Review saving this exchange")).on_press_maybe(
                    eva.dialogue_save.turn_id().map(|id| Message::Eva(Event::DialogueSave(id.into(), context_revision)))
                ))
                .push(
                    checkbox(eva.memory_global)
                        .label("Manage GLOBAL scope explicitly (otherwise task; not model-memory consent)")
                        .on_toggle(|v| Message::Eva(Event::MemoryGlobal(v))),
                )
                .push(
                    text(if eva.memory_scope().is_some() {
                        "Selected scope is available"
                    } else {
                        "No active task scope; choose global explicitly or start a task"
                    })
                    .size(12),
                )
                .push(
                    row![
                        button(text("View / refresh")).on_press(Message::Eva(Event::MemoryList)),
                        button(text("Clear scope…"))
                            .on_press(Message::Eva(Event::MemoryClear(context_revision)))
                    ]
                    .spacing(4),
                )
                .push(
                    checkbox(eva.memory_paused)
                        .label("Pause memory")
                        .on_toggle(|v| Message::Eva(Event::MemoryPause(v))),
                )
                .push(
                    text_input("Memory key (same key edits after refresh)", &eva.memory_key)
                        .on_input(|v| Message::Eva(Event::MemoryKey(v))),
                )
                .push(
                    text_input("Explicit preference / fact", &eva.memory_value)
                        .on_input(|v| Message::Eva(Event::MemoryValue(v))),
                )
                .push(
                    button(text("Propose save / edit…"))
                        .on_press(Message::Eva(Event::MemorySave(context_revision))),
                )
                .push(text(eva.memory_status.clone()).size(12));
            for entry in eva.memory_entries.iter().take(64) {
                controls = controls.push(
                    row![
                        text(format!(
                            "{}: {} · revision {}",
                            entry.key, entry.value, entry.revision
                        ))
                        .size(12)
                        .width(Length::Fill),
                        button(text("Delete…")).on_press(Message::Eva(Event::MemoryDelete(
                            entry.id,
                            entry.revision,
                            context_revision
                        )))
                    ]
                    .spacing(4)
                    .width(Length::Fill),
                );
            }
            if eva.memory_next.is_some() {
                controls = controls.push(
                    button(text("Next memory page")).on_press(Message::Eva(Event::MemoryMore)),
                );
            }
            if let Some((context, review)) = &eva.memory_review {
                controls = controls
                    .push(
                        text(format!(
                            "Review source {} · {:?} · digest {}",
                            context.pane_id, review.change, review.digest
                        ))
                        .size(12)
                        .width(Length::Fill),
                    )
                    .push(
                        row![
                            button(text("Approve exact change")).on_press(Message::Eva(
                                Event::MemoryApprove(
                                    review.id,
                                    review.digest.clone(),
                                    context.revision
                                )
                            )),
                            button(text("Cancel review"))
                                .on_press(Message::Eva(Event::MemoryCancel))
                        ]
                        .spacing(4),
                    );
            }
        }
        controls.into()
    }
    pub(crate) fn eva_witness<'a>(&'a self, root: Element<'a, Message>) -> Element<'a, Message> {
        if let Some(eva) = self.eva.as_ref() {
            let overlay: Option<Element<'a, Message>> = if eva.closing && !eva.close_paused_for_work
            {
                let mut content=column![text("Closing Frost"),text(eva.host.gate.status().to_string()),text("Eva is leaving this window. Closing before it finishes may keep it from appearing in another window.").size(13)];
                if eva.close_guarded {
                    content = content.push(
                        button(text(if eva.close_paused_for_work {
                            "Keep window open"
                        } else {
                            "Keep window open · Esc"
                        }))
                        .on_press(Message::Eva(Event::KeepOpen(eva.close_ui_revision))),
                    );
                }
                let panel = container(
                    content
                        .push(
                            button(text("Close anyway"))
                                .on_press(Message::Eva(Event::ForceClose(eva.close_ui_revision))),
                        )
                        .spacing(12),
                )
                .padding(24)
                .style(container::rounded_box);
                let panel = crate::widget_identity_scope::scope_key(eva.close_ui_revision, panel);
                Some(iced::widget::opaque(container(panel).center(Length::Fill)).into())
            } else if eva.closing && eva.close_guarded && eva.close_paused_for_work {
                let panel = container(
                    row![
                        text("Work is still running. Finish it, or keep this window open."),
                        button(text("Keep window open"))
                            .on_press(Message::Eva(Event::KeepOpen(eva.close_ui_revision))),
                    ]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                )
                .padding(12)
                .style(container::rounded_box);
                let panel = crate::widget_identity_scope::scope_key(eva.close_ui_revision, panel);
                Some(container(panel).center_x(Length::Fill).padding(48).into())
            } else {
                None
            };
            let root = crate::widget_identity_scope::transient_overlay(root, overlay);
            let witness = Witness {
                serial: eva.generation,
                clear: eva.host.gate.token().is_none(),
                submission: eva.submission.clone(),
            };
            iced::widget::stack![
                root,
                canvas::Canvas::new(witness)
                    .width(Length::Fill)
                    .height(Length::Fill)
            ]
            .into()
        } else {
            root
        }
    }
}
struct Witness {
    serial: u64,
    clear: bool,
    submission: Arc<Submission>,
}
impl canvas::Program<Message> for Witness {
    type State = ();
    fn draw(
        &self,
        _: &(),
        _: &iced::Renderer,
        _: &iced::Theme,
        _: Rectangle,
        _: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if self.clear {
            self.submission
                .cleared_tree
                .store(self.serial, Ordering::Release);
        }
        vec![]
    }
}
struct Glyphs<'a> {
    frame: Option<&'a atlas::Frame>,
    submission: Option<&'a Submission>,
    serial: u64,
    cache: &'a canvas::Cache,
    skin: atlas::Skin,
    contrast: bool,
    dim: bool,
}
impl canvas::Program<Message> for Glyphs<'_> {
    type State = Option<Rectangle>;
    fn update(
        &self,
        state: &mut Self::State,
        _: &canvas::Event,
        bounds: Rectangle,
        _: iced::mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        if *state != Some(bounds) {
            *state = Some(bounds);
            Some(iced::widget::Action::publish(Message::Eva(
                Event::Geometry(bounds),
            )))
        } else {
            None
        }
    }
    fn draw(
        &self,
        _: &Self::State,
        renderer: &iced::Renderer,
        _: &iced::Theme,
        bounds: Rectangle,
        _: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let Some(atlas_frame) = self.frame else {
            return vec![];
        };
        if let Some(submission) = self.submission {
            submission.body_tree.store(self.serial, Ordering::Release);
        }
        vec![self.cache.draw(renderer, bounds.size(), |frame| {
            let cw = bounds.width / atlas_frame.width as f32;
            let ch = bounds.height / atlas_frame.height as f32;
            let palette = self.skin.palette(self.contrast);
            for (index, cell) in atlas_frame.cells.iter().enumerate() {
                if cell.glyph == ' ' {
                    continue;
                }
                let [r, g, b] = palette.color(cell.ink);
                let color = Color::from_rgb8(r, g, b);
                let color = if self.dim && !self.contrast {
                    Color::from_rgb(color.r * 0.78, color.g * 0.78, color.b * 0.78)
                } else {
                    color
                };
                frame.fill_text(canvas::Text {
                    content: cell.glyph.to_string(),
                    position: Point::new(
                        (index % atlas_frame.width) as f32 * cw,
                        (index / atlas_frame.width) as f32 * ch,
                    ),
                    color,
                    size: (ch * 0.9).into(),
                    font: iced::Font::MONOSPACE,
                    ..Default::default()
                });
            }
        })]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_consent_binds_snapshot_source_and_context() {
        let c = shared::ContextRef {
            host_id: "host-a".into(),
            pane_id: "pane-1".into(),
            task_id: Some("task-a".into()),
            project_id: Some("project-a".into()),
            revision: 7,
        };
        let p: (shared::ContextRef, String, String) = (
            c.clone(),
            "selected only".into(),
            "selection-uuid-1:2-3:4".into(),
        );
        assert!(shared::identifier(&p.2));
        let mut e = shared::SelectionEvidence {
            context: c.clone(),
            text: p.1.clone(),
            consent_id: "consent-1".into(),
            source_event_id: p.2.clone(),
            captured_at_ms: 1,
        };
        assert!(selection_matches(Some(&c), &p, &e));
        e.source_event_id = "replacement".into();
        assert!(!selection_matches(Some(&c), &p, &e));
        e.source_event_id = p.2.clone();
        e.text.push('x');
        assert!(!selection_matches(Some(&c), &p, &e));
        e.text = p.1.clone();
        e.context.project_id = Some("project-b".into());
        assert!(!selection_matches(Some(&c), &p, &e));
    }
    #[test]
    fn archived_management_review_cannot_cross_revision_digest_or_pane() {
        let c = shared::ContextRef {
            host_id: "host-a".into(),
            pane_id: "pane-1".into(),
            task_id: Some("current-task".into()),
            project_id: None,
            revision: 7,
        };
        let pending = (
            c.clone(),
            shared::MemoryScope::Task("old-task".into()),
            "review-1".into(),
            "digest-1".into(),
        );
        assert!(scope_approval_matches(
            Some(&c),
            &pending,
            "review-1",
            "digest-1",
            7
        ));
        assert!(!scope_approval_matches(
            Some(&c),
            &pending,
            "review-1",
            "changed",
            7
        ));
        assert!(!scope_approval_matches(
            Some(&c),
            &pending,
            "review-2",
            "digest-1",
            7
        ));
        assert!(!scope_approval_matches(
            Some(&c),
            &pending,
            "review-1",
            "digest-1",
            8
        ));
        let mut moved = c.clone();
        moved.pane_id = "pane-2".into();
        assert!(!scope_approval_matches(
            Some(&moved),
            &pending,
            "review-1",
            "digest-1",
            7
        ));
        assert_eq!(c.task_id.as_deref(), Some("current-task"));
    }
    #[test]
    fn native_iced_software_raster_all_skins_and_poses() {
        use canvas::Program;
        use iced::advanced::{
            graphics::{geometry::Renderer as _, Viewport},
            Renderer as _,
        };
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(240., 242.));
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::MONOSPACE,
            12.into(),
        ));
        let mut sheet = image::RgbaImage::new(8 * 240, 13 * 242);
        for (x, skin) in atlas::Skin::ALL.into_iter().enumerate() {
            for (y, pose) in atlas::Pose::ALL.into_iter().enumerate() {
                let mut request = atlas::Request::new(skin, pose);
                request.motion = atlas::Motion::Static;
                let frame = atlas::sample(request).unwrap();
                let cache = canvas::Cache::new();
                let glyphs = Glyphs {
                    frame: Some(&frame),
                    submission: None,
                    serial: 0,
                    cache: &cache,
                    skin,
                    contrast: false,
                    dim: false,
                };
                renderer.reset(bounds);
                for geometry in glyphs.draw(
                    &None,
                    &renderer,
                    &iced::Theme::Dark,
                    bounds,
                    iced::mouse::Cursor::Unavailable,
                ) {
                    renderer.draw_geometry(geometry);
                }
                let iced::Renderer::Secondary(software) = &mut renderer else {
                    unreachable!()
                };
                let mut pixels = tiny_skia::Pixmap::new(240, 242).unwrap();
                let mut mask = tiny_skia::Mask::new(240, 242).unwrap();
                software.draw(
                    &mut pixels.as_mut(),
                    &mut mask,
                    &Viewport::with_physical_size(iced::Size::new(240, 242), 1.),
                    &[bounds],
                    Color::TRANSPARENT,
                );
                assert!(
                    pixels.data().chunks_exact(4).filter(|p| p[3] > 0).count() > 100,
                    "missing native glyph raster: {} {}",
                    skin.key(),
                    pose.key()
                );
                // Iced tiny-skia targets softbuffer's BGRA-premultiplied layout;
                // a PNG is straight RGBA. Preserve the actual native colors.
                let mut rgba = pixels.data().to_vec();
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                    let alpha = pixel[3] as u32;
                    if alpha > 0 {
                        for c in &mut pixel[..3] {
                            *c = ((*c as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                        }
                    }
                }
                if skin == atlas::Skin::Fox {
                    assert!(
                        rgba.chunks_exact(4)
                            .any(|p| p[3] > 100 && p[0] as u16 > p[2] as u16 + 20),
                        "Fox native palette must remain orange, not BGR-blue"
                    );
                }
                let tile = image::RgbaImage::from_raw(240, 242, rgba).unwrap();
                image::imageops::replace(&mut sheet, &tile, (x * 240) as i64, (y * 242) as i64);
            }
        }
        if let Some(dir) = std::env::var_os("EVA_NATIVE_QA_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            sheet.save(dir.join("frost-eva-native-atlas.png")).unwrap();
            std::fs::write(dir.join("frost-eva-native-atlas.txt"),"Actual Iced tiny-skia glyph raster, native BGRA-premultiplied converted to straight RGBA for PNG. Columns: cat fox robot ghost cursor beetle night stardust. Rows: idle blink peek sit curl_up stretch retreat walk listen speak guard_failure celebrate_briefly look_at. Static; seed=0; elapsed_ms=0; 240x242 logical px; scale=1. Not WGPU/window/compositor/IME evidence. Atlas is implementation-derived, not source raster extraction.").unwrap();
        }
    }
    // Exact native ordering and registration lifetime are tested by the
    // production-source harness and real X11 matrix, not source-text searches.
    #[test]
    fn shared_eva_is_opt_in_and_settings_round_trip() {
        let config = crate::config::Config::default();
        assert!(!config.eva_companion_enabled);
        let mut config = config;
        config.eva_companion_enabled = true;
        config.eva_companion_skin = "fox".into();
        config.eva_companion_high_contrast = true;
        let encoded = toml::to_string(&config).unwrap();
        let restored: crate::config::Config = toml::from_str(&encoded).unwrap();
        assert!(restored.eva_companion_enabled);
        assert_eq!(restored.eva_companion_skin, "fox");
        assert!(restored.eva_companion_high_contrast);
        let example: crate::config::Config =
            toml::from_str(include_str!("../config.toml.example")).unwrap();
        assert!(!example.eva_companion_enabled);
        assert_eq!(example.eva_companion_skin, "cat");
    }
    #[test]
    fn memory_approval_binds_exact_review_digest_and_source_context() {
        let context = shared::ContextRef {
            host_id: "h".into(),
            pane_id: "pane-1".into(),
            task_id: Some("task-1".into()),
            project_id: None,
            revision: 4,
        };
        let review = shared::MemoryReview {
            id: 1,
            digest: "exact".into(),
            generation: 1,
            change: shared::MemoryChange::Clear {
                scope: shared::MemoryScope::Task("task-1".into()),
            },
        };
        let pending = (context.clone(), review);
        assert!(review_matches(Some(&context), &pending, 1, "exact", 4));
        assert!(!review_matches(Some(&context), &pending, 2, "exact", 4));
        assert!(!review_matches(Some(&context), &pending, 1, "replaced", 4));
        let mut moved = context.clone();
        moved.pane_id = "pane-2".into();
        assert!(!review_matches(Some(&moved), &pending, 1, "exact", 4));
        moved = context.clone();
        moved.revision = 5;
        assert!(!review_matches(Some(&moved), &pending, 1, "exact", 4));
        assert!(!review_matches(None, &pending, 1, "exact", 4));
    }
    #[test]
    fn body_submission_is_once_per_authorized_draw_not_grant_receipt() {
        let s = Submission::default();
        s.body_after.store(10, Ordering::Release);
        s.body_pending.store(true, Ordering::Release);
        assert!(!s.presented());
        s.body_tree.store(10, Ordering::Release);
        assert!(!s.presented());
        s.body_tree.store(11, Ordering::Release);
        assert!(s.presented());
        assert!(!s.presented());
        assert_eq!(s.body_submitted.load(Ordering::Acquire), 11);
    }
    #[test]
    fn task_memory_scope_is_unique_across_windows_with_identical_local_counters() {
        let a = task_reference("frost-window-a-uuid", 0, 1);
        let b = task_reference("frost-window-b-uuid", 0, 1);
        assert_ne!(a, b);
        assert!(shared::identifier(&a));
        assert!(shared::identifier(&b));
        assert_ne!(shared::MemoryScope::Task(a), shared::MemoryScope::Task(b));
    }
    #[test]
    fn failed_present_then_body_free_draw_cannot_ack_stale_body() {
        let s = Submission::default();
        s.body_after.store(10, Ordering::Release);
        s.body_pending.store(true, Ordering::Release);
        s.begin_draw();
        s.body_tree.store(11, Ordering::Release); // renderer present fails: no presented() callback
        s.begin_draw();
        s.cleared_tree.store(12, Ordering::Release); // next native frame has no body
        assert!(!s.presented());
        assert_eq!(s.body_submitted.load(Ordering::Acquire), 0);
        assert!(s.body_pending.load(Ordering::Acquire));
    }
    #[test]
    fn delayed_snapshot_cannot_be_relabelled_as_a_new_clear_frame() {
        let s = Submission::default();
        s.after.store(7, Ordering::Release);
        s.pending.store(true, Ordering::Release);
        s.cleared_tree.store(8, Ordering::Release);
        let old = s.snapshot();
        s.after.store(9, Ordering::Release);
        s.cleared_tree.store(12, Ordering::Release);
        assert!(!s.presented_snapshot(old));
        assert!(s.pending.load(Ordering::Acquire));
        assert!(s.presented_snapshot(s.snapshot()));
        assert_eq!(s.submitted.load(Ordering::Acquire), 12);
    }
    #[test]
    fn receipt_and_old_frame_are_not_retirement() {
        let s = Submission::default();
        s.after.store(7, Ordering::Release);
        s.pending.store(true, Ordering::Release);
        s.cleared_tree.store(7, Ordering::Release);
        assert!(!s.presented());
        assert!(s.pending.load(Ordering::Acquire));
        s.cleared_tree.store(8, Ordering::Release);
        assert!(s.presented());
        assert!(!s.presented());
        assert_eq!(s.submitted.load(Ordering::Acquire), 8);
    }
    #[test]
    fn atlas_all_skin_pose_modes_native_cache_inputs_are_bounded() {
        for skin in atlas::Skin::ALL {
            for pose in atlas::Pose::ALL {
                for motion in [
                    atlas::Motion::Full,
                    atlas::Motion::Calm,
                    atlas::Motion::Static,
                ] {
                    let mut req = atlas::Request::new(skin, pose);
                    req.motion = motion;
                    let frame = atlas::sample(req).unwrap();
                    assert_eq!(frame.cells.len(), frame.width * frame.height);
                    assert!(frame.cells.iter().all(|c| c.glyph.is_ascii()));
                    if motion == atlas::Motion::Static {
                        assert!(frame.next_frame_in_ms.is_none());
                    }
                    req.visible = false;
                    assert!(atlas::sample(req).is_none());
                }
            }
        }
    }
}
