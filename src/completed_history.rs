//! Frost's periodic checkpoint adapter for pane-bound completed cards.

use crate::session_persistence::{InstanceLock, SessionsSnapshot, MAX_RESTORED_SESSIONS};
use crate::Session;
use jterm_core::completed_history::Checkpoint;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Default)]
struct Progress {
    written: u64,
    retry_after: Option<Instant>,
    blocked: bool,
}

struct Write {
    root: PathBuf,
    pane: Uuid,
    revision: u64,
    result: mpsc::Receiver<io::Result<()>>,
}

#[derive(Default)]
pub(crate) struct PaneHistory {
    root: Option<PathBuf>,
    panes: HashMap<Uuid, Progress>,
    last_requested: Option<Uuid>,
    pending: Option<Write>,
}

fn archive_root(layout: &Path) -> Option<PathBuf> {
    let mut name = layout.file_name()?.to_os_string();
    name.push(".frost-cards");
    Some(layout.with_file_name(name))
}

pub(crate) fn restore(layout: &Path, session: &mut Session, pane: Option<Uuid>) {
    let Some(pane) = pane else { return };
    session.pane_history_id = pane;
    if !jterm_core::execution_journal::output_capture_enabled() {
        return;
    }
    let Some(root) = archive_root(layout) else {
        return;
    };
    let result = Checkpoint::load(&root.join(format!("{pane}.json")), &pane.to_string()).and_then(
        |loaded| match loaded {
            Some(checkpoint) => session.terminal.restore_completed_history(checkpoint),
            None => Ok(()),
        },
    );
    if let Err(error) = result {
        session.pane_history_writes_blocked = true;
        log::warn!("Cannot restore completed pane history: {error}");
    }
}

impl PaneHistory {
    pub(crate) fn bind_layout(&mut self, layout: &Path, snapshot: &SessionsSnapshot) {
        let root = archive_root(layout);
        if self.root != root {
            self.root = root;
            self.panes.clear();
        }
        let mut previous = std::mem::take(&mut self.panes);
        self.panes.extend(
            snapshot
                .sessions
                .iter()
                .take(MAX_RESTORED_SESSIONS)
                .filter_map(|session| session.pane_history_id)
                .map(|id| (id, previous.remove(&id).unwrap_or_default())),
        );
    }

    pub(crate) fn tick(&mut self, sessions: &[Session], owner: &Arc<InstanceLock>) {
        if let Some(write) = self.pending.as_ref() {
            match write.result.try_recv() {
                Ok(result) => self.finish(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.finish(Err(io::Error::other("history writer exited")))
                }
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
        self.start_next(sessions, owner, false);
    }

    pub(crate) fn flush(
        &mut self,
        sessions: &[Session],
        owner: &Arc<InstanceLock>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(write) = self.pending.as_ref() {
                match write
                    .result
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(Ok(())) => self.finish(Ok(())),
                    Ok(Err(error)) => {
                        self.finish(Err(error));
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.finish(Err(io::Error::other("history writer exited")));
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            if !self.start_next(sessions, owner, true) {
                return;
            }
        }
        log::warn!("Completed pane history did not finish saving before exit");
    }

    fn start_next(&mut self, sessions: &[Session], owner: &Arc<InstanceLock>, force: bool) -> bool {
        let Some(root) = self.root.as_ref() else {
            return false;
        };
        let start = sessions
            .iter()
            .position(|session| Some(session.pane_history_id) == self.last_requested)
            .map_or(0, |index| index + 1);
        let now = Instant::now();
        for session in sessions.iter().cycle().skip(start).take(sessions.len()) {
            let pane = session.pane_history_id;
            let Some(progress) = self.panes.get_mut(&pane) else {
                continue;
            };
            let revision = session.terminal.completed_history_revision();
            if session.pane_history_writes_blocked
                || progress.blocked
                || revision == progress.written
                || (!force && progress.retry_after.is_some_and(|due| now < due))
            {
                continue;
            }
            let checkpoint = match session
                .terminal
                .completed_history_checkpoint(&pane.to_string())
            {
                Ok(checkpoint) => checkpoint,
                Err(error) => {
                    progress.blocked = true;
                    log::warn!("Cannot capture completed pane history: {error}");
                    continue;
                }
            };
            let path = root.join(format!("{pane}.json"));
            let (send, result) = mpsc::channel();
            let owner = owner.clone();
            match std::thread::Builder::new()
                .name("frost-pane-history".into())
                .spawn(move || {
                    let status = checkpoint.save(&path);
                    drop(owner);
                    let _ = send.send(status);
                }) {
                Ok(_) => {
                    self.last_requested = Some(pane);
                    self.pending = Some(Write {
                        root: root.clone(),
                        pane,
                        revision,
                        result,
                    });
                    return true;
                }
                Err(error) => {
                    progress.retry_after = Some(now + Duration::from_secs(5));
                    log::warn!("Cannot start completed pane history writer: {error}");
                    return false;
                }
            }
        }
        false
    }

    fn finish(&mut self, result: io::Result<()>) {
        let write = self.pending.take().expect("a history write is pending");
        if self.root.as_ref() != Some(&write.root) {
            return;
        }
        let Some(progress) = self.panes.get_mut(&write.pane) else {
            return;
        };
        match result {
            Ok(()) => {
                progress.written = write.revision;
                progress.retry_after = None;
            }
            Err(error) => {
                progress.blocked = matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData
                        | io::ErrorKind::Unsupported
                        | io::ErrorKind::AlreadyExists
                        | io::ErrorKind::PermissionDenied
                        | io::ErrorKind::FileTooLarge
                );
                progress.retry_after = Some(Instant::now() + Duration::from_secs(5));
                log::warn!("Cannot save completed pane history: {error}");
            }
        }
    }
}
