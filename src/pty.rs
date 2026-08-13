use anyhow::{anyhow, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

const TERM_PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SHELL_NAME: &str = "jsh";

#[cfg(unix)]
mod unix_pty {
    use super::*;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    // Keep local launches effectively immediate while allowing automounts and
    // network-backed working directories a reasonable window to resolve.
    // Most importantly, this bounds the synchronous fork-to-exec handshake.
    const CHILD_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

    // Identifying the `jsh` on PATH runs once per process and blocks the first
    // PTY spawn, so keep the window short: our own shell answers `--version`
    // immediately, and anything slower is not worth waiting on.
    const JSH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

    #[derive(Clone, Copy)]
    #[repr(i32)]
    enum ChildSetupStage {
        ParentDeathSignal = 1,
        ParentAlreadyExited = 2,
        CreateSession = 3,
        ChangeDirectory = 4,
        SetControllingTerminal = 5,
        DuplicateStdin = 6,
        DuplicateStdout = 7,
        DuplicateStderr = 8,
        ExecuteShell = 9,
    }

    impl ChildSetupStage {
        fn name(value: i32) -> &'static str {
            match value {
                1 => "prctl(PR_SET_PDEATHSIG)",
                2 => "parent-liveness check",
                3 => "setsid",
                4 => "chdir",
                5 => "ioctl(TIOCSCTTY)",
                6 => "dup2(stdin)",
                7 => "dup2(stdout)",
                8 => "dup2(stderr)",
                9 => "execve",
                _ => "unknown child setup stage",
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ChildLifecycle {
        Running,
        TerminationStarted,
        Reaped,
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn current_errno() -> libc::c_int {
        // SAFETY: called immediately after a failed libc syscall in the forked
        // child. Reading thread-local errno does not allocate or acquire locks.
        unsafe { *libc::__errno_location() }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe fn current_errno() -> libc::c_int {
        // `last_os_error` only snapshots errno. Keep this fallback for Unix
        // targets whose libc does not expose Linux's `__errno_location`.
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO)
    }

    /// Report a fork-to-exec setup failure to the parent and terminate without
    /// running destructors. The fixed two-word record is well below PIPE_BUF,
    /// and this child-side path uses only async-signal-safe libc operations.
    unsafe fn child_setup_failed_with_errno(
        error_fd: RawFd,
        stage: ChildSetupStage,
        errno: libc::c_int,
    ) -> ! {
        let record = [stage as libc::c_int, errno];
        let mut offset = 0usize;
        let len = std::mem::size_of_val(&record);
        let ptr = record.as_ptr().cast::<u8>();

        while offset < len {
            // SAFETY: `record` remains alive until `_exit`, and `offset < len`.
            let written = unsafe {
                libc::write(
                    error_fd,
                    ptr.add(offset).cast::<libc::c_void>(),
                    len - offset,
                )
            };
            if written > 0 {
                offset += written as usize;
            } else if written < 0 && unsafe { current_errno() } == libc::EINTR {
                continue;
            } else {
                break;
            }
        }
        unsafe { libc::_exit(127) }
    }

    unsafe fn child_setup_failed(error_fd: RawFd, stage: ChildSetupStage) -> ! {
        let errno = unsafe { current_errno() };
        unsafe { child_setup_failed_with_errno(error_fd, stage, errno) }
    }

    unsafe fn reap_child_blocking(child_pid: libc::pid_t) {
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(child_pid, &mut status, 0) };
            if result >= 0 || unsafe { current_errno() } != libc::EINTR {
                break;
            }
        }
    }

    unsafe fn kill_and_reap_child(child_pid: libc::pid_t) {
        // The child may or may not have completed setsid(), so target both its
        // prospective process group and the process itself.
        unsafe {
            let _ = libc::kill(-child_pid, libc::SIGKILL);
            let _ = libc::kill(child_pid, libc::SIGKILL);
            reap_child_blocking(child_pid);
        }
    }

    fn startup_timeout_ms(remaining: Duration) -> libc::c_int {
        let whole_ms = remaining.as_millis();
        let rounded_ms = whole_ms + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
        rounded_ms.clamp(1, libc::c_int::MAX as u128) as libc::c_int
    }

    unsafe fn abort_child_startup(
        startup_read: RawFd,
        child_pid: libc::pid_t,
        error: anyhow::Error,
    ) -> Result<()> {
        // Close first so a child still attempting to report an error cannot
        // keep the handshake alive while it is being torn down.
        unsafe {
            let _ = libc::close(startup_read);
            kill_and_reap_child(child_pid);
        }
        Err(error)
    }

    /// Wait for either a child setup error record or CLOEXEC EOF from execve.
    ///
    /// `startup_read` is owned by this function. Every return path closes it;
    /// error paths also kill and reap the child before returning.
    unsafe fn wait_for_child_startup(
        startup_read: RawFd,
        child_pid: libc::pid_t,
        timeout: Duration,
    ) -> Result<()> {
        let mut record = [0 as libc::c_int; 2];
        let record_len = std::mem::size_of_val(&record);
        let mut received = 0usize;
        let started = Instant::now();

        loop {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return unsafe {
                    abort_child_startup(
                        startup_read,
                        child_pid,
                        anyhow!(
                            "Timed out after {} ms during PTY child fork-to-exec startup \
                             (child setup through execve; received {received}/{record_len} \
                             status bytes)",
                            timeout.as_millis()
                        ),
                    )
                };
            }

            let mut poll_fd = libc::pollfd {
                fd: startup_read,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe {
                libc::poll(
                    &mut poll_fd,
                    1,
                    startup_timeout_ms(timeout.saturating_sub(elapsed)),
                )
            };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return unsafe {
                    abort_child_startup(
                        startup_read,
                        child_pid,
                        anyhow!("Failed to poll PTY child during fork-to-exec startup: {error}"),
                    )
                };
            }
            if ready == 0 {
                return unsafe {
                    abort_child_startup(
                        startup_read,
                        child_pid,
                        anyhow!(
                            "Timed out after {} ms during PTY child fork-to-exec startup \
                             (child setup through execve; received {received}/{record_len} \
                             status bytes)",
                            timeout.as_millis()
                        ),
                    )
                };
            }

            let revents = poll_fd.revents;
            if revents & libc::POLLNVAL != 0 {
                return unsafe {
                    abort_child_startup(
                        startup_read,
                        child_pid,
                        anyhow!("Invalid startup status fd during PTY child fork-to-exec startup"),
                    )
                };
            }
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                return unsafe {
                    abort_child_startup(
                        startup_read,
                        child_pid,
                        anyhow!(
                            "Unexpected poll events 0x{revents:x} during PTY child \
                             fork-to-exec startup"
                        ),
                    )
                };
            }

            // POLLHUP may arrive together with POLLIN. Always read first so a
            // complete or partial failure record already in the pipe is not
            // mistaken for successful exec EOF.
            let n = unsafe {
                libc::read(
                    startup_read,
                    record
                        .as_mut_ptr()
                        .cast::<u8>()
                        .add(received)
                        .cast::<libc::c_void>(),
                    record_len - received,
                )
            };
            if n > 0 {
                received += n as usize;
                if received < record_len {
                    continue;
                }

                unsafe {
                    let _ = libc::close(startup_read);
                    reap_child_blocking(child_pid);
                }
                let stage = ChildSetupStage::name(record[0]);
                let errno = record[1];
                let error = std::io::Error::from_raw_os_error(errno);
                return Err(anyhow!(
                    "Failed to start PTY child during {stage}: {error} (errno {errno})"
                ));
            }
            if n == 0 {
                if received != 0 {
                    return unsafe {
                        abort_child_startup(
                            startup_read,
                            child_pid,
                            anyhow!(
                                "Incomplete PTY child startup status during fork-to-exec \
                                 startup ({received}/{record_len} bytes)"
                            ),
                        )
                    };
                }
                if revents & libc::POLLERR != 0 {
                    return unsafe {
                        abort_child_startup(
                            startup_read,
                            child_pid,
                            anyhow!(
                                "Startup status pipe failed during PTY child fork-to-exec startup"
                            ),
                        )
                    };
                }

                // EOF without an error record is the success signal: execve
                // closed the child's CLOEXEC write end.
                unsafe {
                    let _ = libc::close(startup_read);
                }
                return Ok(());
            }

            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) if revents & libc::POLLERR == 0 => continue,
                _ => {
                    return unsafe {
                        abort_child_startup(
                            startup_read,
                            child_pid,
                            anyhow!(
                                "Failed to read PTY child status during fork-to-exec startup: \
                                 {error}"
                            ),
                        )
                    };
                }
            }
        }
    }

    /// Outcome of polling the PTY fd alongside the shutdown pipe.
    pub enum ReaderPoll {
        Data,
        Timeout,
        Shutdown,
    }

    /// This process's own `PATH`, for the `host::` resolvers. `None` means "no
    /// search path" there rather than "use ours", so it is passed explicitly.
    fn search_path() -> Option<std::ffi::OsString> {
        std::env::var_os("PATH")
    }

    /// frost prefers its companion shell `jsh`, but a PATH hit on that name is
    /// not proof: the name can be taken by an unrelated binary or point
    /// elsewhere through a symlink. Exec'ing such a program as the login shell
    /// only prints a usage line and exits, which closes the sole tab and quits
    /// frost the instant it starts. Accept a PATH hit only when it still
    /// resolves to a binary named `jsh` and identifies itself by banner.
    ///
    /// (The shell was called `rsh` until 0.3, when the name really did collide
    /// with the BSD remote shell. The rename removed that collision; the check
    /// stays because only the banner proves what we are about to exec.)
    fn find_jterm_jsh() -> Option<String> {
        static RESOLVED: OnceLock<Option<String>> = OnceLock::new();
        RESOLVED
            .get_or_init(|| {
                let candidate = jterm_core::host::find_executable_in(
                    DEFAULT_SHELL_NAME,
                    search_path().as_deref(),
                )?;
                let candidate = candidate.to_string_lossy().into_owned();
                if is_jterm_jsh(Path::new(&candidate)) {
                    return Some(candidate);
                }
                eprintln!(
                    "[PTY] '{candidate}' is not frost's jsh shell (it does not identify \
                     itself as one), falling back to the login shell"
                );
                None
            })
            .clone()
    }

    pub(crate) fn is_jterm_jsh(path: &Path) -> bool {
        // A symlink pointing at some other program is rejected on the basename
        // alone, without spawning anything.
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if resolved.file_name().and_then(|name| name.to_str()) != Some(DEFAULT_SHELL_NAME) {
            return false;
        }
        jsh_version_banner(path).is_some_and(|banner| banner.starts_with("jsh "))
    }

    /// Run `<path> --version` under a short deadline. An impostor typically
    /// answers a usage error, and anything that instead waits on the network (or
    /// on stdin, which is /dev/null here) is killed and treated as "not our
    /// shell".
    ///
    /// The probe owns the whole process group it starts, not just the direct
    /// child: a shell that forks a daemon inherits this pipe, and reading to
    /// EOF would then wait for *that* descendant rather than for the program we
    /// asked to identify itself. The banner is read concurrently under a byte
    /// ceiling so the child cannot block on a full pipe either, and the group
    /// is signalled and reaped at one deadline.
    fn jsh_version_banner(path: &Path) -> Option<String> {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        /// A version banner is one short line; anything longer is not one.
        const MAX_BANNER_BYTES: u64 = 4 * 1024;

        let mut command = Command::new(path);
        command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            // SAFETY: setpgid is async-signal-safe and only changes the
            // freshly forked child's process group.
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().ok()?;
        let group = child.id() as libc::pid_t;
        let mut stdout = child.stdout.take()?;

        // Drain concurrently: a child that fills the pipe while we wait for it
        // to exit would deadlock against a reader that only runs afterwards.
        let reader = std::thread::spawn(move || {
            let mut banner = Vec::new();
            let _ = stdout
                .by_ref()
                .take(MAX_BANNER_BYTES)
                .read_to_end(&mut banner);
            banner
        });

        let deadline = Instant::now() + JSH_PROBE_TIMEOUT;
        let exited_cleanly = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => break false,
            }
        };
        // Signal the group whether or not the direct child exited: a daemonised
        // descendant still holds the pipe, and the reader below must be able to
        // reach EOF.
        // SAFETY: killpg only signals the group this probe created.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        let banner = reader.join().ok()?;
        if !exited_cleanly {
            return None;
        }
        String::from_utf8(banner).ok()
    }

    fn shell_from_passwd() -> Option<String> {
        let uid = unsafe { libc::getuid() };
        let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buf = vec![0u8; 16384];
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 || result.is_null() {
            return None;
        }
        let pwd = unsafe { pwd.assume_init() };
        if pwd.pw_shell.is_null() {
            return None;
        }
        let shell = unsafe { std::ffi::CStr::from_ptr(pwd.pw_shell) }
            .to_string_lossy()
            .to_string();
        resolve_program_token(&shell)
    }

    /// Accept a program *token* — a `shell = ` setting, `$SHELL`, `pw_shell` —
    /// and turn it into an absolute path, or reject it.
    ///
    /// Two bugs are fixed by never returning the token itself. A bare name is a
    /// `PATH` lookup and never an implicit `./name`: a terminal opens in whatever
    /// directory the user is browsing, so a project checkout holding an
    /// executable called `bash` must not hijack `shell = "bash"`. And every
    /// accepted candidate comes back absolute, because the child chdirs into the
    /// pane's cwd before `execve` — a relative token that was validated here
    /// would name a *different* file by the time the child opens it.
    fn resolve_program_token(token: &str) -> Option<String> {
        jterm_core::host::resolve_configured_program(token, search_path().as_deref())
            .map(|path| path.to_string_lossy().into_owned())
    }

    /// frost's child-environment policy. A function rather than a literal at
    /// the fork site so the flags can be asserted on without forking.
    pub(crate) fn child_env_options() -> jterm_core::child_env::ChildEnv<'static> {
        jterm_core::child_env::ChildEnv {
            // This crate's version, not jterm_core's: a tool that pairs
            // TERM_PROGRAM with TERM_PROGRAM_VERSION must read frost's.
            app_version: TERM_PROGRAM_VERSION,
            vte_version: Some(jterm_core::child_env::EMULATED_VTE_VERSION),
            // LESS=FR, not the default FRX: -X disables the alternate screen,
            // which leaks pager output into scrollback.
            less_default: Some("FR"),
            // Match mainstream terminal defaults for colour-capable file
            // listings (LS_COLORS for GNU ls, CLICOLOR for BSD `ls -G`).
            color_defaults: true,
            // frost draws UTF-8 whatever the locale says, so a deliberate LANG
            // is not ours to rewrite.
            normalize_locale: false,
        }
    }

    /// The absolute path to hand `execve`, or an error the pane can show.
    ///
    /// `child_cwd` is the directory the child enters before exec; the last-resort
    /// lookup has to agree with it because relative `PATH` entries are resolved
    /// there, not here.
    pub(crate) fn choose_shell(
        configured_shell: Option<&str>,
        child_cwd: Option<&str>,
    ) -> Result<String> {
        // Priority 1: explicit config (needed when PATH is stripped by launchers like wofi)
        if let Some(token) = configured_shell {
            if let Some(path) = resolve_program_token(token) {
                return Ok(path);
            }
            eprintln!("[PTY] Configured shell '{token}' is not executable, falling back");
        }

        // Priority 2: jsh is frost's preferred shell. It gets a session id
        // argv below when one is available, while still letting users override
        // it through config.shell.
        if let Some(jsh_path) = find_jterm_jsh() {
            return Ok(jsh_path);
        }

        // Priority 3: the user's login shell, matching VTE terminals such as
        // GNOME Terminal and Terminator. `$SHELL` is usually already absolute;
        // passwd is the fallback when launchers sanitize the environment.
        if let Some(shell) = std::env::var_os("SHELL").and_then(|s| s.into_string().ok()) {
            if let Some(path) = resolve_program_token(&shell) {
                return Ok(path);
            }
        }
        if let Some(shell) = shell_from_passwd() {
            return Ok(shell);
        }

        // Priority 4: bash (fallback)
        if let Some(bash_path) =
            jterm_core::host::find_executable_in("bash", search_path().as_deref())
        {
            return Ok(bash_path.to_string_lossy().into_owned());
        }

        // Priority 5: sh (last resort). This used to return the bare token "sh",
        // which was then handed to execve — and execve does not search PATH, so
        // in exactly the stripped-environment case this fallback exists for the
        // tab never opened at all. `resolve_executable` searches eagerly and
        // falls back to execvp's own "/bin:/usr/bin" when there is no PATH.
        jterm_core::host::resolve_executable("sh", search_path().as_deref(), child_cwd)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| anyhow!("No usable shell found (looked for sh last): {error}"))
    }

    pub struct Pty {
        master: RawFd,
        child_pid: i32,
        exit_code_cached: Option<i32>,
        lifecycle: ChildLifecycle,
    }

    impl Pty {
        #[allow(dead_code)]
        pub fn new(cols: usize, rows: usize) -> Result<Self> {
            Self::new_with_cwd(cols, rows, None, None, None, None)
        }

        /// `command_argv` replaces the interactive shell with an explicit argv,
        /// for one-shot helper sessions such as the jsh installer.
        pub fn new_with_cwd(
            cols: usize,
            rows: usize,
            cwd: Option<&str>,
            session_id: Option<&str>,
            configured_shell: Option<&str>,
            command_argv: Option<&[String]>,
        ) -> Result<Self> {
            // Reject stale session-history paths before allocating a PTY or
            // forking. The child still reports chdir failures through the setup
            // pipe below, closing the validation-to-fork race.
            if let Some(dir) = cwd {
                let metadata = std::fs::metadata(dir)
                    .map_err(|error| anyhow!("Invalid PTY working directory '{}': {error}", dir))?;
                if !metadata.is_dir() {
                    return Err(anyhow!(
                        "PTY working directory is not a directory: '{}'",
                        dir
                    ));
                }
            }

            // Resolved before the PTY exists so a "no usable shell" failure is a
            // plain error return instead of an fd-closing path inside the unsafe
            // block. It is a pure lookup; nothing below it depends on ordering.
            let shell_path = choose_shell(configured_shell, cwd)?;
            // A shell that exits right away takes the whole window down with
            // it, so make the choice visible in the log for that diagnosis.
            log::info!("[PTY] starting shell: {shell_path}");

            // SAFETY: 这个 unsafe 块包含多个 libc 系统调用用于 PTY 创建和进程 fork。
            // 所有的 libc 调用都检查了返回值并正确处理错误。
            // 文件描述符的生命周期被正确管理（成功时存储在 PtySession 中，失败时关闭）。
            // fork 后的子进程分支永不返回（通过 execve 或 exit），避免了未定义行为。
            unsafe {
                // 1. 创建 PTY
                let mut master = 0;
                let mut slave = 0;

                let win_size = libc::winsize {
                    ws_row: pty_dimension(rows),
                    ws_col: pty_dimension(cols),
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                if libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &win_size,
                ) != 0
                {
                    return Err(anyhow!("Failed to open PTY"));
                }

                // 2. 设置 master 非阻塞模式
                let flags = libc::fcntl(master, libc::F_GETFL, 0);
                if flags < 0 || libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                    let error = std::io::Error::last_os_error();
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!("Failed to make PTY non-blocking: {error}"));
                }

                // 设置 FD_CLOEXEC，防止子进程继承
                let fd_flags = libc::fcntl(master, libc::F_GETFD, 0);
                if fd_flags < 0
                    || libc::fcntl(master, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) < 0
                {
                    let error = std::io::Error::last_os_error();
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!("Failed to set PTY CLOEXEC: {error}"));
                }

                // Prepare everything that needs heap allocation or environment
                // lookups BEFORE fork(). iced spawns winit/wgpu worker threads, so
                // this is a multithreaded process; between fork() and execve() only
                // async-signal-safe calls are legal. malloc/getenv/setenv are not —
                // if another thread held the allocator lock at fork time the child
                // could deadlock. So argv and a custom envp are built here and the
                // child branch only reads already-allocated memory.
                let shell_name = std::path::Path::new(&shell_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sh")
                    .to_string();

                macro_rules! cstr_or_bail {
                    ($e:expr, $msg:expr) => {
                        match CString::new($e) {
                            Ok(c) => c,
                            Err(_) => {
                                libc::close(master);
                                libc::close(slave);
                                return Err(anyhow!($msg));
                            }
                        }
                    };
                }

                let shell_cstr = cstr_or_bail!(shell_path.clone(), "shell path contains NUL");
                let dash_shell_cstr =
                    cstr_or_bail!(format!("-{}", shell_name), "shell name contains NUL");
                let login_arg = if shell_name == "bash" {
                    Some(CString::new("-l").unwrap())
                } else {
                    None
                };
                let session_flag = CString::new("--session").unwrap();
                let session_id_cstr = session_id.and_then(|s| CString::new(s).ok());
                // `find_executable_in` is exec-bit checked and absolutizing, so
                // the separate is_executable filter this used to carry is gone.
                let bash_path = if shell_name == "jsh" {
                    jterm_core::host::find_executable_in("bash", search_path().as_deref())
                        .map(|path| path.to_string_lossy().into_owned())
                } else {
                    None
                };

                // A one-shot helper (for example the jsh installer) is exec'd
                // exactly as given: no login-shell wrapping, no --session.
                let command_cstrings: Option<(CString, Vec<CString>)> = match command_argv {
                    Some(argv) => {
                        let program = argv
                            .first()
                            .ok_or_else(|| anyhow!("command argv must not be empty"))?;
                        // execve does not search PATH, so resolve the helper's
                        // program the way execvp would — against the cwd the
                        // child will be in. On failure the token is passed
                        // through unchanged so the child's exec error is
                        // reported through the startup pipe with the name the
                        // caller actually asked for.
                        let program_path = jterm_core::host::resolve_executable(
                            program,
                            search_path().as_deref(),
                            cwd,
                        )
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| program.clone());
                        let mut args = Vec::with_capacity(argv.len());
                        for arg in argv {
                            args.push(cstr_or_bail!(arg.clone(), "command argument contains NUL"));
                        }
                        Some((
                            cstr_or_bail!(program_path, "command path contains NUL"),
                            args,
                        ))
                    }
                    None => None,
                };

                let (exec_cstr, argv_cstrings): (CString, Vec<CString>) =
                    if let Some(command) = command_cstrings {
                        command
                    } else if let ("jsh", Some(bash_path)) = (shell_name.as_str(), bash_path) {
                        let exec_cmd =
                            jterm_core::process::build_jsh_exec_command(&shell_path, session_id);
                        (
                            cstr_or_bail!(bash_path, "bash path contains NUL"),
                            vec![
                                CString::new("bash").unwrap(),
                                CString::new("-ic").unwrap(),
                                cstr_or_bail!(exec_cmd, "bash wrapper command contains NUL"),
                            ],
                        )
                    } else {
                        let mut argv = vec![dash_shell_cstr.clone()];
                        if let Some(ref arg) = login_arg {
                            argv.push(arg.clone());
                        }
                        if shell_name == "jsh" {
                            if let Some(ref sid) = session_id_cstr {
                                argv.push(session_flag.clone());
                                argv.push(sid.clone());
                            }
                        }
                        (shell_cstr.clone(), argv)
                    };

                // argv pointers borrow the CStrings above; both outlive the fork.
                let mut argv_ptrs: Vec<*const libc::c_char> =
                    argv_cstrings.iter().map(|arg| arg.as_ptr()).collect();
                argv_ptrs.push(std::ptr::null());

                // The child's whole environment, built here because between
                // fork() and execve() only async-signal-safe calls are legal:
                // setenv and malloc could deadlock on a lock another iced/wgpu
                // worker thread held at the moment of the fork. `child_env`
                // returns a ready CString block so the child only collects
                // pointers and execve's.
                let env_cstrings = match jterm_core::child_env::envp(&child_env_options(), &[]) {
                    Ok(block) => block,
                    Err(error) => {
                        libc::close(master);
                        libc::close(slave);
                        return Err(anyhow!("Failed to build the child environment: {error}"));
                    }
                };
                let mut envp: Vec<*const libc::c_char> =
                    env_cstrings.iter().map(|c| c.as_ptr()).collect();
                envp.push(std::ptr::null());

                // cwd prepared before fork; chdir() itself is async-signal-safe.
                let cwd_cstr = match cwd {
                    Some(dir) => Some(cstr_or_bail!(dir, "working directory contains NUL")),
                    None => None,
                };

                // A CLOEXEC pipe is the exec handshake. The child writes a
                // fixed error record if any post-fork setup syscall fails; a
                // successful execve atomically closes its write end, yielding
                // EOF to the parent.
                let (startup_read, startup_write) = match Self::make_shutdown_pipe() {
                    Ok(pipe) => pipe,
                    Err(error) => {
                        libc::close(master);
                        libc::close(slave);
                        return Err(anyhow!("Failed to create PTY startup pipe: {error}"));
                    }
                };
                // Poll is the authority for waiting. Keep the subsequent read
                // non-blocking as a second guard against spurious readiness or
                // a partial status record whose writer remains open.
                let startup_flags = libc::fcntl(startup_read, libc::F_GETFL, 0);
                if startup_flags < 0
                    || libc::fcntl(
                        startup_read,
                        libc::F_SETFL,
                        startup_flags | libc::O_NONBLOCK,
                    ) < 0
                {
                    let error = std::io::Error::last_os_error();
                    libc::close(startup_read);
                    libc::close(startup_write);
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!(
                        "Failed to make PTY startup status pipe non-blocking: {error}"
                    ));
                }

                // 3. Fork 子进程. Remember the parent so the child can close the
                // small fork→prctl race where the parent dies before PDEATHSIG is
                // installed.
                let parent_pid = libc::getpid();
                let fork_result = libc::fork();

                if fork_result < 0 {
                    libc::close(startup_read);
                    libc::close(startup_write);
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!("Failed to fork"));
                }

                if fork_result == 0 {
                    // ===== 子进程分支：以下只允许 async-signal-safe 调用 =====
                    libc::close(startup_read);
                    libc::close(master);

                    // 父进程死亡信号：父进程(frost)退出时此进程收到 SIGTERM，
                    // 作为 SIGKILL/panic 情况下避免孤儿进程的最后一道防线。
                    #[cfg(target_os = "linux")]
                    {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                            child_setup_failed(startup_write, ChildSetupStage::ParentDeathSignal);
                        }
                        if libc::getppid() != parent_pid {
                            child_setup_failed_with_errno(
                                startup_write,
                                ChildSetupStage::ParentAlreadyExited,
                                libc::ESRCH,
                            );
                        }
                    }

                    // 新建会话/进程组，使其成为会话 leader，便于按进程组发信号。
                    if libc::setsid() < 0 {
                        child_setup_failed(startup_write, ChildSetupStage::CreateSession);
                    }

                    // 切换工作目录（CString 已在 fork 前构造好）。
                    if let Some(ref dir_cstr) = cwd_cstr {
                        if libc::chdir(dir_cstr.as_ptr()) != 0 {
                            child_setup_failed(startup_write, ChildSetupStage::ChangeDirectory);
                        }
                    }

                    // 设置 slave 为控制终端
                    if libc::ioctl(slave, libc::TIOCSCTTY, 0) != 0 {
                        child_setup_failed(startup_write, ChildSetupStage::SetControllingTerminal);
                    }

                    if libc::dup2(slave, libc::STDIN_FILENO) < 0 {
                        child_setup_failed(startup_write, ChildSetupStage::DuplicateStdin);
                    }
                    if libc::dup2(slave, libc::STDOUT_FILENO) < 0 {
                        child_setup_failed(startup_write, ChildSetupStage::DuplicateStdout);
                    }
                    if libc::dup2(slave, libc::STDERR_FILENO) < 0 {
                        child_setup_failed(startup_write, ChildSetupStage::DuplicateStderr);
                    }
                    if slave > libc::STDERR_FILENO {
                        libc::close(slave);
                    }

                    // 执行 shell，使用 fork 前构造好的 argv 与 envp。
                    libc::execve(exec_cstr.as_ptr(), argv_ptrs.as_ptr(), envp.as_ptr());

                    // execve 仅在出错时返回。
                    child_setup_failed(startup_write, ChildSetupStage::ExecuteShell);
                } else {
                    // 父进程分支
                    libc::close(startup_write);
                    // 关闭 slave
                    libc::close(slave);

                    if let Err(error) =
                        wait_for_child_startup(startup_read, fork_result, CHILD_STARTUP_TIMEOUT)
                    {
                        libc::close(master);
                        return Err(error);
                    }

                    Ok(Pty {
                        master,
                        child_pid: fork_result as i32,
                        exit_code_cached: None,
                        lifecycle: ChildLifecycle::Running,
                    })
                }
            }
        }

        pub fn get_child_pid(&self) -> i32 {
            self.child_pid
        }

        pub fn master_fd(&self) -> RawFd {
            self.master
        }

        /// Create a self-pipe used to wake the reader thread on shutdown.
        /// Returns (read_end, write_end). Closing the write end makes a `poll`
        /// on the read end report POLLHUP immediately.
        pub fn make_shutdown_pipe() -> Result<(RawFd, RawFd)> {
            let mut fds = [0 as RawFd; 2];
            // CLOEXEC is essential here. A later terminal session is created with
            // fork/exec; if it inherits an older subscription's write end, closing
            // that subscription can never deliver POLLHUP to its reader thread.
            // pipe2 makes creation + CLOEXEC atomic on Linux/Android.
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            if rc != 0 {
                return Err(anyhow!(
                    "Failed to create shutdown pipe: {}",
                    std::io::Error::last_os_error()
                ));
            }

            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            for &fd in &fds {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD, 0) };
                if flags < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
                {
                    let error = std::io::Error::last_os_error();
                    unsafe {
                        libc::close(fds[0]);
                        libc::close(fds[1]);
                    }
                    return Err(anyhow!("Failed to set pipe CLOEXEC: {error}"));
                }
            }
            Ok((fds[0], fds[1]))
        }

        /// Poll the PTY fd for readability while also watching `shutdown_fd`.
        /// Shutdown takes priority: if the shutdown pipe's write end was closed
        /// (POLLHUP) — or the PTY fd became invalid because the session was
        /// dropped — this returns `Shutdown` WITHOUT reporting data, so the
        /// caller never reads from a PTY fd whose number may have been reused.
        pub fn wait_fd_or_shutdown(
            fd: RawFd,
            shutdown_fd: RawFd,
            timeout_ms: i32,
        ) -> Result<ReaderPoll> {
            let mut fds = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: shutdown_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: fds is a valid 2-element array on the stack.
            let ready = loop {
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
                if ready >= 0 {
                    break ready;
                }
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(anyhow!("Failed to poll PTY: {error}"));
                }
            };
            if ready == 0 {
                return Ok(ReaderPoll::Timeout);
            }
            // Shutdown signaled (write end closed -> POLLHUP, or any activity).
            if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
            {
                return Ok(ReaderPoll::Shutdown);
            }
            // PTY fd closed out from under us -> treat as a clean shutdown.
            if fds[0].revents & libc::POLLNVAL != 0 {
                return Ok(ReaderPoll::Shutdown);
            }
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(ReaderPoll::Data);
            }
            Ok(ReaderPoll::Timeout)
        }

        /// Single non-blocking write. Returns bytes written, `Ok(0)` if the
        /// buffer is full (WouldBlock), or an error for a real failure.
        pub fn write(&mut self, data: &[u8]) -> Result<usize> {
            // SAFETY: self.master 是有效的文件描述符，data.as_ptr() 指向有效的内存，
            // data.len() 是正确的长度。write 系统调用不会超出缓冲区边界。
            loop {
                let n = unsafe { libc::write(self.master, data.as_ptr() as *const _, data.len()) };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        return Ok(0);
                    } else if err.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    } else {
                        return Err(anyhow!("Failed to write to PTY: {}", err));
                    }
                } else {
                    return Ok(n as usize);
                }
            }
        }

        pub fn resize(&mut self, cols: usize, rows: usize) -> Result<()> {
            // SAFETY: win_size 是有效的栈上变量，符合 libc::winsize 的内存布局。
            // ioctl TIOCSWINSZ 调用是标准的 PTY 窗口大小设置操作。
            unsafe {
                let win_size = libc::winsize {
                    ws_row: pty_dimension(rows),
                    ws_col: pty_dimension(cols),
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                if libc::ioctl(
                    self.master,
                    libc::TIOCSWINSZ,
                    (&win_size) as *const _ as *mut libc::c_void,
                ) < 0
                {
                    return Err(anyhow!("Failed to resize PTY"));
                }
            }
            Ok(())
        }

        pub fn is_alive(&mut self) -> bool {
            // A detached reaper owns TerminationStarted children. Never issue a
            // second waitpid (or later signal) for a pid once teardown begins.
            if self.lifecycle != ChildLifecycle::Running || self.exit_code_cached.is_some() {
                return false;
            }

            // SAFETY: waitpid 使用 WNOHANG 非阻塞检查子进程状态。
            // status 是有效的栈变量，child_pid 是有效的进程 ID。
            unsafe {
                let mut status = 0;
                let result = loop {
                    let result = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
                    if result >= 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                    {
                        break result;
                    }
                };
                if result == 0 {
                    // Still running.
                    true
                } else if result > 0 {
                    // The child changed state and we just reaped it — decode and
                    // cache the real exit status here so it isn't lost.
                    let code = if libc::WIFEXITED(status) {
                        libc::WEXITSTATUS(status) as i32
                    } else if libc::WIFSIGNALED(status) {
                        -(libc::WTERMSIG(status) as i32)
                    } else {
                        -1
                    };
                    self.exit_code_cached = Some(code);
                    self.lifecycle = ChildLifecycle::Reaped;
                    false
                } else {
                    // waitpid error (typically ECHILD: already reaped elsewhere).
                    // Treat as dead so callers stop polling.
                    self.exit_code_cached = Some(0);
                    self.lifecycle = ChildLifecycle::Reaped;
                    false
                }
            }
        }

        pub fn terminate(&mut self) -> Result<()> {
            // Lifecycle checks must precede every signal. In particular, a
            // second call after the detached reaper starts must not target a
            // stale pid/pgid that the OS may eventually reuse.
            if self.lifecycle != ChildLifecycle::Running {
                return Ok(());
            }
            if !self.is_alive() {
                return Ok(());
            }

            // Claim teardown before sending anything. Subsequent terminate()
            // calls (including from Drop or queued exit events) are now no-ops.
            self.lifecycle = ChildLifecycle::TerminationStarted;
            self.exit_code_cached = Some(-libc::SIGTERM);

            // SAFETY: kill 向进程组(负 PID)发送信号；child_pid 通过 setsid 成为
            // 会话/进程组 leader。The WNOHANG check above also ensures the
            // direct child has not already been reaped.
            let pgid = -self.child_pid;
            unsafe {
                let _ = libc::kill(pgid, libc::SIGHUP);
                let _ = libc::kill(self.child_pid, libc::SIGTERM);
            }

            // Otherwise escalate on a detached thread instead of sleeping here —
            // terminate() runs from Drop, often on the UI thread, which must not
            // block for the grace period. The thread only captures the pid (in
            // its own process group), so there's no aliasing with `self`.
            let child_pid = self.child_pid;
            std::thread::spawn(move || unsafe {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let mut status = 0;
                let observed = loop {
                    let result = libc::waitpid(child_pid, &mut status, libc::WNOHANG);
                    if result >= 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                    {
                        break result;
                    }
                };
                if observed == 0 {
                    // Still alive after the grace period: force kill, then reap.
                    let _ = libc::kill(-child_pid, libc::SIGKILL);
                    let _ = libc::kill(child_pid, libc::SIGKILL);
                    loop {
                        if libc::waitpid(child_pid, &mut status, 0) >= 0
                            || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                        {
                            break;
                        }
                    }
                }
            });
            Ok(())
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.terminate();
            // SAFETY: close 关闭文件描述符。master 是有效的 fd，
            // 关闭后不会再使用（因为这是 Drop 实现）。
            unsafe {
                let _ = libc::close(self.master);
            }
        }
    }

    #[inline]
    fn pty_dimension(value: usize) -> u16 {
        value.clamp(1, u16::MAX as usize) as u16
    }

    #[cfg(test)]
    mod lifecycle_tests {
        use super::*;

        unsafe fn make_test_startup_child(
            partial_status: Option<u8>,
            keep_writer_open: bool,
        ) -> (RawFd, libc::pid_t) {
            let (read_fd, write_fd) = Pty::make_shutdown_pipe().expect("create test startup pipe");
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                unsafe {
                    let _ = libc::close(read_fd);
                    let _ = libc::close(write_fd);
                }
                panic!(
                    "fork test startup child: {}",
                    std::io::Error::last_os_error()
                );
            }
            if pid == 0 {
                unsafe {
                    let _ = libc::close(read_fd);
                    // The production child immediately execs, which closes every
                    // unrelated CLOEXEC descriptor. This fixture can deliberately
                    // pause before exit, so emulate that boundary explicitly: if
                    // it retained a process-wide flock opened by another parallel
                    // test, dropping the lock in the parent would not release it
                    // until this child was killed. Keep the status writer at one
                    // known descriptor and close everything above it.
                    const STATUS_FD: RawFd = 3;
                    if write_fd != STATUS_FD {
                        if libc::dup2(write_fd, STATUS_FD) < 0 {
                            libc::_exit(126);
                        }
                        let _ = libc::close(write_fd);
                    }
                    #[cfg(target_os = "linux")]
                    let close_range_unavailable =
                        libc::close_range(STATUS_FD as libc::c_uint + 1, libc::c_uint::MAX, 0) != 0;
                    #[cfg(not(target_os = "linux"))]
                    let close_range_unavailable = true;
                    if close_range_unavailable {
                        // This test module only exercises Unix targets. The
                        // conservative old-kernel fallback avoids allocation
                        // after fork. Test processes keep descriptors in this
                        // low range; production takes the exec/CLOEXEC path.
                        for fd in (STATUS_FD + 1)..1024 {
                            let _ = libc::close(fd);
                        }
                    }
                    if let Some(byte) = partial_status {
                        let _ =
                            libc::write(STATUS_FD, (&byte as *const u8).cast::<libc::c_void>(), 1);
                    }
                    if keep_writer_open {
                        loop {
                            libc::pause();
                        }
                    }
                    let _ = libc::close(STATUS_FD);
                    libc::_exit(0);
                }
            }

            unsafe {
                let _ = libc::close(write_fd);
            }
            (read_fd, pid)
        }

        #[test]
        fn startup_handshake_times_out_after_partial_record_and_reaps_child() {
            let (read_fd, pid) = unsafe { make_test_startup_child(Some(1), true) };
            let started = Instant::now();
            let error = unsafe {
                wait_for_child_startup(read_fd, pid, Duration::from_millis(50))
                    .expect_err("a child holding a partial record must time out")
            };

            assert!(
                error.to_string().contains("fork-to-exec startup"),
                "unexpected error: {error:#}"
            );
            assert!(
                error.to_string().contains("received 1/"),
                "partial byte count missing from error: {error:#}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "startup timeout exceeded its bounded cleanup window"
            );

            let mut status = 0;
            let wait_result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            assert_eq!(wait_result, -1, "startup child was not already reaped");
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }

        #[test]
        fn startup_handshake_rejects_eof_after_partial_record() {
            let (read_fd, pid) = unsafe { make_test_startup_child(Some(1), false) };
            let error = unsafe {
                wait_for_child_startup(read_fd, pid, Duration::from_secs(1))
                    .expect_err("EOF after a partial record must be rejected")
            };
            assert!(
                error
                    .to_string()
                    .contains("Incomplete PTY child startup status"),
                "unexpected error: {error:#}"
            );
        }

        #[test]
        fn terminate_is_idempotent_at_the_lifecycle_boundary() {
            let mut pty = Pty::new_with_cwd(80, 24, Some("/"), None, Some("/bin/sh"), None)
                .expect("start /bin/sh in a PTY");
            assert_eq!(pty.lifecycle, ChildLifecycle::Running);

            pty.terminate().expect("first terminate succeeds");
            assert_eq!(pty.lifecycle, ChildLifecycle::TerminationStarted);

            pty.terminate().expect("repeated terminate is a no-op");
            assert_eq!(pty.lifecycle, ChildLifecycle::TerminationStarted);
        }
    }
}

#[cfg(windows)]
mod windows_pty {
    use super::*;

    pub struct Pty;

    impl Pty {
        pub fn new(_cols: usize, _rows: usize) -> Result<Self> {
            Err(anyhow!("PTY support not yet implemented on Windows"))
        }

        pub fn write(&mut self, _data: &[u8]) -> Result<usize> {
            Err(anyhow!("PTY not available"))
        }

        pub fn resize(&mut self, _cols: usize, _rows: usize) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }

        pub fn is_alive(&self) -> bool {
            false
        }

        pub fn terminate(&mut self) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }
    }
}

#[cfg(unix)]
pub use unix_pty::{Pty, ReaderPoll};

#[cfg(windows)]
pub use windows_pty::Pty;

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::unix_pty::Pty;

    #[cfg(unix)]
    #[test]
    fn ssh_masquerading_as_jsh_is_not_taken_for_the_jterm_shell() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("frost-jsh-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create probe dir");

        // A `jsh` on PATH that is really ssh: right name, wrong binary.
        let ssh_like = dir.join("ssh");
        std::fs::write(&ssh_like, b"#!/bin/sh\necho 'usage: ssh' >&2\nexit 255\n")
            .expect("write ssh stand-in");
        std::fs::set_permissions(&ssh_like, std::fs::Permissions::from_mode(0o700))
            .expect("make ssh stand-in executable");
        let alternatives_link = dir.join("jsh");
        let _ = std::fs::remove_file(&alternatives_link);
        std::os::unix::fs::symlink(&ssh_like, &alternatives_link).expect("link jsh -> ssh");
        assert!(!super::unix_pty::is_jterm_jsh(&alternatives_link));

        // A real jsh answers `--version` with its own banner.
        let real = dir.join("real").join("jsh");
        std::fs::create_dir_all(real.parent().expect("parent")).expect("create jsh dir");
        std::fs::write(&real, b"#!/bin/sh\necho 'jsh 0.1.0'\n").expect("write jsh stand-in");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700))
            .expect("make jsh stand-in executable");
        // Retry briefly: a sibling test that forks while this file is still
        // open for writing makes the exec fail with ETXTBSY until that child
        // execs and drops the inherited descriptor.
        let mut identified = false;
        for _ in 0..50 {
            if super::unix_pty::is_jterm_jsh(&real) {
                identified = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(identified, "a real jsh banner must identify the shell");

        // Named jsh, but silent about `--version`: not ours either.
        let mute = dir.join("mute").join("jsh");
        std::fs::create_dir_all(mute.parent().expect("parent")).expect("create mute dir");
        std::fs::write(&mute, b"#!/bin/sh\nexit 2\n").expect("write mute stand-in");
        std::fs::set_permissions(&mute, std::fs::Permissions::from_mode(0o700))
            .expect("make mute stand-in executable");
        assert!(!super::unix_pty::is_jterm_jsh(&mute));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The environment the child gets, asserted through `child_env::pairs` so the
    /// move to the shared module cannot quietly change what a shell sees. The
    /// two conditional entries are only set when the parent has none, so this
    /// test asserts on the identity block and on the *policy flags* instead of
    /// on the pairs those flags produce in whatever environment CI provides.
    #[cfg(unix)]
    #[test]
    fn the_child_environment_policy_is_unchanged_by_the_move_to_child_env() {
        let options = super::unix_pty::child_env_options();
        assert_eq!(options.less_default, Some("FR"));
        assert!(options.color_defaults);
        assert!(
            !options.normalize_locale,
            "frost draws UTF-8 regardless; a deliberate LANG is not ours to rewrite"
        );
        assert_eq!(options.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            options.vte_version,
            Some(jterm_core::child_env::EMULATED_VTE_VERSION),
            "distro vte.sh gates its OSC 7 cwd emitter on this variable"
        );

        let pairs = jterm_core::child_env::pairs(&options, &[]);
        let value = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
        };
        assert_eq!(value("TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(value("COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(value("VTE_VERSION").as_deref(), Some("7802"));
        assert_eq!(
            value("TERM_PROGRAM_VERSION").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        // LESS is only defaulted when the parent has none; assert whichever case
        // this environment is in, so the test is not flaky under `LESS=...`.
        match std::env::var_os("LESS") {
            Some(_) => assert_eq!(value("LESS"), None, "a user's LESS must not be overridden"),
            None => assert_eq!(value("LESS").as_deref(), Some("FR")),
        }
        if std::env::var_os("LS_COLORS").is_none() {
            assert_eq!(
                value("LS_COLORS").as_deref(),
                Some(jterm_core::child_env::DEFAULT_LS_COLORS)
            );
        }
    }

    /// `choose_shell`'s result goes straight to `execve`, which does not search
    /// PATH and runs after the child has chdir'd into the pane's cwd. Every
    /// accepted candidate must therefore come back as an absolute path, and a
    /// bare configured name must be a PATH lookup rather than an implicit
    /// `./name` picked up from whatever directory the user was browsing.
    #[cfg(unix)]
    #[test]
    fn the_chosen_shell_is_always_an_absolute_path() {
        let resolved = super::unix_pty::choose_shell(Some("sh"), Some("/"))
            .expect("a bare 'sh' must resolve through PATH");
        assert!(
            std::path::Path::new(&resolved).is_absolute(),
            "bare name returned verbatim: {resolved}"
        );
        assert!(resolved.ends_with("/sh"), "{resolved}");

        // A configured shell that does not exist falls through to the rest of the
        // chain instead of being handed to execve as-is.
        let fallback = super::unix_pty::choose_shell(Some("/nonexistent/frost-shell"), Some("/"))
            .expect("some shell must be found");
        assert_ne!(fallback, "/nonexistent/frost-shell");
        assert!(std::path::Path::new(&fallback).is_absolute(), "{fallback}");
    }

    /// The last resort used to return the bare token "sh". execve does not
    /// search PATH, so in exactly the stripped-environment case that fallback
    /// exists for, the tab never opened at all.
    #[cfg(unix)]
    #[test]
    fn the_last_resort_shell_resolves_with_no_path_in_the_environment() {
        let sh = jterm_core::host::resolve_executable("sh", None, None)
            .expect("execvp's own /bin:/usr/bin fallback must still find sh");
        assert!(sh.is_absolute(), "{sh:?}");
        assert!(jterm_core::host::is_executable_file(&sh));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_cwd_is_rejected_before_spawning() {
        let missing = std::env::temp_dir().join(format!(
            "frost-missing-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after Unix epoch")
                .as_nanos()
        ));
        assert!(!missing.exists());

        let error = Pty::new_with_cwd(80, 24, missing.to_str(), None, Some("/bin/sh"), None)
            .err()
            .expect("a missing cwd must be rejected");
        assert!(
            error.to_string().contains("Invalid PTY working directory"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exec_failure_is_reported_before_returning() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "frost-invalid-exec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after Unix epoch")
                .as_nanos()
        ));
        std::fs::write(&script, b"#!/definitely/missing/frost-interpreter\n")
            .expect("write invalid executable");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("make test script executable");

        let error = Pty::new_with_cwd(80, 24, Some("/"), None, script.to_str(), None)
            .err()
            .expect("execve failure must be returned to the parent");
        let _ = std::fs::remove_file(&script);
        assert!(
            error.to_string().contains("execve"),
            "unexpected error: {error:#}"
        );
    }

    /// The one-shot helper path (jsh installer) must exec the given argv
    /// verbatim: no login-shell wrapping, no --session injection.
    #[cfg(unix)]
    #[test]
    fn an_explicit_argv_is_exec_ed_verbatim() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'argv0=%s arg=%s' \"$0\" \"$1\"".to_string(),
            "jterm-command-label".to_string(),
            "payload".to_string(),
        ];
        let pty = Pty::new_with_cwd(80, 24, Some("/"), None, None, Some(&argv))
            .expect("explicit argv must spawn");

        let fd = pty.master_fd();
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut buf = [0u8; 1024];
        while std::time::Instant::now() < deadline && !seen.contains("arg=payload") {
            // SAFETY: `fd` is the live PTY master owned by `pty`, and `buf` is a
            // valid mutable slice of the length passed to read(2).
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                seen.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        assert!(
            seen.contains("argv0=jterm-command-label"),
            "argv[0] must reach the child unchanged: {seen:?}"
        );
        assert!(
            seen.contains("arg=payload"),
            "remaining arguments must reach the child: {seen:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn normal_shell_startup_and_exit_succeeds() {
        let mut pty = Pty::new_with_cwd(80, 24, Some("/"), None, Some("/bin/sh"), None)
            .expect("start /bin/sh in a PTY");

        let command = b"exit 0\n";
        let mut written = 0usize;
        for _ in 0..100 {
            written += pty
                .write(&command[written..])
                .expect("write exit command to shell");
            if written == command.len() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(written, command.len(), "exit command was not fully written");

        for _ in 0..200 {
            if !pty.is_alive() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("shell did not exit after receiving `exit 0`");
    }
}
