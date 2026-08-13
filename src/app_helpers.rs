//! Process boundary for helpers started automatically by frost.
//!
//! These integrations must not inherit command resolution from the shell: a
//! project-local executable or a user-writable PATH entry must never decide
//! what the terminal starts in the background.  Every helper is resolved from
//! a fixed absolute system location, and every invocation is output- and
//! time-bounded while frost owns its whole process group.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const FONT_HELPER_TIMEOUT: Duration = Duration::from_secs(5);
const NOTIFICATION_HELPER_TIMEOUT: Duration = Duration::from_secs(3);
const FONT_LIST_STDOUT_LIMIT: usize = 4 * 1024 * 1024;
const FONT_MATCH_STDOUT_LIMIT: usize = 64 * 1024;
const HELPER_STDERR_LIMIT: usize = 64 * 1024;
const NOTIFICATION_OUTPUT_LIMIT: usize = 16 * 1024;
const TRUSTED_CHILD_PATH: &str = "/usr/bin:/bin";

#[derive(Clone, Copy, Debug)]
enum Helper {
    FcList,
    FcMatch,
    NotifySend,
}

impl Helper {
    fn name(self) -> &'static str {
        match self {
            Self::FcList => "fc-list",
            Self::FcMatch => "fc-match",
            Self::NotifySend => "notify-send",
        }
    }

    fn candidates(self) -> &'static [&'static str] {
        match self {
            Self::FcList => &["/usr/bin/fc-list", "/bin/fc-list", "/usr/local/bin/fc-list"],
            Self::FcMatch => &[
                "/usr/bin/fc-match",
                "/bin/fc-match",
                "/usr/local/bin/fc-match",
            ],
            Self::NotifySend => &[
                "/usr/bin/notify-send",
                "/bin/notify-send",
                "/usr/local/bin/notify-send",
            ],
        }
    }
}

pub(crate) fn fc_list(args: &[&str]) -> io::Result<Output> {
    run_helper(
        Helper::FcList,
        args.iter().copied(),
        FONT_LIST_STDOUT_LIMIT,
        HELPER_STDERR_LIMIT,
        FONT_HELPER_TIMEOUT,
    )
}

pub(crate) fn fc_match(args: &[&str]) -> io::Result<Output> {
    run_helper(
        Helper::FcMatch,
        args.iter().copied(),
        FONT_MATCH_STDOUT_LIMIT,
        HELPER_STDERR_LIMIT,
        FONT_HELPER_TIMEOUT,
    )
}

pub(crate) fn notify_send(title: &str, body: &str) -> io::Result<Output> {
    // `--` keeps notification text beginning with `-` out of option parsing.
    run_helper(
        Helper::NotifySend,
        ["--", title, body],
        NOTIFICATION_OUTPUT_LIMIT,
        NOTIFICATION_OUTPUT_LIMIT,
        NOTIFICATION_HELPER_TIMEOUT,
    )
}

fn run_helper<I, S>(
    helper: Helper,
    args: I,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = trusted_helper_program(helper).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no trusted {} executable is available", helper.name()),
        )
    })?;
    let mut command = Command::new(program);
    command.args(args).env("PATH", TRUSTED_CHILD_PATH);
    let output = bounded_command_output(&mut command, stdout_limit, stderr_limit, timeout)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(io::Error::other(format!(
            "{} exited unsuccessfully ({})",
            helper.name(),
            output.status
        )))
    }
}

/// Resolve to the canonical target of one fixed absolute system candidate.
///
/// Canonicalising before exec closes the symlink-swap window at the original
/// pathname.  The target and every directory above it must be owned by root
/// (or by this process's user) and not writable by group or other.  A
/// non-root user's own owner-writable component is also refused for an
/// automatic helper; such a component is mutable application state, not a
/// system executable.
fn trusted_helper_program(helper: Helper) -> Option<PathBuf> {
    helper
        .candidates()
        .iter()
        .find_map(|candidate| trusted_system_executable(Path::new(candidate)))
}

#[cfg(unix)]
fn trusted_system_executable(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if !candidate.is_absolute() {
        return None;
    }
    let canonical = std::fs::canonicalize(candidate).ok()?;
    let euid = unsafe { libc::geteuid() };
    for (index, component) in canonical.ancestors().enumerate() {
        let metadata = std::fs::metadata(component).ok()?;
        let mode = metadata.permissions().mode();
        if index == 0 {
            if !metadata.is_file() || mode & 0o111 == 0 {
                return None;
            }
        } else if !metadata.is_dir() {
            return None;
        }
        if !trusted_component(mode, metadata.uid(), euid) {
            return None;
        }
    }
    Some(canonical)
}

#[cfg(unix)]
fn trusted_component(mode: u32, owner: u32, euid: u32) -> bool {
    if mode & 0o022 != 0 || (owner != 0 && owner != euid) {
        return false;
    }
    // Root can write every root-owned system file regardless of its mode, so
    // applying the ordinary self-writable rule to euid 0 would disable every
    // helper in containers.  A non-root user's writable file is not an
    // automatic system helper, even when it occupies a fixed candidate path.
    euid == 0 || owner != euid || mode & 0o200 == 0
}

#[cfg(not(unix))]
fn trusted_system_executable(_candidate: &Path) -> Option<PathBuf> {
    None
}

/// Capture both child streams under independent byte limits and one deadline.
///
/// The child leads a new process group.  Every return path terminates that
/// group and waits for the direct child, including successful completion; a
/// descendant cannot survive merely by closing the inherited pipes early.
#[cfg(unix)]
fn bounded_command_output(
    command: &mut Command,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> io::Result<Output> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;

    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = terminate_group_and_reap(&mut child);
            return Err(io::Error::other("helper stdout pipe was not created"));
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = terminate_group_and_reap(&mut child);
            return Err(io::Error::other("helper stderr pipe was not created"));
        }
    };

    use std::os::fd::AsRawFd;
    if let Err(error) =
        set_nonblocking(stdout.as_raw_fd()).and_then(|()| set_nonblocking(stderr.as_raw_fd()))
    {
        let _ = terminate_group_and_reap(&mut child);
        return Err(error);
    }

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        if Instant::now() >= deadline {
            let _ = terminate_group_and_reap(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "helper process exceeded its time limit",
            ));
        }

        let drained = drain_pipe(
            &mut stdout,
            &mut stdout_bytes,
            stdout_limit,
            &mut stdout_closed,
        )
        .and_then(|()| {
            drain_pipe(
                &mut stderr,
                &mut stderr_bytes,
                stderr_limit,
                &mut stderr_closed,
            )
        });
        if let Err(error) = drained {
            let _ = terminate_group_and_reap(&mut child);
            return Err(error);
        }

        let exited = match child_exited_unreaped(&child) {
            Ok(exited) => exited,
            Err(error) => {
                let _ = terminate_group_and_reap(&mut child);
                return Err(error);
            }
        };
        if exited && stdout_closed && stderr_closed {
            let status = terminate_group_and_reap(&mut child)?;
            return Ok(Output {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            });
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = remaining.as_millis().min(100).try_into().unwrap_or(100);
        let mut descriptors = Vec::with_capacity(2);
        if !stdout_closed {
            descriptors.push(libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            });
        }
        if !stderr_closed {
            descriptors.push(libc::pollfd {
                fd: stderr.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            });
        }
        let polled = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len().try_into().unwrap_or(0),
                timeout_ms,
            )
        };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                let _ = terminate_group_and_reap(&mut child);
                return Err(error);
            }
        }
    }
}

#[cfg(not(unix))]
fn bounded_command_output(
    _command: &mut Command,
    _stdout_limit: usize,
    _stderr_limit: usize,
    _timeout: Duration,
) -> io::Result<Output> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded app helpers are only supported on Unix",
    ))
}

#[cfg(unix)]
fn child_exited_unreaped(child: &std::process::Child) -> io::Result<bool> {
    let child_id = libc::id_t::try_from(child.id())
        .map_err(|_| io::Error::other("helper process id is out of range"))?;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child_id,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // WNOWAIT leaves an observed child waitable.  Keeping the leader
            // unreaped also reserves its PID/PGID until killpg below, so that
            // signal can never target an unrelated, newly reused group id.
            let observed_pid = unsafe { info.assume_init().si_pid() };
            if observed_pid == 0 {
                return Ok(false);
            }
            return Ok(u32::try_from(observed_pid).ok() == Some(child.id()));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn terminate_group_and_reap(
    child: &mut std::process::Child,
) -> io::Result<std::process::ExitStatus> {
    if let Ok(process_group) = i32::try_from(child.id()) {
        // The child is the group leader configured by `process_group(0)`.
        let _ = unsafe { libc::killpg(process_group, libc::SIGKILL) };
    }
    // The direct signal is a fallback if the group signal itself fails. It is
    // still safe here because no caller reaps the leader before this function.
    let _ = child.kill();
    child.wait()
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn drain_pipe(
    reader: &mut impl io::Read,
    output: &mut Vec<u8>,
    limit: usize,
    closed: &mut bool,
) -> io::Result<()> {
    if *closed {
        return Ok(());
    }
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *closed = true;
                return Ok(());
            }
            Ok(read) => {
                if output.len().saturating_add(read) > limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("helper output exceeds the {limit} byte limit"),
                    ));
                }
                output.extend_from_slice(&buffer[..read]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "frost-helper-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&path).expect("create helper scratch directory");
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn automatic_helpers_are_canonical_absolute_system_programs() {
        assert!(trusted_system_executable(Path::new("fc-list")).is_none());

        let scratch = ScratchDir::new("untrusted-path");
        let fake = scratch.0.join("fc-list");
        std::fs::write(&fake, "#!/bin/sh\n").expect("write fake helper");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("make fake helper executable");
        assert!(
            trusted_system_executable(&fake).is_none(),
            "a helper below the world-writable temporary namespace is not trusted"
        );

        for helper in [Helper::FcList, Helper::FcMatch, Helper::NotifySend] {
            if let Some(program) = trusted_helper_program(helper) {
                assert!(program.is_absolute(), "{program:?}");
                assert_eq!(std::fs::canonicalize(&program).unwrap(), program);
            }
        }
    }

    #[test]
    fn trust_rejects_mutable_or_foreign_components_without_disabling_root() {
        const ROOT: u32 = 0;
        const USER: u32 = 1000;
        const OTHER: u32 = 2000;

        assert!(trusted_component(0o755, ROOT, USER));
        assert!(trusted_component(0o755, ROOT, ROOT));
        assert!(!trusted_component(0o775, ROOT, USER));
        assert!(!trusted_component(0o757, ROOT, USER));
        assert!(!trusted_component(0o755, USER, USER));
        assert!(trusted_component(0o555, USER, USER));
        assert!(!trusted_component(0o555, OTHER, USER));
    }

    #[test]
    fn stdout_and_stderr_are_drained_concurrently_under_independent_caps() {
        let script = "i=0; while [ \"$i\" -lt 4096 ]; do \
                      printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n'; \
                      printf 'yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\\n' >&2; \
                      i=$((i + 1)); done";
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);

        let output =
            bounded_command_output(&mut command, 256 * 1024, 256 * 1024, Duration::from_secs(5))
                .expect("both streams should drain without a pipe deadlock");

        assert!(output.status.success());
        assert!(output.stdout.len() > 128 * 1024);
        assert!(output.stderr.len() > 128 * 1024);
    }

    #[test]
    fn exceeding_either_stream_limit_fails_closed() {
        let mut stdout = Command::new("/bin/sh");
        stdout.args(["-c", "printf too-large"]);
        let error = bounded_command_output(&mut stdout, 4, 64, Duration::from_secs(2))
            .expect_err("stdout cap must be enforced");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut stderr = Command::new("/bin/sh");
        stderr.args(["-c", "printf too-large >&2"]);
        let error = bounded_command_output(&mut stderr, 64, 4, Duration::from_secs(2))
            .expect_err("stderr cap must be enforced");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn exit_observation_keeps_the_group_leader_waitable_until_cleanup() {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;

        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn observed child");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child_exited_unreaped(&child).expect("observe child without reaping it") {
            assert!(Instant::now() < deadline, "child did not exit in time");
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(
            child_exited_unreaped(&child).expect("the exited child remains observable"),
            "WNOWAIT must reserve the leader PID until group cleanup"
        );
        let status = terminate_group_and_reap(&mut child).expect("reap observed child");
        assert_eq!(status.code(), Some(23));
    }

    #[test]
    fn deadline_kills_descendants_and_reaps_the_direct_child() {
        let scratch = ScratchDir::new("deadline");
        let pid_file = scratch.0.join("leader-pid");
        let survivor_file = scratch.0.join("survived");
        let script = "printf '%s' \"$$\" > \"$1\"; \
                      (/bin/sleep 0.3; printf survived > \"$2\") & exit 0";
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .arg("frost-helper-test")
            .arg(&pid_file)
            .arg(&survivor_file);

        let started = Instant::now();
        let error = bounded_command_output(&mut command, 64, 64, Duration::from_millis(50))
            .expect_err("a descendant holding both pipes must meet the same deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));

        let leader: libc::pid_t = std::fs::read_to_string(&pid_file)
            .expect("leader wrote its pid before exiting")
            .parse()
            .expect("numeric leader pid");
        let mut status = 0;
        let waited = unsafe { libc::waitpid(leader, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1, "the direct helper child was not reaped");
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );

        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !survivor_file.exists(),
            "a process in the helper group survived timeout cleanup"
        );
    }
}
