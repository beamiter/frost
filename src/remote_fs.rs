//! Sidebar filesystem backend: local paths through `std::fs`, remote hosts by
//! spawning the system `ssh` / `docker` binaries with a small POSIX sh probe
//! script on their stdin (`sh -s -- <op> [args...]`). No sshfs and no extra
//! crates: the far side needs nothing but a Bourne-compatible shell, the same
//! philosophy the jsh-remote-over-ssh sessions already use.
//!
//! Everything here is blocking and is meant to run inside an iced worker task
//! (`Task::perform`), never on the UI update path. Buffers are bounded, argv
//! is validated before spawn, and unknown hosts/locations fail closed.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
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
    /// An SSH destination observed from the active terminal process. It lives
    /// only for this app session and never mutates the saved host list.
    Transient(RemoteHostConfig),
}

const REMOTE_LOCATION_LABEL_CHARS: usize = 56;

fn compact_remote_display_name(value: &str) -> String {
    let safe = jterm_core::review_input::safe_inline_display(value, 2_048);
    if safe.is_empty() {
        return "unnamed".to_string();
    }
    let count = safe.chars().count();
    if count <= REMOTE_LOCATION_LABEL_CHARS {
        return safe;
    }
    let content = REMOTE_LOCATION_LABEL_CHARS.saturating_sub(1);
    let head_chars = content * 3 / 5;
    let tail_chars = content.saturating_sub(head_chars);
    let head: String = safe.chars().take(head_chars).collect();
    let tail: String = safe
        .chars()
        .skip(count.saturating_sub(tail_chars))
        .collect();
    format!("{head}…{tail}")
}

impl FsLocation {
    /// Picker/menu label: `Local`, `ssh: <name>`, or `docker: <name>`.
    pub fn label(&self, hosts: &[RemoteHostConfig]) -> String {
        match self {
            FsLocation::Local => "Local".to_string(),
            FsLocation::Remote(index) => match hosts.get(*index) {
                Some(host) => {
                    let unavailable =
                        crate::config::validate_remote_host_at(hosts, *index).is_err();
                    let display = crate::config::remote_host_display_name(host, *index);
                    let mut label = format!(
                        "{}: {}",
                        if host.docker { "docker" } else { "ssh" },
                        compact_remote_display_name(&display)
                    );
                    if unavailable {
                        label.push_str(" (unavailable)");
                    }
                    label
                }
                None => format!("Remote #{index}"),
            },
            FsLocation::Transient(host) => {
                format!(
                    "ssh: {} (temporary)",
                    compact_remote_display_name(host.display_name())
                )
            }
        }
    }

    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local)
    }
}

fn ssh_args_without_control_path(args: &[String]) -> Vec<&str> {
    let mut normalized = Vec::with_capacity(args.len());
    let mut index = 0usize;
    while index < args.len() {
        let argument = args[index].as_str();
        if argument == "-S" && args.get(index + 1).is_some() {
            index += 2;
            continue;
        }
        if argument.starts_with("-S") && argument.len() > 2 {
            index += 1;
            continue;
        }
        if argument == "-o" {
            if args.get(index + 1).is_some_and(|option| {
                option
                    .split_once('=')
                    .map_or(option.as_str(), |(key, _)| key)
                    .eq_ignore_ascii_case("controlpath")
            }) {
                index += 2;
                continue;
            }
        } else if argument.strip_prefix("-o").is_some_and(|option| {
            option
                .split_once('=')
                .map_or(option, |(key, _)| key)
                .eq_ignore_ascii_case("controlpath")
        }) {
            index += 1;
            continue;
        }
        normalized.push(argument);
        index += 1;
    }
    normalized
}

pub(crate) fn has_control_path_argument(args: &[String]) -> bool {
    ssh_args_without_control_path(args).len() != args.len()
}

pub(crate) fn with_reusable_control_path(
    profile: &RemoteHostConfig,
    control_path: Option<&str>,
) -> RemoteHostConfig {
    let Some(control_path) = control_path else {
        return profile.clone();
    };
    let mut overlaid = profile.clone();
    // A provenance-checked path derived from the currently running jsh
    // connection is fresher than a socket stored in a saved profile. Replace
    // only ControlPath syntax; every other reachability/auth option remains.
    overlaid.ssh_args = ssh_args_without_control_path(&profile.ssh_args)
        .into_iter()
        .map(str::to_string)
        .collect();
    overlaid.ssh_args.push("-S".to_string());
    overlaid.ssh_args.push(control_path.to_string());
    if crate::config::validate_remote_host(&overlaid).is_ok() {
        overlaid
    } else {
        profile.clone()
    }
}

/// Whether two profiles name the same Files namespace. Display/deployment
/// policy and a jsh ControlMaster socket do not change the remote filesystem;
/// reachability/authentication/destination options still must match exactly.
pub(crate) fn same_files_transport(left: &RemoteHostConfig, right: &RemoteHostConfig) -> bool {
    left.docker == right.docker
        && left.host == right.host
        && left.user == right.user
        && (left.docker
            || ssh_args_without_control_path(&left.ssh_args)
                == ssh_args_without_control_path(&right.ssh_args))
}

/// Opaque identity of one Files namespace. The scheduler and cache need to
/// isolate unrelated destinations, but must never retain or render a username,
/// hostname, or authentication argument as diagnostic text.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FilesAuthorityKey(u64);

impl std::fmt::Debug for FilesAuthorityKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FilesAuthorityKey(<redacted>)")
    }
}

/// Derive the same namespace identity used by [`same_files_transport`]. A
/// ControlPath overlay is deliberately ignored, so reconnecting through a live
/// multiplexed socket neither splits scan fairness nor loses cached subtrees.
pub(crate) fn files_authority_key(
    location: &FsLocation,
    hosts: &[RemoteHostConfig],
) -> FilesAuthorityKey {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match location {
        FsLocation::Local => {
            0_u8.hash(&mut hasher);
        }
        FsLocation::Remote(index) => {
            1_u8.hash(&mut hasher);
            if let Some(host) = hosts.get(*index) {
                hash_files_transport(host, &mut hasher);
            } else {
                // Invalid snapshots still get a stable, non-local bucket and
                // fail closed in the backend. Including the slot prevents all
                // malformed profiles from blocking one another indefinitely.
                index.hash(&mut hasher);
            }
        }
        FsLocation::Transient(host) => {
            1_u8.hash(&mut hasher);
            hash_files_transport(host, &mut hasher);
        }
    }
    FilesAuthorityKey(hasher.finish())
}

fn hash_files_transport(host: &RemoteHostConfig, hasher: &mut impl Hasher) {
    host.docker.hash(hasher);
    host.host.hash(hasher);
    host.user.hash(hasher);
    if !host.docker {
        for argument in ssh_args_without_control_path(&host.ssh_args) {
            argument.hash(hasher);
        }
    }
}

/// Exact runtime route for file probes. Unlike [`same_files_transport`], this
/// includes the live SSH argument vector so a new ControlPath can upgrade an
/// already visible tree before it is treated as fully joined.
pub(crate) fn same_files_execution(left: &RemoteHostConfig, right: &RemoteHostConfig) -> bool {
    left.docker == right.docker
        && left.host == right.host
        && left.user == right.user
        && (left.docker || left.ssh_args == right.ssh_args)
}

/// Whether two runtime locations address one filesystem namespace. This is
/// intentionally broader than enum equality: a saved profile and its
/// temporary ControlPath overlay are the same host, so copy/move can run
/// directly through the currently active execution profile.
pub(crate) fn same_files_location(
    left: &FsLocation,
    right: &FsLocation,
    hosts: &[RemoteHostConfig],
) -> bool {
    match (left.is_remote(), right.is_remote()) {
        (false, false) => true,
        (true, true) => remote_host(left, hosts)
            .and_then(|left| {
                remote_host(right, hosts).map(|right| same_files_transport(left, right))
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Pick the runtime route for an operation inside one namespace. A temporary
/// observed route carrying a live ControlPath wins from either direction;
/// otherwise the destination/current tree is the least surprising choice.
pub(crate) fn preferred_same_files_execution_location<'a>(
    source: &'a FsLocation,
    destination: &'a FsLocation,
    hosts: &[RemoteHostConfig],
) -> Option<&'a FsLocation> {
    if !same_files_location(source, destination, hosts) {
        return None;
    }
    let source_has_socket = remote_host(source, hosts)
        .ok()
        .is_some_and(|profile| has_control_path_argument(&profile.ssh_args));
    let destination_has_socket = remote_host(destination, hosts)
        .ok()
        .is_some_and(|profile| has_control_path_argument(&profile.ssh_args));
    let source_is_live = matches!(source, FsLocation::Transient(_));
    let destination_is_live = matches!(destination, FsLocation::Transient(_));
    if source_has_socket && (!destination_has_socket || (source_is_live && !destination_is_live)) {
        Some(source)
    } else {
        Some(destination)
    }
}

/// One directory entry, one level deep (never recursively walked here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// One bounded directory snapshot. `truncated` is true whenever the backend
/// proved that more entries exist than the UI retains (or its defensive scan
/// bound was reached), so a partial tree is never presented as complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryListing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// Remote-fs probe v4 — runs under `sh -s -- <op> [args...]`.
///
/// `list` stdout is NUL-separated pairs `"<t>\0<name>\0"`, t in {d,f,l}, names
/// relative. Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir,
/// 4 op failed, 17 target exists. Keep this string byte-for-byte in sync with
/// the protocol the parser and the exit-code mapping below implement. v2 added
/// the streaming ops `cat` (file → stdout), `put` (stdin → new file of an
/// optionally declared size; temp + count re-check + mv, so a truncated or
/// partial stream never lands on the target, with a trap cleaning the temp
/// when the shell dies), and `tar`/`untar` (directory ⇄ tar stream). v3 turns
/// `untar` into `untar <dir> <name>`, refusing an existing `<dir>/<name>`
/// with 17 BEFORE extracting (the microscopic check-extract window is
/// documented at `upload`), and adds `stat <path>` printing one `<t> <size>`
/// line for pre-checks. v4 makes `list` take an explicit positive entry limit
/// plus a 0/1 hidden-entry policy, stops emitting at that limit, and classifies
/// symlinks before directories so a link to a directory is never expandable.
/// Every v1 op and v2's `cat`/`put`/`tar` stay
/// byte-identical. The stdin-consuming ops print one `fsprobe-ready` line on
/// stdout first: the client must not stream payload before it arrives,
/// because the shell's own script buffering would otherwise eat payload bytes.
const PROBE_SCRIPT: &str = r#"# remote-fs probe v4 — runs under `sh -s -- <op> [args...]`.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
# `list <dir> <limit> <hidden>` emits at most limit entries; hidden is 0 or 1.
# Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir, 4 op failed, 17 target exists.
# put/untar print "fsprobe-ready" on stdout before reading stdin as payload;
# stream only after the marker, or the shell's script buffering eats the bytes.
# put takes an optional declared size ($3): a short stream fails 4, never commits.
# untar takes <dir> <name> and refuses an existing <dir>/<name> with 17 up front.
# stat prints "<t> <size>" (t in {d,f,l}; size is the byte count for f, 0 otherwise).
set -u
op=${1:-}
case "$op" in
  home)
    cd 2>/dev/null || cd / || exit 3
    pwd
    ;;
  list)
    d=${2:-}; limit=${3:-}; hidden=${4:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    case "$limit" in ''|0|*[!0-9]*) exit 2 ;; esac
    case "$hidden" in 0|1) ;; *) exit 2 ;; esac
    cd "$d" 2>/dev/null || exit 3
    count=0
    emit_list_entry() {
      f=$1
      if [ -L "$f" ]; then t=l
      elif [ -d "$f" ]; then t=d
      elif [ -e "$f" ]; then t=f
      else return
      fi
      printf '%s\0%s\0' "$t" "$f"
      count=$((count + 1))
    }
    for f in *; do
      [ "$count" -ge "$limit" ] && break
      emit_list_entry "$f"
    done
    if [ "$hidden" = 1 ] && [ "$count" -lt "$limit" ]; then
      for f in .[!.]* ..?*; do
        [ "$count" -ge "$limit" ] && break
        emit_list_entry "$f"
      done
    fi
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
  cat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -f "$p" ] && [ -r "$p" ]; then :; else exit 3; fi
    cat "$p" || exit 4
    ;;
  put)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    t="$p.fspart.$$"
    trap 'rm -f "$t" 2>/dev/null' EXIT
    printf 'fsprobe-ready\n'
    if ! cat > "$t"; then rm -f "$t"; exit 4; fi
    if [ -n "${3:-}" ]; then
      bytes=$(wc -c < "$t" | tr -d '[:space:]')
      [ "$bytes" = "$3" ] || { rm -f "$t"; exit 4; }
    fi
    if [ -e "$p" ]; then rm -f "$t"; exit 17; fi
    mv "$t" "$p" || { rm -f "$t"; exit 4; }
    ;;
  tar)
    p=${2%/}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -d "$p" ] || exit 3
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    parent=${p%/*}
    [ -n "$parent" ] || parent=/
    tar cf - -C "$parent" "${p##*/}" || exit 4
    ;;
  untar)
    d=${2:-}; n=${3:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    case "$n" in ""|.|..|*/*) exit 2 ;; esac
    [ -d "$d" ] || exit 3
    if [ -e "$d/$n" ] || [ -L "$d/$n" ]; then exit 17; fi
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    printf 'fsprobe-ready\n'
    tar xf - -C "$d" || exit 4
    ;;
  stat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -d "$p" ]; then t=d
    elif [ -L "$p" ]; then t=l
    elif [ -e "$p" ]; then t=f
    else exit 3
    fi
    if [ "$t" = f ]; then s=$(wc -c < "$p" | tr -d '[:space:]'); else s=0; fi
    printf '%s %s\n' "$t" "$s"
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
/// One directory renders at most this many entries. The byte cap above bounds
/// the probe read, but the parsed tree is capped too so a huge remote
/// directory cannot fill the sidebar with an unbounded listing.
pub const MAX_DIRECTORY_ENTRIES: usize = 4_096;
/// Ask the remote for exactly one extra entry. Receiving it proves the visible
/// 4096-entry snapshot is truncated while still bounding far-side output.
const DIRECTORY_FETCH_ENTRIES: usize = MAX_DIRECTORY_ENTRIES + 1;
/// Skipped pairs (dotfiles, unknown kinds) still cost a loop iteration, so
/// scanning also stops once far more pairs than the entry cap were seen.
const MAX_SCANNED_PAIRS: usize = MAX_DIRECTORY_ENTRIES * 4;
/// `home`/`mkdir`/`mv`/`cp` print next to nothing on stdout.
const MAX_OP_BYTES: usize = 64 * 1024;
/// Local recursive copies refuse to descend deeper than this.
const MAX_COPY_DEPTH: usize = 128;
/// Hard byte limit for one cross-location transfer, enforced while streaming
/// (and up front via local metadata on upload) so a payload is never buffered
/// whole and a runaway source cannot fill the disk.
pub const MAX_TRANSFER_BYTES: u64 = 512 * 1024 * 1024;
/// Transfers get a generous overall leash: 512 MiB over a slow ssh link is
/// still bounded, and the watchdog kills the whole process group at the end
/// of it either way.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Progress publishes at most about four times a second…
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(250);
/// …and at least every 256 KiB transferred, whichever comes first.
const PROGRESS_MIN_BYTES: u64 = 256 * 1024;

/// The pure throttle rule behind [`TransferProgress::report`]: publish when
/// either enough time or enough new bytes have passed since the last publish.
fn should_publish(last: (Instant, u64), now: Instant, total: u64) -> bool {
    now.duration_since(last.0) >= PROGRESS_MIN_INTERVAL
        || total.saturating_sub(last.1) >= PROGRESS_MIN_BYTES
}

/// Cooperative cancellation shared by queued work and the child-process
/// watchdog. A caller can retire work before spawn or kill its running process
/// group through the same atomic flag.
#[derive(Debug, Default)]
pub struct CancellationToken {
    cancelled: std::sync::atomic::AtomicBool,
}

impl CancellationToken {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Shared state for one in-flight transfer: a throttled byte counter the UI
/// polls, plus the cancellation flag the worker's watchdog polls. One fresh
/// handle per transfer; cancel racing completion is a no-op because the
/// outcome is only reported as cancelled when the transfer actually failed.
#[derive(Debug)]
pub struct TransferProgress {
    bytes: std::sync::atomic::AtomicU64,
    cancellation: CancellationToken,
    last_publish: std::sync::Mutex<(Instant, u64)>,
}

impl TransferProgress {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            bytes: std::sync::atomic::AtomicU64::new(0),
            cancellation: CancellationToken::default(),
            // Backdated so the very first report publishes immediately.
            last_publish: std::sync::Mutex::new((
                Instant::now()
                    .checked_sub(PROGRESS_MIN_INTERVAL)
                    .unwrap_or_else(Instant::now),
                0,
            )),
        })
    }

    /// Bytes transferred so far (last published value).
    pub fn bytes(&self) -> u64 {
        self.bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Ask the worker to stop; the watchdog's group kill does the rest.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Record `total` bytes so far, throttled: at most ~4 publishes a second
    /// unless at least 256 KiB went by since the last one.
    pub fn report(&self, total: u64) {
        let now = Instant::now();
        let mut last = self.last_publish.lock().expect("progress lock");
        if should_publish(*last, now, total) {
            *last = (now, total);
            self.bytes
                .store(total, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Publish the final byte count, unthrottled, so the last UI poll before
    /// the result message shows the complete transfer.
    pub fn finish(&self, total: u64) {
        let now = Instant::now();
        let mut last = self.last_publish.lock().expect("progress lock");
        *last = (now, total);
        self.bytes
            .store(total, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The sentinel error any cancelled worker surfaces. Transfer callers map it
/// (and other artifacts of the group kill) to a neutral cancelled report;
/// stale directory callers reject it by request identity.
fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "operation cancelled")
}

/// Human-readable byte counts for the transfer notice: `512 B`, `2.0 KiB`,
/// `12.4 MiB`, `1.0 GiB`.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    }
}

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

/// Kill the child (Unix: its whole process group, which it was made to lead
/// at spawn) and reap the direct child.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        // SAFETY: one kill call on the group the child was made to lead at
        // spawn; the pid came from a live Child handle.
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

/// Wait for `child`, killing the whole group and reporting
/// [`io::ErrorKind::TimedOut`] when it outlives `timeout`. When a cancellation
/// token is attached, a set flag takes the exact same path — group kill, then
/// [`io::ErrorKind::Interrupted`] — so cancel and timeout are indistinguishable
/// to the pipes downstream.
fn wait_status(
    child: &mut std::process::Child,
    timeout: Duration,
    program: &str,
    cancel: Option<&CancellationToken>,
) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if cancel.is_some_and(CancellationToken::is_cancelled) {
                    kill_tree(child);
                    return Err(cancelled_error());
                }
                if Instant::now() >= deadline {
                    kill_tree(child);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("{program} exceeded its {}s limit", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                kill_tree(child);
                return Err(error);
            }
        }
    }
}

/// Spawn `argv` with all three streams piped and (Unix) its own process
/// group, so [`kill_tree`] can reap the probe and anything it forked.
fn spawn_grouped(argv: &[String]) -> io::Result<std::process::Child> {
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
    command.spawn()
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
    run_capture_with_cancel(argv, stdin_bytes, timeout, max_out, None)
}

fn run_capture_with_cancel(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: usize,
    cancel: Option<&CancellationToken>,
) -> io::Result<Capture> {
    let program = argv
        .first()
        .map(String::as_str)
        .unwrap_or("<empty argv>")
        .to_string();
    // A task may have sat in the executor after its tree generation was
    // retired. Fail before spawn so cancelled queued work cannot start a late
    // ssh/docker process at all.
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(cancelled_error());
    }
    let mut child = spawn_grouped(argv)?;
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

    let status = match wait_status(&mut child, timeout, &program, cancel) {
        Ok(status) => status,
        Err(error) => {
            // Join the readers so no thread outlives the pipes.
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(error);
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
    jterm_core::review_input::safe_inline_display(text.trim(), EXCERPT)
}

/// Map the probe's documented exit codes onto io error kinds; ssh's reserved
/// 255 is connectivity, and every remaining code is `Other` with bounded
/// stderr context.
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
        // OpenSSH reserves 255 for client/transport failure. Preserve that as
        // a retryable connectivity kind instead of collapsing it into Other.
        Some(255) => io::Error::new(
            io::ErrorKind::NotConnected,
            with_detail("remote connection failed"),
        ),
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
    run_probe_with_cancel(host, op, args, timeout, max_out, None)
}

fn run_probe_with_cancel(
    host: &RemoteHostConfig,
    op: &str,
    args: &[&str],
    timeout: Duration,
    max_out: usize,
    cancel: Option<&CancellationToken>,
) -> io::Result<Vec<u8>> {
    validate_host_for_execution(host)?;
    let argv = if host.docker {
        docker_argv(host, op, args)
    } else {
        ssh_argv(host, &probe_command(op, args))
    };
    let capture =
        run_capture_with_cancel(&argv, PROBE_SCRIPT.as_bytes(), timeout, max_out, cancel)?;
    if !capture.status.success() {
        return Err(probe_status_error(&capture));
    }
    Ok(capture.stdout)
}

/// Resolve and validate the host a remote location points at. Out-of-range
/// indices and grammar violations fail closed before any process is spawned.
pub(crate) fn remote_host<'a>(
    loc: &'a FsLocation,
    hosts: &'a [RemoteHostConfig],
) -> io::Result<&'a RemoteHostConfig> {
    match loc {
        FsLocation::Local => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local location has no remote host",
        )),
        FsLocation::Remote(index) => crate::config::validate_remote_host_at(hosts, *index)
            .map_err(|problem| io::Error::new(io::ErrorKind::InvalidInput, problem)),
        FsLocation::Transient(host) => {
            validate_host_for_execution(host)?;
            Ok(host)
        }
    }
}

/// Defense-in-depth for private helpers that receive a host reference instead
/// of its config index. Public entry points resolve through [`remote_host`]
/// first (which also enforces the 128-entry boundary); this keeps future
/// internal callers from turning an invalid retained draft into process argv.
fn validate_host_for_execution(host: &RemoteHostConfig) -> io::Result<()> {
    crate::config::validate_remote_host(host).map_err(|problem| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "remote host {}: {problem}",
                crate::config::remote_host_runtime_label(host)
            ),
        )
    })
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
/// Invalid UTF-8 and non-component names are skipped: turning raw bytes into a
/// lossy String would make later Rename/Delete target a different path. Exact
/// path collisions are also kept only once. At most MAX_DIRECTORY_ENTRIES are
/// retained, while the one extra pair requested from the probe produces an
/// explicit truncation signal. MAX_SCANNED_PAIRS remains defense-in-depth for
/// malformed output that did not honor the negotiated limit.
#[allow(dead_code)] // compatibility wrapper and default-policy test surface
fn parse_list(bytes: &[u8], dir: &Path) -> Vec<Entry> {
    parse_list_listing(bytes, dir, false).entries
}

#[allow(dead_code)] // compatibility wrapper and hidden-policy test surface
fn parse_list_with_hidden(bytes: &[u8], dir: &Path, show_hidden: bool) -> Vec<Entry> {
    parse_list_listing(bytes, dir, show_hidden).entries
}

fn parse_list_listing(bytes: &[u8], dir: &Path, show_hidden: bool) -> DirectoryListing {
    let mut entries = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut scanned = 0usize;
    let mut truncated = false;
    while let (Some(kind), Some(name)) = (fields.next(), fields.next()) {
        scanned += 1;
        if scanned > MAX_SCANNED_PAIRS {
            truncated = true;
            break;
        }
        // The normal v4 probe emits exactly entries+1 at most. Seeing the
        // extra pair proves at least one remote entry is not displayed.
        if scanned >= DIRECTORY_FETCH_ENTRIES {
            truncated = true;
        }
        let is_dir = match kind {
            b"d" => true,
            b"f" | b"l" => false,
            _ => continue,
        };
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            continue;
        }
        // Match the sidebar behavior. Hidden files remain available by typing
        // paths in the terminal without overwhelming the visual tree.
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = dir.join(name);
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        if entries.len() >= MAX_DIRECTORY_ENTRIES {
            truncated = true;
            continue;
        }
        entries.push(Entry {
            path,
            name: name.to_string(),
            is_dir,
        });
    }
    sort_entries(&mut entries);
    DirectoryListing { entries, truncated }
}

/// Local one-level listing with the same policy (dotfiles hidden, same sort,
/// same caps) and the same error wording the sidebar has always produced.
/// Like parse_list, at most MAX_DIRECTORY_ENTRIES entries are kept and
/// scanning stops after MAX_SCANNED_PAIRS entries, so a huge local directory
/// cannot grow the tree without bound.
#[allow(dead_code)] // compatibility wrapper and default-policy test surface
fn local_list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    local_list_dir_listing(dir, false).map(|listing| listing.entries)
}

#[allow(dead_code)] // compatibility wrapper for Vec-only callers
fn local_list_dir_with_hidden(dir: &Path, show_hidden: bool) -> io::Result<Vec<Entry>> {
    local_list_dir_listing(dir, show_hidden).map(|listing| listing.entries)
}

fn local_list_dir_listing(dir: &Path, show_hidden: bool) -> io::Result<DirectoryListing> {
    local_list_dir_listing_with_cancel(dir, show_hidden, None)
}

fn local_list_dir_listing_with_cancel(
    dir: &Path,
    show_hidden: bool,
    cancel: Option<&CancellationToken>,
) -> io::Result<DirectoryListing> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(cancelled_error());
    }
    let entries = std::fs::read_dir(dir).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Cannot read {}: {error}", dir.display()),
        )
    })?;
    let mut nodes = Vec::new();
    let mut truncated = false;
    for (scanned, entry) in entries.enumerate() {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(cancelled_error());
        }
        if scanned >= MAX_SCANNED_PAIRS {
            truncated = true;
            break;
        }
        let entry = entry.map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Cannot read {}: {error}", dir.display()),
            )
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if nodes.len() >= MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        let file_type = entry.file_type().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Cannot inspect {}: {error}", entry.path().display()),
            )
        })?;
        nodes.push(Entry {
            name,
            path: entry.path(),
            is_dir: file_type.is_dir(),
        });
    }
    sort_entries(&mut nodes);
    Ok(DirectoryListing {
        entries: nodes,
        truncated,
    })
}

/// List exactly one directory level at `dir`, locally or through the probe.
#[allow(dead_code)] // retained for callers that want the default hidden policy
pub fn list_dir(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
) -> io::Result<Vec<Entry>> {
    list_dir_with_hidden(loc, hosts, dir, false)
}

pub fn list_dir_with_hidden(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
    show_hidden: bool,
) -> io::Result<Vec<Entry>> {
    list_dir_listing_with_hidden(loc, hosts, dir, show_hidden).map(|listing| listing.entries)
}

/// List one directory level and retain whether the backend stopped at the
/// negotiated UI limit. Sidebar loads use this form so partial results are
/// visibly identified; the Vec-returning wrappers remain for compatibility.
pub fn list_dir_listing_with_hidden(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
    show_hidden: bool,
) -> io::Result<DirectoryListing> {
    list_dir_listing_with_hidden_cancellable(loc, hosts, dir, show_hidden, None)
}

/// Cancellable directory listing used by the lazy file tree. A retired queued
/// request observes the token before any helper spawn; an in-flight remote
/// probe is killed and reaped by the same process-group watchdog as transfers.
pub fn list_dir_listing_with_hidden_cancellable(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
    show_hidden: bool,
    cancel: Option<&CancellationToken>,
) -> io::Result<DirectoryListing> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(cancelled_error());
    }
    match loc {
        FsLocation::Local => local_list_dir_listing_with_cancel(dir, show_hidden, cancel),
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
            let host = remote_host(loc, hosts)?;
            let limit = DIRECTORY_FETCH_ENTRIES.to_string();
            let hidden = if show_hidden { "1" } else { "0" };
            let stdout = run_probe_with_cancel(
                host,
                "list",
                &[remote_path(dir)?, &limit, hidden],
                LIST_TIMEOUT,
                MAX_LIST_BYTES,
                cancel,
            )
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Cannot read {}: {error}", dir.display()),
                )
            })?;
            Ok(parse_list_listing(&stdout, dir, show_hidden))
        }
    }
}

/// The directory a freshly switched location opens at: today's behavior for
/// local (process cwd), the login home for a remote host.
fn parse_start_dir_output(stdout: &[u8]) -> io::Result<PathBuf> {
    let text = std::str::from_utf8(stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "remote host reported a non-UTF-8 home directory",
        )
    })?;
    let home = text.strip_suffix('\n').unwrap_or(text);
    let home = home.strip_suffix('\r').unwrap_or(home);
    let unsafe_text = home.is_empty()
        || home.chars().any(char::is_control)
        || home.chars().any(|ch| {
            matches!(
                ch,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'
                    | '\u{202b}'
                    | '\u{202c}'
                    | '\u{202d}'
                    | '\u{202e}'
                    | '\u{2066}'
                    | '\u{2067}'
                    | '\u{2068}'
                    | '\u{2069}'
            )
        });
    if unsafe_text || !Path::new(home).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote host did not report one safe absolute home directory",
        ));
    }
    Ok(PathBuf::from(home))
}

pub fn start_dir(loc: &FsLocation, hosts: &[RemoteHostConfig]) -> io::Result<PathBuf> {
    match loc {
        FsLocation::Local => Ok(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
            let host = remote_host(loc, hosts)?;
            let stdout = run_probe(host, "home", &[], LIST_TIMEOUT, MAX_OP_BYTES)?;
            parse_start_dir_output(&stdout)
        }
    }
}

/// Create one directory. Fails (`AlreadyExists`) when anything is there.
pub fn create_dir(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    match loc {
        FsLocation::Local => std::fs::create_dir(path),
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
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
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
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
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
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
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
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
        FsLocation::Remote(_) | FsLocation::Transient(_) => {
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

// ── Cross-location streaming transfers ────────────────────────────────────

/// The argv one probe op spawns with (ssh one-command-string, docker raw).
fn probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> Vec<String> {
    if host.docker {
        docker_argv(host, op, args)
    } else {
        ssh_argv(host, &probe_command(op, args))
    }
}

fn checked_probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> io::Result<Vec<String>> {
    validate_host_for_execution(host)?;
    Ok(probe_argv(host, op, args))
}

/// A transfer overrun, sized to the cap that was actually in force.
fn transfer_cap_error(cap: u64) -> io::Error {
    let limit = if cap >= 1024 * 1024 {
        format!("{} MiB", cap / (1024 * 1024))
    } else {
        format!("{cap} bytes")
    };
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("transfer exceeds the {limit} limit"),
    )
}

/// Copy `reader` → `writer` counting bytes; past `cap` the copy aborts with
/// `InvalidData` instead of streaming unboundedly. With a progress handle
/// attached, the running total is reported (throttled) after every chunk.
fn stream_capped<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    cap: u64,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<u64> {
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(total);
        }
        total = total.saturating_add(read as u64);
        if total > cap {
            return Err(transfer_cap_error(cap));
        }
        writer.write_all(&buffer[..read])?;
        if let Some(progress) = progress {
            progress.report(total);
        }
    }
}

/// A unique, dot-prefixed staging name next to the destination: same
/// filesystem (so the final rename is atomic), hidden from the sidebar's
/// listing, and clearly recognizable as a partial transfer.
fn unique_staging_path(dir: &Path, name: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.join(format!(".{name}.fspart-{}-{n}", std::process::id()))
}

/// Outcome of a finished streaming run: the exit status plus bounded stderr,
/// for the caller to judge (probe exit codes vs. a plain local tar failure).
#[derive(Debug)]
struct StreamOutcome {
    status: std::process::ExitStatus,
    stderr: Vec<u8>,
}

/// Stream one argv's stdout into the fresh temp `temp`, bounded by `cap` and
/// the transfer watchdog. Transport errors (spawn, timeout, cancel, cap
/// overflow, io) unlink the temp and return `Err`; a clean run with a
/// non-zero status leaves the temp for the caller to judge and remove.
fn stream_argv_to_temp(
    argv: &[String],
    stdin_bytes: &[u8],
    temp: &Path,
    cap: u64,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<StreamOutcome> {
    // create_new: the name is unique by construction, and anything already
    // there is not ours to overwrite.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)?;
    let program = argv
        .first()
        .map(String::as_str)
        .unwrap_or("<empty argv>")
        .to_string();
    let result = (|| {
        let mut child = spawn_grouped(argv)?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_bytes);
        }
        let mut stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("child stdout pipe was not created"))?;
        let mut stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("child stderr pipe was not created"))?;
        let stderr_reader =
            std::thread::spawn(move || read_bounded(&mut stderr_pipe, MAX_OP_BYTES));
        let streamer_progress = progress.cloned();
        let streamer = std::thread::spawn(move || {
            stream_capped(&mut stdout_pipe, &mut file, cap, streamer_progress.as_ref())
        });
        // The watchdog covers a stalled remote and the cancel flag; after a
        // group kill the pipes close and both threads end, so the joins below
        // always terminate.
        let status = wait_status(
            &mut child,
            TRANSFER_TIMEOUT,
            &program,
            progress.map(|progress| &progress.cancellation),
        );
        let streamed = streamer
            .join()
            .map_err(|_| io::Error::other("stream writer thread panicked"));
        let stderr = stderr_reader
            .join()
            .map_err(|_| io::Error::other("stream reader thread panicked"));
        let status = status?;
        let total = streamed??;
        if let Some(progress) = progress {
            progress.finish(total);
        }
        let (stderr, _truncated) = stderr??;
        Ok(StreamOutcome { status, stderr })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

/// Stream the probe script plus one local file into one probe op's stdin
/// (`sh -s` reads the script first, the op's `cat`/tar reads the payload
/// after it), bounded by `cap` and the transfer watchdog. A local stream
/// error kills the whole process group, so the probe can never commit a
/// truncated payload as if it were complete.
fn stream_file_to_probe(
    host: &RemoteHostConfig,
    op: &str,
    args: &[&str],
    local_file: &Path,
    cap: u64,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<()> {
    stream_file_to_argv(
        &checked_probe_argv(host, op, args)?,
        local_file,
        cap,
        progress,
    )
}

/// The one line the probe prints on stdout right before it consumes stdin as
/// payload. Streaming earlier races the shell's script buffering and loses
/// bytes; see the probe header.
const READY_MARKER: &[u8] = b"fsprobe-ready\n";

/// Drain a child's stdout, watching for the readiness marker. The marker (or
/// a protocol violation) is reported over `ready`; EOF before it simply drops
/// the sender, which the streamer reads as "defer to the exit status". The
/// stream is drained to the void either way — put/untar stdout carries no
/// payload — so a chatty remote can never wedge on a full pipe.
fn drain_with_ready_marker<R: Read>(
    mut reader: R,
    ready: std::sync::mpsc::Sender<io::Result<()>>,
) -> io::Result<()> {
    let mut window: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut signaled = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        if !signaled {
            window.extend_from_slice(&buffer[..read]);
            if window.ends_with(READY_MARKER) {
                signaled = true;
                let _ = ready.send(Ok(()));
            } else if window.len() > READY_MARKER.len() * 4 + 64 {
                signaled = true;
                let _ = ready.send(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "probe did not signal readiness",
                )));
            } else if window.len() > READY_MARKER.len() {
                // Only a marker-length tail matters for the suffix scan.
                window = window.split_off(window.len() - READY_MARKER.len());
            }
        }
    }
}

/// [`stream_file_to_probe`] at the argv level; `argv` must be a `sh -s`-style
/// probe invocation, because stdin carries the probe script then the file.
fn stream_file_to_argv(
    argv: &[String],
    local_file: &Path,
    cap: u64,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<()> {
    let size = std::fs::metadata(local_file)?.len();
    if size > cap {
        return Err(transfer_cap_error(cap));
    }
    let program = argv
        .first()
        .map(String::as_str)
        .unwrap_or("<empty argv>")
        .to_string();
    let mut child = spawn_grouped(argv)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("child stdin pipe was not created"))?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe was not created"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr pipe was not created"))?;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<()>>();
    let stdout_reader = std::thread::spawn(move || drain_with_ready_marker(stdout_pipe, ready_tx));
    let stderr_reader = std::thread::spawn(move || read_bounded(&mut stderr_pipe, MAX_OP_BYTES));
    let pgid = child.id();
    let path = local_file.to_path_buf();
    let writer_progress = progress.cloned();
    let writer = std::thread::spawn(move || -> io::Result<u64> {
        let result = stdin
            .write_all(PROBE_SCRIPT.as_bytes())
            .and_then(|()| {
                // Wait for the probe's readiness marker before one payload
                // byte: the shell's own script buffering would eat them.
                match ready_rx.recv() {
                    Ok(ready) => ready,
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "probe exited before signaling readiness",
                    )),
                }
            })
            .and_then(|()| {
                let mut file = std::fs::File::open(&path)?;
                stream_capped(&mut file, &mut stdin, cap, writer_progress.as_ref())
            });
        if let Err(error) = &result {
            // BrokenPipe/UnexpectedEof mean the child is already gone and its
            // exit status carries the truth; anything else must not let the
            // probe mistake a truncated stream for a complete one.
            if !matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
            ) {
                #[cfg(unix)]
                unsafe {
                    // SAFETY: one kill call on the group the child led; the
                    // pid came from a live Child handle.
                    libc::kill(-(pgid as i32), libc::SIGKILL);
                }
            }
        }
        result
    });
    let status = wait_status(
        &mut child,
        TRANSFER_TIMEOUT,
        &program,
        progress.map(|progress| &progress.cancellation),
    );
    let written = writer
        .join()
        .map_err(|_| io::Error::other("stream writer thread panicked"));
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("stream reader thread panicked"));
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("stream reader thread panicked"));
    let status = status?;
    match written? {
        Ok(total) => {
            if let Some(progress) = progress {
                progress.finish(total);
            }
        }
        // The probe closed its end early (a pre-read exit 17, say); its exit
        // status below carries the real reason.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
            ) => {}
        Err(error) => return Err(error),
    }
    stdout??;
    let (stderr, _truncated) = stderr??;
    if !status.success() {
        return Err(probe_status_error(&Capture {
            status,
            stdout: Vec::new(),
            stderr,
        }));
    }
    Ok(())
}

/// Rename a staged download into place after a final existence check; the
/// temp is unlinked on failure either way.
fn finalize_download(temp: &Path, dst: &Path) -> io::Result<()> {
    let result = ensure_absent(dst).and_then(|()| std::fs::rename(temp, dst));
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

/// Directory transfers shell out to the system tar on both ends; say so
/// clearly when this end has none, before any remote work starts.
fn verify_local_tar() -> io::Result<()> {
    let argv = ["tar".to_string(), "--version".to_string()];
    match run_capture(&argv, b"", Duration::from_secs(10), MAX_OP_BYTES) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "local `tar` is required for directory transfers",
        )),
        Err(error) => Err(error),
    }
}

/// Stage a local directory as a capped tar stream in a temp file, ready to
/// feed a remote `untar`.
fn stage_local_tar(
    src: &Path,
    cap: u64,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<PathBuf> {
    verify_local_tar()?;
    let name = src
        .file_name()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory has no name to pack")
        })?
        .to_string_lossy()
        .into_owned();
    let parent = src.parent().unwrap_or_else(|| Path::new("/"));
    let parent = parent.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path is not valid UTF-8",
        )
    })?;
    let temp = unique_staging_path(&std::env::temp_dir(), "frost-upload");
    let argv: Vec<String> = ["tar", "cf", "-", "-C", parent, &name]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    let outcome = stream_argv_to_temp(&argv, b"", &temp, cap, progress)?;
    if !outcome.status.success() {
        let _ = std::fs::remove_file(&temp);
        return Err(io::Error::other(format!(
            "local tar failed: {}",
            stderr_excerpt(&outcome.stderr)
        )));
    }
    Ok(temp)
}

/// Extract a staged tar into `dst_parent` with the system tar.
fn extract_tar(archive: &Path, dst_parent: &Path) -> io::Result<()> {
    let archive = archive.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "archive path is not valid UTF-8",
        )
    })?;
    let parent = dst_parent.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path is not valid UTF-8",
        )
    })?;
    let argv: Vec<String> = ["tar", "xf", archive, "-C", parent]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    let capture = run_capture(&argv, b"", TRANSFER_TIMEOUT, MAX_OP_BYTES)?;
    if !capture.status.success() {
        return Err(io::Error::other(format!(
            "local tar failed: {}",
            stderr_excerpt(&capture.stderr)
        )));
    }
    Ok(())
}

/// One parsed `stat` probe line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProbeStat {
    is_dir: bool,
    size: u64,
}

/// `stat <path>` on a validated host: `<t> <size>`, `NotFound` on exit 3.
fn remote_stat(host: &RemoteHostConfig, path: &Path) -> io::Result<ProbeStat> {
    let stdout = run_probe(
        host,
        "stat",
        &[remote_path(path)?],
        LIST_TIMEOUT,
        MAX_OP_BYTES,
    )?;
    let text = String::from_utf8_lossy(&stdout);
    let mut fields = text.split_whitespace();
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unparseable stat reply: {}", text.trim()),
        )
    };
    let (Some(kind), Some(size), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(invalid());
    };
    let is_dir = match kind {
        "d" => true,
        "f" | "l" => false,
        _ => return Err(invalid()),
    };
    let size = size.parse::<u64>().map_err(|_| invalid())?;
    Ok(ProbeStat { is_dir, size })
}

/// Friendly pre-stream check: does `dst` already exist on this remote? `untar`
/// re-refuses with 17 before extracting and `put` with 17 twice, so this scan
/// only makes the common collision cheap to report — and unlike a `list` scan
/// it sees dotfiles too.
fn remote_name_taken(loc: &FsLocation, hosts: &[RemoteHostConfig], dst: &Path) -> io::Result<bool> {
    let host = remote_host(loc, hosts)?;
    match remote_stat(host, dst) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Directory tars carry the source's top-level name, so a directory transfer
/// only lands at `dst` when source and destination share that name.
fn require_same_name(src: &Path, dst: &Path) -> io::Result<()> {
    if src.file_name() != dst.file_name() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory transfers require the destination to keep the source's name",
        ));
    }
    Ok(())
}

/// Download `src` from a remote host to the local `dst` (which must not
/// exist): stream-to-temp + rename for regular files, a capped tar relay for
/// directories. Partial results never land on `dst`.
pub fn download(
    remote_loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
    is_dir: bool,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<()> {
    let host = remote_host(remote_loc, hosts)?;
    // The friendly before-check: fail on an existing destination before a
    // single byte streams.
    ensure_absent(dst)?;
    let dst_parent = dst.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination has no parent directory",
        )
    })?;
    let name = dst
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name"))?
        .to_string_lossy()
        .into_owned();
    if is_dir {
        require_same_name(src, dst)?;
        verify_local_tar()?;
        let temp = unique_staging_path(dst_parent, &name);
        let outcome = stream_argv_to_temp(
            &checked_probe_argv(host, "tar", &[remote_path(src)?])?,
            PROBE_SCRIPT.as_bytes(),
            &temp,
            MAX_TRANSFER_BYTES,
            progress,
        )?;
        if !outcome.status.success() {
            let _ = std::fs::remove_file(&temp);
            return Err(probe_status_error(&Capture {
                status: outcome.status,
                stdout: Vec::new(),
                stderr: outcome.stderr,
            }));
        }
        // The archive names the source's top directory; extracting into the
        // destination's parent recreates it at `dst`.
        if let Err(error) = extract_tar(&temp, dst_parent) {
            let _ = std::fs::remove_file(&temp);
            // Nothing was at `dst` before; drop the partial extraction.
            let _ = std::fs::remove_dir_all(dst);
            return Err(error);
        }
        let _ = std::fs::remove_file(&temp);
        Ok(())
    } else {
        // `stat` up front: a friendly NotFound (or "is a directory") before
        // one byte streams, and an expected size to verify the stream against
        // — the download mirror of put's declared-size re-check.
        let expected = match remote_stat(host, src)? {
            ProbeStat { is_dir: true, .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is a directory", src.display()),
                ));
            }
            ProbeStat { size, .. } => size,
        };
        let temp = unique_staging_path(dst_parent, &name);
        let outcome = stream_argv_to_temp(
            &checked_probe_argv(host, "cat", &[remote_path(src)?])?,
            PROBE_SCRIPT.as_bytes(),
            &temp,
            MAX_TRANSFER_BYTES,
            progress,
        )?;
        if !outcome.status.success() {
            let _ = std::fs::remove_file(&temp);
            return Err(probe_status_error(&Capture {
                status: outcome.status,
                stdout: Vec::new(),
                stderr: outcome.stderr,
            }));
        }
        let landed = std::fs::metadata(&temp).map(|metadata| metadata.len())?;
        if landed != expected {
            let _ = std::fs::remove_file(&temp);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "download truncated: expected {}, got {}",
                    format_bytes(expected),
                    format_bytes(landed)
                ),
            ));
        }
        finalize_download(&temp, dst)
    }
}

/// Upload the local `src` to `dst` on a remote host (which must not exist):
/// `put` checks the collision before reading a byte and again before its mv,
/// so the friendly error costs no upload. Directories go staged tar → `untar`.
pub fn upload(
    remote_loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
    is_dir: bool,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<()> {
    let host = remote_host(remote_loc, hosts)?;
    if is_dir {
        require_same_name(src, dst)?;
        verify_local_tar()?;
        // The v3 `untar` refuses an existing `<dir>/<name>` with 17 before
        // extracting; this scan only makes the common collision cheap to
        // report. Between it and the extraction a third party could still
        // create the path — tar would then extract INTO it, a documented
        // microscopic TOCTOU window the protocol cannot close.
        if remote_name_taken(remote_loc, hosts, dst)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", dst.display()),
            ));
        }
        let remote_parent = dst.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination has no parent directory",
            )
        })?;
        let name = dst
            .file_name()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
            })?
            .to_string_lossy()
            .into_owned();
        let staged = stage_local_tar(src, MAX_TRANSFER_BYTES, progress)?;
        let result = stream_file_to_probe(
            host,
            "untar",
            &[remote_path(remote_parent)?, &name],
            &staged,
            MAX_TRANSFER_BYTES,
            progress,
        );
        let _ = std::fs::remove_file(&staged);
        result
    } else {
        let metadata = std::fs::metadata(src)?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", src.display()),
            ));
        }
        // The declared size lets the probe refuse a truncated stream instead
        // of committing it (a cancelled/killed upload never lands partial).
        let size = metadata.len().to_string();
        stream_file_to_probe(
            host,
            "put",
            &[remote_path(dst)?, &size],
            src,
            MAX_TRANSFER_BYTES,
            progress,
        )
    }
}

/// Copy `src` to `dst` across locations, streaming with hard byte and time
/// bounds. Local→Local is the plain recursive copy; Local⇄Remote is
/// upload/download; two distinct remote namespaces relay through a unique
/// local temp path — download, upload, clean up. Saved/temporary routes to one
/// namespace instead execute directly through the live ControlPath side.
pub fn transfer(
    src_loc: &FsLocation,
    dst_loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
    is_dir: bool,
    progress: Option<&std::sync::Arc<TransferProgress>>,
) -> io::Result<()> {
    match (src_loc.is_remote(), dst_loc.is_remote()) {
        (false, false) => copy(src_loc, hosts, src, dst),
        (false, true) => upload(dst_loc, hosts, src, dst, is_dir, progress),
        (true, false) => download(src_loc, hosts, src, dst, is_dir, progress),
        (true, true) if same_files_location(src_loc, dst_loc, hosts) => {
            let execution = preferred_same_files_execution_location(src_loc, dst_loc, hosts)
                .expect("same namespace has an execution route");
            copy(execution, hosts, src, dst)
        }
        (true, true) => {
            let relay = unique_staging_path(&std::env::temp_dir(), "frost-relay");
            let (staged, is_relay_dir) = if is_dir {
                // A directory's tar carries its name, so the relay download
                // must land inside a fresh parent it can keep that name in.
                std::fs::create_dir(&relay)?;
                let name = src.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "source has no file name")
                })?;
                (relay.join(name), true)
            } else {
                (relay.clone(), false)
            };
            let result = download(src_loc, hosts, src, &staged, is_dir, progress)
                .and_then(|()| upload(dst_loc, hosts, &staged, dst, is_dir, progress));
            let cleanup = if is_relay_dir {
                std::fs::remove_dir_all(&relay)
            } else {
                std::fs::remove_file(&relay)
            };
            match (result, cleanup) {
                (Err(error), _) => Err(error),
                (Ok(()), Err(error)) => Err(io::Error::other(format!(
                    "transfer finished but relay temp {} could not be removed: {error}",
                    relay.display()
                ))),
                (Ok(()), Ok(())) => Ok(()),
            }
        }
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
        assert_eq!(
            probe_command("list", &["/tmp", "4097", "0"]),
            "sh -s -- list '/tmp' '4097' '0'"
        );
        assert_eq!(
            probe_command("mv", &["/a o'ne", "/b two"]),
            "sh -s -- mv '/a o'\\''ne' '/b two'"
        );
    }

    #[test]
    fn remote_home_output_is_strict_absolute_utf8_and_single_line() {
        assert_eq!(
            parse_start_dir_output(b"/home/alice\n").expect("ordinary home"),
            PathBuf::from("/home/alice")
        );
        assert_eq!(
            parse_start_dir_output("/home/雪\n".as_bytes()).expect("unicode home"),
            PathBuf::from("/home/雪")
        );
        for unsafe_output in [
            b"relative\n".as_slice(),
            b"/safe\n/forged\n".as_slice(),
            b"/safe\0tail\n".as_slice(),
            b"/safe\x1b[31m\n".as_slice(),
            b"/bad\xff\n".as_slice(),
        ] {
            assert!(parse_start_dir_output(unsafe_output).is_err());
        }
        assert!(parse_start_dir_output("/safe\u{202e}txt\n".as_bytes()).is_err());
    }

    #[test]
    fn stderr_excerpt_is_bounded_and_safe_for_inline_ui() {
        let excerpt = stderr_excerpt(
            "first\nsecond\u{1b}[31mred\u{202e}txt雪雪雪"
                .repeat(100)
                .as_bytes(),
        );
        assert!(!excerpt.contains('\n'));
        assert!(!excerpt.contains('\u{1b}'));
        assert!(!excerpt.contains('\u{202e}'));
        assert!(excerpt.len() <= 256);
        assert!(std::str::from_utf8(excerpt.as_bytes()).is_ok());
    }

    #[test]
    fn ssh_argv_places_options_args_dest_and_one_command() {
        let argv = ssh_argv(&ssh_host(), "sh -s -- list '/tmp' '4097' '0'");
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
                "sh -s -- list '/tmp' '4097' '0'",
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
        let argv = docker_argv(&docker_host(), "list", &["/var/log", "4097", "0"]);
        assert_eq!(
            argv,
            vec![
                "docker", "exec", "-i", "-u", "root", "myubuntu", "sh", "-s", "--", "list",
                "/var/log", "4097", "0",
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

    /// Drive one stdin-consuming probe op through the real client path:
    /// `payload` staged in a file, streamed after the readiness marker.
    fn probe_stream_in(op: &str, args: &[&str], payload: &[u8]) -> io::Result<()> {
        let staging = std::env::temp_dir().join(format!("frost-payload-{}", uuid::Uuid::new_v4()));
        std::fs::write(&staging, payload).expect("stage payload");
        let mut argv: Vec<String> = ["sh", "-s", "--", op]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        let result = stream_file_to_argv(&argv, &staging, MAX_TRANSFER_BYTES, None);
        let _ = std::fs::remove_file(&staging);
        result
    }

    /// Binary-safe fixture: NULs, 0xFF, no trailing newline.
    fn binary_payload() -> Vec<u8> {
        let mut bytes: Vec<u8> = (0u16..=1024).map(|i| (i % 251) as u8).collect();
        bytes.push(0);
        bytes.push(0xff);
        bytes
    }

    #[test]
    fn probe_list_reports_types_and_relative_names() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let capture = probe("list", &[&root_str, "4097", "0"]);
        assert!(capture.status.success());
        let entries = parse_list(&capture.stdout, &root);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // The probe applies the requested hidden policy; the parser repeats it
        // as defense-in-depth. Directories still sort first.
        assert_eq!(names, vec!["nested", "file.txt", "with space.txt"]);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, root.join("file.txt"));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_list_rejects_relative_and_missing_dirs() {
        assert_eq!(
            probe("list", &["relative/path", "4097", "0"]).status.code(),
            Some(2)
        );
        assert_eq!(
            probe("list", &["/definitely/not/there", "4097", "0"])
                .status
                .code(),
            Some(3)
        );
        assert_eq!(probe("list", &["/tmp", "0", "0"]).status.code(), Some(2));
        assert_eq!(
            probe("list", &["/tmp", "4097", "maybe"]).status.code(),
            Some(2)
        );
    }

    #[test]
    fn probe_list_honors_the_client_limit_and_hidden_policy() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let limited = probe("list", &[&root_str, "2", "0"]);
        assert!(limited.status.success());
        assert_eq!(limited.stdout.split(|byte| *byte == 0).count() / 2, 2);
        let hidden = probe("list", &[&root_str, "16", "1"]);
        assert!(hidden.status.success());
        assert!(parse_list_with_hidden(&hidden.stdout, &root, true)
            .iter()
            .any(|entry| entry.name == ".hidden"));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[cfg(unix)]
    #[test]
    fn probe_list_classifies_a_directory_symlink_as_a_non_directory() {
        let root = temp_tree();
        let link = root.join("nested-link");
        std::os::unix::fs::symlink(root.join("nested"), &link).expect("create directory symlink");
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let capture = probe("list", &[&root_str, "4097", "0"]);
        assert!(capture.status.success());
        let entries = parse_list(&capture.stdout, &root);
        let link = entries
            .iter()
            .find(|entry| entry.name == "nested-link")
            .expect("listed symlink");
        assert!(!link.is_dir, "a directory symlink must never be expandable");
        std::fs::remove_dir_all(root).expect("remove test tree");
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

        let relative = probe("list", &["nope", "4097", "0"]);
        assert_eq!(
            probe_status_error(&relative).kind(),
            io::ErrorKind::InvalidInput
        );
        let absent = probe("list", &["/definitely/not/there", "4097", "0"]);
        assert_eq!(probe_status_error(&absent).kind(), io::ErrorKind::NotFound);

        let ssh_failure = Capture {
            status: Command::new("sh")
                .args(["-c", "exit 255"])
                .status()
                .expect("run exit fixture"),
            stdout: Vec::new(),
            stderr: b"connection reset".to_vec(),
        };
        assert_eq!(
            probe_status_error(&ssh_failure).kind(),
            io::ErrorKind::NotConnected
        );
    }

    // ── Probe v2 streaming ops, exercised against the real `sh` ────────

    #[test]
    fn probe_cat_streams_binary_and_rejects_non_files() {
        let root = temp_tree();
        let path = root.join("blob.bin");
        std::fs::write(&path, binary_payload()).expect("write blob");
        let path_str = path.to_str().expect("utf-8 temp path").to_string();
        let capture = probe("cat", &[&path_str]);
        assert!(capture.status.success());
        assert_eq!(capture.stdout, binary_payload());

        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        // A directory and a missing file are both exit 3, never a stream.
        assert_eq!(probe("cat", &[&root_str]).status.code(), Some(3));
        let missing = format!("{root_str}/not-there");
        assert_eq!(probe("cat", &[&missing]).status.code(), Some(3));
        assert_eq!(probe("cat", &["relative"]).status.code(), Some(2));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_put_writes_new_files_and_refuses_collisions() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let path = format!("{root_str}/uploaded.bin");
        probe_stream_in("put", &[&path], &binary_payload()).expect("put");
        assert_eq!(
            std::fs::read(root.join("uploaded.bin")).expect("read uploaded"),
            binary_payload()
        );
        // The staging temp is moved into place, not left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .expect("read test tree")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains("fspart"))
            .collect();
        assert!(leftovers.is_empty());
        // Existing target: 17 before reading a byte, surfaced as AlreadyExists.
        let error = probe_stream_in("put", &[&path], b"again").expect_err("exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(root.join("uploaded.bin")).expect("read uploaded"),
            binary_payload()
        );
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_tar_untar_roundtrip() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        std::fs::write(root.join("nested").join("blob.bin"), binary_payload())
            .expect("write nested blob");
        // Stream the dir as tar, then untar it into a fresh dir next door.
        let packed = probe("tar", &[&format!("{root_str}/nested")]);
        assert!(packed.status.success());
        assert!(packed.stdout.len() > 512);
        let unpacked = format!("{root_str}/unpacked");
        std::fs::create_dir(&unpacked).expect("create unpack dir");
        probe_stream_in("untar", &[&unpacked, "nested"], &packed.stdout).expect("untar");
        assert_eq!(
            std::fs::read(root.join("unpacked").join("nested").join("blob.bin"))
                .expect("read unpacked blob"),
            binary_payload()
        );
        // tar keeps the fixture's empty directory and its sibling file too.
        assert!(root.join("unpacked").join("nested").join("deep").is_dir());
        assert_eq!(
            std::fs::read(root.join("unpacked").join("nested").join("inner.txt"))
                .expect("read inner"),
            b"y"
        );
        // v3: an existing <dir>/<name> is refused with 17 before extracting.
        let error = probe_stream_in("untar", &[&unpacked, "nested"], &packed.stdout)
            .expect_err("collision");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        // A bogus name fails usage, a missing dir is 3, a non-dir tar is 3.
        let bad_name = probe("untar", &[&unpacked, "a/b"]);
        assert_eq!(bad_name.status.code(), Some(2));
        let gone = format!("{root_str}/gone");
        let error =
            probe_stream_in("untar", &[&gone, "nested"], &packed.stdout).expect_err("missing dir");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let file = format!("{root_str}/file.txt");
        assert_eq!(probe("tar", &[&file]).status.code(), Some(3));
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_stat_reports_type_and_size() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let stat = probe("stat", &[&root_str]);
        assert!(stat.status.success());
        assert_eq!(String::from_utf8_lossy(&stat.stdout).trim(), "d 0");
        let file = format!("{root_str}/file.txt");
        let stat = probe("stat", &[&file]);
        assert_eq!(String::from_utf8_lossy(&stat.stdout).trim(), "f 1");
        let link = root.join("link");
        std::os::unix::fs::symlink(root.join("file.txt"), &link).expect("symlink");
        let link_str = link.to_str().expect("utf-8 temp path").to_string();
        let stat = probe("stat", &[&link_str]);
        assert_eq!(String::from_utf8_lossy(&stat.stdout).trim(), "l 0");
        // A dangling symlink is still `l`, a missing path is exit 3.
        let dangling = root.join("dangling");
        std::os::unix::fs::symlink(root.join("gone"), &dangling).expect("symlink");
        let dangling_str = dangling.to_str().expect("utf-8 temp path").to_string();
        let stat = probe("stat", &[&dangling_str]);
        assert_eq!(String::from_utf8_lossy(&stat.stdout).trim(), "l 0");
        let missing = format!("{root_str}/missing");
        let stat = probe("stat", &[&missing]);
        assert_eq!(stat.status.code(), Some(3));
        assert_eq!(probe_status_error(&stat).kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    // ── Streaming helpers ───────────────────────────────────────────────

    #[test]
    fn stream_capped_aborts_past_the_cap() {
        let mut out = Vec::new();
        let error = stream_capped(&b"0123456789"[..], &mut out, 4, None).expect_err("over cap");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let mut out = Vec::new();
        let bytes = stream_capped(&b"0123"[..], &mut out, 4, None).expect("under cap");
        assert_eq!(bytes, 4);
        assert_eq!(out, b"0123");
    }

    #[test]
    fn stream_argv_to_temp_cleans_up_on_overflow() {
        let root = temp_tree();
        let temp = unique_staging_path(&root, "flood");
        let argv: Vec<String> = ["sh", "-c", "yes flooded | head -c 100000"]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        let error = stream_argv_to_temp(&argv, b"", &temp, 4096, None).expect_err("over cap");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!temp.exists(), "partial temp must be unlinked");
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn stream_file_to_argv_feeds_script_then_payload() {
        let root = temp_tree();
        let blob = root.join("blob.bin");
        std::fs::write(&blob, binary_payload()).expect("write blob");
        let dst = root.join("deposited.bin");
        let dst_str = dst.to_str().expect("utf-8 temp path").to_string();
        let argv: Vec<String> = ["sh", "-s", "--", "put", &dst_str]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        stream_file_to_argv(&argv, &blob, MAX_TRANSFER_BYTES, None).expect("upload via sh");
        assert_eq!(
            std::fs::read(&dst).expect("read deposited"),
            binary_payload()
        );
        // A collision surfaces as AlreadyExists through the same path.
        let error =
            stream_file_to_argv(&argv, &blob, MAX_TRANSFER_BYTES, None).expect_err("exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        // The up-front metadata check fires before any spawn.
        let huge = root.join("huge.bin");
        std::fs::write(&huge, b"xx").expect("write huge stub");
        let error = stream_file_to_argv(&argv, &huge, 1, None).expect_err("over cap up front");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn finalize_download_refuses_a_raced_target() {
        let root = temp_tree();
        let temp = root.join("staged");
        std::fs::write(&temp, b"new").expect("write staged");
        let dst = root.join("file.txt");
        // `file.txt` exists in the fixture: the final check fails closed and
        // the staged bytes are removed rather than clobbering anything.
        let error = finalize_download(&temp, &dst).expect_err("raced target");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!temp.exists());
        assert_eq!(std::fs::read(&dst).expect("read original"), b"x");

        let fresh = root.join("fresh.txt");
        let temp2 = root.join("staged2");
        std::fs::write(&temp2, b"new2").expect("write staged2");
        finalize_download(&temp2, &fresh).expect("rename into place");
        assert_eq!(std::fs::read(&fresh).expect("read fresh"), b"new2");
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn unique_staging_path_never_repeats() {
        let root = PathBuf::from("/tmp");
        let first = unique_staging_path(&root, "name");
        let second = unique_staging_path(&root, "name");
        assert_ne!(first, second);
        assert!(first
            .file_name()
            .expect("name")
            .to_string_lossy()
            .starts_with(".name.fspart-"));
    }

    #[test]
    fn stage_and_extract_tar_roundtrip() {
        let root = temp_tree();
        std::fs::write(root.join("nested").join("blob.bin"), binary_payload())
            .expect("write nested blob");
        let staged =
            stage_local_tar(&root.join("nested"), MAX_TRANSFER_BYTES, None).expect("stage tar");
        let unpack = root.join("unpacked");
        std::fs::create_dir(&unpack).expect("create unpack dir");
        extract_tar(&staged, &unpack).expect("extract");
        let _ = std::fs::remove_file(&staged);
        assert_eq!(
            std::fs::read(unpack.join("nested").join("blob.bin")).expect("read blob"),
            binary_payload()
        );
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    // ── Cross-location dispatch ─────────────────────────────────────────

    // ── Cross-location dispatch ─────────────────────────────────────────

    #[test]
    fn transfer_dispatch_delegates_and_fails_closed() {
        let root = temp_tree();
        // Local→Local is the plain recursive copy.
        let dst = root.join("copied.txt");
        transfer(
            &FsLocation::Local,
            &FsLocation::Local,
            &[],
            &root.join("file.txt"),
            &dst,
            false,
            None,
        )
        .expect("local transfer");
        assert_eq!(std::fs::read(&dst).expect("read copy"), b"x");
        // Unknown hosts fail closed before any process is spawned, on every
        // cross-location shape including the relay.
        let file = root.join("file.txt");
        let up = transfer(
            &FsLocation::Local,
            &FsLocation::Remote(9),
            &[],
            &file,
            Path::new("/tmp/file.txt"),
            false,
            None,
        );
        assert_eq!(up.expect_err("upload").kind(), io::ErrorKind::InvalidInput);
        let down = transfer(
            &FsLocation::Remote(9),
            &FsLocation::Local,
            &[],
            Path::new("/etc/hostname"),
            &root.join("downloaded"),
            false,
            None,
        );
        assert_eq!(
            down.expect_err("download").kind(),
            io::ErrorKind::InvalidInput
        );
        let relay = transfer(
            &FsLocation::Remote(0),
            &FsLocation::Remote(1),
            &[],
            Path::new("/etc/hostname"),
            Path::new("/tmp/hostname"),
            false,
            None,
        );
        assert_eq!(
            relay.expect_err("relay").kind(),
            io::ErrorKind::InvalidInput
        );
        // The relay left no temp behind after failing before it started.
        let strays: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .expect("read temp dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains("frost-relay"))
            .collect();
        assert!(strays.is_empty());
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    // ── Progress, formatting, cancellation ─────────────────────────────

    #[test]
    fn format_bytes_picks_the_right_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(13_002_342), "12.4 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(
            format_bytes(3 * 1024 * 1024 * 1024 + 608_000_000),
            "3.6 GiB"
        );
    }

    #[test]
    fn progress_throttle_publishes_on_time_or_bytes() {
        let start = Instant::now();
        // After a publish, either 250 ms or 256 KiB must pass before the next.
        assert!(!should_publish(
            (start, 0),
            start + Duration::from_millis(10),
            100
        ));
        assert!(should_publish(
            (start, 0),
            start + Duration::from_millis(250),
            100
        ));
        assert!(should_publish(
            (start, 0),
            start + Duration::from_millis(10),
            256 * 1024
        ));
        assert!(!should_publish(
            (start, 1000),
            start + Duration::from_millis(10),
            1000 + 256 * 1024 - 1
        ));

        let progress = TransferProgress::new();
        progress.report(64);
        assert_eq!(progress.bytes(), 64, "first report publishes immediately");
        progress.report(128);
        assert_eq!(progress.bytes(), 64, "throttled: too soon, too few bytes");
        progress.report(64 + 256 * 1024);
        assert_eq!(progress.bytes(), 64 + 256 * 1024, "byte-delta publishes");
        progress.finish(999);
        assert_eq!(progress.bytes(), 999, "finish always publishes");
    }

    #[test]
    fn cancel_kills_the_group_and_cleans_the_temp() {
        let root = temp_tree();
        let temp = unique_staging_path(&root, "sleep");
        let argv: Vec<String> = ["sh", "-c", "sleep 30"]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        let progress = TransferProgress::new();
        let token = progress.clone();
        let gate = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            token.cancel();
        });
        let started = Instant::now();
        let error = stream_argv_to_temp(&argv, b"", &temp, 4096, Some(&progress))
            .expect_err("cancel must abort the stream");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!temp.exists(), "partial temp must be unlinked on cancel");
        gate.join().expect("cancel thread");
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn probe_put_verifies_the_declared_size() {
        let root = temp_tree();
        let root_str = root.to_str().expect("utf-8 temp path").to_string();
        let path = format!("{root_str}/sized.bin");
        // Declared size matches: the file lands.
        probe_stream_in("put", &[&path, "6"], b"abcdef").expect("put with size");
        assert_eq!(
            std::fs::read(root.join("sized.bin")).expect("read"),
            b"abcdef"
        );
        // Declared size short: truncated stream fails 4 and leaves nothing —
        // this is what keeps a killed upload from committing a partial file.
        let short = format!("{root_str}/short.bin");
        let error = probe_stream_in("put", &[&short, "100"], b"abc").expect_err("size mismatch");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(!root.join("short.bin").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .expect("read test tree")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains("fspart"))
            .collect();
        assert!(leftovers.is_empty(), "failed put must not leave its temp");
        std::fs::remove_dir_all(root).expect("remove test tree");
    } // ── List parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_list_skips_unsafe_names_and_deduplicates_paths() {
        let dir = Path::new("/base");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"d\0dir one\0");
        bytes.extend_from_slice(b"f\0line\nbreak\0");
        bytes.extend_from_slice(b"l\0sym link\0");
        bytes.extend_from_slice(b"f\0.hidden\0");
        bytes.extend_from_slice(b"f\0\xff\xfe raw\0");
        bytes.extend_from_slice("f\0�� raw\0".as_bytes());
        bytes.extend_from_slice(b"f\0dir one\0");
        bytes.extend_from_slice(b"f\0nested/name\0");
        bytes.extend_from_slice(b"x\0unknown kind\0");
        // A dangling half-pair (as a capped read leaves) is simply dropped.
        bytes.extend_from_slice(b"d\0");
        let entries = parse_list(&bytes, dir);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["dir one", "line\nbreak", "sym link", "�� raw"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir && !entries[2].is_dir);
        assert_eq!(entries[3].path, dir.join("�� raw"));
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.name == "dir one")
                .count(),
            1,
            "the first type for a duplicate path wins"
        );
        assert!(entries.iter().all(|entry| entry.name != "nested/name"));

        let shown = parse_list_with_hidden(&bytes, dir, true);
        let shown_names: Vec<&str> = shown.iter().map(|entry| entry.name.as_str()).collect();
        assert!(shown_names.contains(&".hidden"));
    }

    #[test]
    fn parse_list_sorts_dirs_first_case_insensitively() {
        let dir = Path::new("/base");
        let bytes = b"f\0Zulu\0d\0beta\0f\0apple\0d\0Alpha\0".to_vec();
        let entries = parse_list(&bytes, dir);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "apple", "Zulu"]);
    }

    #[test]
    fn parse_list_caps_entries_at_max() {
        let dir = Path::new("/base");
        let mut bytes = Vec::new();
        for index in 0..MAX_DIRECTORY_ENTRIES + 50 {
            bytes.extend_from_slice(format!("f\0file-{index:06}\0").as_bytes());
        }
        let listing = parse_list_listing(&bytes, dir, false);
        // Parsing stays bounded; the tree never sees more than the cap.
        assert_eq!(listing.entries.len(), MAX_DIRECTORY_ENTRIES);
        assert!(listing.truncated);
    }

    #[test]
    fn parse_list_stops_scanning_past_the_pair_bound() {
        let dir = Path::new("/base");
        let mut bytes = Vec::new();
        // Skipped pairs (here dotfiles) must not keep the loop alive forever:
        // after MAX_SCANNED_PAIRS of them the parse stops, so the visible
        // entries behind them are never reached.
        for index in 0..MAX_SCANNED_PAIRS + 10 {
            bytes.extend_from_slice(format!("f\0.hidden-{index:06}\0").as_bytes());
        }
        bytes.extend_from_slice(b"f\0visible\0");
        let listing = parse_list_listing(&bytes, dir, false);
        assert!(listing.entries.is_empty());
        assert!(listing.truncated);
    }

    #[test]
    fn local_list_dir_caps_entries_at_max() {
        let root = std::env::temp_dir().join(format!("frost-remote-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create test tree");
        for index in 0..MAX_DIRECTORY_ENTRIES + 50 {
            std::fs::write(root.join(format!("file-{index:06}")), b"x").expect("write test file");
        }
        let listing = local_list_dir_listing(&root, false).expect("list dir");
        // The local listing is bounded by the same cap as the remote one.
        assert_eq!(listing.entries.len(), MAX_DIRECTORY_ENTRIES);
        assert!(listing.truncated);
        std::fs::remove_dir_all(root).expect("remove test tree");
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
        let mut visually_deceptive = ssh_host();
        visually_deceptive.name.push('\u{202e}');
        let direct = run_probe(
            &visually_deceptive,
            "home",
            &[],
            Duration::from_secs(1),
            MAX_OP_BYTES,
        );
        assert_eq!(
            direct
                .expect_err("private probe boundary must recheck the app gate")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let listed = list_dir(
            &FsLocation::Remote(0),
            &[visually_deceptive],
            Path::new("/tmp"),
        );
        assert_eq!(
            listed.expect_err("visual-spoofing host").kind(),
            io::ErrorKind::InvalidInput
        );
        let over_limit = vec![ssh_host(); crate::config::MAX_REMOTE_HOSTS + 1];
        let listed = list_dir(
            &FsLocation::Remote(crate::config::MAX_REMOTE_HOSTS),
            &over_limit,
            Path::new("/tmp"),
        );
        let error = listed.expect_err("129th host must remain inactive");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("active limit"));
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
    fn cancelled_queued_capture_never_spawns_its_child() {
        let root = temp_tree();
        let marker = root.join("spawned");
        let argv = vec!["touch".to_string(), marker.display().to_string()];
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = run_capture_with_cancel(
            &argv,
            b"",
            Duration::from_secs(5),
            1024,
            Some(&cancellation),
        )
        .expect_err("a retired queued request must fail before spawn");

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!marker.exists(), "the cancelled command must never run");
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn cancellation_kills_an_in_flight_capture_group() {
        let argv: Vec<String> = ["sh", "-c", "sleep 30"]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        let cancellation = CancellationToken::new();
        let canceller = cancellation.clone();
        let gate = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            canceller.cancel();
        });
        let started = Instant::now();

        let error = run_capture_with_cancel(
            &argv,
            b"",
            Duration::from_secs(30),
            1024,
            Some(&cancellation),
        )
        .expect_err("an in-flight probe must stop on cancellation");

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(10));
        gate.join().expect("cancel thread");
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
        let transient = FsLocation::Transient(hosts[0].clone());
        assert_eq!(
            transient.label(&[]),
            "ssh: dev (temporary)",
            "an observed target stays visible without becoming saved config"
        );
        assert_eq!(remote_host(&transient, &[]).unwrap(), &hosts[0]);

        let mut invalid = hosts[0].clone();
        invalid.host.clear();
        assert!(remote_host(&FsLocation::Transient(invalid), &[]).is_err());

        let mut dsw = hosts[0].clone();
        dsw.name = "root@dsw-notebook-dsw-l8rnh0wm7vs81o7z6j-22.vpc-0jlbz3pri2042fd5xw2ov.instance-forward.dsw.cn-wulanchabu.aliyuncs.com".to_string();
        let compact = FsLocation::Transient(dsw).label(&[]);
        assert!(compact.starts_with("ssh: root@dsw-notebook"));
        assert!(compact.contains('…'));
        assert!(compact.ends_with("aliyuncs.com (temporary)"));
    }

    #[test]
    fn reusable_control_path_does_not_change_files_identity() {
        let base = ssh_host();
        let overlaid = with_reusable_control_path(&base, Some("/run/user/1000/jsh/cm-%C"));
        assert_eq!(
            overlaid.ssh_args,
            ["-p", "22", "-S", "/run/user/1000/jsh/cm-%C"]
        );
        assert!(same_files_transport(&base, &overlaid));
        assert_eq!(
            files_authority_key(&FsLocation::Remote(0), std::slice::from_ref(&base)),
            files_authority_key(&FsLocation::Transient(overlaid.clone()), &[]),
            "ControlPath overlays share scheduler and cache isolation"
        );
        let mut other = base.clone();
        other.host = "other.example".to_string();
        assert_ne!(
            files_authority_key(&FsLocation::Remote(0), std::slice::from_ref(&base)),
            files_authority_key(&FsLocation::Transient(other), &[])
        );
        assert_ne!(
            files_authority_key(&FsLocation::Local, &[]),
            files_authority_key(&FsLocation::Remote(0), std::slice::from_ref(&base))
        );
        assert!(!same_files_execution(&base, &overlaid));
        assert!(same_files_execution(&overlaid, &overlaid));
        assert!(same_files_location(
            &FsLocation::Remote(0),
            &FsLocation::Transient(overlaid.clone()),
            std::slice::from_ref(&base),
        ));

        let mut explicit = base.clone();
        explicit
            .ssh_args
            .extend(["-o".to_string(), "ControlPath=/tmp/existing-%C".to_string()]);
        let replaced = with_reusable_control_path(&explicit, Some("/tmp/replacement-%C"));
        assert!(replaced
            .ssh_args
            .windows(2)
            .any(|pair| pair == ["-S", "/tmp/replacement-%C"]));
        assert!(!replaced
            .ssh_args
            .iter()
            .any(|argument| argument.contains("existing-%C")));
        assert!(same_files_transport(&base, &explicit));

        let saved = FsLocation::Remote(0);
        let live = FsLocation::Transient(overlaid);
        assert_eq!(
            preferred_same_files_execution_location(&saved, &live, std::slice::from_ref(&base)),
            Some(&live)
        );
        assert_eq!(
            preferred_same_files_execution_location(&live, &saved, std::slice::from_ref(&base)),
            Some(&live),
            "the live socket wins even when it is the clipboard source"
        );

        let saved_with_old_socket = with_reusable_control_path(&base, Some("/tmp/saved-old-%C"));
        assert_eq!(
            preferred_same_files_execution_location(
                &live,
                &saved,
                std::slice::from_ref(&saved_with_old_socket),
            ),
            Some(&live),
            "an observed temporary socket is fresher than a saved socket"
        );
    }
}
