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

use std::path::{Path, PathBuf};

use jterm_core::jsh_remote::RemoteHostConfig;

use crate::remote_fs::{self, FsLocation};

/// Loading lifecycle for a directory node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryState {
    Unloaded,
    Loading,
    Loaded,
    Error(String),
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
        }
    }
}

/// A filesystem request created by [`Sidebar`]. `generation` prevents a slow
/// response for an old cwd from replacing the tree after the user navigates.
/// `location` + `hosts` snapshot where the read happens, so a config edit
/// mid-flight cannot redirect an already-issued request to another host.
#[derive(Clone, Debug)]
pub struct DirectoryRequest {
    pub generation: u64,
    pub path: PathBuf,
    pub location: FsLocation,
    pub hosts: Vec<RemoteHostConfig>,
}

/// Worker result consumed by [`Sidebar::apply_load`].
#[derive(Clone, Debug)]
pub struct DirectoryResult {
    pub generation: u64,
    pub path: PathBuf,
    pub entries: Result<Vec<FileTreeNode>, String>,
}

/// File-sidebar state.
#[derive(Clone, Debug)]
pub struct Sidebar {
    pub current_dir: PathBuf,
    pub root: FileTreeNode,
    /// Where the tree is rooted: this machine or one of `hosts`.
    pub location: FsLocation,
    /// Snapshot of `config.remote_hosts`, kept in sync by the UI; indices in
    /// [`FsLocation::Remote`] resolve against it.
    hosts: Vec<RemoteHostConfig>,
    generation: u64,
}

impl Sidebar {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            root: FileTreeNode::directory(current_dir.clone(), true),
            current_dir,
            location: FsLocation::Local,
            hosts: Vec::new(),
            generation: 0,
        }
    }

    /// Replace the remote-host snapshot (called when the config changes).
    pub fn set_hosts(&mut self, hosts: Vec<RemoteHostConfig>) {
        self.hosts = hosts;
    }

    /// The snapshot every request and file operation must travel with.
    pub fn hosts_snapshot(&self) -> &[RemoteHostConfig] {
        &self.hosts
    }

    /// Begin switching the tree to `location`. The generation bump drops
    /// every in-flight load for the old location; the new start directory is
    /// resolved asynchronously and applied through [`Sidebar::resolve_location`].
    pub fn begin_location_change(&mut self, location: FsLocation) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.location = location;
        self.root.children.clear();
        self.root.state = DirectoryState::Loading;
        self.generation
    }

    /// Apply an asynchronously resolved start directory for a pending
    /// location change. Returns `None` for stale resolutions and for failures
    /// (the root then shows the error instead of a tree).
    pub fn resolve_location(
        &mut self,
        generation: u64,
        start: Result<PathBuf, String>,
    ) -> Option<DirectoryRequest> {
        if generation != self.generation {
            return None;
        }
        match start {
            Ok(dir) => Some(self.set_current_dir(dir)),
            Err(error) => {
                self.root = FileTreeNode::directory(self.current_dir.clone(), true);
                self.root.state = DirectoryState::Error(error);
                None
            }
        }
    }

    /// Point the tree at a new root and return the one-level load request.
    pub fn set_current_dir(&mut self, path: PathBuf) -> DirectoryRequest {
        self.generation = self.generation.wrapping_add(1);
        self.current_dir = path.clone();
        self.root = FileTreeNode::directory(path, true);
        self.begin_load_root()
    }

    /// Load the initial root without changing its generation.
    pub fn begin_load_root(&mut self) -> DirectoryRequest {
        self.root.state = DirectoryState::Loading;
        self.request_for(self.root.path.clone())
    }

    /// A request for one directory level, stamped with the current
    /// generation, location, and host snapshot.
    fn request_for(&self, path: PathBuf) -> DirectoryRequest {
        DirectoryRequest {
            generation: self.generation,
            path,
            location: self.location.clone(),
            hosts: self.hosts.clone(),
        }
    }

    /// Toggle a directory and, when necessary, request its first one-level load.
    pub fn toggle_node(&mut self, path: &Path) -> Option<DirectoryRequest> {
        let node = find_node_mut(&mut self.root, path)?;
        if !node.is_dir {
            return None;
        }
        let node_path = node.path.clone();

        match node.state {
            DirectoryState::Unloaded | DirectoryState::Error(_) => {
                node.expanded = true;
                node.state = DirectoryState::Loading;
                Some(self.request_for(node_path))
            }
            DirectoryState::Loading => {
                node.expanded = !node.expanded;
                None
            }
            DirectoryState::Loaded => {
                node.expanded = !node.expanded;
                None
            }
        }
    }

    /// Invalidate outstanding responses and reload the current root.
    pub fn refresh(&mut self) -> DirectoryRequest {
        self.set_current_dir(self.current_dir.clone())
    }

    /// Apply a worker response. Returns `false` for stale or unknown responses.
    pub fn apply_load(&mut self, result: DirectoryResult) -> bool {
        if result.generation != self.generation {
            return false;
        }
        let Some(node) = find_node_mut(&mut self.root, &result.path) else {
            return false;
        };
        match result.entries {
            Ok(entries) => {
                node.children = entries;
                node.state = DirectoryState::Loaded;
            }
            Err(error) => {
                node.children.clear();
                node.state = DirectoryState::Error(error);
            }
        }
        true
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
    let entries = remote_fs::list_dir(&request.location, &request.hosts, &request.path)
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| FileTreeNode::entry(entry.name, entry.path, entry.is_dir))
                .collect()
        })
        .map_err(|error| error.to_string());
    DirectoryResult {
        generation: request.generation,
        path: request.path,
        entries,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn find_node_mut<'a>(node: &'a mut FileTreeNode, path: &Path) -> Option<&'a mut FileTreeNode> {
    if node.path == path {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("frost-sidebar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("nested").join("deep")).expect("create test tree");
        for index in 0..32 {
            std::fs::write(root.join(format!("file-{index:02}.txt")), b"x")
                .expect("write test file");
        }
        root
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
    fn location_change_is_generation_guarded() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let first = sidebar.begin_location_change(FsLocation::Remote(0));
        let second = sidebar.begin_location_change(FsLocation::Local);
        assert_ne!(first, second);
        // A stale resolution for the older change is dropped, the current one
        // re-roots the tree at the resolved start directory.
        assert!(sidebar.resolve_location(first, Ok(root.clone())).is_none());
        let request = sidebar
            .resolve_location(second, Ok(root.clone()))
            .expect("current resolution applies");
        assert!(sidebar.apply_load(load_directory(request)));
        assert_eq!(sidebar.root.path, root);
        assert_eq!(sidebar.location, FsLocation::Local);

        // A failed resolution becomes the root's error state, never a panic.
        let generation = sidebar.begin_location_change(FsLocation::Remote(9));
        assert!(sidebar
            .resolve_location(generation, Err("no such host".to_string()))
            .is_none());
        assert!(matches!(sidebar.root.state, DirectoryState::Error(_)));

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn remote_listing_failure_becomes_an_error_state() {
        let mut sidebar = Sidebar::new();
        let generation = sidebar.begin_location_change(FsLocation::Remote(0));
        let request = sidebar
            .resolve_location(generation, Ok(PathBuf::from("/tmp")))
            .expect("resolution applies");
        // No hosts in the snapshot: the request fails closed and the error
        // lands in the node's error slot.
        let result = load_directory(request);
        assert!(result.entries.is_err());
        assert!(sidebar.apply_load(result));
        assert!(matches!(sidebar.root.state, DirectoryState::Error(_)));
    }
}
