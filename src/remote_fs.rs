//! Sidebar filesystem backend: local paths through `std::fs`, remote hosts by
//! spawning the system `ssh` / `docker` binaries with a small POSIX sh probe
//! script on their stdin (`sh -s -- <op> [args...]`). No sshfs and no extra
//! crates: the far side needs nothing but a Bourne-compatible shell, the same
//! philosophy the jsh-remote-over-ssh sessions already use.
//!
//! Everything here is blocking and is meant to run inside an iced worker task
//! (`Task::perform`), never on the UI update path. Buffers are bounded, argv
//! is validated before spawn, and unknown hosts/locations fail closed.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jterm_core::jsh_remote::RemoteHostConfig;

/// Where the sidebar tree (or a file operation) is rooted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FsLocation {
    /// The machine frost runs on.
    Local,
    /// Index into `config.remote_hosts`.
    Remote(usize),
}

impl FsLocation {
    /// Picker/menu label: `Local`, `ssh: <name>`, or `docker: <name>`.
    pub fn label(&self, hosts: &[RemoteHostConfig]) -> String {
        match self {
            FsLocation::Local => "Local".to_string(),
            FsLocation::Remote(index) => match hosts.get(*index) {
                Some(host) => format!(
                    "{}: {}",
                    if host.docker { "docker" } else { "ssh" },
                    host.display_name()
                ),
                None => format!("Remote #{index}"),
            },
        }
    }
}

/// One directory entry, one level deep (never recursively walked here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Remote-fs probe v1 — runs under `sh -s -- <op> [args...]`.
///
/// `list` stdout is NUL-separated pairs `"<t>\0<name>\0"`, t in {d,f,l}, names
/// relative. Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir,
/// 4 op failed, 17 target exists. Keep this string byte-for-byte in sync with
/// the protocol the parser and the exit-code mapping below implement.
const PROBE_SCRIPT: &str = r#"# remote-fs probe v1 — runs under `sh -s -- <op> [args...]`.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
# Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir, 4 op failed, 17 target exists.
set -u
op=${1:-}
case "$op" in
  home)
    cd 2>/dev/null || cd / || exit 3
    pwd
    ;;
  list)
    d=${2:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    cd "$d" 2>/dev/null || exit 3
    for f in * .[!.]* ..?*; do
      if [ -d "$f" ]; then t=d
      elif [ -L "$f" ]; then t=l
      elif [ -e "$f" ]; then t=f
      else continue
      fi
      printf '%s\0%s\0' "$t" "$f"
    done
    ;;
  mkdir)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    mkdir "$p" || exit 4
    ;;
  mkfile)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    : > "$p" || exit 4
    ;;
  rm)
    p=${2:-}
    case "$p" in /*?*) ;; *) exit 2 ;; esac
    if [ -d "$p" ] && [ ! -L "$p" ]; then rm -rf "$p" || exit 4; else rm -f "$p" || exit 4; fi
    ;;
  mv)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    [ -e "$n" ] && exit 17
    mv "$s" "$n" || exit 4
    ;;
  cp)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    [ -e "$n" ] && exit 17
    cp -a "$s" "$n" || exit 4
    ;;
  *) exit 2 ;;
esac
exit 0
"#;

/// Directory scans and the `home` probe get a shorter leash than mutations.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// A `list` larger than this is almost certainly a mistake (or an attack on
/// the parser); fail closed instead of buffering unboundedly.
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;
/// `home`/`mkdir`/`mv`/`cp` print next to nothing on stdout.
const MAX_OP_BYTES: usize = 64 * 1024;
/// Local recursive copies refuse to descend deeper than this.
const MAX_COPY_DEPTH: usize = 128;

/// What a helper run produced, with both streams already capped.
#[derive(Debug)]
pub struct Capture {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Single-quote one value for POSIX sh: `'` becomes `'\''`.
fn sq(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// The one remote command string ssh receives: `sh -s -- <op> '<arg>' ...`.
/// The probe script itself travels on stdin, never through the command line.
fn probe_command(op: &str, args: &[&str]) -> String {
    let mut command = String::from("sh -s -- ");
    command.push_str(op);
    for arg in args {
        command.push(' ');
        command.push_str(&sq(arg));
    }
    command
}

/// `ssh -o BatchMode=yes -o ConnectTimeout=10 <ssh_args...> -- <dest> <cmd>`
/// with `<cmd>` a single argv element; quoting happens inside [`probe_command`].
fn ssh_argv(host: &RemoteHostConfig, command: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    argv.extend(host.ssh_args.iter().cloned());
    argv.push("--".to_string());
    let destination = match &host.user {
        Some(user) => format!("{user}@{}", host.host),
        None => host.host.clone(),
    };
    argv.push(destination);
    argv.push(command.to_string());
    argv
}

/// `docker exec -i [-u user] <container> sh -s -- <op> <args...>` — raw argv,
/// stdin attached (`-i`), never a tty (`-t` would corrupt the byte stream).
fn docker_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> Vec<String> {
    let mut argv: Vec<String> = ["docker", "exec", "-i"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    if let Some(user) = &host.user {
        argv.push("-u".to_string());
        argv.push(user.clone());
    }
    argv.push(host.host.clone());
    argv.push("sh".to_string());
    argv.push("-s".to_string());
    argv.push("--".to_string());
    argv.push(op.to_string());
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    argv
}

/// Read a child stream to EOF with a hard byte cap. Overflow keeps draining
/// into the void so the child is never wedged on a full pipe; the caller
/// treats `truncated` as an error.
fn read_bounded<R: Read>(reader: R, max: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut limited = reader.take(max as u64 + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes)?;
    if bytes.len() <= max {
        return Ok((bytes, false));
    }
    bytes.truncate(max);
    let mut rest = limited.into_inner();
    io::copy(&mut rest, &mut io::sink())?;
    Ok((bytes, true))
}

/// Run `argv` with piped stdio, feed `stdin_bytes` (then close stdin), and
/// capture both streams bounded to `max_out` bytes each. A child still alive
/// at `timeout` is killed — with its whole process group, so a probe that
/// forked (`rm -rf` mid-run, a `sleep` under `sh -c`) cannot survive and hold
/// the pipes — and reported as [`io::ErrorKind::TimedOut`].
fn run_capture(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: usize,
) -> io::Result<Capture> {
    let (program, args) = argv.split_first().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "empty argv cannot be spawned")
    })?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group, led by the child: one group kill below reaps the
        // probe and every descendant that did not setsid away.
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    /// Kill the child (Unix: its whole group) and reap the direct child.
    fn kill_tree(child: &mut std::process::Child) {
        #[cfg(unix)]
        unsafe {
            // SAFETY: one kill call on the group the child was made to lead
            // at spawn; the pid came from a live Child handle.
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = child.kill();
        let _ = child.wait();
    }
    // The probe exits early on a usage error, so a broken stdin pipe is not
    // itself a failure; the exit-status mapping reports the real problem.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (mut stdout_pipe, mut stderr_pipe) = match (stdout_pipe, stderr_pipe) {
        (Some(stdout), Some(stderr)) => (stdout, stderr),
        _ => {
            kill_tree(&mut child);
            return Err(io::Error::other("child stdio pipes were not created"));
        }
    };
    // Each stream drains on its own thread: a child filling stderr must not
    // deadlock against us reading only stdout.
    let stdout_reader = std::thread::spawn(move || read_bounded(&mut stdout_pipe, max_out));
    let stderr_reader = std::thread::spawn(move || read_bounded(&mut stderr_pipe, max_out));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_tree(&mut child);
                    // Join the readers so no thread outlives the pipes.
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("{program} exceeded its {}s limit", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                kill_tree(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(error);
            }
        }
    };
    let join_err = || io::Error::other("stream reader thread panicked");
    let (stdout, stdout_truncated) = stdout_reader.join().map_err(|_| join_err())??;
    let (stderr, stderr_truncated) = stderr_reader.join().map_err(|_| join_err())??;
    if stdout_truncated || stderr_truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{program} produced more than {max_out} bytes of output"),
        ));
    }
    Ok(Capture {
        status,
        stdout,
        stderr,
    })
}

/// A bounded, printable excerpt of probe stderr for error messages.
fn stderr_excerpt(stderr: &[u8]) -> String {
    const EXCERPT: usize = 256;
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let mut excerpt: String = text.chars().take(EXCERPT).collect();
    if text.chars().count() > EXCERPT {
        excerpt.push('…');
    }
    excerpt
}

/// Map the probe's documented exit codes onto io error kinds; everything
/// else (ssh's own 255 included) is `Other` with bounded stderr context.
fn probe_status_error(capture: &Capture) -> io::Error {
    let detail = stderr_excerpt(&capture.stderr);
    let with_detail = |message: &str| {
        if detail.is_empty() {
            message.to_string()
        } else {
            format!("{message}: {detail}")
        }
    };
    match capture.status.code() {
        Some(17) => io::Error::new(
            io::ErrorKind::AlreadyExists,
            with_detail("target already exists"),
        ),
        Some(3) => io::Error::new(
            io::ErrorKind::NotFound,
            with_detail("directory does not exist"),
        ),
        Some(2) => io::Error::new(
            io::ErrorKind::InvalidInput,
            with_detail("probe rejected its arguments"),
        ),
        Some(code) => io::Error::other(with_detail(&format!("remote operation failed ({code})"))),
        None => io::Error::other(with_detail("remote operation was killed by a signal")),
    }
}

/// Run one probe op against a validated host and return its stdout on exit 0.
fn run_probe(
    host: &RemoteHostConfig,
    op: &str,
    args: &[&str],
    timeout: Duration,
    max_out: usize,
) -> io::Result<Vec<u8>> {
    let argv = if host.docker {
        docker_argv(host, op, args)
    } else {
        ssh_argv(host, &probe_command(op, args))
    };
    let capture = run_capture(&argv, PROBE_SCRIPT.as_bytes(), timeout, max_out)?;
    if !capture.status.success() {
        return Err(probe_status_error(&capture));
    }
    Ok(capture.stdout)
}

/// Resolve and validate the host a remote location points at. Out-of-range
/// indices and grammar violations fail closed before any process is spawned.
fn remote_host<'a>(
    loc: &FsLocation,
    hosts: &'a [RemoteHostConfig],
) -> io::Result<&'a RemoteHostConfig> {
    match loc {
        FsLocation::Local => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local location has no remote host",
        )),
        FsLocation::Remote(index) => {
            let host = hosts.get(*index).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("remote host #{index} is not configured"),
                )
            })?;
            host.validate().map_err(|problem| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("remote host {}: {problem}", host.display_name()),
                )
            })?;
            Ok(host)
        }
    }
}

/// A remote path argument: absolute and UTF-8 (argv and sh demand both).
fn remote_path(path: &Path) -> io::Result<&str> {
    let text = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote paths must be valid UTF-8",
        )
    })?;
    if !text.starts_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote paths must be absolute",
        ));
    }
    Ok(text)
}

/// Dirs first, then case-insensitive name — the same order the local sidebar
/// has always shown.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Parse a `list` probe buffer: NUL-separated `"<t>\0<name>\0"` pairs. `d` is
/// a directory, `f`/`l` are files (symlinks are never expanded into dirs).
/// Non-UTF-8 names are kept lossy; dotfiles are dropped here so remote and
/// local listings share one hidden-file policy.
fn parse_list(bytes: &[u8], dir: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut fields = bytes.split(|byte| *byte == 0);
    while let (Some(kind), Some(name)) = (fields.next(), fields.next()) {
        if name.is_empty() || name == b"." || name == b".." {
            continue;
        }
        // Match the sidebar behavior. Hidden files remain available by typing
        // paths in the terminal without overwhelming the visual tree.
        if name[0] == b'.' {
            continue;
        }
        let is_dir = match kind {
            b"d" => true,
            b"f" | b"l" => false,
            _ => continue,
        };
        let name = String::from_utf8_lossy(name).into_owned();
        entries.push(Entry {
            path: dir.join(&name),
            name,
            is_dir,
        });
    }
    sort_entries(&mut entries);
    entries
}

/// Local one-level listing with the same policy (dotfiles hidden, same sort)
/// and the same error wording the sidebar has always produced.
fn local_list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| io::Error::other(format!("Cannot read {}: {error}", dir.display())))?;
    let mut nodes = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| io::Error::other(format!("Cannot read {}: {error}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| {
            io::Error::other(format!(
                "Cannot inspect {}: {error}",
                entry.path().display()
            ))
        })?;
        nodes.push(Entry {
            name,
            path: entry.path(),
            is_dir: file_type.is_dir(),
        });
    }
    sort_entries(&mut nodes);
    Ok(nodes)
}

/// List exactly one directory level at `dir`, locally or through the probe.
pub fn list_dir(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
) -> io::Result<Vec<Entry>> {
    match loc {
        FsLocation::Local => local_list_dir(dir),
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            let stdout = run_probe(
                host,
                "list",
                &[remote_path(dir)?],
                LIST_TIMEOUT,
                MAX_LIST_BYTES,
            )
            .map_err(|error| io::Error::other(format!("Cannot read {}: {error}", dir.display())))?;
            Ok(parse_list(&stdout, dir))
        }
    }
}

/// The directory a freshly switched location opens at: today's behavior for
/// local (process cwd), the login home for a remote host.
pub fn start_dir(loc: &FsLocation, hosts: &[RemoteHostConfig]) -> io::Result<PathBuf> {
    match loc {
        FsLocation::Local => Ok(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            let stdout = run_probe(host, "home", &[], LIST_TIMEOUT, MAX_OP_BYTES)?;
            let text = String::from_utf8_lossy(&stdout);
            let home = text.lines().next().unwrap_or("").trim();
            if !home.starts_with('/') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "remote host did not report a home directory",
                ));
            }
            Ok(PathBuf::from(home))
        }
    }
}

/// Create one directory. Fails (`AlreadyExists`) when anything is there.
pub fn create_dir(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    match loc {
        FsLocation::Local => std::fs::create_dir(path),
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            run_probe(
                host,
                "mkdir",
                &[remote_path(path)?],
                OP_TIMEOUT,
                MAX_OP_BYTES,
            )?;
            Ok(())
        }
    }
}

/// Create one empty file. Fails (`AlreadyExists`) when anything is there.
pub fn create_file(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    match loc {
        FsLocation::Local => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ()),
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            run_probe(
                host,
                "mkfile",
                &[remote_path(path)?],
                OP_TIMEOUT,
                MAX_OP_BYTES,
            )?;
            Ok(())
        }
    }
}

/// A name typed into a New File / New Folder / Rename dialog. Reused by the
/// dialogs so what they accept is exactly what the ops will run.
pub fn validate_new_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name must not be empty".to_string());
    }
    if name.len() > 255 {
        return Err("name must be at most 255 bytes".to_string());
    }
    if name == "." || name == ".." {
        return Err("name must not be \".\" or \"..\"".to_string());
    }
    if name.contains('/') || name.contains('\0') {
        return Err("name must not contain '/' or NUL".to_string());
    }
    Ok(())
}

/// Deletion's one absolute rule, on both sides of the wire: never `/`.
pub fn validate_delete_path(path: &Path) -> Result<(), String> {
    if path.parent().is_none() {
        return Err("refusing to delete the filesystem root".to_string());
    }
    Ok(())
}

/// Remove a file, symlink, or (recursively) a directory.
pub fn delete(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    if let Err(problem) = validate_delete_path(path) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, problem));
    }
    match loc {
        FsLocation::Local => {
            // symlink_metadata so a symlink to a directory is unlinked, never
            // descended into.
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            run_probe(host, "rm", &[remote_path(path)?], OP_TIMEOUT, MAX_OP_BYTES)?;
            Ok(())
        }
    }
}

/// Rename `src` to `dst`. Fails (`AlreadyExists`) when `dst` exists.
pub fn rename(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match loc {
        FsLocation::Local => {
            ensure_absent(dst)?;
            std::fs::rename(src, dst)
        }
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            run_probe(
                host,
                "mv",
                &[remote_path(src)?, remote_path(dst)?],
                OP_TIMEOUT,
                MAX_OP_BYTES,
            )?;
            Ok(())
        }
    }
}

/// Copy a file, symlink, or (recursively) a directory to `dst`, which must
/// not exist yet (`AlreadyExists`).
pub fn copy(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match loc {
        FsLocation::Local => {
            ensure_absent(dst)?;
            copy_recursive(src, dst, 0)
        }
        FsLocation::Remote(_) => {
            let host = remote_host(loc, hosts)?;
            run_probe(
                host,
                "cp",
                &[remote_path(src)?, remote_path(dst)?],
                OP_TIMEOUT,
                MAX_OP_BYTES,
            )?;
            Ok(())
        }
    }
}

/// Where a paste lands inside `target_dir` for a clipboard carrying `source`:
/// the source's file name joined onto the target. `/` has no name and cannot
/// be pasted anywhere.
pub fn paste_destination(target_dir: &Path, source: &Path) -> Option<PathBuf> {
    let name = source.file_name()?;
    Some(target_dir.join(name))
}

fn ensure_absent(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    Ok(())
}

/// Small recursive copier (no extra crates): directories first, symlinks
/// re-created as links, everything else as a plain byte copy. Depth-bounded
/// and refusing to copy a directory into itself.
fn copy_recursive(src: &Path, dst: &Path, depth: usize) -> io::Result<()> {
    if depth >= MAX_COPY_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is nested too deeply to copy", src.display()),
        ));
    }
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.is_dir() {
        if depth == 0 {
            let canonical = src.canonicalize()?;
            let dst_parent = dst
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"))
                .canonicalize()
                .unwrap_or_else(|_| dst.parent().map(Path::to_path_buf).unwrap_or_default());
            if dst_parent
                .join(dst.file_name().unwrap_or_default())
                .starts_with(&canonical)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot copy a directory into itself",
                ));
            }
        }
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()), depth + 1)?;
        }
        std::fs::set_permissions(dst, metadata.permissions())?;
        Ok(())
    } else if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
        }
        #[cfg(not(unix))]
        {
            std::fs::copy(src, dst).map(|_| ())
        }
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("frost-remote-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("nested").join("deep")).expect("create test tree");
        std::fs::write(root.join("file.txt"), b"x").expect("write test file");
        std::fs::write(root.join("nested").join("inner.txt"), b"y").expect("write test file");
        std::fs::write(root.join(".hidden"), b"h").expect("write hidden file");
        std::fs::write(root.join("with space.txt"), b"s").expect("write spaced file");
        root
    }

    fn ssh_host() -> RemoteHostConfig {
        RemoteHostConfig {
            name: "dev".to_string(),
            host: "dev.example.com".to_string(),
            user: Some("yj".to_string()),
            docker: false,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: vec!["-p".to_string(), "22".to_string()],
            deploy: String::new(),
            deploy_artifact: None,
        }
    }

    fn docker_host() -> RemoteHostConfig {
        RemoteHostConfig {
            name: "myubuntu".to_string(),
            host: "myubuntu".to_string(),
            user: Some("root".to_string()),
            docker: true,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: vec!["-p".to_string(), "ignored".to_string()],
            deploy: String::new(),
            deploy_artifact: None,
        }
    }

    // ── Quoting and argv ────────────────────────────────────────────────

    #[test]
    fn sq_wraps_and_escapes_single_quotes() {
        assert_eq!(sq("plain"), "'plain'");
        assert_eq!(sq("it's"), "'it'\\''s'");
        assert_eq!(sq(""), "''");
        assert_eq!(sq("a b\nc"), "'a b\nc'");
    }

    #[test]
    fn probe_command_is_one_shell_string() {
        assert_eq!(probe_command("list", &["/tmp"]), "sh -s -- list '/tmp'");
        assert_eq!(
            probe_command("mv", &["/a o'ne", "/b two"]),
            "sh -s -- mv '/a o'\\''ne' '/b two'"
        );
    }

    #[test]
    fn ssh_argv_places_options_args_dest_and_one_command() {
        let argv = ssh_argv(&ssh_host(), "sh -s -- list '/tmp'");
        assert_eq!(
            argv,
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "22",
                "--",
                "yj@dev.example.com",
                "sh -s -- list '/tmp'",
            ]
        );
    }

    #[test]
    fn ssh_argv_without_user_uses_bare_host() {
        let mut host = ssh_host();
        host.user = None;
        let argv = ssh_argv(&host, "sh -s -- home");
        assert_eq!(argv[argv.len() - 2], "dev.example.com");
    }

    #[test]
    fn docker_argv_is_raw_and_never_allocates_a_tty() {
        let argv = docker_argv(&docker_host(), "list", &["/var/log"]);
        assert_eq!(
            argv,
            vec![
                "docker", "exec", "-i", "-u", "root", "myubuntu", "sh", "-s", "--", "list",
                "/var/log",
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t"));
        assert!(!argv.iter().any(|arg| arg.contains("/var/log')")));
    }

    // ── Probe script protocol, exercised against the real `sh` ─────────

    fn probe(op: &str, args: &[&str]) -> Capture {
        let mut argv: Vec<String> = ["sh", "-s", "--", op]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        run_capture(
            &argv,
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(10),
            MAX_LIST_BYTES,
        )
        .expect("probe run")
    }

    #[test]
    fn probe_list_reports_types_and_relative_names() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let capture = probe("list", &[&root_str]);
        assert!(capture.status.success());
        let entries = parse_list(&capture.stdout, &root);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // Dotfiles are filtered by the Rust-side policy, dirs sort first.
        assert_eq!(names, vec!["nested", "file.txt", "with space.txt"]);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, root.join("file.txt"));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_list_rejects_relative_and_missing_dirs() {
        assert_eq!(probe("list", &["relative/path"]).status.code(), Some(2));
        assert_eq!(
            probe("list", &["/definitely/not/there"]).status.code(),
            Some(3)
        );
    }

    #[test]
    fn probe_home_prints_an_absolute_directory() {
        let capture = probe("home", &[]);
        assert!(capture.status.success());
        let text = String::from_utf8_lossy(&capture.stdout);
        assert!(text.trim().starts_with('/'));
    }

    #[test]
    fn probe_mutations_create_move_copy_and_remove() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let dir = format!("{root_str}/made");
        let file = format!("{root_str}/made/note.txt");
        let moved = format!("{root_str}/made/renamed.txt");
        let copied = format!("{root_str}/copy of made");

        assert!(probe("mkdir", &[&dir]).status.success());
        // Second create must fail closed with the documented code.
        assert_eq!(probe("mkdir", &[&dir]).status.code(), Some(17));
        assert!(probe("mkfile", &[&file]).status.success());
        assert_eq!(probe("mkfile", &[&file]).status.code(), Some(17));
        assert!(probe("mv", &[&file, &moved]).status.success());
        assert!(root.join("made").join("renamed.txt").is_file());
        assert!(probe("cp", &[&dir, &copied]).status.success());
        assert!(root.join("copy of made").join("renamed.txt").is_file());
        assert_eq!(probe("cp", &[&dir, &copied]).status.code(), Some(17));
        assert!(probe("rm", &[&copied]).status.success());
        assert!(!root.join("copy of made").exists());
        // "/" is refused before rm ever runs.
        assert_eq!(probe("rm", &["/"]).status.code(), Some(2));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_exit_codes_map_to_error_kinds() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        // mkdir on a missing parent exits 4 (op failed) → plain Other.
        let missing = format!("{root_str}/not-here/child");
        let capture = probe("mkdir", &[&missing]);
        assert_eq!(probe_status_error(&capture).kind(), io::ErrorKind::Other);
        let exists = probe("mkdir", &[&root_str]);
        assert_eq!(
            probe_status_error(&exists).kind(),
            io::ErrorKind::AlreadyExists
        );
        std::fs::remove_dir_all(root).expect("remove test tree");

        let relative = probe("list", &["nope"]);
        assert_eq!(
            probe_status_error(&relative).kind(),
            io::ErrorKind::InvalidInput
        );
        let absent = probe("list", &["/definitely/not/there"]);
        assert_eq!(probe_status_error(&absent).kind(), io::ErrorKind::NotFound);
    }

    // ── List parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_list_handles_odd_names_and_truncation() {
        let dir = Path::new("/base");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"d\0dir one\0");
        bytes.extend_from_slice(b"f\0line\nbreak\0");
        bytes.extend_from_slice(b"l\0sym link\0");
        bytes.extend_from_slice(b"f\0.hidden\0");
        bytes.extend_from_slice(b"f\0\xff\xfe raw\0");
        bytes.extend_from_slice(b"x\0unknown kind\0");
        // A dangling half-pair (as a capped read leaves) is simply dropped.
        bytes.extend_from_slice(b"d\0");
        let entries = parse_list(&bytes, dir);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["dir one", "line\nbreak", "sym link", "�� raw"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir && !entries[2].is_dir);
        assert_eq!(entries[3].path, dir.join("�� raw"));
    }

    #[test]
    fn parse_list_sorts_dirs_first_case_insensitively() {
        let dir = Path::new("/base");
        let bytes = b"f\0Zulu\0d\0beta\0f\0apple\0d\0Alpha\0".to_vec();
        let entries = parse_list(&bytes, dir);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "apple", "Zulu"]);
    }

    // ── Validation ──────────────────────────────────────────────────────

    #[test]
    fn new_name_validation_rejects_paths_and_oddities() {
        assert!(validate_new_name("notes.txt").is_ok());
        assert!(validate_new_name("").is_err());
        assert!(validate_new_name(".").is_err());
        assert!(validate_new_name("..").is_err());
        assert!(validate_new_name("a/b").is_err());
        assert!(validate_new_name("a\0b").is_err());
        assert!(validate_new_name(&"x".repeat(256)).is_err());
        assert!(validate_new_name(&"x".repeat(255)).is_ok());
    }

    #[test]
    fn delete_validation_refuses_only_the_root() {
        assert!(validate_delete_path(Path::new("/")).is_err());
        assert!(validate_delete_path(Path::new("/tmp")).is_ok());
    }

    #[test]
    fn remote_paths_must_be_absolute_utf8() {
        assert_eq!(remote_path(Path::new("/tmp")).expect("absolute"), "/tmp");
        assert!(remote_path(Path::new("tmp")).is_err());
    }

    #[test]
    fn paste_destination_joins_the_source_name() {
        assert_eq!(
            paste_destination(Path::new("/target"), Path::new("/a/b/file.txt")),
            Some(PathBuf::from("/target/file.txt"))
        );
        assert_eq!(
            paste_destination(Path::new("/target"), Path::new("/")),
            None
        );
    }

    // ── Local ops ───────────────────────────────────────────────────────

    #[test]
    fn local_create_rename_copy_delete_roundtrip() {
        let root = temp_tree();
        let made = root.join("made");
        create_dir(&FsLocation::Local, &[], &made).expect("mkdir");
        let error = create_dir(&FsLocation::Local, &[], &made).expect_err("exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        let note = made.join("note.txt");
        create_file(&FsLocation::Local, &[], &note).expect("mkfile");
        let error = create_file(&FsLocation::Local, &[], &note).expect_err("exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        let renamed = made.join("renamed.txt");
        rename(&FsLocation::Local, &[], &note, &renamed).expect("rename");
        assert!(renamed.is_file() && !note.exists());
        let error = rename(&FsLocation::Local, &[], &renamed, &root.join("file.txt"))
            .expect_err("dst exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        let copied = root.join("copy");
        copy(&FsLocation::Local, &[], &made, &copied).expect("copy dir");
        assert!(copied.join("renamed.txt").is_file());
        let error = copy(&FsLocation::Local, &[], &made, &copied).expect_err("dst exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let inside = made.join("inside");
        assert!(copy(&FsLocation::Local, &[], &made, &inside).is_err());

        delete(&FsLocation::Local, &[], &copied).expect("delete dir");
        assert!(!copied.exists());
        delete(&FsLocation::Local, &[], &renamed).expect("delete file");
        assert!(!renamed.exists());
        assert!(delete(&FsLocation::Local, &[], Path::new("/")).is_err());
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn local_listing_matches_sidebar_policy() {
        let root = temp_tree();
        let entries = list_dir(&FsLocation::Local, &[], &root).expect("list");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["nested", "file.txt", "with space.txt"]);
        let error = list_dir(&FsLocation::Local, &[], &root.join("nope")).expect_err("missing");
        assert!(error.to_string().contains("Cannot read"));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn remote_ops_fail_closed_on_unknown_or_invalid_hosts() {
        let hosts = vec![ssh_host()];
        let missing = list_dir(&FsLocation::Remote(7), &hosts, Path::new("/tmp"));
        assert_eq!(
            missing.expect_err("unknown host").kind(),
            io::ErrorKind::InvalidInput
        );
        let mut broken = ssh_host();
        broken.host = "white space".to_string();
        let listed = list_dir(&FsLocation::Remote(0), &[broken], Path::new("/tmp"));
        assert_eq!(
            listed.expect_err("invalid host").kind(),
            io::ErrorKind::InvalidInput
        );
        let relative = list_dir(&FsLocation::Remote(0), &hosts, Path::new("tmp"));
        assert_eq!(
            relative.expect_err("relative path").kind(),
            io::ErrorKind::InvalidInput
        );
    }

    // ── Runner bounds ───────────────────────────────────────────────────

    #[test]
    fn run_capture_roundtrips_stdin_and_stdout() {
        let argv: Vec<String> = ["sh", "-c", "cat"]
            .iter()
            .map(|a| (*a).to_string())
            .collect();
        let capture =
            run_capture(&argv, b"hello probe", Duration::from_secs(5), 1024).expect("capture");
        assert!(capture.status.success());
        assert_eq!(capture.stdout, b"hello probe");
    }

    #[test]
    fn run_capture_kills_overdue_children() {
        let argv: Vec<String> = ["sh", "-c", "sleep 30"]
            .iter()
            .map(|a| (*a).to_string())
            .collect();
        let started = Instant::now();
        let error =
            run_capture(&argv, b"", Duration::from_millis(200), 1024).expect_err("must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_capture_fails_closed_on_output_flood() {
        let argv: Vec<String> = ["sh", "-c", "yes flooded | head -c 100000"]
            .iter()
            .map(|a| (*a).to_string())
            .collect();
        let error = run_capture(&argv, b"", Duration::from_secs(10), 4096)
            .expect_err("must refuse oversized output");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn location_labels_follow_the_transport() {
        let hosts = vec![ssh_host(), docker_host()];
        assert_eq!(FsLocation::Local.label(&hosts), "Local");
        assert_eq!(FsLocation::Remote(0).label(&hosts), "ssh: dev");
        assert_eq!(FsLocation::Remote(1).label(&hosts), "docker: myubuntu");
        assert_eq!(FsLocation::Remote(9).label(&hosts), "Remote #9");
    }
}
