//! Display-free regression fixture for the actual automatic Files worker and callback.
//!
//! Run with `python3 scripts/test-files-follow.py`. Production worker, callback,
//! context gates, invalidators, synchronous commit helper, cancellation token,
//! and selected native tests are inserted from the current checkout each run.
//! Sidebar/UI, process observation, and transport boundaries are deterministic
//! doubles. This does not test real rendering, SSH, or observation admission.

#![allow(dead_code)]
use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

mod jterm_core {
    pub mod jsh_remote {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct RemoteHostConfig {
            pub name: String,
            pub host: String,
            pub control_path: Option<String>,
        }
        impl RemoteHostConfig {
            pub fn display_name(&self) -> &str {
                &self.name
            }
        }
        #[derive(Clone, Debug)]
        pub enum ObservedSshTarget {
            Target(RemoteHostConfig),
            Unsupported,
        }
    }
    pub mod process {
        use super::jsh_remote::*;
        #[derive(Clone, Debug)]
        pub struct ObservedSshCommand {
            pub argv: Vec<String>,
            pub target: ObservedSshTarget,
            pub reusable_control_path: Option<String>,
        }
    }
    pub mod review_input {
        pub fn safe_inline_display(value: &str, limit: usize) -> String {
            value.chars().take(limit).collect()
        }
    }
}
mod config {
    pub fn default_remote_hosts() -> Vec<crate::jterm_core::jsh_remote::RemoteHostConfig> {
        vec![crate::jterm_core::jsh_remote::RemoteHostConfig {
            name: "test host".into(),
            host: "test.example".into(),
            control_path: None,
        }]
    }
}
mod remote_fs {
    use crate::jterm_core::jsh_remote::RemoteHostConfig;
    use std::{
        io,
        path::{Path, PathBuf},
    };
    // @files-follow:cancel-token
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FsLocation {
        Local,
        Remote(usize),
        Transient(RemoteHostConfig),
    }
    impl FsLocation {
        pub fn label(&self, hosts: &[RemoteHostConfig]) -> String {
            match self {
                Self::Local => "Local".into(),
                Self::Remote(i) => hosts[*i].name.clone(),
                Self::Transient(p) => p.name.clone(),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Entry {
        pub name: String,
        pub path: PathBuf,
        pub is_dir: bool,
    }
    #[derive(Clone, Debug)]
    pub struct DirectoryListing {
        pub entries: Vec<Entry>,
        pub truncated: bool,
    }
    pub fn start_dir_cancellable(
        location: &FsLocation,
        hosts: &[RemoteHostConfig],
        cancellation: &CancellationToken,
    ) -> io::Result<PathBuf> {
        if cancellation.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        start_dir(location, hosts)
    }
    pub fn start_dir(_: &FsLocation, _: &[RemoteHostConfig]) -> io::Result<PathBuf> {
        crate::HOME_CALLS.with(|n| n.set(n.get() + 1));
        crate::run_hook(&crate::HOME_HOOK);
        Ok("/remote/home".into())
    }
    pub fn list_dir_listing_with_hidden_cancellable(
        _: &FsLocation,
        _: &[RemoteHostConfig],
        _: &Path,
        _: bool,
        _: Option<&CancellationToken>,
    ) -> io::Result<DirectoryListing> {
        crate::LIST_CALLS.with(|n| n.set(n.get() + 1));
        crate::run_hook(&crate::LIST_HOOK);
        if crate::LIST_FAIL.with(std::cell::Cell::get) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "list failed",
            ));
        }
        // Deliberately allow a just-completed listing to race cancellation.
        // The actual worker/callback must recheck the token before publishing.
        Ok(DirectoryListing {
            entries: vec![Entry {
                name: "remote child".into(),
                path: "/remote/home/child".into(),
                is_dir: false,
            }],
            truncated: false,
        })
    }
}
mod sidebar {
    use super::*;
    use remote_fs::FsLocation;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct FileTreeNode {
        pub name: String,
        pub path: PathBuf,
        pub is_dir: bool,
    }
    impl FileTreeNode {
        pub fn entry(name: String, path: PathBuf, is_dir: bool) -> Self {
            Self { name, path, is_dir }
        }
    }
    pub struct DirectoryRequest {
        pub generation: u64,
        pub request_id: u64,
        pub path: PathBuf,
    }
    pub struct DirectoryResult {
        pub generation: u64,
        pub request_id: u64,
        pub path: PathBuf,
        pub entries: Result<Vec<FileTreeNode>, String>,
        pub truncated: bool,
    }
    pub struct Sidebar {
        pub location: FsLocation,
        pub current_dir: PathBuf,
        pub rows: Vec<FileTreeNode>,
        pub commits: usize,
        pub rebinds: usize,
        generation: u64,
        hosts: Vec<jterm_core::jsh_remote::RemoteHostConfig>,
        pending_location: Option<FsLocation>,
        pending_path: Option<PathBuf>,
    }
    impl Sidebar {
        pub fn new() -> Self {
            Self {
                location: FsLocation::Local,
                current_dir: "/local/kept".into(),
                rows: vec![FileTreeNode::entry(
                    "kept child".into(),
                    "/local/kept/child".into(),
                    false,
                )],
                commits: 0,
                rebinds: 0,
                generation: 0,
                hosts: config::default_remote_hosts(),
                pending_location: None,
                pending_path: None,
            }
        }
        pub fn generation(&self) -> u64 {
            self.generation
        }
        pub fn hosts_snapshot(&self) -> &[jterm_core::jsh_remote::RemoteHostConfig] {
            &self.hosts
        }
        pub fn navigation_pending_target(&self) -> Option<&Path> {
            self.pending_path.as_deref()
        }
        pub fn location_change_pending(&self) -> bool {
            self.pending_location.is_some()
        }
        pub fn refresh(&mut self) {
            self.generation += 1
        }
        pub fn begin_location_change(&mut self, location: FsLocation) -> u64 {
            self.generation += 1;
            self.pending_location = Some(location);
            self.pending_path = None;
            self.generation
        }
        pub fn resolve_location(
            &mut self,
            generation: u64,
            result: Result<PathBuf, String>,
        ) -> Option<DirectoryRequest> {
            if generation != self.generation || self.pending_location.is_none() {
                return None;
            }
            let path = result.ok()?;
            self.pending_path = Some(path.clone());
            Some(DirectoryRequest {
                generation,
                request_id: 1,
                path,
            })
        }
        pub fn apply_load(&mut self, result: DirectoryResult) -> bool {
            if result.generation != self.generation
                || self.pending_path.as_ref() != Some(&result.path)
            {
                return false;
            }
            let Ok(rows) = result.entries else {
                return false;
            };
            self.location = self.pending_location.take().unwrap();
            self.current_dir = result.path;
            self.rows = rows;
            self.pending_path = None;
            self.commits += 1;
            true
        }
        pub fn rebind_same_namespace_preserving_tree(&mut self, location: FsLocation) -> bool {
            let (FsLocation::Transient(old), FsLocation::Transient(new)) =
                (&self.location, &location)
            else {
                return false;
            };
            if old.host != new.host {
                return false;
            }
            self.location = location;
            self.generation += 1;
            self.rebinds += 1;
            true
        }
        // @files-follow:sidebar-commit
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum SidebarPanel {
    Tabs,
    Files,
}
#[derive(Debug)]
enum Message {}
struct Task<T> {
    deferred: bool,
    marker: PhantomData<T>,
}
impl<T> Task<T> {
    fn none() -> Self {
        Self {
            deferred: false,
            marker: PhantomData,
        }
    }
}
enum ToastKind {
    Warning,
}
#[derive(Clone)]
struct Session {
    id: usize,
    managed_remote: bool,
    command: Option<jterm_core::process::ObservedSshCommand>,
}
impl Session {
    fn observed_ssh_command(&self) -> Option<jterm_core::process::ObservedSshCommand> {
        self.command.clone()
    }
}
// @files-follow:types
// @files-follow:helpers
// The Files-follow fixture observes this unrelated invalidation boundary.
// Drop planner ownership/capacity is tested by the production drop tests;
// this stub deliberately does not pretend to model those worker lifetimes.
#[derive(Default)]
struct DropIntentInvalidation {
    invalidations: usize,
}
impl DropIntentInvalidation {
    fn clear(&mut self) {
        self.invalidations += 1;
    }
}

struct TerminalApp {
    sidebar: sidebar::Sidebar,
    sidebar_follow_pending: Option<SidebarRemoteFollow>,
    sidebar_follow_observed: Option<(usize, Vec<String>)>,
    sidebar_follow_retry_once: Option<(usize, Vec<String>)>,
    sidebar_follow_retry_available: Option<(usize, Vec<String>)>,
    sidebar_follow_intent_epoch: Option<u64>,
    sessions: Vec<Session>,
    active: usize,
    sidebar_transfer: Option<()>,
    sidebar_menu: Option<()>,
    sidebar_dialog: Option<()>,
    sidebar_delete_confirm: Option<()>,
    sidebar_drop_burst: Vec<()>,
    sidebar_drop_intents: DropIntentInvalidation,
    sidebar_drop_debounce_generation: Option<u64>,
    sidebar_open: bool,
    sidebar_panel: SidebarPanel,
    sidebar_hosts_epoch: u64,
    sidebar_selection: Vec<()>,
    sidebar_selection_anchor: Option<()>,
    sidebar_filter: Option<()>,
    sidebar_path_input: Option<()>,
    mutations: usize,
    queued_loads: usize,
}
impl TerminalApp {
    fn set_sidebar_notice(&mut self, _: String, _: bool) {
        self.mutations += 1
    }
    fn push_toast(&mut self, _: String, _: ToastKind) {
        self.mutations += 1
    }
    fn apply_config(&mut self) {
        self.mutations += 1
    }
    fn invalidate_sidebar_pending_work(&mut self) {}
    // Keep the former staged API visible: a reintroduced second async hop must
    // fail the no-pending/no-deferred assertions, rather than silently commit.
    fn queue_sidebar_load(&mut self, _: sidebar::DirectoryRequest) -> Task<Message> {
        self.queued_loads += 1;
        Task {
            deferred: true,
            marker: PhantomData,
        }
    }
    // @files-follow:methods
}
type Hook = Box<dyn FnOnce()>;
thread_local! {
    static HOME_CALLS:Cell<usize>=const{Cell::new(0)};
    static LIST_CALLS:Cell<usize>=const{Cell::new(0)};
    static HOME_HOOK:RefCell<Option<Hook>>=RefCell::new(None);
    static LIST_HOOK:RefCell<Option<Hook>>=RefCell::new(None);
    static LIST_FAIL:Cell<bool>=const{Cell::new(false)};
}
fn run_hook(slot: &'static std::thread::LocalKey<RefCell<Option<Hook>>>) {
    let hook = slot.with(|s| s.borrow_mut().take());
    if let Some(f) = hook {
        f()
    }
}
fn on_home(f: impl FnOnce() + 'static) {
    HOME_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)))
}
fn on_list(f: impl FnOnce() + 'static) {
    LIST_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)))
}
fn setup() -> Rc<RefCell<TerminalApp>> {
    let sidebar = sidebar::Sidebar::new();
    let p = config::default_remote_hosts()[0].clone();
    let argv = vec!["ssh".into(), p.host.clone()];
    let pending = SidebarRemoteFollow {
        cancellation: remote_fs::CancellationToken::new(),
        token: 1,
        intent_epoch: 7,
        session_id: 3,
        source_argv: argv.clone(),
        source_profile: p.clone(),
        source_control_path: None,
        attempt: 0,
        target_location: remote_fs::FsLocation::Transient(p.clone()),
        preserve_loaded_tree: false,
        hosts_epoch: 0,
        tree_generation: sidebar.generation(),
        tree_location: sidebar.location.clone(),
        tree_root: sidebar.current_dir.clone(),
        sidebar_open: false,
        sidebar_panel: SidebarPanel::Tabs,
    };
    Rc::new(RefCell::new(TerminalApp {
        sidebar,
        sidebar_follow_pending: Some(pending),
        sidebar_follow_observed: Some((3, argv.clone())),
        sidebar_follow_retry_once: None,
        sidebar_follow_retry_available: None,
        sidebar_follow_intent_epoch: Some(7),
        sessions: vec![Session {
            id: 3,
            managed_remote: false,
            command: Some(jterm_core::process::ObservedSshCommand {
                argv,
                target: jterm_core::jsh_remote::ObservedSshTarget::Target(p),
                reusable_control_path: None,
            }),
        }],
        active: 0,
        sidebar_transfer: None,
        sidebar_menu: None,
        sidebar_dialog: None,
        sidebar_delete_confirm: None,
        sidebar_drop_burst: vec![],
        sidebar_drop_intents: DropIntentInvalidation::default(),
        sidebar_drop_debounce_generation: None,
        sidebar_open: false,
        sidebar_panel: SidebarPanel::Tabs,
        sidebar_hosts_epoch: 0,
        sidebar_selection: vec![()],
        sidebar_selection_anchor: Some(()),
        sidebar_filter: Some(()),
        sidebar_path_input: Some(()),
        mutations: 0,
        queued_loads: 0,
    }))
}
fn work(app: &Rc<RefCell<TerminalApp>>) -> Result<SidebarRemoteFollowResult, String> {
    let (pending, hosts) = {
        let app = app.borrow();
        (
            app.sidebar_follow_pending.as_ref().unwrap().clone(),
            app.sidebar.hosts_snapshot().to_vec(),
        )
    };
    probe_sidebar_remote_follow(
        &pending.target_location,
        &hosts,
        false,
        pending.preserve_loaded_tree,
        &pending.cancellation,
    )
    .map_err(|e| e.to_string())
}
fn finish(app: &Rc<RefCell<TerminalApp>>, result: Result<SidebarRemoteFollowResult, String>) {
    let task = app.borrow_mut().resolve_sidebar_remote_follow(1, result);
    assert!(
        !task.deferred,
        "automatic publication must not escape into a second async hop"
    )
}
fn assert_kept(app: &Rc<RefCell<TerminalApp>>) {
    let app = app.borrow();
    assert_eq!(app.sidebar.location, remote_fs::FsLocation::Local);
    assert_eq!(app.sidebar.current_dir, PathBuf::from("/local/kept"));
    assert_eq!(app.sidebar.rows[0].name, "kept child");
    assert_eq!(app.sidebar.commits, 0);
    assert_eq!(app.queued_loads, 0);
    assert_eq!(app.mutations, 0)
}
fn same_target() -> Rc<RefCell<TerminalApp>> {
    let app = setup();
    {
        let mut a = app.borrow_mut();
        let mut old = config::default_remote_hosts()[0].clone();
        old.control_path = Some("/run/old.sock".into());
        a.sidebar.location = remote_fs::FsLocation::Transient(old);
        a.sidebar.current_dir = "/remote/kept".into();
        let mut pending = a.sidebar_follow_pending.take().unwrap();
        let mut target = pending.source_profile.clone();
        target.control_path = Some("/run/live.sock".into());
        pending.target_location = remote_fs::FsLocation::Transient(target);
        pending.preserve_loaded_tree = true;
        pending.tree_location = a.sidebar.location.clone();
        pending.tree_root = a.sidebar.current_dir.clone();
        a.sidebar_follow_pending = Some(pending);
    }
    app
}
#[test]
fn automatic_current_source_commits_listing_without_second_hop() {
    let app = setup();
    let result = work(&app);
    finish(&app, result);
    let app = app.borrow();
    assert!(matches!(
        app.sidebar.location,
        remote_fs::FsLocation::Transient(_)
    ));
    assert_eq!(app.sidebar.current_dir, PathBuf::from("/remote/home"));
    assert_eq!(app.sidebar.rows[0].name, "remote child");
    assert_eq!(app.sidebar.commits, 1);
    assert_eq!(app.queued_loads, 0);
    assert_eq!(app.mutations, 2);
    assert!(app.sidebar.navigation_pending_target().is_none());
    assert!(app.sidebar_selection.is_empty());
    assert_eq!(HOME_CALLS.with(Cell::get), 1);
    assert_eq!(LIST_CALLS.with(Cell::get), 1)
}
#[test]
fn focus_change_during_initial_listing_cannot_publish() {
    let app = setup();
    let source = app.clone();
    on_list(move || {
        source
            .borrow_mut()
            .active_session_changed_for_remote_follow()
    });
    let result = work(&app);
    assert!(result.is_err());
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn exited_process_during_initial_listing_cannot_publish() {
    let app = setup();
    let source = app.clone();
    on_list(move || source.borrow_mut().sessions[0].command = None);
    let result = work(&app);
    assert!(result.is_ok());
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn replaced_process_argv_during_initial_listing_cannot_publish() {
    let app = setup();
    let source = app.clone();
    on_list(move || {
        source.borrow_mut().sessions[0]
            .command
            .as_mut()
            .unwrap()
            .argv
            .push("changed".into())
    });
    let result = work(&app);
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn files_action_during_initial_listing_cannot_publish() {
    let app = setup();
    let source = app.clone();
    on_list(move || {
        source
            .borrow_mut()
            .invalidate_sidebar_remote_follow_intent()
    });
    let result = work(&app);
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn files_busy_during_initial_listing_cannot_publish() {
    let app = setup();
    let source = app.clone();
    on_list(move || source.borrow_mut().sidebar_dialog = Some(()));
    let result = work(&app);
    assert!(result.is_ok());
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn source_focus_changed_after_worker_cannot_publish() {
    let app = setup();
    let result = work(&app);
    app.borrow_mut().active_session_changed_for_remote_follow();
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn stale_source_listing_error_does_not_show_failure() {
    let app = setup();
    LIST_FAIL.with(|f| f.set(true));
    let source = app.clone();
    on_list(move || source.borrow_mut().sessions[0].command = None);
    let result = work(&app);
    finish(&app, result);
    assert_kept(&app)
}
#[test]
fn cancellation_before_worker_skips_home_and_listing() {
    let app = setup();
    app.borrow()
        .sidebar_follow_pending
        .as_ref()
        .unwrap()
        .cancellation
        .cancel();
    let result = work(&app);
    assert!(result.is_err());
    finish(&app, result);
    assert_kept(&app);
    assert_eq!(HOME_CALLS.with(Cell::get), 0);
    assert_eq!(LIST_CALLS.with(Cell::get), 0)
}
#[test]
fn cancellation_during_home_skips_listing() {
    let app = setup();
    let source = app.clone();
    on_home(move || {
        source
            .borrow_mut()
            .invalidate_sidebar_remote_follow_intent()
    });
    let result = work(&app);
    assert!(result.is_err());
    finish(&app, result);
    assert_kept(&app);
    assert_eq!(HOME_CALLS.with(Cell::get), 1);
    assert_eq!(LIST_CALLS.with(Cell::get), 0)
}
#[test]
fn manual_navigation_survives_source_focus_change() {
    let app = setup();
    let request = {
        let mut a = app.borrow_mut();
        let generation = a
            .sidebar
            .begin_location_change(remote_fs::FsLocation::Remote(0));
        a.sidebar
            .resolve_location(generation, Ok("/manual/chosen".into()))
            .unwrap()
    };
    app.borrow_mut().active_session_changed_for_remote_follow();
    assert!(app
        .borrow_mut()
        .sidebar
        .apply_load(sidebar::DirectoryResult {
            generation: request.generation,
            request_id: request.request_id,
            path: request.path,
            entries: Ok(vec![]),
            truncated: false
        }));
    assert_eq!(
        app.borrow().sidebar.current_dir,
        PathBuf::from("/manual/chosen")
    )
}
#[test]
fn same_target_rebind_skips_listing_and_preserves_rows() {
    let app = same_target();
    let result = work(&app);
    assert!(result.as_ref().unwrap().listing.is_none());
    finish(&app, result);
    let app = app.borrow();
    assert_eq!(HOME_CALLS.with(Cell::get), 1);
    assert_eq!(LIST_CALLS.with(Cell::get), 0);
    assert_eq!(app.sidebar.current_dir, PathBuf::from("/remote/kept"));
    assert_eq!(app.sidebar.rows[0].name, "kept child");
    assert_eq!(app.sidebar.commits, 0);
    assert_eq!(app.sidebar.rebinds, 1);
    let remote_fs::FsLocation::Transient(p) = &app.sidebar.location else {
        panic!()
    };
    assert_eq!(p.control_path.as_deref(), Some("/run/live.sock"))
}
#[test]
fn same_target_stale_source_cannot_rebind() {
    let app = same_target();
    let source = app.clone();
    on_home(move || source.borrow_mut().sessions[0].command = None);
    let result = work(&app);
    finish(&app, result);
    let app = app.borrow();
    assert_eq!(LIST_CALLS.with(Cell::get), 0);
    assert_eq!(app.sidebar.rebinds, 0);
    let remote_fs::FsLocation::Transient(p) = &app.sidebar.location else {
        panic!()
    };
    assert_eq!(p.control_path.as_deref(), Some("/run/old.sock"));
    assert_eq!(app.mutations, 0)
}
// @files-follow:native-tests
