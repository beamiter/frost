//! Asynchronous, lazily-loaded file-tree sidebar.
//!
//! The UI owns [`Sidebar`] and sends [`DirectoryRequest`] values to a worker
//! task. Only one directory level is read per request, so opening the sidebar or
//! expanding a node never recursively walks the filesystem on the UI thread.
//!
//! A request carries its [`FsLocation`] and a snapshot of the configured
//! remote hosts, so the worker reads either the local disk or, through
//! [`crate::remote_fs`]'s sh probe, an ssh destination / running container —
//! with the generation guard unchanged.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jterm_core::jsh_remote::RemoteHostConfig;

use crate::remote_fs::{self, FsLocation};

pub const MAX_DIRECTORY_SCANS_RUNNING: usize = 2;
pub const MAX_DIRECTORY_SCANS_QUEUED: usize = 64;
pub const MAX_DIRECTORY_SCANS_TOTAL: usize =
    MAX_DIRECTORY_SCANS_RUNNING + MAX_DIRECTORY_SCANS_QUEUED;
pub const MAX_DIRECTORY_SCANS_PER_AUTHORITY: usize = 1;
pub const MAX_DIRECTORY_SCANS_QUEUED_PER_REMOTE_AUTHORITY: usize = 16;
const HIGH_PRIORITY_BURST: usize = 3;
const DIRECTORY_RETRY_BASE: Duration = Duration::from_secs(2);
const DIRECTORY_RETRY_CAP: Duration = Duration::from_secs(60);
const MAX_NAVIGATION_HISTORY: usize = 32;
const MAX_CACHED_ROOTS: usize = 8;
pub const MAX_NAVIGATION_PATH_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryRequestPriority {
    High,
    Lazy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryRequestPhase {
    Queued,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryErrorKind {
    Cancelled,
    NotFound,
    PermissionDenied,
    TimedOut,
    Unavailable,
    InvalidRequest,
    InvalidResponse,
    Busy,
    Internal,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryError {
    pub kind: DirectoryErrorKind,
    pub message: String,
    pub retryable: bool,
}

impl DirectoryError {
    pub fn from_io(error: io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::Interrupted => DirectoryErrorKind::Cancelled,
            io::ErrorKind::NotFound => DirectoryErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => DirectoryErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut => DirectoryErrorKind::TimedOut,
            io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe => DirectoryErrorKind::Unavailable,
            io::ErrorKind::InvalidInput => DirectoryErrorKind::InvalidRequest,
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
                DirectoryErrorKind::InvalidResponse
            }
            io::ErrorKind::WouldBlock => DirectoryErrorKind::Busy,
            _ => DirectoryErrorKind::Other,
        };
        // Do not surface backend stderr, hostnames, usernames, or absolute
        // paths in the tree. The row already supplies directory context and
        // the typed category is enough to choose a useful retry action.
        let message = match kind {
            DirectoryErrorKind::Cancelled => "Directory scan was cancelled",
            DirectoryErrorKind::NotFound => "Directory no longer exists",
            DirectoryErrorKind::PermissionDenied => "Permission denied while reading directory",
            DirectoryErrorKind::TimedOut => "Directory scan timed out",
            DirectoryErrorKind::Unavailable => "Remote filesystem is unavailable",
            DirectoryErrorKind::InvalidRequest => "Directory scan request was rejected",
            DirectoryErrorKind::InvalidResponse => "Remote directory response was invalid",
            DirectoryErrorKind::Busy => "Directory scan is temporarily busy",
            DirectoryErrorKind::Internal => "Directory scan worker stopped unexpectedly",
            DirectoryErrorKind::Other => "Directory scan failed",
        }
        .to_string();
        Self {
            kind,
            message,
            retryable: !matches!(
                kind,
                DirectoryErrorKind::Cancelled
                    | DirectoryErrorKind::InvalidRequest
                    | DirectoryErrorKind::InvalidResponse
            ),
        }
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self {
            kind: DirectoryErrorKind::Busy,
            message: message.into(),
            retryable: true,
        }
    }

    pub fn internal() -> Self {
        Self {
            kind: DirectoryErrorKind::Internal,
            message: "Directory scan worker stopped unexpectedly".to_string(),
            retryable: true,
        }
    }

    #[cfg(test)]
    fn other(message: impl AsRef<str>, retryable: bool) -> Self {
        let message = jterm_core::review_input::safe_inline_display(message.as_ref(), 192);
        Self {
            kind: DirectoryErrorKind::Other,
            message: if message.is_empty() {
                "Directory scan failed".to_string()
            } else {
                message
            },
            retryable,
        }
    }
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Loading lifecycle for a directory node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryState {
    Unloaded,
    Loading,
    /// A last-good snapshot remains visible while a same-root scan runs.
    Refreshing,
    Loaded,
    Error(DirectoryError),
    /// A same-root scan failed; the last-good children remain usable/visible.
    RefreshError(DirectoryError),
}

/// One visible file-tree node.
#[derive(Clone, Debug)]
pub struct FileTreeNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub children: Vec<FileTreeNode>,
    pub expanded: bool,
    pub state: DirectoryState,
    /// The backend proved that this directory has more entries than retained.
    pub truncated: bool,
    /// Completion time of the last accepted snapshot. Failed refreshes retain
    /// it so the UI can state how old the visible last-good children are.
    pub last_loaded_at: Option<Instant>,
}

impl FileTreeNode {
    fn directory(path: PathBuf, expanded: bool) -> Self {
        let name = display_name(&path);
        Self {
            name,
            path,
            is_dir: true,
            children: Vec::new(),
            expanded,
            state: DirectoryState::Unloaded,
            truncated: false,
            last_loaded_at: None,
        }
    }

    fn entry(name: String, path: PathBuf, is_dir: bool) -> Self {
        Self {
            name,
            path,
            is_dir,
            children: Vec::new(),
            expanded: false,
            state: if is_dir {
                DirectoryState::Unloaded
            } else {
                DirectoryState::Loaded
            },
            truncated: false,
            last_loaded_at: None,
        }
    }
}

/// A filesystem request created by [`Sidebar`]. `generation` prevents a slow
/// response for an old cwd from replacing the tree after the user navigates.
/// `request_id` additionally orders same-generation retries of one path.
/// `location` + `hosts` snapshot where the read happens, so a config edit
/// mid-flight cannot redirect an already-issued request to another host. The
/// cancellation token actively retires both queued and running backend work.
#[derive(Clone, Debug)]
pub struct DirectoryRequest {
    pub generation: u64,
    pub request_id: u64,
    pub path: PathBuf,
    pub location: FsLocation,
    pub hosts: Vec<RemoteHostConfig>,
    pub show_hidden: bool,
    pub priority: DirectoryRequestPriority,
    /// Explicit, user-invoked Retry bypasses one active cooldown. The flag is
    /// immutable and consumed with this request; follow-up automatic work is
    /// throttled normally again.
    pub bypass_cooldown: bool,
    pub cancellation: std::sync::Arc<remote_fs::CancellationToken>,
}

/// Worker result consumed by [`Sidebar::apply_load`].
#[derive(Clone, Debug)]
pub struct DirectoryResult {
    pub generation: u64,
    pub request_id: u64,
    pub path: PathBuf,
    pub entries: Result<Vec<FileTreeNode>, DirectoryError>,
    pub truncated: bool,
}

impl DirectoryResult {
    pub fn failed(request: DirectoryRequest, error: DirectoryError) -> Self {
        Self {
            generation: request.generation,
            request_id: request.request_id,
            path: request.path,
            entries: Err(error),
            truncated: false,
        }
    }
}

#[derive(Clone, Debug)]
struct ActiveDirectoryRequest {
    request_id: u64,
    phase: DirectoryRequestPhase,
    cancellation: std::sync::Arc<remote_fs::CancellationToken>,
}

/// Bounded, UI-owned scan coordinator. It never runs filesystem code itself;
/// it only admits, merges, prioritizes, and accounts for immutable requests.
#[derive(Clone, Debug)]
struct QueuedDirectoryRequest {
    request: DirectoryRequest,
    enqueued_at: Instant,
}

impl QueuedDirectoryRequest {
    fn authority(&self) -> remote_fs::FilesAuthorityKey {
        remote_fs::files_authority_key(&self.request.location, &self.request.hosts)
    }
}

#[derive(Clone, Debug)]
struct RunningDirectoryScan {
    authority: remote_fs::FilesAuthorityKey,
    path: PathBuf,
    enqueued_at: Instant,
    started_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CooldownKey {
    Authority(remote_fs::FilesAuthorityKey),
    Path(remote_fs::FilesAuthorityKey, PathBuf),
}

#[derive(Clone, Copy, Debug)]
struct CooldownState {
    failures: u32,
    until: Instant,
}

/// Queue and execution latency of the last terminal worker result. Authority
/// identity stays opaque; diagnostics expose only durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryScanTiming {
    pub queued_for: Duration,
    pub ran_for: Duration,
}

/// Bounded, UI-owned scan coordinator. It never runs filesystem code itself;
/// it only admits, merges, prioritizes, throttles, and accounts for immutable
/// requests. One authority may occupy at most one of the two global slots, and
/// one remote authority may queue only a bounded share of global admission, so
/// an offline destination cannot starve Local or another remote.
#[derive(Debug, Default)]
pub struct DirectoryScanCoordinator {
    high: VecDeque<QueuedDirectoryRequest>,
    lazy: VecDeque<QueuedDirectoryRequest>,
    running: HashMap<(u64, u64), RunningDirectoryScan>,
    running_by_authority: HashMap<remote_fs::FilesAuthorityKey, usize>,
    cooldowns: HashMap<CooldownKey, CooldownState>,
    high_streak: usize,
    last_dispatched_authority: Option<remote_fs::FilesAuthorityKey>,
    last_timing: Option<DirectoryScanTiming>,
}

impl DirectoryScanCoordinator {
    pub fn enqueue(&mut self, request: DirectoryRequest) -> Vec<DirectoryRequest> {
        self.enqueue_at(request, Instant::now())
    }

    pub fn enqueue_at(
        &mut self,
        request: DirectoryRequest,
        enqueued_at: Instant,
    ) -> Vec<DirectoryRequest> {
        self.compact_cancelled();
        let authority = remote_fs::files_authority_key(&request.location, &request.hosts);
        self.remove_queued_path(authority, &request.path);

        let mut rejected = Vec::new();
        let remote_authority_full = request.location.is_remote()
            && self.queued_for_authority(authority)
                >= MAX_DIRECTORY_SCANS_QUEUED_PER_REMOTE_AUTHORITY;
        if remote_authority_full {
            if request.priority == DirectoryRequestPriority::High {
                if let Some(evicted) = pop_back_for_authority(&mut self.lazy, authority) {
                    rejected.push(evicted.request);
                } else {
                    rejected.push(request);
                    return rejected;
                }
            } else {
                rejected.push(request);
                return rejected;
            }
        }

        let queue_full = self.queued_len() >= MAX_DIRECTORY_SCANS_QUEUED;
        let total_full = self.pending_len() >= MAX_DIRECTORY_SCANS_TOTAL;
        if queue_full || total_full {
            if request.priority == DirectoryRequestPriority::High {
                if let Some(evicted) = self.lazy.pop_back() {
                    rejected.push(evicted.request);
                } else {
                    rejected.push(request);
                    return rejected;
                }
            } else {
                rejected.push(request);
                return rejected;
            }
        }

        let queued = QueuedDirectoryRequest {
            request,
            enqueued_at,
        };
        match queued.request.priority {
            DirectoryRequestPriority::High => self.high.push_back(queued),
            DirectoryRequestPriority::Lazy => self.lazy.push_back(queued),
        }
        self.compact_expired_cooldowns(enqueued_at);
        rejected
    }

    pub fn take_ready(&mut self) -> Vec<DirectoryRequest> {
        self.take_ready_at(Instant::now())
    }

    pub fn take_ready_at(&mut self, now: Instant) -> Vec<DirectoryRequest> {
        self.compact_cancelled();
        self.compact_expired_cooldowns(now);
        let mut ready = Vec::new();
        while self.running.len() < MAX_DIRECTORY_SCANS_RUNNING {
            let high_ready =
                queue_has_ready(&self.high, &self.running_by_authority, &self.cooldowns, now);
            let lazy_ready =
                queue_has_ready(&self.lazy, &self.running_by_authority, &self.cooldowns, now);
            let prefer_high = high_ready && (!lazy_ready || self.high_streak < HIGH_PRIORITY_BURST);
            let queued = if prefer_high {
                pop_ready(
                    &mut self.high,
                    &self.running_by_authority,
                    &self.cooldowns,
                    now,
                    self.last_dispatched_authority,
                )
                .or_else(|| {
                    pop_ready(
                        &mut self.lazy,
                        &self.running_by_authority,
                        &self.cooldowns,
                        now,
                        self.last_dispatched_authority,
                    )
                })
            } else {
                pop_ready(
                    &mut self.lazy,
                    &self.running_by_authority,
                    &self.cooldowns,
                    now,
                    self.last_dispatched_authority,
                )
                .or_else(|| {
                    pop_ready(
                        &mut self.high,
                        &self.running_by_authority,
                        &self.cooldowns,
                        now,
                        self.last_dispatched_authority,
                    )
                })
            };
            let Some(queued) = queued else {
                break;
            };
            if queued.request.cancellation.is_cancelled() {
                continue;
            }
            if queued.request.priority == DirectoryRequestPriority::High {
                self.high_streak = self.high_streak.saturating_add(1);
            } else {
                self.high_streak = 0;
            }
            let authority = queued.authority();
            self.last_dispatched_authority = Some(authority);
            *self.running_by_authority.entry(authority).or_default() += 1;
            self.running.insert(
                (queued.request.generation, queued.request.request_id),
                RunningDirectoryScan {
                    authority,
                    path: queued.request.path.clone(),
                    enqueued_at: queued.enqueued_at,
                    started_at: now,
                },
            );
            ready.push(queued.request);
        }
        ready
    }

    pub fn finish(&mut self, generation: u64, request_id: u64) -> bool {
        self.finish_at(generation, request_id, Instant::now())
            .is_some()
    }

    pub fn finish_at(
        &mut self,
        generation: u64,
        request_id: u64,
        now: Instant,
    ) -> Option<DirectoryScanTiming> {
        let running = self.remove_running(generation, request_id)?;
        let timing = DirectoryScanTiming {
            queued_for: running
                .started_at
                .saturating_duration_since(running.enqueued_at),
            ran_for: now.saturating_duration_since(running.started_at),
        };
        self.last_timing = Some(timing);
        self.compact_expired_cooldowns(now);
        Some(timing)
    }

    /// Finish one worker and update classified cooldown state. Transport-wide
    /// outages back off the whole authority; path-local permission/not-found
    /// failures throttle only that exact directory. Any success proves the
    /// authority reachable and clears both relevant buckets.
    pub fn finish_result(&mut self, result: &DirectoryResult) -> Option<DirectoryScanTiming> {
        self.finish_result_at(result, Instant::now())
    }

    pub fn finish_result_at(
        &mut self,
        result: &DirectoryResult,
        now: Instant,
    ) -> Option<DirectoryScanTiming> {
        self.compact_expired_cooldowns(now);
        let running = self
            .running
            .get(&(result.generation, result.request_id))
            .cloned()?;
        match &result.entries {
            Ok(_) => {
                self.cooldowns
                    .remove(&CooldownKey::Authority(running.authority));
                self.cooldowns
                    .remove(&CooldownKey::Path(running.authority, result.path.clone()));
            }
            Err(error) => {
                if let Some(key) = cooldown_key(running.authority, &result.path, error) {
                    let previous = self.cooldowns.get(&key).copied();
                    let failures = previous.map_or(1, |state| state.failures.saturating_add(1));
                    let exponent = failures.saturating_sub(1).min(5);
                    let seconds = DIRECTORY_RETRY_BASE
                        .as_secs()
                        .saturating_mul(1_u64 << exponent)
                        .min(DIRECTORY_RETRY_CAP.as_secs());
                    self.cooldowns.insert(
                        key,
                        CooldownState {
                            failures,
                            until: now + Duration::from_secs(seconds),
                        },
                    );
                }
            }
        }
        self.finish_at(result.generation, result.request_id, now)
    }

    pub fn queued_len(&self) -> usize {
        self.high
            .iter()
            .chain(self.lazy.iter())
            .filter(|queued| !queued.request.cancellation.is_cancelled())
            .count()
    }

    fn queued_for_authority(&self, authority: remote_fs::FilesAuthorityKey) -> usize {
        self.high
            .iter()
            .chain(self.lazy.iter())
            .filter(|queued| {
                !queued.request.cancellation.is_cancelled() && queued.authority() == authority
            })
            .count()
    }

    pub fn running_len(&self) -> usize {
        self.running.len()
    }

    pub fn pending_len(&self) -> usize {
        self.queued_len() + self.running_len()
    }

    pub fn last_timing(&self) -> Option<DirectoryScanTiming> {
        self.last_timing
    }

    pub fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.high
            .iter()
            .chain(self.lazy.iter())
            .filter(|queued| !queued.request.cancellation.is_cancelled())
            .map(|queued| now.saturating_duration_since(queued.enqueued_at))
            .max()
    }

    fn remove_running(&mut self, generation: u64, request_id: u64) -> Option<RunningDirectoryScan> {
        let running = self.running.remove(&(generation, request_id))?;
        if let Some(count) = self.running_by_authority.get_mut(&running.authority) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.running_by_authority.remove(&running.authority);
            }
        }
        Some(running)
    }

    fn compact_cancelled(&mut self) {
        self.high
            .retain(|queued| !queued.request.cancellation.is_cancelled());
        self.lazy
            .retain(|queued| !queued.request.cancellation.is_cancelled());
    }

    /// Drop elapsed cooldown buckets once no queued/running request can still
    /// turn that bucket into the next exponential-backoff step. Retaining a
    /// bucket while its retry is live preserves failure history; abandoned
    /// unique paths cannot grow this map forever.
    fn compact_expired_cooldowns(&mut self, now: Instant) {
        let high = &self.high;
        let lazy = &self.lazy;
        let running = &self.running;
        self.cooldowns.retain(|key, state| {
            if state.until > now {
                return true;
            }
            let queued_matches = |authority, path: Option<&Path>| {
                high.iter().chain(lazy.iter()).any(|queued| {
                    !queued.request.cancellation.is_cancelled()
                        && queued.authority() == authority
                        && path.is_none_or(|path| queued.request.path == path)
                })
            };
            let running_matches = |authority, path: Option<&Path>| {
                running.values().any(|scan| {
                    scan.authority == authority && path.is_none_or(|path| scan.path == path)
                })
            };
            match key {
                CooldownKey::Authority(authority) => {
                    queued_matches(*authority, None) || running_matches(*authority, None)
                }
                CooldownKey::Path(authority, path) => {
                    queued_matches(*authority, Some(path))
                        || running_matches(*authority, Some(path))
                }
            }
        });
    }

    fn remove_queued_path(&mut self, authority: remote_fs::FilesAuthorityKey, path: &Path) {
        self.high.retain(|queued| {
            let keep = queued.authority() != authority || queued.request.path.as_path() != path;
            if !keep {
                queued.request.cancellation.cancel();
            }
            keep
        });
        self.lazy.retain(|queued| {
            let keep = queued.authority() != authority || queued.request.path.as_path() != path;
            if !keep {
                queued.request.cancellation.cancel();
            }
            keep
        });
    }
}

fn pop_back_for_authority(
    queue: &mut VecDeque<QueuedDirectoryRequest>,
    authority: remote_fs::FilesAuthorityKey,
) -> Option<QueuedDirectoryRequest> {
    let index = queue
        .iter()
        .rposition(|queued| queued.authority() == authority)?;
    queue.remove(index)
}

fn cooldown_key(
    authority: remote_fs::FilesAuthorityKey,
    path: &Path,
    error: &DirectoryError,
) -> Option<CooldownKey> {
    if !error.retryable {
        return None;
    }
    match error.kind {
        DirectoryErrorKind::TimedOut | DirectoryErrorKind::Unavailable => {
            Some(CooldownKey::Authority(authority))
        }
        DirectoryErrorKind::NotFound
        | DirectoryErrorKind::PermissionDenied
        | DirectoryErrorKind::Internal
        | DirectoryErrorKind::Other => Some(CooldownKey::Path(authority, path.to_path_buf())),
        DirectoryErrorKind::Cancelled
        | DirectoryErrorKind::InvalidRequest
        | DirectoryErrorKind::InvalidResponse
        | DirectoryErrorKind::Busy => None,
    }
}

fn queue_has_ready(
    queue: &VecDeque<QueuedDirectoryRequest>,
    running_by_authority: &HashMap<remote_fs::FilesAuthorityKey, usize>,
    cooldowns: &HashMap<CooldownKey, CooldownState>,
    now: Instant,
) -> bool {
    queue
        .iter()
        .any(|queued| queue_entry_is_ready(queued, running_by_authority, cooldowns, now))
}

fn pop_ready(
    queue: &mut VecDeque<QueuedDirectoryRequest>,
    running_by_authority: &HashMap<remote_fs::FilesAuthorityKey, usize>,
    cooldowns: &HashMap<CooldownKey, CooldownState>,
    now: Instant,
    avoid_authority: Option<remote_fs::FilesAuthorityKey>,
) -> Option<QueuedDirectoryRequest> {
    let index = queue
        .iter()
        .position(|queued| {
            queue_entry_is_ready(queued, running_by_authority, cooldowns, now)
                && Some(queued.authority()) != avoid_authority
        })
        .or_else(|| {
            queue.iter().position(|queued| {
                queue_entry_is_ready(queued, running_by_authority, cooldowns, now)
            })
        })?;
    queue.remove(index)
}

fn queue_entry_is_ready(
    queued: &QueuedDirectoryRequest,
    running_by_authority: &HashMap<remote_fs::FilesAuthorityKey, usize>,
    cooldowns: &HashMap<CooldownKey, CooldownState>,
    now: Instant,
) -> bool {
    if queued.request.cancellation.is_cancelled() {
        return false;
    }
    let authority = queued.authority();
    if running_by_authority.get(&authority).copied().unwrap_or(0)
        >= MAX_DIRECTORY_SCANS_PER_AUTHORITY
    {
        return false;
    }
    if queued.request.bypass_cooldown {
        return true;
    }
    let authority_until = cooldowns
        .get(&CooldownKey::Authority(authority))
        .map(|state| state.until);
    let path_until = cooldowns
        .get(&CooldownKey::Path(authority, queued.request.path.clone()))
        .map(|state| state.until);
    authority_until
        .into_iter()
        .chain(path_until)
        .all(|until| until <= now)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavigationHistoryAction {
    Push,
    Back,
    Forward,
    Location,
}

#[derive(Clone, Debug)]
struct PendingNavigation {
    generation: u64,
    request_id: u64,
    target: PathBuf,
    action: NavigationHistoryAction,
    location: FsLocation,
    home_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct PendingLocationChange {
    generation: u64,
    location: FsLocation,
}

#[derive(Clone, Debug)]
pub struct NavigationFailure {
    pub error: DirectoryError,
}

#[derive(Clone, Debug)]
struct CachedRoot {
    authority: remote_fs::FilesAuthorityKey,
    path: PathBuf,
    show_hidden: bool,
    root: FileTreeNode,
}

/// File-sidebar state.
#[derive(Clone, Debug)]
pub struct Sidebar {
    pub current_dir: PathBuf,
    pub root: FileTreeNode,
    /// Stable start directory for the selected location. Remote navigation
    /// uses the successfully probed login home without spawning another probe.
    home_dir: Option<PathBuf>,
    /// Where the tree is rooted: this machine or one of `hosts`.
    pub location: FsLocation,
    /// Snapshot of `config.remote_hosts`, kept in sync by the UI; indices in
    /// [`FsLocation::Remote`] resolve against it.
    hosts: Vec<RemoteHostConfig>,
    generation: u64,
    next_request_id: u64,
    active_requests: HashMap<PathBuf, ActiveDirectoryRequest>,
    show_hidden: bool,
    pending_navigation: Option<PendingNavigation>,
    pending_location_change: Option<PendingLocationChange>,
    navigation_failure: Option<NavigationFailure>,
    back_history: VecDeque<PathBuf>,
    forward_history: VecDeque<PathBuf>,
    cached_roots: VecDeque<CachedRoot>,
}

impl Sidebar {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            root: FileTreeNode::directory(current_dir.clone(), true),
            home_dir: Some(current_dir.clone()),
            current_dir,
            location: FsLocation::Local,
            hosts: Vec::new(),
            generation: 0,
            next_request_id: 0,
            active_requests: HashMap::new(),
            show_hidden: false,
            pending_navigation: None,
            pending_location_change: None,
            navigation_failure: None,
            back_history: VecDeque::new(),
            forward_history: VecDeque::new(),
            cached_roots: VecDeque::new(),
        }
    }

    /// Replace the remote-host snapshot (called when the config changes).
    ///
    /// A staged location switch contains a numeric index interpreted against
    /// the previous snapshot. Retire both its home-probe phase and its candidate
    /// root phase before installing a different vector, so an old home/path can
    /// never be re-issued through the profile that later occupies that slot.
    pub fn set_hosts(&mut self, hosts: Vec<RemoteHostConfig>) {
        if self.hosts != hosts
            && (self.pending_location_change.is_some()
                || self
                    .pending_navigation
                    .as_ref()
                    .is_some_and(|pending| pending.action == NavigationHistoryAction::Location))
        {
            self.advance_generation();
            retire_in_flight(&mut self.root);
        }
        self.hosts = hosts;
    }

    /// The snapshot every request and file operation must travel with.
    pub fn hosts_snapshot(&self) -> &[RemoteHostConfig] {
        &self.hosts
    }

    /// Begin staging a switch to `location`. The generation bump retires old
    /// loads, but the accepted location/root remain visible. The asynchronously
    /// resolved home and its candidate listing commit together through
    /// [`Sidebar::resolve_location`] + [`Sidebar::apply_load_at`].
    pub fn begin_location_change(&mut self, location: FsLocation) -> u64 {
        self.advance_generation();
        self.navigation_failure = None;
        retire_in_flight(&mut self.root);
        self.pending_location_change = Some(PendingLocationChange {
            generation: self.generation,
            location,
        });
        self.generation
    }

    /// Whether an asynchronous location/home probe still belongs to the
    /// currently selected tree. Callers use this before turning a failed
    /// remote probe into a Local fallback; a late failure from an older
    /// selection must not pull a newer tree back to this machine.
    pub fn accepts_generation(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// Stamp non-load work (for example an off-thread drop preflight) with the
    /// same tree identity used by directory requests.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn home_dir(&self) -> Option<&Path> {
        self.home_dir.as_deref()
    }

    /// Stage a root change without mutating the accepted tree. The candidate's
    /// one-level scan must succeed before [`apply_load_at`](Self::apply_load_at)
    /// atomically swaps roots. A newer candidate cancels the older one while
    /// ordinary in-tree loads may continue until a commit advances generation.
    pub fn begin_navigation(&mut self, target: PathBuf) -> Option<DirectoryRequest> {
        self.begin_navigation_with_action(target, NavigationHistoryAction::Push)
    }

    pub fn navigate_back(&mut self) -> Option<DirectoryRequest> {
        let target = self.back_history.back()?.clone();
        self.begin_navigation_with_action(target, NavigationHistoryAction::Back)
    }

    pub fn navigate_forward(&mut self) -> Option<DirectoryRequest> {
        let target = self.forward_history.back()?.clone();
        self.begin_navigation_with_action(target, NavigationHistoryAction::Forward)
    }

    pub fn can_navigate_back(&self) -> bool {
        !self.back_history.is_empty()
    }

    pub fn can_navigate_forward(&self) -> bool {
        !self.forward_history.is_empty()
    }

    pub fn navigation_pending_target(&self) -> Option<&Path> {
        self.pending_navigation
            .as_ref()
            .map(|pending| pending.target.as_path())
    }

    pub fn location_change_pending(&self) -> bool {
        self.pending_location_change.is_some()
    }

    pub fn navigation_failure(&self) -> Option<&NavigationFailure> {
        self.navigation_failure.as_ref()
    }

    pub fn is_pending_navigation_result(
        &self,
        generation: u64,
        request_id: u64,
        path: &Path,
    ) -> bool {
        self.pending_navigation.as_ref().is_some_and(|pending| {
            pending.generation == generation
                && pending.request_id == request_id
                && pending.target == path
        })
    }

    fn begin_navigation_with_action(
        &mut self,
        target: PathBuf,
        action: NavigationHistoryAction,
    ) -> Option<DirectoryRequest> {
        if target == self.current_dir {
            return None;
        }
        self.pending_location_change = None;
        self.cancel_pending_navigation();
        self.navigation_failure = None;
        let request = self.request_for(target.clone(), DirectoryRequestPriority::High);
        self.pending_navigation = Some(PendingNavigation {
            generation: request.generation,
            request_id: request.request_id,
            target,
            action,
            location: self.location.clone(),
            home_dir: None,
        });
        Some(request)
    }

    /// Return up to `limit` oldest visible stale directory snapshots. Root is
    /// visible whenever Files is visible; descendants qualify only through an
    /// expanded ancestor. Loads and failed snapshots are not auto-retried.
    pub fn refresh_stale_visible(
        &mut self,
        now: Instant,
        stale_after: Duration,
        limit: usize,
    ) -> Vec<DirectoryRequest> {
        let mut candidates = Vec::new();
        collect_stale_visible_directories(&self.root, true, now, stale_after, &mut candidates);
        candidates.sort_by_key(|(_, loaded_at)| *loaded_at);
        candidates
            .into_iter()
            .take(limit)
            .filter_map(|(path, _)| {
                self.refresh_directory_with_priority(&path, DirectoryRequestPriority::Lazy)
            })
            .collect()
    }

    /// Invalidate only cached directory snapshots a completed operation could
    /// have changed. The currently visible tree is refreshed separately by the
    /// caller; cached roots are authority-bound and never crossed.
    pub fn invalidate_cached_directories(&mut self, directories: &[PathBuf]) {
        let authority = self.authority();
        for cached in &mut self.cached_roots {
            if cached.authority != authority {
                continue;
            }
            for directory in directories {
                if directory == &cached.root.path {
                    // A future candidate scan always refreshes the root's one
                    // level and can still reuse healthy descendant caches.
                    continue;
                }
                let Some(node) = find_node_mut(&mut cached.root, directory) else {
                    continue;
                };
                if !node.is_dir {
                    continue;
                }
                node.children.clear();
                node.state = DirectoryState::Unloaded;
                node.truncated = false;
                node.last_loaded_at = None;
            }
        }
    }

    fn authority(&self) -> remote_fs::FilesAuthorityKey {
        remote_fs::files_authority_key(&self.location, &self.hosts)
    }

    /// Whether `path` still names a row in the accepted tree snapshot. The UI
    /// uses this after a preserving refresh to retire selection and delayed
    /// actions whose targets disappeared during reconciliation.
    pub fn contains_path(&self, path: &Path) -> bool {
        find_node(&self.root, path).is_some()
    }

    /// The accepted row kind for `path`, used to invalidate a menu/hover when
    /// reconciliation keeps the text path but changes directory ↔ file type.
    pub fn node_is_dir(&self, path: &Path) -> Option<bool> {
        find_node(&self.root, path).map(|node| node.is_dir)
    }

    /// Whether the accepted row is retaining a last-good snapshot while its
    /// exact-path replacement request runs.
    pub fn node_is_refreshing(&self, path: &Path) -> bool {
        find_node(&self.root, path).is_some_and(|node| node.state == DirectoryState::Refreshing)
    }

    pub fn request_phase(&self, path: &Path) -> Option<DirectoryRequestPhase> {
        self.active_requests.get(path).map(|request| request.phase)
    }

    /// Move one exact admitted request from Queued to Running. A request that
    /// became stale while waiting is refused and must not consume a worker.
    pub fn mark_request_running(&mut self, request: &DirectoryRequest) -> bool {
        if request.generation != self.generation {
            return false;
        }
        let Some(active) = self.active_requests.get_mut(&request.path) else {
            return false;
        };
        if active.request_id != request.request_id || request.cancellation.is_cancelled() {
            return false;
        }
        active.phase = DirectoryRequestPhase::Running;
        true
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Change the dotfile policy under a new generation. The last-good root
    /// remains visible while an old-policy result is made stale and replaced.
    pub fn set_show_hidden(&mut self, show_hidden: bool) -> Option<DirectoryRequest> {
        if self.show_hidden == show_hidden {
            return None;
        }
        self.show_hidden = show_hidden;
        Some(self.refresh())
    }

    /// Atomically cache the accepted root under the *old* host snapshot, install
    /// a replacement snapshot, and fall back to Local. This ordering matters
    /// when a config edit reuses `Remote(index)` for another host: caching after
    /// the replacement would label the old host's loaded descendants with the
    /// new host's authority key.
    pub fn reset_to_local_with_hosts(
        &mut self,
        path: PathBuf,
        hosts: Vec<RemoteHostConfig>,
    ) -> DirectoryRequest {
        self.cache_current_root();
        self.location = FsLocation::Local;
        self.hosts = hosts;
        self.home_dir = Some(path.clone());
        self.set_current_dir(path)
    }

    /// A proven-identical remote profile moved to another config slot. Keep
    /// its path, but invalidate the old index-stamped load and immediately
    /// issue a replacement against the new host snapshot.
    pub fn rebind_location_and_refresh(&mut self, location: FsLocation) -> DirectoryRequest {
        self.location = location;
        self.refresh()
    }

    /// Swap only the execution route after the caller has proved it names the
    /// same remote filesystem and successfully probed the replacement route.
    /// Loaded rows/root/expansion stay visible; old in-flight node loads are
    /// retired by the generation bump and can be explicitly reopened.
    pub fn rebind_same_namespace_preserving_tree(&mut self, location: FsLocation) -> bool {
        if !self.has_loaded_snapshot() {
            return false;
        }

        self.advance_generation();
        self.location = location;
        retire_in_flight(&mut self.root);
        true
    }

    /// Whether the root owns a last-good directory snapshot. Refreshing and a
    /// failed refresh both retain that snapshot even though their latest scan
    /// has not completed successfully.
    pub fn has_loaded_snapshot(&self) -> bool {
        matches!(
            self.root.state,
            DirectoryState::Loaded | DirectoryState::Refreshing | DirectoryState::RefreshError(_)
        )
    }

    /// Apply an asynchronously resolved start directory for a staged location
    /// change. Success starts a candidate scan through the new route; neither
    /// the location nor accepted tree changes until that scan succeeds.
    /// Failure leaves the old location/tree intact and records safe feedback.
    pub fn resolve_location(
        &mut self,
        generation: u64,
        start: Result<PathBuf, DirectoryError>,
    ) -> Option<DirectoryRequest> {
        let pending_location = self.pending_location_change.as_ref()?;
        if generation != self.generation || pending_location.generation != generation {
            return None;
        }
        let pending_location = self
            .pending_location_change
            .take()
            .expect("validated pending location");
        match start {
            Ok(dir) => {
                self.cancel_pending_navigation();
                let request = self.request_for_location(
                    dir.clone(),
                    DirectoryRequestPriority::High,
                    false,
                    pending_location.location.clone(),
                );
                self.pending_navigation = Some(PendingNavigation {
                    generation: request.generation,
                    request_id: request.request_id,
                    target: dir.clone(),
                    action: NavigationHistoryAction::Location,
                    location: pending_location.location,
                    home_dir: Some(dir),
                });
                Some(request)
            }
            Err(error) => {
                self.navigation_failure = Some(NavigationFailure { error });
                None
            }
        }
    }

    /// Point the tree at a new root and return the one-level load request.
    pub fn set_current_dir(&mut self, path: PathBuf) -> DirectoryRequest {
        self.advance_generation();
        self.back_history.clear();
        self.forward_history.clear();
        self.navigation_failure = None;
        self.current_dir = path.clone();
        self.root = FileTreeNode::directory(path, true);
        self.begin_load_root()
    }

    /// Load the initial root without changing its generation.
    pub fn begin_load_root(&mut self) -> DirectoryRequest {
        self.root.state = DirectoryState::Loading;
        self.request_for(self.root.path.clone(), DirectoryRequestPriority::High)
    }

    /// A request for one directory level, stamped with the current
    /// generation, location, and host snapshot.
    fn request_for(
        &mut self,
        path: PathBuf,
        priority: DirectoryRequestPriority,
    ) -> DirectoryRequest {
        self.request_for_options(path, priority, false)
    }

    fn request_for_options(
        &mut self,
        path: PathBuf,
        priority: DirectoryRequestPriority,
        bypass_cooldown: bool,
    ) -> DirectoryRequest {
        self.request_for_location(path, priority, bypass_cooldown, self.location.clone())
    }

    fn request_for_location(
        &mut self,
        path: PathBuf,
        priority: DirectoryRequestPriority,
        bypass_cooldown: bool,
        location: FsLocation,
    ) -> DirectoryRequest {
        if self
            .pending_navigation
            .as_ref()
            .is_some_and(|pending| pending.target == path)
        {
            self.cancel_pending_navigation();
        }
        if let Some(previous) = self.active_requests.remove(&path) {
            previous.cancellation.cancel();
        }
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let request_id = self.next_request_id;
        let cancellation = remote_fs::CancellationToken::new();
        self.active_requests.insert(
            path.clone(),
            ActiveDirectoryRequest {
                request_id,
                phase: DirectoryRequestPhase::Queued,
                cancellation: cancellation.clone(),
            },
        );
        DirectoryRequest {
            generation: self.generation,
            request_id,
            path,
            location,
            hosts: self.hosts.clone(),
            show_hidden: self.show_hidden,
            priority,
            bypass_cooldown,
            cancellation,
        }
    }

    /// Toggle a directory and, when necessary, request its first one-level load.
    pub fn toggle_node(&mut self, path: &Path) -> Option<DirectoryRequest> {
        let request_path = {
            let node = find_node_mut(&mut self.root, path)?;
            if !node.is_dir {
                return None;
            }
            match node.state {
                DirectoryState::Unloaded | DirectoryState::Error(_) => {
                    node.expanded = true;
                    node.state = DirectoryState::Loading;
                    Some(node.path.clone())
                }
                DirectoryState::Loading
                | DirectoryState::Refreshing
                | DirectoryState::Loaded
                | DirectoryState::RefreshError(_) => {
                    node.expanded = !node.expanded;
                    None
                }
            }
        };
        request_path.map(|path| self.request_for(path, DirectoryRequestPriority::Lazy))
    }

    /// Retry a failed directory in place. An initial-load error returns to
    /// `Loading`; a failed preserving refresh returns to `Refreshing` without
    /// clearing its last-good children, expansion, or truncation marker.
    pub fn retry_node(&mut self, path: &Path) -> Option<DirectoryRequest> {
        let request_path = {
            let node = find_node_mut(&mut self.root, path)?;
            if !node.is_dir {
                return None;
            }
            match node.state {
                DirectoryState::Error(_) => {
                    node.expanded = true;
                    node.state = DirectoryState::Loading;
                    Some(node.path.clone())
                }
                DirectoryState::RefreshError(_) => {
                    node.state = DirectoryState::Refreshing;
                    Some(node.path.clone())
                }
                _ => None,
            }
        };
        request_path
            .map(|path| self.request_for_options(path, DirectoryRequestPriority::High, true))
    }

    /// Invalidate outstanding responses and reload the current root. A
    /// last-good snapshot stays visible while the replacement is in flight;
    /// first loads and retries after an initial failure keep the original
    /// empty-tree Loading behavior.
    pub fn refresh(&mut self) -> DirectoryRequest {
        let preserve_snapshot = self.has_loaded_snapshot();
        self.advance_generation();
        // Every descendant load was stamped with the previous generation and
        // can no longer apply. Make those nodes explicitly reopenable instead
        // of leaving permanent Loading indicators in the retained snapshot.
        for child in &mut self.root.children {
            retire_in_flight(child);
        }
        self.root.state = if preserve_snapshot {
            DirectoryState::Refreshing
        } else {
            DirectoryState::Loading
        };
        self.request_for(self.root.path.clone(), DirectoryRequestPriority::High)
    }

    /// Refresh one directory without invalidating unrelated loads or throwing
    /// away descendants. Explicitly refreshing an unloaded/collapsed directory
    /// warms its cache so a mutation made through that row is immediately
    /// reflected when it is next expanded.
    pub fn refresh_directory(&mut self, path: &Path) -> Option<DirectoryRequest> {
        self.refresh_directory_with_priority(path, DirectoryRequestPriority::High)
    }

    fn refresh_directory_with_priority(
        &mut self,
        path: &Path,
        priority: DirectoryRequestPriority,
    ) -> Option<DirectoryRequest> {
        let request_path = {
            let node = find_node_mut(&mut self.root, path)?;
            if !node.is_dir {
                return None;
            }
            node.state = match node.state {
                DirectoryState::Loaded
                | DirectoryState::Refreshing
                | DirectoryState::RefreshError(_) => DirectoryState::Refreshing,
                DirectoryState::Loading | DirectoryState::Error(_) => DirectoryState::Loading,
                DirectoryState::Unloaded => DirectoryState::Loading,
            };
            node.path.clone()
        };
        Some(self.request_for(request_path, priority))
    }

    /// Apply a worker response. Returns `false` for stale or unknown responses.
    pub fn apply_load(&mut self, result: DirectoryResult) -> bool {
        self.apply_load_at(result, Instant::now())
    }

    pub fn apply_load_at(&mut self, result: DirectoryResult, completed_at: Instant) -> bool {
        if result.generation != self.generation {
            return false;
        }
        if self
            .active_requests
            .get(&result.path)
            .map(|request| request.request_id)
            != Some(result.request_id)
        {
            return false;
        }
        self.active_requests.remove(&result.path);
        let pending_navigation = self.pending_navigation.as_ref().and_then(|pending| {
            (pending.generation == result.generation
                && pending.request_id == result.request_id
                && pending.target == result.path)
                .then(|| pending.clone())
        });
        if let Some(pending) = pending_navigation {
            match result.entries {
                Ok(entries) => {
                    self.commit_navigation(pending, entries, result.truncated, completed_at);
                }
                Err(error) => {
                    self.pending_navigation = None;
                    self.navigation_failure = Some(NavigationFailure { error });
                }
            }
            return true;
        }
        let Some(node) = find_node_mut(&mut self.root, &result.path) else {
            return false;
        };
        match result.entries {
            Ok(entries) => {
                if node.state == DirectoryState::Refreshing {
                    reconcile_children(&mut node.children, entries);
                } else {
                    node.children = entries;
                }
                node.state = DirectoryState::Loaded;
                node.truncated = result.truncated;
                node.last_loaded_at = Some(completed_at);
            }
            Err(error) => {
                if node.state == DirectoryState::Refreshing {
                    node.state = DirectoryState::RefreshError(error);
                } else {
                    node.children.clear();
                    node.state = DirectoryState::Error(error);
                    node.truncated = false;
                }
            }
        }
        true
    }

    fn commit_navigation(
        &mut self,
        pending: PendingNavigation,
        entries: Vec<FileTreeNode>,
        truncated: bool,
        completed_at: Instant,
    ) {
        let previous_dir = self.current_dir.clone();
        self.cache_current_root();
        let candidate_authority = remote_fs::files_authority_key(&pending.location, &self.hosts);
        let cached = self.take_cached_root(candidate_authority, &pending.target, self.show_hidden);

        // Commit is the only point that retires work belonging to the old
        // root. The candidate result has already been fully validated.
        self.advance_generation();
        let mut root = cached.map_or_else(
            || FileTreeNode::directory(pending.target.clone(), true),
            |cached| cached.root,
        );
        reconcile_children(&mut root.children, entries);
        root.name = display_name(&pending.target);
        root.path = pending.target.clone();
        root.is_dir = true;
        root.expanded = true;
        root.state = DirectoryState::Loaded;
        root.truncated = truncated;
        root.last_loaded_at = Some(completed_at);
        self.location = pending.location;
        if let Some(home_dir) = pending.home_dir {
            self.home_dir = Some(home_dir);
        }
        self.current_dir = pending.target.clone();
        self.root = root;
        self.navigation_failure = None;

        match pending.action {
            NavigationHistoryAction::Push => {
                push_navigation_history(&mut self.back_history, previous_dir);
                self.forward_history.clear();
            }
            NavigationHistoryAction::Back => {
                if self.back_history.back() == Some(&pending.target) {
                    self.back_history.pop_back();
                    push_navigation_history(&mut self.forward_history, previous_dir);
                }
            }
            NavigationHistoryAction::Forward => {
                if self.forward_history.back() == Some(&pending.target) {
                    self.forward_history.pop_back();
                    push_navigation_history(&mut self.back_history, previous_dir);
                }
            }
            NavigationHistoryAction::Location => {
                self.back_history.clear();
                self.forward_history.clear();
            }
        }
    }

    fn cache_current_root(&mut self) {
        if !self.has_loaded_snapshot() {
            return;
        }
        let authority = self.authority();
        let path = self.root.path.clone();
        if let Some(index) = self.cached_roots.iter().position(|cached| {
            cached.authority == authority
                && cached.path == path
                && cached.show_hidden == self.show_hidden
        }) {
            self.cached_roots.remove(index);
        }
        let mut root = self.root.clone();
        retire_in_flight(&mut root);
        self.cached_roots.push_back(CachedRoot {
            authority,
            path,
            show_hidden: self.show_hidden,
            root,
        });
        while self.cached_roots.len() > MAX_CACHED_ROOTS {
            self.cached_roots.pop_front();
        }
    }

    fn take_cached_root(
        &mut self,
        authority: remote_fs::FilesAuthorityKey,
        path: &Path,
        show_hidden: bool,
    ) -> Option<CachedRoot> {
        let index = self.cached_roots.iter().position(|cached| {
            cached.authority == authority
                && cached.path == path
                && cached.show_hidden == show_hidden
        })?;
        self.cached_roots.remove(index)
    }

    fn cancel_pending_navigation(&mut self) {
        let Some(pending) = self.pending_navigation.take() else {
            return;
        };
        if self
            .active_requests
            .get(&pending.target)
            .is_some_and(|active| active.request_id == pending.request_id)
        {
            if let Some(active) = self.active_requests.remove(&pending.target) {
                active.cancellation.cancel();
            }
        }
    }

    fn advance_generation(&mut self) {
        for request in self.active_requests.values() {
            request.cancellation.cancel();
        }
        self.active_requests.clear();
        self.pending_navigation = None;
        self.pending_location_change = None;
        self.generation = self.generation.wrapping_add(1);
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

/// Read exactly one directory level. This function is intentionally synchronous;
/// callers run it inside an iced worker task instead of the UI update loop.
/// The request's location picks the backend: local disk, or the remote-fs sh
/// probe over ssh / `docker exec`.
pub fn load_directory(request: DirectoryRequest) -> DirectoryResult {
    let listing = remote_fs::list_dir_listing_with_hidden_cancellable(
        &request.location,
        &request.hosts,
        &request.path,
        request.show_hidden,
        Some(&request.cancellation),
    );
    let truncated = listing.as_ref().is_ok_and(|listing| listing.truncated);
    let entries = listing
        .map(|listing| {
            listing
                .entries
                .into_iter()
                .map(|entry| FileTreeNode::entry(entry.name, entry.path, entry.is_dir))
                .collect()
        })
        .map_err(DirectoryError::from_io);
    DirectoryResult {
        generation: request.generation,
        request_id: request.request_id,
        path: request.path,
        entries,
        truncated,
    }
}

fn find_node<'a>(node: &'a FileTreeNode, path: &Path) -> Option<&'a FileTreeNode> {
    if node.path == path {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_node(child, path))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn push_navigation_history(history: &mut VecDeque<PathBuf>, path: PathBuf) {
    if history.back() == Some(&path) {
        return;
    }
    history.push_back(path);
    while history.len() > MAX_NAVIGATION_HISTORY {
        history.pop_front();
    }
}

fn collect_stale_visible_directories(
    node: &FileTreeNode,
    visible: bool,
    now: Instant,
    stale_after: Duration,
    candidates: &mut Vec<(PathBuf, Instant)>,
) {
    if !visible || !node.is_dir {
        return;
    }
    if node.state == DirectoryState::Loaded
        && node
            .last_loaded_at
            .is_some_and(|loaded_at| now.saturating_duration_since(loaded_at) >= stale_after)
    {
        candidates.push((
            node.path.clone(),
            node.last_loaded_at.expect("checked snapshot time"),
        ));
    }
    if node.expanded {
        for child in &node.children {
            collect_stale_visible_directories(child, true, now, stale_after, candidates);
        }
    }
}

fn path_has_unsafe_directional_mark(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// Validate path-bar input before it can become filesystem authority. Remote
/// probes require absolute POSIX paths; rejecting dot/parent components keeps
/// the displayed target identical to the directory actually requested.
pub fn validate_absolute_navigation_path(input: &str) -> Result<PathBuf, &'static str> {
    if input.is_empty() {
        return Err("Enter an absolute path");
    }
    if input.len() > MAX_NAVIGATION_PATH_BYTES {
        return Err("Path is too long");
    }
    if input
        .chars()
        .any(|character| character.is_control() || path_has_unsafe_directional_mark(character))
    {
        return Err("Path contains unsafe control or direction characters");
    }
    if !input.starts_with('/') || !Path::new(input).is_absolute() {
        return Err("Path must be absolute");
    }
    if input
        .split('/')
        .any(|component| component == "." || component == "..")
    {
        return Err("Path cannot contain . or .. components");
    }
    let mut normalized = PathBuf::from("/");
    for component in Path::new(input).components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => normalized.push(component),
            std::path::Component::CurDir
            | std::path::Component::ParentDir
            | std::path::Component::Prefix(_) => {
                return Err("Path contains unsupported components");
            }
        }
    }
    Ok(normalized)
}

fn find_node_mut<'a>(node: &'a mut FileTreeNode, path: &Path) -> Option<&'a mut FileTreeNode> {
    if node.path == path {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, path))
}

/// Retire work stamped with an older tree generation without throwing away a
/// last-good subtree. A retained Loading node has no accepted result coming,
/// so it must become explicitly loadable again; a retained root refresh falls
/// back to its last successful Loaded snapshot.
fn retire_in_flight(node: &mut FileTreeNode) {
    node.state = match std::mem::replace(&mut node.state, DirectoryState::Unloaded) {
        DirectoryState::Loading => DirectoryState::Unloaded,
        DirectoryState::Refreshing => DirectoryState::Loaded,
        state => state,
    };
    for child in &mut node.children {
        retire_in_flight(child);
    }
}

/// Reconcile one freshly scanned directory level with its last-good children.
/// Surviving directories reuse their loaded descendants and expansion state;
/// files and entries whose type changed use the fresh node. Iterating the new
/// list preserves the backend's deterministic display order.
fn reconcile_children(current: &mut Vec<FileTreeNode>, fresh: Vec<FileTreeNode>) {
    let mut previous: HashMap<PathBuf, FileTreeNode> = std::mem::take(current)
        .into_iter()
        .map(|node| (node.path.clone(), node))
        .collect();
    *current = fresh
        .into_iter()
        .map(|fresh_node| {
            if !fresh_node.is_dir {
                return fresh_node;
            }
            let Some(mut previous_node) = previous.remove(&fresh_node.path) else {
                return fresh_node;
            };
            if !previous_node.is_dir {
                return fresh_node;
            }
            previous_node.name = fresh_node.name;
            previous_node.path = fresh_node.path;
            previous_node
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_error(message: &str) -> DirectoryError {
        DirectoryError::other(message, true)
    }

    fn temp_tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("frost-sidebar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("nested").join("deep")).expect("create test tree");
        for index in 0..32 {
            std::fs::write(root.join(format!("file-{index:02}.txt")), b"x")
                .expect("write test file");
        }
        root
    }

    fn coordinator_request(
        request_id: u64,
        path: impl Into<PathBuf>,
        priority: DirectoryRequestPriority,
    ) -> DirectoryRequest {
        DirectoryRequest {
            generation: 7,
            request_id,
            path: path.into(),
            location: FsLocation::Local,
            hosts: Vec::new(),
            show_hidden: false,
            priority,
            bypass_cooldown: false,
            cancellation: remote_fs::CancellationToken::new(),
        }
    }

    fn remote_coordinator_request(
        request_id: u64,
        path: impl Into<PathBuf>,
        priority: DirectoryRequestPriority,
    ) -> DirectoryRequest {
        let mut request = coordinator_request(request_id, path, priority);
        request.location = FsLocation::Transient(RemoteHostConfig {
            name: "test".to_string(),
            host: "example.invalid".to_string(),
            user: Some("tester".to_string()),
            docker: false,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: Vec::new(),
            deploy: String::new(),
            deploy_artifact: None,
        });
        request
    }

    fn remote_coordinator_request_for_host(
        request_id: u64,
        path: impl Into<PathBuf>,
        priority: DirectoryRequestPriority,
        host_name: &str,
    ) -> DirectoryRequest {
        let mut request = remote_coordinator_request(request_id, path, priority);
        let FsLocation::Transient(profile) = &mut request.location else {
            unreachable!("remote coordinator helper always creates a transient profile");
        };
        profile.host = host_name.to_string();
        request
    }

    #[test]
    fn scan_coordinator_enforces_caps_and_allows_high_priority_to_evict_lazy() {
        let mut coordinator = DirectoryScanCoordinator::default();
        coordinator.enqueue(coordinator_request(
            1,
            "/root",
            DirectoryRequestPriority::High,
        ));
        coordinator.enqueue(remote_coordinator_request(
            2,
            "/retry",
            DirectoryRequestPriority::High,
        ));
        let running = coordinator.take_ready();
        assert_eq!(running.len(), MAX_DIRECTORY_SCANS_RUNNING);

        for index in 0..MAX_DIRECTORY_SCANS_QUEUED {
            assert!(coordinator
                .enqueue(coordinator_request(
                    10 + index as u64,
                    format!("/lazy/{index}"),
                    DirectoryRequestPriority::Lazy,
                ))
                .is_empty());
        }
        assert_eq!(coordinator.queued_len(), MAX_DIRECTORY_SCANS_QUEUED);
        assert_eq!(coordinator.pending_len(), MAX_DIRECTORY_SCANS_TOTAL);

        let rejected = coordinator.enqueue(coordinator_request(
            10_000,
            "/lazy/overflow",
            DirectoryRequestPriority::Lazy,
        ));
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].path, Path::new("/lazy/overflow"));
        assert_eq!(coordinator.pending_len(), MAX_DIRECTORY_SCANS_TOTAL);

        let evicted = coordinator.enqueue(coordinator_request(
            10_001,
            "/urgent",
            DirectoryRequestPriority::High,
        ));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].priority, DirectoryRequestPriority::Lazy);
        assert_eq!(coordinator.pending_len(), MAX_DIRECTORY_SCANS_TOTAL);
    }

    #[test]
    fn one_remote_authority_cannot_fill_the_global_scan_queue() {
        let mut coordinator = DirectoryScanCoordinator::default();
        let first = remote_coordinator_request(1, "/remote/0", DirectoryRequestPriority::Lazy);
        let saturated_authority = remote_fs::files_authority_key(&first.location, &first.hosts);
        assert!(coordinator.enqueue(first).is_empty());
        for index in 1..MAX_DIRECTORY_SCANS_QUEUED_PER_REMOTE_AUTHORITY {
            assert!(coordinator
                .enqueue(remote_coordinator_request(
                    1 + index as u64,
                    format!("/remote/{index}"),
                    DirectoryRequestPriority::Lazy,
                ))
                .is_empty());
        }
        assert_eq!(
            coordinator.queued_for_authority(saturated_authority),
            MAX_DIRECTORY_SCANS_QUEUED_PER_REMOTE_AUTHORITY
        );

        let overflow = coordinator.enqueue(remote_coordinator_request(
            10_000,
            "/remote/overflow",
            DirectoryRequestPriority::Lazy,
        ));
        assert_eq!(overflow.len(), 1);
        assert_eq!(overflow[0].path, Path::new("/remote/overflow"));

        let other = remote_coordinator_request_for_host(
            20_000,
            "/other/ready",
            DirectoryRequestPriority::Lazy,
            "other.example.invalid",
        );
        let other_authority = remote_fs::files_authority_key(&other.location, &other.hosts);
        assert!(coordinator.enqueue(other).is_empty());
        assert_eq!(coordinator.queued_for_authority(other_authority), 1);

        let evicted = coordinator.enqueue(remote_coordinator_request(
            30_000,
            "/remote/urgent",
            DirectoryRequestPriority::High,
        ));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].priority, DirectoryRequestPriority::Lazy);
        assert_eq!(
            coordinator.queued_for_authority(saturated_authority),
            MAX_DIRECTORY_SCANS_QUEUED_PER_REMOTE_AUTHORITY
        );
        assert_eq!(
            coordinator.queued_for_authority(other_authority),
            1,
            "same-authority admission must not evict another host"
        );
    }

    #[test]
    fn scan_coordinator_is_latest_wins_and_fair_to_lazy_work() {
        let mut coordinator = DirectoryScanCoordinator::default();
        coordinator.enqueue(coordinator_request(
            1,
            "/same",
            DirectoryRequestPriority::Lazy,
        ));
        coordinator.enqueue(coordinator_request(
            2,
            "/same",
            DirectoryRequestPriority::High,
        ));
        assert_eq!(coordinator.queued_len(), 1);
        let ready = coordinator.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].request_id, 2);
        coordinator.finish(7, 2);

        for request_id in 10..14 {
            let request = if request_id % 2 == 0 {
                coordinator_request(
                    request_id,
                    format!("/high/{request_id}"),
                    DirectoryRequestPriority::High,
                )
            } else {
                remote_coordinator_request(
                    request_id,
                    format!("/high/{request_id}"),
                    DirectoryRequestPriority::High,
                )
            };
            coordinator.enqueue(request);
        }
        coordinator.enqueue(coordinator_request(
            20,
            "/lazy",
            DirectoryRequestPriority::Lazy,
        ));
        let first = coordinator.take_ready();
        assert_eq!(first.len(), 2);
        assert!(first
            .iter()
            .all(|request| request.priority == DirectoryRequestPriority::High));
        for request in first {
            assert!(coordinator.finish(request.generation, request.request_id));
        }
        let second = coordinator.take_ready();
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].priority, DirectoryRequestPriority::Lazy);
        assert_eq!(second[1].priority, DirectoryRequestPriority::High);
    }

    #[test]
    fn cancelled_queued_scan_is_not_started_or_counted() {
        let mut coordinator = DirectoryScanCoordinator::default();
        let request = coordinator_request(1, "/cancelled", DirectoryRequestPriority::Lazy);
        request.cancellation.cancel();
        coordinator.enqueue(request);
        assert_eq!(coordinator.pending_len(), 0);
        assert!(coordinator.take_ready().is_empty());
    }

    #[test]
    fn scan_phase_and_pending_accounting_converge_after_completion() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let request = sidebar.set_current_dir(root.clone());
        let mut coordinator = DirectoryScanCoordinator::default();
        assert_eq!(
            sidebar.request_phase(&root),
            Some(DirectoryRequestPhase::Queued)
        );
        assert!(coordinator.enqueue(request).is_empty());
        let request = coordinator.take_ready().pop().expect("admitted scan");
        assert!(sidebar.mark_request_running(&request));
        assert_eq!(
            sidebar.request_phase(&root),
            Some(DirectoryRequestPhase::Running)
        );
        assert_eq!(coordinator.pending_len(), 1);

        let generation = request.generation;
        let request_id = request.request_id;
        assert!(sidebar.apply_load(load_directory(request)));
        assert!(coordinator.finish(generation, request_id));
        assert_eq!(sidebar.request_phase(&root), None);
        assert_eq!(coordinator.pending_len(), 0);
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn queue_refusal_and_worker_join_failure_results_end_loading_states() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(DirectoryResult::failed(
            initial,
            DirectoryError::busy("Directory scan queue is full"),
        )));
        assert!(matches!(sidebar.root.state, DirectoryState::Error(_)));

        let retry = sidebar.retry_node(&root).expect("retry refused root");
        assert!(sidebar.apply_load(load_directory(retry)));
        let refresh = sidebar.refresh();
        assert!(sidebar.apply_load(DirectoryResult::failed(refresh, DirectoryError::internal(),)));
        assert!(matches!(
            sidebar.root.state,
            DirectoryState::RefreshError(DirectoryError {
                kind: DirectoryErrorKind::Internal,
                ..
            })
        ));
        assert!(!sidebar.root.children.is_empty());
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn directory_errors_are_typed_retryable_and_inline_safe() {
        let denied = DirectoryError::from_io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied\n\u{1b}[31m\u{202e}txt",
        ));
        assert_eq!(denied.kind, DirectoryErrorKind::PermissionDenied);
        assert!(denied.retryable);
        assert!(!denied.message.contains('\n'));
        assert!(!denied.message.contains('\u{1b}'));
        assert!(!denied.message.contains('\u{202e}'));

        let invalid =
            DirectoryError::from_io(io::Error::new(io::ErrorKind::InvalidData, "bad response"));
        assert_eq!(invalid.kind, DirectoryErrorKind::InvalidResponse);
        assert!(!invalid.retryable);
    }

    #[test]
    fn loads_all_entries_and_expands_lazily() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let request = sidebar.set_current_dir(root.clone());
        assert_eq!(sidebar.root.state, DirectoryState::Loading);
        assert!(sidebar.apply_load(load_directory(request)));
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);
        assert_eq!(sidebar.root.children.len(), 33);

        let nested = root.join("nested");
        let request = sidebar
            .toggle_node(&nested)
            .expect("unloaded directory should request a load");
        assert!(sidebar.apply_load(load_directory(request)));
        let nested_node = find_node_mut(&mut sidebar.root, &nested).expect("nested node");
        assert_eq!(nested_node.state, DirectoryState::Loaded);
        assert_eq!(nested_node.children.len(), 1);

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn stale_response_cannot_replace_new_root() {
        let first = temp_tree();
        let second = temp_tree();
        let mut sidebar = Sidebar::new();
        let stale = sidebar.set_current_dir(first.clone());
        let current = sidebar.set_current_dir(second.clone());

        assert!(!sidebar.apply_load(load_directory(stale)));
        assert!(sidebar.apply_load(load_directory(current)));
        assert_eq!(sidebar.root.path, second);

        std::fs::remove_dir_all(first).expect("remove first tree");
        std::fs::remove_dir_all(second).expect("remove second tree");
    }

    #[test]
    fn refresh_preserves_subtrees_and_reconciles_the_root() {
        let root = temp_tree();
        let nested = root.join("nested");
        let deep = nested.join("deep");
        let changed_type = root.join("changed-type");
        std::fs::create_dir(&changed_type).expect("create type-change directory");
        std::fs::write(changed_type.join("old-child"), b"old").expect("write type-change child");
        let mut sidebar = Sidebar::new();
        let root_request = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(load_directory(root_request)));

        let nested_request = sidebar
            .toggle_node(&nested)
            .expect("nested directory should load");
        assert!(sidebar.apply_load(load_directory(nested_request)));
        let stale_deep_request = sidebar
            .toggle_node(&deep)
            .expect("deep directory should begin loading");
        let changed_type_request = sidebar
            .toggle_node(&changed_type)
            .expect("type-change directory should load");
        assert!(sidebar.apply_load(load_directory(changed_type_request)));
        assert_eq!(
            find_node_mut(&mut sidebar.root, &deep)
                .expect("deep node")
                .state,
            DirectoryState::Loading
        );

        std::fs::remove_file(root.join("file-00.txt")).expect("remove old root entry");
        std::fs::write(root.join("fresh.txt"), b"new").expect("write new root entry");
        std::fs::remove_dir_all(&changed_type).expect("remove type-change directory");
        std::fs::write(&changed_type, b"now a file").expect("replace directory with file");
        let old_generation = sidebar.generation();
        let refresh = sidebar.refresh();
        assert_ne!(refresh.generation, old_generation);
        assert_eq!(sidebar.root.state, DirectoryState::Refreshing);
        let nested_node = find_node_mut(&mut sidebar.root, &nested).expect("retained nested node");
        assert!(nested_node.expanded);
        assert_eq!(nested_node.state, DirectoryState::Loaded);
        let deep_node = find_node_mut(&mut sidebar.root, &deep).expect("retained deep node");
        assert!(deep_node.expanded);
        assert_eq!(
            deep_node.state,
            DirectoryState::Unloaded,
            "an old-generation descendant load must be reopenable"
        );
        assert!(
            !sidebar.apply_load(load_directory(stale_deep_request)),
            "the retired descendant result must stay stale"
        );

        assert!(sidebar.apply_load(load_directory(refresh)));
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);
        assert!(sidebar
            .root
            .children
            .iter()
            .any(|node| node.name == "fresh.txt"));
        assert!(!sidebar
            .root
            .children
            .iter()
            .any(|node| node.name == "file-00.txt"));
        let changed_type_node = find_node_mut(&mut sidebar.root, &changed_type)
            .expect("fresh entry at the type-changed path");
        assert!(!changed_type_node.is_dir);
        assert!(changed_type_node.children.is_empty());
        let nested_node = find_node_mut(&mut sidebar.root, &nested).expect("reused nested node");
        assert!(nested_node.expanded);
        assert!(
            find_node_mut(&mut sidebar.root, &deep).is_some(),
            "the surviving directory must retain its loaded subtree"
        );

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn refresh_failure_keeps_the_last_good_snapshot() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        let mut initial_result = load_directory(initial);
        initial_result.truncated = true;
        assert!(sidebar.apply_load(initial_result));
        assert!(sidebar.root.truncated);
        let original_paths: Vec<_> = sidebar
            .root
            .children
            .iter()
            .map(|node| node.path.clone())
            .collect();

        let refresh = sidebar.refresh();
        assert_eq!(sidebar.root.state, DirectoryState::Refreshing);
        assert!(sidebar.apply_load(DirectoryResult {
            generation: refresh.generation,
            request_id: refresh.request_id,
            path: refresh.path,
            entries: Err(test_error("remote is temporarily unavailable")),
            truncated: false,
        }));
        assert_eq!(
            sidebar.root.state,
            DirectoryState::RefreshError(test_error("remote is temporarily unavailable"))
        );
        assert!(sidebar.has_loaded_snapshot());
        assert!(
            sidebar.root.truncated,
            "a failed refresh retains the last-good truncation status"
        );
        assert_eq!(
            sidebar
                .root
                .children
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>(),
            original_paths,
            "a failed replacement must not erase the last-good rows"
        );

        let retry = sidebar
            .retry_node(&root)
            .expect("root RefreshError should retry in place");
        assert_eq!(sidebar.root.state, DirectoryState::Refreshing);
        assert!(sidebar.apply_load(load_directory(retry)));
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);
        assert!(!sidebar.root.truncated);

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn accepted_snapshot_records_time_and_failed_refresh_retains_it() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let loaded_at = Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .expect("test instant");
        let initial = sidebar.set_current_dir(root.clone());
        sidebar.apply_load_at(load_directory(initial), loaded_at);
        assert_eq!(sidebar.root.last_loaded_at, Some(loaded_at));

        let refresh = sidebar.refresh();
        let failed_at = Instant::now();
        assert!(sidebar.apply_load_at(
            DirectoryResult {
                generation: refresh.generation,
                request_id: refresh.request_id,
                path: refresh.path,
                entries: Err(test_error("offline")),
                truncated: false,
            },
            failed_at,
        ));
        assert_eq!(sidebar.root.last_loaded_at, Some(loaded_at));

        let retry = sidebar.retry_node(&root).expect("retry refresh");
        assert!(sidebar.apply_load_at(load_directory(retry), failed_at));
        assert_eq!(sidebar.root.last_loaded_at, Some(failed_at));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn exact_nested_refresh_updates_collapsed_cached_directory() {
        let root = temp_tree();
        let nested = root.join("nested");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let load_nested = sidebar.toggle_node(&nested).expect("load nested");
        assert!(sidebar.apply_load(load_directory(load_nested)));
        assert!(sidebar.toggle_node(&nested).is_none());
        assert!(!find_node(&sidebar.root, &nested).expect("nested").expanded);

        let fresh = nested.join("created-after-collapse");
        std::fs::write(&fresh, b"new").expect("write nested entry");
        let refresh = sidebar
            .refresh_directory(&nested)
            .expect("cached collapsed directory refreshes");
        assert_eq!(
            find_node(&sidebar.root, &nested).expect("nested").state,
            DirectoryState::Refreshing
        );
        assert!(sidebar.apply_load(load_directory(refresh)));
        let node = find_node(&sidebar.root, &nested).expect("nested");
        assert!(!node.expanded, "refresh preserves collapsed state");
        assert!(node.children.iter().any(|child| child.path == fresh));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn hidden_policy_is_generation_guarded_and_reloads_the_root() {
        let root = temp_tree();
        std::fs::write(root.join(".env"), b"hidden").expect("write hidden file");
        let mut sidebar = Sidebar::new();
        let hidden_request = sidebar.set_current_dir(root.clone());
        assert!(!hidden_request.show_hidden);
        assert!(sidebar.apply_load(load_directory(hidden_request)));
        assert!(!sidebar.root.children.iter().any(|node| node.name == ".env"));

        let stale_hidden_request = sidebar.refresh();
        let old_generation = sidebar.generation();
        let shown_request = sidebar
            .set_show_hidden(true)
            .expect("changed policy reloads the root");
        assert!(shown_request.show_hidden);
        assert_ne!(shown_request.generation, old_generation);
        assert!(
            !sidebar.apply_load(load_directory(stale_hidden_request)),
            "a result issued under the old dotfile policy must be stale"
        );
        assert!(sidebar.apply_load(load_directory(shown_request)));
        assert!(sidebar.root.children.iter().any(|node| node.name == ".env"));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn location_change_is_generation_guarded() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let first = sidebar.begin_location_change(FsLocation::Remote(0));
        let second = sidebar.begin_location_change(FsLocation::Local);
        assert_ne!(first, second);
        assert!(!sidebar.accepts_generation(first));
        assert!(sidebar.accepts_generation(second));
        // A stale resolution for the older change is dropped without stealing
        // the newer pending switch. The current candidate commits only after
        // its one-level listing succeeds.
        assert!(sidebar.resolve_location(first, Ok(root.clone())).is_none());
        let request = sidebar
            .resolve_location(second, Ok(root.clone()))
            .expect("current resolution applies");
        assert_ne!(sidebar.root.path, root);
        assert!(sidebar.apply_load(load_directory(request)));
        assert_eq!(sidebar.root.path, root);
        assert_eq!(sidebar.location, FsLocation::Local);

        // A failed home probe keeps the accepted location/tree and history.
        let accepted_paths: Vec<_> = sidebar
            .root
            .children
            .iter()
            .map(|node| node.path.clone())
            .collect();
        let generation = sidebar.begin_location_change(FsLocation::Remote(9));
        assert!(sidebar
            .resolve_location(
                generation,
                Err(DirectoryError::from_io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no such host",
                ))),
            )
            .is_none());
        assert_eq!(sidebar.location, FsLocation::Local);
        assert_eq!(sidebar.root.path, root);
        assert_eq!(
            sidebar
                .root
                .children
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>(),
            accepted_paths
        );
        assert!(sidebar.navigation_failure().is_some());

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn resolved_location_home_survives_directory_navigation() {
        let mut sidebar = Sidebar::new();
        let generation = sidebar.begin_location_change(FsLocation::Remote(0));
        let home = PathBuf::from("/home/remote-user");
        let request = sidebar
            .resolve_location(generation, Ok(home.clone()))
            .expect("current home resolution");
        assert_ne!(sidebar.home_dir(), Some(home.as_path()));
        assert!(sidebar.apply_load(DirectoryResult {
            generation: request.generation,
            request_id: request.request_id,
            path: request.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        assert_eq!(sidebar.home_dir(), Some(home.as_path()));
        assert_eq!(sidebar.current_dir, home);

        let nested = PathBuf::from("/home/remote-user/project/src");
        let navigation = sidebar
            .begin_navigation(nested.clone())
            .expect("stage nested navigation");
        assert_ne!(sidebar.current_dir, nested);
        assert!(sidebar.apply_load(DirectoryResult {
            generation: navigation.generation,
            request_id: navigation.request_id,
            path: navigation.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        assert_eq!(sidebar.current_dir, nested);
        assert_eq!(sidebar.home_dir(), Some(home.as_path()));
    }

    #[test]
    fn delayed_file_intent_expires_when_tree_identity_changes() {
        let mut sidebar = Sidebar::new();
        let menu_generation = sidebar.generation();
        assert!(sidebar.accepts_generation(menu_generation));

        sidebar.begin_location_change(FsLocation::Remote(0));
        assert!(
            !sidebar.accepts_generation(menu_generation),
            "an old menu/dialog cannot act after a location switch"
        );

        let remote_generation = sidebar.generation();
        sidebar.set_current_dir(PathBuf::from("/tmp"));
        assert!(
            !sidebar.accepts_generation(remote_generation),
            "an old root-scoped action cannot act after rerooting"
        );
    }

    #[test]
    fn remote_home_failure_can_recover_to_a_loaded_local_root() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let failed_probe = sidebar.begin_location_change(FsLocation::Remote(0));
        assert!(sidebar.accepts_generation(failed_probe));

        let request = sidebar.reset_to_local_with_hosts(root.clone(), Vec::new());
        assert_eq!(sidebar.location, FsLocation::Local);
        assert!(!sidebar.accepts_generation(failed_probe));
        assert!(sidebar.apply_load(load_directory(request)));
        assert_eq!(sidebar.root.path, root);
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn host_snapshot_change_retires_both_phases_of_a_staged_location_switch() {
        let mut host_a = crate::config::default_remote_hosts()[0].clone();
        host_a.host = "host-a.example.test".to_string();
        let mut host_b = host_a.clone();
        host_b.host = "host-b.example.test".to_string();

        let mut sidebar = Sidebar::new();
        sidebar.set_hosts(vec![host_a.clone()]);
        let original_location = sidebar.location.clone();
        let original_root = sidebar.current_dir.clone();

        // Phase 1: an A home probe is still running when Remote(0) becomes B.
        let home_generation = sidebar.begin_location_change(FsLocation::Remote(0));
        sidebar.set_hosts(vec![host_b.clone()]);
        assert!(!sidebar.accepts_generation(home_generation));
        assert!(!sidebar.location_change_pending());
        assert!(sidebar
            .resolve_location(home_generation, Ok(PathBuf::from("/home/from-a")))
            .is_none());
        assert_eq!(sidebar.location, original_location);
        assert_eq!(sidebar.current_dir, original_root);

        // Phase 2: A's home resolved, but its candidate listing has not. The
        // config replacement must cancel that exact request before it can
        // commit A's path/profile index against B's host snapshot.
        sidebar.set_hosts(vec![host_a]);
        let candidate_generation = sidebar.begin_location_change(FsLocation::Remote(0));
        let candidate = sidebar
            .resolve_location(candidate_generation, Ok(PathBuf::from("/home/from-a")))
            .expect("candidate listing for A");
        assert_eq!(candidate.hosts[0].host, "host-a.example.test");

        sidebar.set_hosts(vec![host_b]);
        assert!(candidate.cancellation.is_cancelled());
        assert!(!sidebar.accepts_generation(candidate.generation));
        assert!(!sidebar.apply_load(DirectoryResult {
            generation: candidate.generation,
            request_id: candidate.request_id,
            path: candidate.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        assert_eq!(sidebar.location, original_location);
        assert_eq!(sidebar.current_dir, original_root);
    }

    #[test]
    fn config_fallback_caches_the_old_tree_under_the_old_authority() {
        let mut host_a = crate::config::default_remote_hosts()[0].clone();
        host_a.host = "host-a.example.test".to_string();
        let mut host_b = host_a.clone();
        host_b.host = "host-b.example.test".to_string();

        let shared_root = PathBuf::from("/shared");
        let shared_dir = shared_root.join("common");
        let leaked = shared_dir.join("a-only.txt");
        let mut sidebar = Sidebar::new();
        sidebar.set_hosts(vec![host_a]);
        sidebar.location = FsLocation::Remote(0);
        sidebar.current_dir = shared_root.clone();
        sidebar.root = FileTreeNode::directory(shared_root.clone(), true);
        sidebar.root.state = DirectoryState::Loaded;
        sidebar.root.last_loaded_at = Some(Instant::now());
        let mut common = FileTreeNode::directory(shared_dir.clone(), true);
        common.state = DirectoryState::Loaded;
        common.last_loaded_at = Some(Instant::now());
        common.children.push(FileTreeNode::entry(
            "a-only.txt".to_string(),
            leaked.clone(),
            false,
        ));
        sidebar.root.children.push(common);

        let local =
            sidebar.reset_to_local_with_hosts(PathBuf::from("/local"), vec![host_b.clone()]);
        assert!(sidebar.apply_load(DirectoryResult {
            generation: local.generation,
            request_id: local.request_id,
            path: local.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        let b_authority =
            remote_fs::files_authority_key(&FsLocation::Remote(0), sidebar.hosts_snapshot());
        assert!(sidebar
            .cached_roots
            .iter()
            .all(|cached| cached.authority != b_authority));

        let generation = sidebar.begin_location_change(FsLocation::Remote(0));
        let candidate = sidebar
            .resolve_location(generation, Ok(shared_root.clone()))
            .expect("candidate listing for B");
        assert_eq!(candidate.hosts[0], host_b);
        assert!(sidebar.apply_load(DirectoryResult {
            generation: candidate.generation,
            request_id: candidate.request_id,
            path: candidate.path,
            entries: Ok(vec![FileTreeNode::entry(
                "common".to_string(),
                shared_dir.clone(),
                true,
            )]),
            truncated: false,
        }));
        assert_eq!(sidebar.location, FsLocation::Remote(0));
        assert!(find_node(&sidebar.root, &shared_dir)
            .expect("B common directory")
            .children
            .is_empty());
        assert!(find_node(&sidebar.root, &leaked).is_none());
    }

    #[test]
    fn exact_profile_reindex_replaces_pending_load_without_sticking() {
        let mut first = crate::config::default_remote_hosts()[0].clone();
        first.name = "first".to_string();
        let mut second = first.clone();
        second.name = "second".to_string();

        let mut sidebar = Sidebar::new();
        sidebar.set_hosts(vec![first.clone(), second.clone()]);
        sidebar.location = FsLocation::Remote(0);
        let pending = sidebar.set_current_dir(PathBuf::from("/remote/home"));

        sidebar.set_hosts(vec![second, first]);
        let replacement = sidebar.rebind_location_and_refresh(FsLocation::Remote(1));
        assert_ne!(pending.generation, replacement.generation);
        assert_eq!(replacement.location, FsLocation::Remote(1));
        assert_eq!(replacement.hosts, sidebar.hosts_snapshot());

        assert!(!sidebar.apply_load(DirectoryResult {
            generation: pending.generation,
            request_id: pending.request_id,
            path: pending.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        assert!(sidebar.apply_load(DirectoryResult {
            generation: replacement.generation,
            request_id: replacement.request_id,
            path: replacement.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);
    }

    #[test]
    fn same_namespace_route_upgrade_preserves_loaded_tree_and_retires_old_loads() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let request = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(load_directory(request)));
        let old_generation = sidebar.generation();
        let old_children = sidebar.root.children.len();

        assert!(sidebar.rebind_same_namespace_preserving_tree(FsLocation::Remote(0)));
        assert_eq!(sidebar.location, FsLocation::Remote(0));
        assert_eq!(sidebar.current_dir, root);
        assert_eq!(sidebar.root.children.len(), old_children);
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);
        assert!(!sidebar.accepts_generation(old_generation));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn remote_location_listing_failure_keeps_the_accepted_tree() {
        let accepted = temp_tree();
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(accepted.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let accepted_paths: Vec<_> = sidebar
            .root
            .children
            .iter()
            .map(|child| child.path.clone())
            .collect();
        let generation = sidebar.begin_location_change(FsLocation::Remote(0));
        let request = sidebar
            .resolve_location(generation, Ok(PathBuf::from("/tmp")))
            .expect("resolution applies");
        // No hosts in the snapshot: the candidate fails closed, but never
        // replaces the accepted Local root with an empty error tree.
        let result = load_directory(request);
        assert!(result.entries.is_err());
        assert!(sidebar.apply_load(result));
        assert_eq!(sidebar.location, FsLocation::Local);
        assert_eq!(sidebar.root.path, accepted);
        assert_eq!(
            sidebar
                .root
                .children
                .iter()
                .map(|child| child.path.clone())
                .collect::<Vec<_>>(),
            accepted_paths
        );
        assert!(sidebar.navigation_failure().is_some());
        std::fs::remove_dir_all(accepted).expect("remove accepted tree");
    }

    #[test]
    fn initial_error_retries_in_place_with_a_new_request_identity() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(DirectoryResult {
            generation: initial.generation,
            request_id: initial.request_id,
            path: initial.path,
            entries: Err(test_error("temporary failure")),
            truncated: false,
        }));
        assert_eq!(
            sidebar.root.state,
            DirectoryState::Error(test_error("temporary failure"))
        );

        let retry = sidebar
            .retry_node(&root)
            .expect("an Error row exposes an exact-path retry");
        assert_eq!(retry.generation, sidebar.generation());
        assert_ne!(retry.request_id, initial.request_id);
        assert_eq!(sidebar.root.state, DirectoryState::Loading);
        assert!(sidebar.apply_load(load_directory(retry)));
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn nested_refresh_retry_keeps_last_good_children_and_expansion() {
        let root = temp_tree();
        let nested = root.join("nested");
        let deep = nested.join("deep");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let nested_load = sidebar.toggle_node(&nested).expect("load nested");
        assert!(sidebar.apply_load(load_directory(nested_load)));
        {
            let node = find_node_mut(&mut sidebar.root, &nested).expect("nested node");
            node.state = DirectoryState::RefreshError(test_error("offline"));
            node.truncated = true;
            assert!(node.expanded);
            assert!(node.children.iter().any(|child| child.path == deep));
        }

        let retry = sidebar
            .retry_node(&nested)
            .expect("a nested RefreshError exposes Retry");
        let node = find_node_mut(&mut sidebar.root, &nested).expect("retrying nested node");
        assert_eq!(node.state, DirectoryState::Refreshing);
        assert!(node.expanded, "retry must retain expansion");
        assert!(node.truncated, "retry must retain last-good metadata");
        assert!(node.children.iter().any(|child| child.path == deep));

        assert!(sidebar.apply_load(DirectoryResult {
            generation: retry.generation,
            request_id: retry.request_id,
            path: retry.path,
            entries: Err(test_error("still offline")),
            truncated: false,
        }));
        let node = find_node_mut(&mut sidebar.root, &nested).expect("failed nested node");
        assert_eq!(
            node.state,
            DirectoryState::RefreshError(test_error("still offline"))
        );
        assert!(node.expanded);
        assert!(node.truncated);
        assert!(node.children.iter().any(|child| child.path == deep));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn nested_initial_error_can_retry_after_the_directory_returns() {
        let root = temp_tree();
        let nested = root.join("nested");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        std::fs::remove_dir_all(&nested).expect("remove nested before its lazy load");

        let failed = sidebar.toggle_node(&nested).expect("start nested load");
        assert!(sidebar.apply_load(load_directory(failed)));
        assert!(matches!(
            find_node(&sidebar.root, &nested).map(|node| &node.state),
            Some(DirectoryState::Error(_))
        ));

        std::fs::create_dir_all(nested.join("deep")).expect("restore nested directory");
        let retry = sidebar
            .retry_node(&nested)
            .expect("nested Error should expose Retry");
        assert_eq!(
            find_node(&sidebar.root, &nested).map(|node| &node.state),
            Some(&DirectoryState::Loading)
        );
        assert!(sidebar.apply_load(load_directory(retry)));
        assert!(find_node(&sidebar.root, &nested)
            .is_some_and(|node| node.children.iter().any(|child| child.name == "deep")));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn newer_same_path_request_cancels_and_rejects_the_old_one() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let old = sidebar.set_current_dir(root.clone());
        let newer = sidebar.begin_load_root();

        assert!(old.cancellation.is_cancelled());
        assert_ne!(old.request_id, newer.request_id);
        let old_result = load_directory(old);
        assert!(old_result.entries.is_err());
        assert!(!sidebar.apply_load(old_result));
        assert_eq!(sidebar.root.state, DirectoryState::Loading);
        assert!(sidebar.apply_load(load_directory(newer)));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn generation_and_location_changes_cancel_pending_directory_work() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let rerooted = sidebar.set_current_dir(root.clone());
        let refreshed = sidebar.refresh();
        assert!(rerooted.cancellation.is_cancelled());
        assert!(!refreshed.cancellation.is_cancelled());

        let location_generation = sidebar.begin_location_change(FsLocation::Remote(0));
        assert!(refreshed.cancellation.is_cancelled());
        assert!(sidebar.accepts_generation(location_generation));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn candidate_navigation_failure_and_stale_result_keep_the_accepted_tree() {
        let accepted = temp_tree();
        let first_target = temp_tree();
        let second_target = temp_tree();
        let nested = accepted.join("nested");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(accepted.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let nested_load = sidebar.toggle_node(&nested).expect("load nested cache");
        assert!(sidebar.apply_load(load_directory(nested_load)));
        let accepted_generation = sidebar.generation();

        let stale = sidebar
            .begin_navigation(first_target.clone())
            .expect("stage first candidate");
        let current = sidebar
            .begin_navigation(second_target.clone())
            .expect("new candidate supersedes first");
        assert!(stale.cancellation.is_cancelled());
        assert_eq!(sidebar.current_dir, accepted);
        assert_eq!(sidebar.generation(), accepted_generation);
        assert!(find_node(&sidebar.root, &nested).is_some_and(|node| node.expanded));

        assert!(!sidebar.apply_load(load_directory(stale)));
        assert!(sidebar.apply_load(DirectoryResult::failed(
            current,
            DirectoryError::from_io(io::Error::new(io::ErrorKind::TimedOut, "secret host")),
        )));
        assert_eq!(sidebar.current_dir, accepted);
        assert_eq!(sidebar.generation(), accepted_generation);
        assert!(find_node(&sidebar.root, &nested)
            .is_some_and(|node| { node.expanded && !node.children.is_empty() }));
        assert!(sidebar.navigation_failure().is_some());
        assert!(!sidebar.can_navigate_back());

        std::fs::remove_dir_all(accepted).expect("remove accepted tree");
        std::fs::remove_dir_all(first_target).expect("remove first target");
        std::fs::remove_dir_all(second_target).expect("remove second target");
    }

    #[test]
    fn successful_navigation_commits_history_and_reuses_authority_bound_cache() {
        let first = temp_tree();
        let second = temp_tree();
        let nested = first.join("nested");
        let deep = nested.join("deep");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(first.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let nested_load = sidebar.toggle_node(&nested).expect("load first subtree");
        assert!(sidebar.apply_load(load_directory(nested_load)));

        let to_second = sidebar
            .begin_navigation(second.clone())
            .expect("stage second root");
        assert_eq!(
            sidebar.current_dir, first,
            "candidate must not commit early"
        );
        assert!(sidebar.apply_load(load_directory(to_second)));
        assert_eq!(sidebar.current_dir, second);
        assert!(sidebar.can_navigate_back());
        assert!(!sidebar.can_navigate_forward());

        let back = sidebar.navigate_back().expect("back history entry");
        assert_eq!(sidebar.current_dir, second, "back also commits after scan");
        assert!(sidebar.apply_load(load_directory(back)));
        assert_eq!(sidebar.current_dir, first);
        let restored = find_node(&sidebar.root, &nested).expect("cached nested directory");
        assert!(restored.expanded);
        assert!(restored.children.iter().any(|child| child.path == deep));
        assert!(sidebar.can_navigate_forward());

        let forward = sidebar.navigate_forward().expect("forward history entry");
        assert!(sidebar.apply_load(load_directory(forward)));
        assert_eq!(sidebar.current_dir, second);

        std::fs::remove_dir_all(first).expect("remove first tree");
        std::fs::remove_dir_all(second).expect("remove second tree");
    }

    #[test]
    fn navigation_history_and_root_cache_are_hard_bounded() {
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(PathBuf::from("/start"));
        assert!(sidebar.apply_load(DirectoryResult {
            generation: initial.generation,
            request_id: initial.request_id,
            path: initial.path,
            entries: Ok(Vec::new()),
            truncated: false,
        }));
        for index in 0..40 {
            let request = sidebar
                .begin_navigation(PathBuf::from(format!("/history-{index}")))
                .expect("new history target");
            assert!(sidebar.apply_load(DirectoryResult {
                generation: request.generation,
                request_id: request.request_id,
                path: request.path,
                entries: Ok(Vec::new()),
                truncated: false,
            }));
        }
        assert_eq!(sidebar.back_history.len(), MAX_NAVIGATION_HISTORY);
        assert!(sidebar.cached_roots.len() <= MAX_CACHED_ROOTS);

        let mut back_commits = 0;
        while let Some(request) = sidebar.navigate_back() {
            assert!(sidebar.apply_load(DirectoryResult {
                generation: request.generation,
                request_id: request.request_id,
                path: request.path,
                entries: Ok(Vec::new()),
                truncated: false,
            }));
            back_commits += 1;
        }
        assert_eq!(back_commits, MAX_NAVIGATION_HISTORY);
        assert_eq!(sidebar.forward_history.len(), MAX_NAVIGATION_HISTORY);
    }

    #[test]
    fn precise_cache_invalidation_retires_only_the_affected_directory_snapshot() {
        let first = temp_tree();
        let second = temp_tree();
        let nested = first.join("nested");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(first.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let nested_load = sidebar.toggle_node(&nested).expect("load nested");
        assert!(sidebar.apply_load(load_directory(nested_load)));
        let away = sidebar
            .begin_navigation(second.clone())
            .expect("navigate away");
        assert!(sidebar.apply_load(load_directory(away)));

        sidebar.invalidate_cached_directories(std::slice::from_ref(&nested));
        let back = sidebar.navigate_back().expect("navigate to cached root");
        assert!(sidebar.apply_load(load_directory(back)));
        let invalidated = find_node(&sidebar.root, &nested).expect("surviving directory row");
        assert_eq!(invalidated.state, DirectoryState::Unloaded);
        assert!(invalidated.children.is_empty());

        std::fs::remove_dir_all(first).expect("remove first tree");
        std::fs::remove_dir_all(second).expect("remove second tree");
    }

    #[test]
    fn visible_stale_refresh_is_oldest_first_bounded_and_lazy() {
        let root = temp_tree();
        let nested = root.join("nested");
        let mut sidebar = Sidebar::new();
        let initial = sidebar.set_current_dir(root.clone());
        assert!(sidebar.apply_load(load_directory(initial)));
        let nested_load = sidebar.toggle_node(&nested).expect("load nested");
        assert!(sidebar.apply_load(load_directory(nested_load)));
        let now = Instant::now();
        sidebar.root.last_loaded_at = Some(now - Duration::from_secs(600));
        find_node_mut(&mut sidebar.root, &nested)
            .expect("nested")
            .last_loaded_at = Some(now - Duration::from_secs(900));

        let requests = sidebar.refresh_stale_visible(now, Duration::from_secs(300), 1);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, nested);
        assert_eq!(requests[0].priority, DirectoryRequestPriority::Lazy);
        assert_eq!(
            find_node(&sidebar.root, &nested).map(|node| &node.state),
            Some(&DirectoryState::Refreshing)
        );
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn coordinator_isolates_authorities_and_reports_queue_and_run_latency() {
        let base = Instant::now();
        let mut coordinator = DirectoryScanCoordinator::default();
        coordinator.enqueue_at(
            coordinator_request(1, "/local-a", DirectoryRequestPriority::High),
            base,
        );
        coordinator.enqueue_at(
            coordinator_request(2, "/local-b", DirectoryRequestPriority::High),
            base,
        );
        coordinator.enqueue_at(
            remote_coordinator_request(3, "/remote", DirectoryRequestPriority::High),
            base,
        );
        let started = base + Duration::from_secs(3);
        let ready = coordinator.take_ready_at(started);
        assert_eq!(ready.len(), MAX_DIRECTORY_SCANS_RUNNING);
        assert_eq!(
            ready
                .iter()
                .filter(|request| matches!(request.location, FsLocation::Local))
                .count(),
            1,
            "one authority cannot occupy both global slots"
        );
        assert_eq!(coordinator.queued_len(), 1);

        let first = &ready[0];
        let result = DirectoryResult {
            generation: first.generation,
            request_id: first.request_id,
            path: first.path.clone(),
            entries: Ok(Vec::new()),
            truncated: false,
        };
        let timing = coordinator
            .finish_result_at(&result, started + Duration::from_secs(5))
            .expect("running timing");
        assert_eq!(timing.queued_for, Duration::from_secs(3));
        assert_eq!(timing.ran_for, Duration::from_secs(5));
        assert_eq!(coordinator.last_timing(), Some(timing));
        assert_eq!(
            coordinator.oldest_queued_age(started + Duration::from_secs(5)),
            Some(Duration::from_secs(8))
        );
        for request in ready.into_iter().skip(1) {
            assert!(coordinator.finish(request.generation, request.request_id));
        }
    }

    #[test]
    fn classified_cooldown_is_exponential_and_retry_bypasses_once() {
        let base = Instant::now();
        let mut coordinator = DirectoryScanCoordinator::default();
        coordinator.enqueue_at(
            coordinator_request(1, "/first", DirectoryRequestPriority::High),
            base,
        );
        let failed = coordinator.take_ready_at(base).pop().expect("first scan");
        let failure = DirectoryResult {
            generation: failed.generation,
            request_id: failed.request_id,
            path: failed.path,
            entries: Err(DirectoryError::from_io(io::Error::new(
                io::ErrorKind::TimedOut,
                "private endpoint",
            ))),
            truncated: false,
        };
        coordinator.finish_result_at(&failure, base);

        coordinator.enqueue_at(
            coordinator_request(2, "/ordinary", DirectoryRequestPriority::High),
            base + Duration::from_secs(1),
        );
        assert!(coordinator
            .take_ready_at(base + Duration::from_secs(1))
            .is_empty());

        let mut retry = coordinator_request(3, "/retry", DirectoryRequestPriority::High);
        retry.bypass_cooldown = true;
        coordinator.enqueue_at(retry, base + Duration::from_secs(1));
        let bypassed = coordinator
            .take_ready_at(base + Duration::from_secs(1))
            .pop()
            .expect("explicit retry bypasses once");
        assert_eq!(bypassed.request_id, 3);
        let second_failure = DirectoryResult {
            generation: bypassed.generation,
            request_id: bypassed.request_id,
            path: bypassed.path,
            entries: Err(DirectoryError::from_io(io::Error::new(
                io::ErrorKind::TimedOut,
                "private endpoint",
            ))),
            truncated: false,
        };
        coordinator.finish_result_at(&second_failure, base + Duration::from_secs(1));
        assert!(
            coordinator
                .take_ready_at(base + Duration::from_secs(4))
                .is_empty(),
            "second authority failure backs off for four seconds"
        );
        assert_eq!(
            coordinator
                .take_ready_at(base + Duration::from_secs(5))
                .pop()
                .expect("cooldown elapsed")
                .request_id,
            2
        );
    }

    #[test]
    fn elapsed_cooldown_with_a_live_retry_keeps_exponential_failure_history() {
        let base = Instant::now();
        let mut coordinator = DirectoryScanCoordinator::default();
        coordinator.enqueue_at(
            remote_coordinator_request(1, "/first", DirectoryRequestPriority::High),
            base,
        );
        let first = coordinator.take_ready_at(base).pop().expect("first scan");
        let authority = remote_fs::files_authority_key(&first.location, &first.hosts);
        coordinator.finish_result_at(
            &DirectoryResult::failed(
                first,
                DirectoryError::from_io(io::Error::new(io::ErrorKind::TimedOut, "offline")),
            ),
            base,
        );

        // Queue the retry exactly when the first cooldown elapses. Compaction
        // must retain the bucket while this request is queued/running so a
        // second failure advances to the four-second step instead of resetting.
        coordinator.enqueue_at(
            remote_coordinator_request(2, "/second", DirectoryRequestPriority::High),
            base + Duration::from_secs(2),
        );
        let second = coordinator
            .take_ready_at(base + Duration::from_secs(2))
            .pop()
            .expect("elapsed cooldown dispatches retry");
        coordinator.finish_result_at(
            &DirectoryResult::failed(
                second,
                DirectoryError::from_io(io::Error::new(io::ErrorKind::TimedOut, "still offline")),
            ),
            base + Duration::from_secs(2),
        );
        let state = coordinator
            .cooldowns
            .get(&CooldownKey::Authority(authority))
            .expect("second authority cooldown");
        assert_eq!(state.failures, 2);
        assert_eq!(state.until, base + Duration::from_secs(6));

        coordinator.take_ready_at(base + Duration::from_secs(7));
        assert!(coordinator.cooldowns.is_empty());
    }

    #[test]
    fn abandoned_unique_path_cooldowns_are_compacted_after_expiry() {
        let base = Instant::now();
        let mut coordinator = DirectoryScanCoordinator::default();
        for index in 0..4 {
            coordinator.enqueue_at(
                coordinator_request(
                    100 + index,
                    format!("/denied/{index}"),
                    DirectoryRequestPriority::High,
                ),
                base,
            );
            let request = coordinator
                .take_ready_at(base)
                .pop()
                .expect("path-local scan");
            coordinator.finish_result_at(
                &DirectoryResult::failed(
                    request,
                    DirectoryError::from_io(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "denied",
                    )),
                ),
                base,
            );
        }
        assert_eq!(coordinator.cooldowns.len(), 4);

        coordinator.take_ready_at(base + Duration::from_secs(3));
        assert!(
            coordinator.cooldowns.is_empty(),
            "expired, unreferenced path buckets must not accumulate"
        );
    }

    #[test]
    fn absolute_navigation_path_rejects_ambiguous_or_unsafe_authority() {
        assert_eq!(
            validate_absolute_navigation_path("/srv/project").expect("safe absolute"),
            PathBuf::from("/srv/project")
        );
        assert_eq!(
            validate_absolute_navigation_path("/srv//project/").expect("lexically normalized"),
            PathBuf::from("/srv/project")
        );
        assert!(validate_absolute_navigation_path("relative/path").is_err());
        assert!(validate_absolute_navigation_path("/srv/../secret").is_err());
        assert!(validate_absolute_navigation_path("/srv/./project").is_err());
        assert!(validate_absolute_navigation_path("/srv/\nproject").is_err());
        assert!(validate_absolute_navigation_path("/srv/\u{202e}txt").is_err());
        assert!(validate_absolute_navigation_path(&format!(
            "/{}",
            "a".repeat(MAX_NAVIGATION_PATH_BYTES)
        ))
        .is_err());
    }
}
