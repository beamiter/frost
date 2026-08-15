//! Small-file persistence primitives shared by configuration state.
//!
//! Revisions retain the exact bytes that were loaded. Writers take a sibling
//! advisory lock, compare that exact revision, then use this module's private,
//! durable atomic replacement. This prevents two frost processes from
//! silently overwriting one another while keeping failed writes off the live
//! destination path.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_API_KEY_FILE_BYTES: u64 = 16 * 1024;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Exact content identity for optimistic concurrency checks.
///
/// `Debug` deliberately exposes only the byte count: configuration bytes may
/// contain private host names and paths even though API key material is kept
/// in a separate credential file.
#[derive(Clone, PartialEq, Eq)]
pub enum FileRevision {
    Missing,
    Present(Box<[u8]>),
}

impl FileRevision {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self::Present(bytes.to_vec().into_boxed_slice())
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present(bytes) => Some(bytes),
        }
    }
}

impl fmt::Debug for FileRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Present(bytes) => formatter
                .debug_struct("Present")
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

#[derive(Debug)]
pub enum AtomicWriteError {
    Conflict {
        path: PathBuf,
    },
    Locked {
        path: PathBuf,
    },
    RevisionUnavailable {
        path: PathBuf,
    },
    UnsafeSymlink {
        path: PathBuf,
    },
    /// The rename is visible, but syncing its parent directory failed. Callers
    /// must adopt this revision in memory while retaining dirty/retry state.
    DurabilityUncertain {
        path: PathBuf,
        revision: FileRevision,
        detail: String,
    },
    Io(String),
}

impl AtomicWriteError {
    pub fn blocks_automatic_writes(&self) -> bool {
        matches!(
            self,
            Self::Conflict { .. } | Self::RevisionUnavailable { .. } | Self::UnsafeSymlink { .. }
        )
    }

    pub fn committed_revision(&self) -> Option<&FileRevision> {
        match self {
            Self::DurabilityUncertain { revision, .. } => Some(revision),
            _ => None,
        }
    }
}

impl fmt::Display for AtomicWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { path } => write!(
                formatter,
                "{} changed in another window or editor; reload or Reset before saving",
                path.display()
            ),
            Self::Locked { path } => write!(
                formatter,
                "timed out waiting for the persistence lock {}",
                path.display()
            ),
            Self::RevisionUnavailable { path } => write!(
                formatter,
                "cannot safely save {} because its loaded revision is unavailable",
                path.display()
            ),
            Self::UnsafeSymlink { path } => write!(
                formatter,
                "refusing to replace symbolic link {}; use a regular file",
                path.display()
            ),
            Self::DurabilityUncertain { path, detail, .. } => write!(
                formatter,
                "{} was replaced, but its directory sync failed ({detail}); retry to make it durable",
                path.display()
            ),
            Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AtomicWriteError {}

fn io_error(operation: &str, path: &Path, error: impl fmt::Display) -> AtomicWriteError {
    AtomicWriteError::Io(format!("{operation} {}: {error}", path.display()))
}

/// Read one regular file into an exact, bounded revision. A nonblocking open
/// keeps a malicious FIFO at a config path from freezing the UI thread.
pub fn read_revision(path: &Path, max_bytes: u64) -> io::Result<FileRevision> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(FileRevision::Missing);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must have exactly one hard link", path.display()),
            ));
        }
        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not owned by the current user", path.display()),
            ));
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must not be group- or world-writable", path.display()),
            ));
        }
    }
    if metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds {max_bytes} bytes", path.display()),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds {max_bytes} bytes", path.display()),
        ));
    }
    Ok(FileRevision::from_bytes(&bytes))
}

/// Read a bounded UTF-8 persistence file through the same no-follow,
/// owner-only, singly-linked descriptor checks as revision tracking.
pub fn read_text_bounded(path: &Path, max_bytes: u64) -> io::Result<String> {
    match read_revision(path, max_bytes)? {
        FileRevision::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", path.display()),
        )),
        FileRevision::Present(bytes) => String::from_utf8(bytes.into_vec()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not valid UTF-8", path.display()),
            )
        }),
    }
}

fn expand_private_path(raw_path: &str) -> io::Result<PathBuf> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "credential path is empty",
        ));
    }
    if raw_path == "~" || raw_path.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "HOME is unavailable for ~/ credential path",
                )
            })?;
        let mut path = PathBuf::from(home);
        if let Some(rest) = raw_path.strip_prefix("~/") {
            path.push(rest);
        }
        return Ok(path);
    }
    let path = PathBuf::from(raw_path);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "credential path must be absolute or begin with ~/",
        ));
    }
    Ok(path)
}

fn open_private_key_file(path: &Path) -> io::Result<(fs::File, fs::Metadata)> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular credential file", path.display()),
        ));
    }
    if metadata.len() > MAX_API_KEY_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!(
                "{} exceeds the {MAX_API_KEY_FILE_BYTES}-byte credential limit",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} must be owned by the current user and have exactly one hard link",
                    path.display()
                ),
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} must not be accessible by group or other users",
                    path.display()
                ),
            ));
        }
    }
    Ok((file, metadata))
}

/// Read one configured API key without following links or blocking on a FIFO.
/// The pinned core revision predates these descriptor-level credential checks,
/// so frontends keep this local guard until they can pin a published fix.
pub fn read_api_key_file(raw_path: &str) -> io::Result<String> {
    let path = expand_private_path(raw_path)?;
    let (file, metadata) = open_private_key_file(&path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_API_KEY_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_API_KEY_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!(
                "{} exceeds the {MAX_API_KEY_FILE_BYTES}-byte credential limit",
                path.display()
            ),
        ));
    }
    let contents = String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not valid UTF-8", path.display()),
        )
    })?;
    let key = contents.trim();
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is empty", path.display()),
        ));
    }
    if key.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} must contain one line without control characters",
                path.display()
            ),
        ));
    }
    Ok(key.to_string())
}

/// Store one settings-entered key using the bounded, locked private snapshot
/// writer. Unsafe legacy entries are rejected rather than followed or silently
/// adopted.
pub fn write_api_key_file(raw_path: &str, raw_key: &str) -> io::Result<()> {
    let path = expand_private_path(raw_path)?;
    let key = raw_key.trim();
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "API key must not be empty",
        ));
    }
    if key.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "API key must be one line without control characters",
        ));
    }
    if key.len() as u64 + 1 > MAX_API_KEY_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("API key exceeds {} bytes", MAX_API_KEY_FILE_BYTES - 1),
        ));
    }
    match open_private_key_file(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut encoded = Vec::with_capacity(key.len() + 1);
    encoded.extend_from_slice(key.as_bytes());
    encoded.push(b'\n');
    write_snapshot_atomic(&path, &encoded, MAX_API_KEY_FILE_BYTES)
}

/// Validate the configured command-history handoff before entering core's
/// background writer/reader. The policy now lives in the pinned core as
/// `jterm_core::command_history::prepare_path`: the immediate parent is
/// owner-controlled, and existing history/lock entries are descriptor-checked
/// without following links or blocking on FIFOs.
pub fn prepare_command_history_path(path: &Path, for_write: bool) -> io::Result<()> {
    jterm_core::command_history::prepare_path(path, for_write)
}

/// Atomically replace a private snapshot without chmodding a configured
/// parent such as `$HOME`. A missing final parent is created private; an
/// existing one is validated through a no-follow directory descriptor, and a
/// group/world-writable boundary such as `/tmp` is rejected.
pub fn write_snapshot_atomic(path: &Path, contents: &[u8], max_bytes: u64) -> io::Result<()> {
    if contents.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds {max_bytes} bytes", path.display()),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", path.display()),
        )
    })?;
    if !parent.as_os_str().is_empty() {
        match fs::symlink_metadata(parent) {
            Ok(_) => drop(open_snapshot_parent(parent)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_private_snapshot_parent(parent)?;
            }
            Err(error) => return Err(error),
        }
    }
    let directory = open_snapshot_parent(if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    })?;
    lock_with_timeout(&directory, path)?;
    atomic_replace_with_parent(path, contents, &directory)
}

/// Create one new owner-private file without ever replacing an existing path.
///
/// Session exports use timestamped names and may be requested concurrently by
/// multiple frost windows. Bytes first land in an owner-only staging inode in
/// the destination directory; only after `fsync` does an atomic, no-replace
/// hard-link publish the final name. A crash can therefore leave a hidden
/// staging file, but never a visible truncated export. On Unix every staging,
/// publish, and cleanup operation is relative to the already validated parent
/// descriptor, so replacing the parent pathname cannot redirect the write.
pub fn write_new_private_file(path: &Path, contents: &[u8], max_bytes: u64) -> io::Result<()> {
    if contents.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds {max_bytes} bytes", path.display()),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", path.display()),
        )
    })?;
    if !parent.as_os_str().is_empty() {
        match fs::symlink_metadata(parent) {
            Ok(_) => drop(open_snapshot_parent(parent)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_private_snapshot_parent(parent)?;
            }
            Err(error) => return Err(error),
        }
    }
    let directory = open_snapshot_parent(if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    })?;
    let destination = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        )
    })?;
    publish_new_private_file(&directory, parent, destination, contents)
}

#[cfg(unix)]
fn publish_new_private_file(
    directory: &fs::File,
    _parent: &Path,
    destination: &std::ffi::OsStr,
    contents: &[u8],
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    fn component(name: &std::ffi::OsStr) -> io::Result<CString> {
        if name.as_bytes().contains(&b'/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private file name must be one path component",
            ));
        }
        CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "private file name contains a NUL byte",
            )
        })
    }

    fn unlink_at(directory: &fs::File, name: &CString) -> io::Result<()> {
        // SAFETY: `directory` remains open for this call and `name` is a
        // NUL-terminated single path component owned by this function.
        let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    let destination = component(destination)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // Descriptor-relative preflight avoids rewriting a potentially large
    // staging file for an ordinary timestamp collision. It is only an
    // optimization: `linkat` below remains the authoritative atomic check.
    // SAFETY: the directory fd/name are valid and `metadata` points to enough
    // writable storage for `fstatat`.
    let exists = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            destination.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if exists == 0 {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    let lookup_error = io::Error::last_os_error();
    if lookup_error.kind() != io::ErrorKind::NotFound {
        return Err(lookup_error);
    }

    let mut opened = None;
    for _ in 0..128 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let staging_name = CString::new(format!(".frost-export.tmp.{}.{}", std::process::id(), id))
            .expect("generated staging name has no NUL");
        // SAFETY: the directory fd and CString are valid for the call. The
        // returned fd is uniquely owned and converted into `File` exactly once.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                staging_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd >= 0 {
            // SAFETY: `openat` returned a fresh owned descriptor on success.
            opened = Some((unsafe { fs::File::from_raw_fd(fd) }, staging_name));
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let (mut staging, staging_name) = opened.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique export staging file",
        )
    })?;

    if let Err(error) = staging.write_all(contents).and_then(|_| staging.sync_all()) {
        drop(staging);
        let _ = unlink_at(directory, &staging_name);
        return Err(error);
    }
    drop(staging);

    // `linkat` is an atomic no-replace publish: an existing regular file,
    // symlink, FIFO, or racing peer all produce EEXIST and remain untouched.
    // SAFETY: both names are valid C strings and both directory fds stay open.
    let published = unsafe {
        libc::linkat(
            directory.as_raw_fd(),
            staging_name.as_ptr(),
            directory.as_raw_fd(),
            destination.as_ptr(),
            0,
        )
    };
    if published != 0 {
        let error = io::Error::last_os_error();
        let _ = unlink_at(directory, &staging_name);
        return Err(error);
    }

    let _ = unlink_at(directory, &staging_name);
    if let Err(error) = directory.sync_all() {
        // The visible name is complete (it was linked only after the staging
        // inode's fsync), but the directory entry is not durably committed.
        // Best-effort rollback keeps an ordinary error from masquerading as a
        // successful export that merely chose a different collision suffix.
        let _ = unlink_at(directory, &destination);
        let _ = directory.sync_all();
        return Err(error);
    }
    Ok(())
}

#[cfg(not(unix))]
fn publish_new_private_file(
    directory: &fs::File,
    parent: &Path,
    destination: &std::ffi::OsStr,
    contents: &[u8],
) -> io::Result<()> {
    let path = parent.join(destination);
    match fs::symlink_metadata(&path) {
        Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let (mut staging, staging_path) = create_unique_temp(&path)?;
    let mut cleanup = TempFileGuard {
        path: staging_path.clone(),
        committed: false,
    };
    staging.write_all(contents)?;
    staging.sync_all()?;
    drop(staging);
    fs::hard_link(&staging_path, &path)?;
    fs::remove_file(&staging_path)?;
    cleanup.committed = true;
    directory.sync_all()
}

/// Create or tighten one application-owned directory to owner-only access.
/// The final path is opened without following symlinks and must be owned by
/// the current user; callers should use this only for product-managed paths,
/// never for a user-selected parent such as `$HOME` itself.
pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            // This is the one caller allowed to repair an application-owned
            // directory that is currently too permissive. Open/ownership
            // checks happen on the descriptor before chmod; ordinary
            // persistence callers continue to reject writable parents.
            let directory = open_owned_directory(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                directory.set_permissions(fs::Permissions::from_mode(0o700))?;
            }
            directory.sync_all()
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_snapshot_parent(path)
        }
        Err(error) => Err(error),
    }
}

fn open_owned_directory(parent: &Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", parent.display()),
            ));
        }
        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not owned by the current user", parent.display()),
            ));
        }
        Ok(directory)
    }
    #[cfg(not(unix))]
    {
        if fs::metadata(parent)?.is_dir() {
            fs::File::open(parent)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", parent.display()),
            ))
        }
    }
}

fn open_snapshot_parent(parent: &Path) -> io::Result<fs::File> {
    let directory = open_owned_directory(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if directory.metadata()?.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must not be group- or world-writable", parent.display()),
            ));
        }
    }
    Ok(directory)
}

fn create_private_snapshot_parent(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)?;
        let metadata = directory.metadata()?;
        // SAFETY: geteuid has no preconditions and only reads process state.
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a private directory we own", parent.display()),
            ));
        }
        directory.set_permissions(fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(parent)
    }
}

struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_unique_temp(path: &Path) -> io::Result<(fs::File, PathBuf)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
    })?;
    for _ in 0..128 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(destination);
        name.push(format!(".tmp.{}.{id}", std::process::id()));
        let temp_path = parent.join(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((file, temp_path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique persistence staging file",
    ))
}

fn atomic_replace_with_parent(
    path: &Path,
    contents: &[u8],
    directory: &fs::File,
) -> io::Result<()> {
    let (mut file, temp_path) = create_unique_temp(path)?;
    let mut cleanup = TempFileGuard {
        path: temp_path.clone(),
        committed: false,
    };
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp_path, path)?;
    cleanup.committed = true;
    directory.sync_all()
}

fn lock_path_for(path: &Path) -> Result<PathBuf, AtomicWriteError> {
    let parent = path.parent().ok_or_else(|| {
        AtomicWriteError::Io(format!("{} has no parent directory", path.display()))
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| AtomicWriteError::Io(format!("{} has no file name", path.display())))?;
    let mut lock_name = std::ffi::OsString::from(".");
    lock_name.push(name);
    lock_name.push(".lock");
    Ok(parent.join(lock_name))
}

fn ensure_private_parent(path: &Path) -> Result<(), AtomicWriteError> {
    let parent = path.parent().ok_or_else(|| {
        AtomicWriteError::Io(format!("{} has no parent directory", path.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|error| io_error("create directory", parent, error))?;
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|error| io_error("open directory", parent, error))?;
        let metadata = directory
            .metadata()
            .map_err(|error| io_error("inspect directory", parent, error))?;
        if !metadata.is_dir() {
            return Err(AtomicWriteError::Io(format!(
                "{} is not a directory",
                parent.display()
            )));
        }
        // SAFETY: geteuid has no preconditions and only returns process state.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AtomicWriteError::Io(format!(
                "{} is not owned by the current user",
                parent.display()
            )));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            directory
                .set_permissions(fs::Permissions::from_mode(0o700))
                .map_err(|error| io_error("set private permissions on", parent, error))?;
        }
    }
    #[cfg(not(unix))]
    fs::create_dir_all(parent).map_err(|error| io_error("create directory", parent, error))?;
    Ok(())
}

fn open_lock_file(path: &Path) -> Result<fs::File, AtomicWriteError> {
    let open = |create_new: bool| {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(create_new);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        options.open(path)
    };
    let file = match open(true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open(false).map_err(|error| io_error("open lock", path, error))?
        }
        Err(error) => return Err(io_error("create lock", path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect lock", path, error))?;
    if !metadata.is_file() {
        return Err(AtomicWriteError::Io(format!(
            "{} is not a regular lock file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions and only returns process state.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(AtomicWriteError::Io(format!(
                "{} is not a private, singly-linked lock file owned by the current user",
                path.display()
            )));
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn try_lock(file: &fs::File) -> io::Result<bool> {
    // SAFETY: file owns a live descriptor for the duration of this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK)
    {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn try_lock(_file: &fs::File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "persistence locking is only supported on Unix",
    ))
}

fn lock_with_timeout(file: &fs::File, path: &Path) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match try_lock(file)? {
            true => return Ok(()),
            false if started.elapsed() < LOCK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            false => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out waiting for persistence lock {}", path.display()),
                ));
            }
        }
    }
}

struct FileLock {
    file: fs::File,
    _directory: fs::File,
}

impl FileLock {
    fn acquire(path: &Path) -> Result<Self, AtomicWriteError> {
        ensure_private_parent(path)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = open_snapshot_parent(parent)
            .map_err(|error| io_error("open parent of", path, error))?;
        match lock_with_timeout(&directory, parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return Err(AtomicWriteError::Locked {
                    path: parent.to_path_buf(),
                });
            }
            Err(error) => return Err(io_error("lock directory", parent, error)),
        }
        let lock_path = lock_path_for(path)?;
        let file = open_lock_file(&lock_path)?;
        let start = Instant::now();
        loop {
            match try_lock(&file) {
                Ok(true) => {
                    return Ok(Self {
                        file,
                        _directory: directory,
                    });
                }
                Ok(false) if start.elapsed() < LOCK_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Ok(false) => return Err(AtomicWriteError::Locked { path: lock_path }),
                Err(error) => return Err(io_error("lock", &lock_path, error)),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the descriptor remains live until after Drop returns.
        if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            log::warn!(
                "Failed to release persistence lock: {}",
                io::Error::last_os_error()
            );
        }
    }
}

/// Acquire a long-lived nonblocking process lock at an exact path. The file is
/// opened with the same owner/mode/symlink/hardlink checks as transactional
/// write locks; dropping the returned File releases the advisory lock.
pub fn try_acquire_process_lock(path: &Path) -> Result<Option<fs::File>, AtomicWriteError> {
    ensure_private_parent(path)?;
    let file = open_lock_file(path)?;
    match try_lock(&file) {
        Ok(true) => Ok(Some(file)),
        Ok(false) => Ok(None),
        Err(error) => Err(io_error("lock", path, error)),
    }
}

fn reject_destination_symlink(path: &Path) -> Result<(), AtomicWriteError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(AtomicWriteError::UnsafeSymlink {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect", path, error)),
    }
}

fn replace_private(
    path: &Path,
    contents: &[u8],
    max_bytes: u64,
) -> Result<FileRevision, AtomicWriteError> {
    if contents.len() as u64 > max_bytes {
        return Err(AtomicWriteError::Io(format!(
            "refusing to write {}: serialized contents exceed {max_bytes} bytes",
            path.display()
        )));
    }
    let intended = FileRevision::from_bytes(contents);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = match open_snapshot_parent(parent) {
        Ok(directory) => directory,
        Err(error) => return Err(io_error("open parent of", path, error)),
    };
    match atomic_replace_with_parent(path, contents, &directory) {
        Ok(()) => Ok(intended),
        Err(error) => {
            // The shared primitive fsyncs after rename. If only that final
            // directory sync failed, the new inode is already visible. Carry
            // its exact revision so memory never remains on the old version.
            if persistence_revision_matches(path, max_bytes, &intended) {
                return Err(AtomicWriteError::DurabilityUncertain {
                    path: path.to_path_buf(),
                    revision: intended,
                    detail: error.to_string(),
                });
            }
            Err(io_error("replace", path, error))
        }
    }
}

fn persistence_revision_matches(path: &Path, max_bytes: u64, expected: &FileRevision) -> bool {
    read_revision(path, max_bytes)
        .map(|actual| &actual == expected)
        .unwrap_or(false)
}

pub fn write_atomic_private_if_unchanged(
    path: &Path,
    contents: &[u8],
    expected: Option<&FileRevision>,
    max_bytes: u64,
) -> Result<FileRevision, AtomicWriteError> {
    let _lock = FileLock::acquire(path)?;
    let Some(expected) = expected else {
        return Err(AtomicWriteError::RevisionUnavailable {
            path: path.to_path_buf(),
        });
    };
    let actual = read_revision(path, max_bytes)
        .map_err(|error| io_error("read current revision of", path, error))?;
    if &actual != expected {
        return Err(AtomicWriteError::Conflict {
            path: path.to_path_buf(),
        });
    }
    reject_destination_symlink(path)?;
    replace_private(path, contents, max_bytes)
}

pub fn write_atomic_private_force(
    path: &Path,
    contents: &[u8],
    max_bytes: u64,
) -> Result<FileRevision, AtomicWriteError> {
    let _lock = FileLock::acquire(path)?;
    reject_destination_symlink(path)?;
    replace_private(path, contents, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "frost-persistence-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn revisions_never_follow_symlinks_or_accept_hard_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = Scratch::new("revision-links");
        let victim = root.0.join("victim.toml");
        let symlink_path = root.0.join("symlink.toml");
        let hard_link_path = root.0.join("hard-link.toml");
        fs::write(&victim, b"font_size = 99\n").unwrap();
        symlink(&victim, &symlink_path).unwrap();
        fs::hard_link(&victim, &hard_link_path).unwrap();

        assert!(read_revision(&symlink_path, 4096).is_err());
        assert!(read_revision(&hard_link_path, 4096).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"font_size = 99\n");

        let writable_path = root.0.join("writable.toml");
        fs::write(&writable_path, b"font_size = 1\n").unwrap();
        fs::set_permissions(&writable_path, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            read_revision(&writable_path, 4096).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_revision_is_rejected_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let root = Scratch::new("revision-fifo");
        let path = root.0.join("config.toml");
        let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: encoded is a live NUL-terminated pathname and the mode has
        // no invalid bit pattern.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let started = Instant::now();
        assert!(read_revision(&path, 4096).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn api_key_io_is_private_bounded_and_rejects_links_and_fifos() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = Scratch::new("api-key");
        let path = root.0.join("ai.key");
        write_api_key_file(path.to_str().unwrap(), "  sk-secret  ").unwrap();
        assert_eq!(
            read_api_key_file(path.to_str().unwrap()).unwrap(),
            "sk-secret"
        );
        assert_eq!(fs::read(&path).unwrap(), b"sk-secret\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            read_api_key_file(path.to_str().unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(write_api_key_file(path.to_str().unwrap(), "replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"sk-secret\n");

        let victim = root.0.join("victim.key");
        fs::write(&victim, b"victim\n").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
        let linked = root.0.join("linked.key");
        symlink(&victim, &linked).unwrap();
        assert!(read_api_key_file(linked.to_str().unwrap()).is_err());
        assert!(write_api_key_file(linked.to_str().unwrap(), "replacement").is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"victim\n");

        let hard_linked = root.0.join("hard-linked.key");
        fs::hard_link(&victim, &hard_linked).unwrap();
        assert!(read_api_key_file(hard_linked.to_str().unwrap()).is_err());
        assert!(write_api_key_file(hard_linked.to_str().unwrap(), "replacement").is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"victim\n");

        let fifo = root.0.join("fifo.key");
        let encoded = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: encoded is a live NUL-terminated pathname for this call.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(read_api_key_file(fifo.to_str().unwrap()).is_err());
        assert!(write_api_key_file(fifo.to_str().unwrap(), "replacement").is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn persistence_never_chmods_or_writes_through_a_parent_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = Scratch::new("parent-symlink");
        let victim = root.0.join("victim");
        let linked_parent = root.0.join("config");
        fs::create_dir(&victim).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&victim, &linked_parent).unwrap();

        assert!(write_atomic_private_force(
            &linked_parent.join("config.toml"),
            b"font_size = 14\n",
            4096,
        )
        .is_err());
        assert!(write_snapshot_atomic(&linked_parent.join("session.json"), b"{}", 4096,).is_err());
        assert!(!victim.join("config.toml").exists());
        assert!(!victim.join("session.json").exists());
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_write_preserves_an_existing_shared_parents_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = Scratch::new("shared-parent");
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755)).unwrap();
        let path = root.0.join("session.json");

        write_snapshot_atomic(&path, b"{}", 4096).unwrap();

        assert_eq!(
            fs::metadata(&root.0).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_write_rejects_a_writable_parent_without_chmodding_it() {
        use std::os::unix::fs::PermissionsExt;

        let root = Scratch::new("writable-parent");
        let parent = root.0.join("shared");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();

        assert!(write_snapshot_atomic(&parent.join("state.json"), b"{}", 4096).is_err());
        assert!(!parent.join("state.json").exists());
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o777
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_lock_survives_lock_filename_replacement() {
        let root = Scratch::new("directory-lock");
        let path = root.0.join("config.toml");
        let guard = FileLock::acquire(&path).unwrap();
        let lock_path = lock_path_for(&path).unwrap();
        let moved_lock = root.0.join("moved.lock");
        fs::rename(&lock_path, &moved_lock).unwrap();
        fs::write(&lock_path, b"replacement").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let result = write_atomic_private_force(&writer_path, b"font_size = 16\n", 4096);
            tx.send(result).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(guard);
        assert!(rx.recv_timeout(Duration::from_secs(3)).unwrap().is_ok());
        writer.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"font_size = 16\n");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_snapshot_writers_publish_one_complete_generation() {
        let root = Scratch::new("snapshot-concurrency");
        let path = root.0.join("session.json");
        let mut writers = Vec::new();
        for byte in b'a'..=b'h' {
            let path = path.clone();
            writers.push(std::thread::spawn(move || {
                write_snapshot_atomic(&path, &vec![byte; 64 * 1024], 128 * 1024)
            }));
        }

        let mut completed = 0;
        for writer in writers {
            match writer.join().expect("snapshot writer panicked") {
                Ok(()) => completed += 1,
                // Every successful writer holds one directory flock across a
                // file fsync, rename and directory fsync. Those durable writes
                // are intentionally serial, so on a saturated filesystem the
                // two-second production deadline may expire for a writer at
                // the back of this eight-way race. That is the lock contract,
                // not evidence of a torn publish or a leaked lock.
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => panic!("snapshot writer failed unexpectedly: {error}"),
            }
        }
        assert!(completed > 0, "no concurrent snapshot writer completed");

        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 64 * 1024);
        assert!(matches!(bytes[0], b'a'..=b'h'));
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);

        // A timeout above is acceptable only as bounded contention. Once all
        // owners have returned, the same production acquisition path must be
        // usable again; this keeps the relaxed race assertion from hiding a
        // descriptor/lock leak.
        write_snapshot_atomic(&path, b"lock released", 128 * 1024)
            .expect("snapshot lock remained held after every writer returned");
        assert_eq!(fs::read(&path).unwrap(), b"lock released");
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }
}
