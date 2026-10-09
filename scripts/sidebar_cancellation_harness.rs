#![allow(dead_code)]
// The worker below this harness is copied verbatim by test-sidebar-cancellation.py.
// This deterministic backend publishes another actor's directory immediately
// before cancelling/failing the transfer. Only an unauthorized cleanup delete
// can remove its marker.
mod jterm_core {
    pub mod jsh_remote {
        pub struct RemoteHostConfig;
    }
}
mod remote_fs {
    use super::jterm_core::jsh_remote::RemoteHostConfig;
    use std::{
        io,
        path::Path,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FsLocation {
        Local,
        Remote(usize),
    }
    impl FsLocation {
        pub fn is_remote(&self) -> bool {
            matches!(self, Self::Remote(_))
        }
    }
    #[derive(Default)]
    pub struct TransferProgress(AtomicBool);
    impl TransferProgress {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        pub fn cancel(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
        pub fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    pub fn same_files_location(a: &FsLocation, b: &FsLocation, _: &[RemoteHostConfig]) -> bool {
        a == b
    }
    pub fn preferred_same_files_execution_location<'a>(
        a: &'a FsLocation,
        _: &FsLocation,
        _: &[RemoteHostConfig],
    ) -> Option<&'a FsLocation> {
        Some(a)
    }
    pub fn create_file(_: &FsLocation, _: &[RemoteHostConfig], p: &Path) -> io::Result<()> {
        std::fs::write(p, b"")
    }
    pub fn create_dir(_: &FsLocation, _: &[RemoteHostConfig], p: &Path) -> io::Result<()> {
        std::fs::create_dir(p)
    }
    pub fn rename(_: &FsLocation, _: &[RemoteHostConfig], a: &Path, b: &Path) -> io::Result<()> {
        std::fs::rename(a, b)
    }
    pub fn copy(_: &FsLocation, _: &[RemoteHostConfig], a: &Path, b: &Path) -> io::Result<()> {
        std::fs::copy(a, b).map(drop)
    }
    pub fn delete(_: &FsLocation, _: &[RemoteHostConfig], p: &Path) -> io::Result<()> {
        if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        }
    }
    pub fn transfer(
        _: &FsLocation,
        _: &FsLocation,
        _: &[RemoteHostConfig],
        _: &Path,
        dst: &Path,
        _: bool,
        progress: Option<&Arc<TransferProgress>>,
    ) -> io::Result<()> {
        std::fs::create_dir(dst)?;
        std::fs::write(dst.join("unrelated-owner"), b"preserve me")?;
        if let Some(progress) = progress {
            progress.cancel();
        }
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cancelled after another creator won",
        ))
    }
}
fn run_case(cancel_before: bool, cancel_during: bool) {
    let root = std::env::temp_dir().join(format!(
        "frost-sidebar-race-{}-{}",
        std::process::id(),
        if cancel_before {
            "before"
        } else if cancel_during {
            "during"
        } else {
            "failed"
        }
    ));
    std::fs::create_dir(&root).unwrap();
    let dst = root.join("raced-target");
    let progress = remote_fs::TransferProgress::new();
    if cancel_before {
        progress.cancel();
    }
    let op = SidebarOp::BatchTransfer {
        src_loc: remote_fs::FsLocation::Local,
        items: vec![DropItem {
            src: root.join("source"),
            dst: dst.clone(),
            is_dir: true,
            error: None,
        }],
        cut: false,
        verb: "Imported",
    };
    let outcome = run_sidebar_op(
        &remote_fs::FsLocation::Remote(0),
        &[],
        &op,
        (cancel_before || cancel_during).then_some(&progress),
    );
    if cancel_before {
        assert!(outcome.cancelled);
        assert!(!dst.exists());
    } else {
        assert_eq!(outcome.cancelled, cancel_during);
        let marker = std::fs::read(dst.join("unrelated-owner"));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            marker.unwrap(),
            b"preserve me",
            "a failed/cancelled upload owns no destination to delete"
        );
        return;
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn cancelled_directory_upload_preserves_raced_destination() {
    run_case(false, true);
}
#[test]
fn failed_directory_upload_preserves_raced_destination() {
    run_case(false, false);
}
#[test]
fn cancelled_queued_upload_never_starts() {
    run_case(true, false);
}
