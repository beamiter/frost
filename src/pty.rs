use anyhow::{anyhow, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

const TERM_PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SHELL_NAME: &str = "jsh";

#[cfg(unix)]
mod unix_pty {
    use super::*;
    use jterm_core::native_pty_launch::{
        launch_pty, PtyLaunch, PtyLaunchError, StartupError, StartupStage, WorkingDirectory,
    };
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    // Identifying the `jsh` on PATH runs once per process and blocks the first
    // PTY spawn, so keep the window short: our own shell answers `--version`
    // immediately, and anything slower is not worth waiting on.
    const JSH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ChildLifecycle {
        Running,
        TerminationStarted,
        Reaped,
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
    /// asked to identify itself. Drain a nonblocking pipe under a byte ceiling
    /// while observing the leader without reaping it. Group cleanup precedes
    /// the final wait, and a descendant that escaped the group cannot extend
    /// the deadline merely by retaining stdout.
    fn jsh_version_banner(path: &Path) -> Option<String> {
        use std::io::Read;
        use std::os::fd::AsRawFd;
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

        // A descendant may leave our process group but retain the pipe. A
        // blocking reader/join would then outlive the nominal probe deadline.
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            // The direct child has not been waited on, so the group is still
            // ours even if the leader already exited.
            unsafe { libc::killpg(group, libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let mut banner = Vec::new();
        let mut drain_available = || -> std::io::Result<()> {
            let remaining = MAX_BANNER_BYTES.saturating_sub(banner.len() as u64);
            match stdout.by_ref().take(remaining).read_to_end(&mut banner) {
                Ok(_) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
                Err(error) => Err(error),
            }
        };
        let deadline = Instant::now() + JSH_PROBE_TIMEOUT;
        let exited_cleanly = loop {
            if drain_available().is_err() {
                break false;
            }
            match Pty::observe_child_exit(group) {
                Ok(Some(code)) => break code == 0,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) => break false,
                // An external reaper may have released the PID. Do not send
                // any further signals through its now-unowned numeric group.
                Err(_) => return None,
            }
        };
        // WNOWAIT retained the leader's PID/PGID through this last signal,
        // including when it exited before its same-group descendants.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
        // The leader may have written between the last drain and observation.
        // This read never waits for EOF from escaped descendants.
        let output_ok = drain_available().is_ok();
        if !exited_cleanly || !output_ok {
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
        master: OwnedFd,
        child_pid: i32,
        session: companion_host::pty_session::PtySession,
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
            Self::new_with_cwd_env(
                cols,
                rows,
                cwd,
                session_id,
                configured_shell,
                command_argv,
                &[],
            )
        }

        /// [`Self::new_with_cwd`] plus explicit environment overrides, used by
        /// the task-validation terminal to neutralize shell startup files
        /// (`BASH_ENV`/`ENV`/`ZDOTDIR`) even beyond the no-rc argv flags.
        pub fn new_with_cwd_env(
            cols: usize,
            rows: usize,
            cwd: Option<&str>,
            session_id: Option<&str>,
            configured_shell: Option<&str>,
            command_argv: Option<&[String]>,
            extra_env: &[(&str, &str)],
        ) -> Result<Self> {
            // A CLI selected as the configured terminal program is still a
            // direct agent launch, rather than a login shell. Route its exact
            // argv through the same inline defaults as task/resume launches.
            let configured_agent = command_argv
                .is_none()
                .then(|| {
                    configured_shell.and_then(|program| {
                        let argv = vec![program.to_owned()];
                        crate::immersive_command::reading_agent_argv(&argv).then_some(argv)
                    })
                })
                .flatten();
            let inline_command = command_argv
                .or(configured_agent.as_deref())
                .map(crate::agent_cli::inline_argv);
            let command_argv = inline_command.as_deref();
            // Reject malformed commands before looking up the shell or allocating a PTY.
            if command_argv.is_some_and(|argv| argv.is_empty()) {
                return Err(anyhow!("command argv must not be empty"));
            }
            // Report stale session-history paths before launching. The shared
            // launcher checks chdir again in the child.
            if let Some(dir) = cwd {
                let metadata = std::fs::metadata(dir).map_err(|error| {
                    PtyLaunchError::PrepareWorkingDirectory(std::io::Error::new(
                        error.kind(),
                        format!("Invalid PTY working directory '{dir}': {error}"),
                    ))
                })?;
                if !metadata.is_dir() {
                    return Err(PtyLaunchError::PrepareWorkingDirectory(std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        format!("PTY working directory is not a directory: '{dir}'"),
                    ))
                    .into());
                }
            }

            let shell_path = choose_shell(configured_shell, cwd)?;
            log::info!("[PTY] starting shell: {shell_path}");

            let shell_name = std::path::Path::new(&shell_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("sh")
                .to_string();

            let shell_cstr = CString::new(shell_path.as_str())
                .map_err(|_| anyhow!("shell path contains NUL"))?;
            let dash_shell_cstr = CString::new(format!("-{shell_name}"))
                .map_err(|_| anyhow!("shell name contains NUL"))?;
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
                        args.push(
                            CString::new(arg.as_str())
                                .map_err(|_| anyhow!("command argument contains NUL"))?,
                        );
                    }
                    Some((
                        CString::new(program_path)
                            .map_err(|_| anyhow!("command path contains NUL"))?,
                        args,
                    ))
                }
                None => None,
            };

            let (exec_cstr, argv_cstrings): (CString, Vec<CString>) =
                if let Some(command) = command_cstrings {
                    command
                } else if let ("jsh", Some(bash_path)) = (shell_name.as_str(), bash_path) {
                    // Bash sources user startup first; restore the managed
                    // child PATH immediately before exec so rc files cannot
                    // accidentally hide native Vim from the jsh prompt.
                    let mut argv = vec![
                        CString::new("bash").unwrap(),
                        CString::new("-ic").unwrap(),
                        CString::new(jterm_core::vim9::VIM9_SHELL_EXEC).unwrap(),
                        shell_cstr.clone(),
                    ];
                    if let Some(ref sid) = session_id_cstr {
                        argv.push(session_flag.clone());
                        argv.push(sid.clone());
                    }
                    (
                        CString::new(bash_path).map_err(|_| anyhow!("bash path contains NUL"))?,
                        argv,
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

            let env_cstrings = jterm_core::child_env::envp(&child_env_options(), extra_env)
                .map(crate::agent_cli::environment)
                .map_err(|error| anyhow!("Failed to build the child environment: {error}"))?;
            let cwd_cstr = cwd.map(CString::new).transpose().map_err(|error| {
                PtyLaunchError::PrepareWorkingDirectory(std::io::Error::from(error))
            })?;
            let directory = cwd_cstr
                .as_deref()
                .map_or(WorkingDirectory::Inherit, WorkingDirectory::Path);
            launch_pty(PtyLaunch {
                executable: &exec_cstr,
                argv: &argv_cstrings,
                environment: &env_cstrings,
                directory,
                rows: pty_dimension(rows),
                columns: pty_dimension(cols),
                parent_death_signal: true,
            })
            .map_err(|error| {
                if let PtyLaunchError::Start(StartupError::Child {
                    stage: StartupStage::Exec,
                    error: cause,
                }) = &error
                {
                    let message = format!("PTY child setup failed at execve: {cause}");
                    anyhow::Error::new(error).context(message)
                } else {
                    error.into()
                }
            })?
            .adopt(|master, child_pid| {
                Ok(Self {
                    master,
                    child_pid,
                    session: companion_host::pty_session::PtySession::capture(child_pid),
                    exit_code_cached: None,
                    lifecycle: ChildLifecycle::Running,
                })
            })
        }

        pub fn get_child_pid(&self) -> i32 {
            self.child_pid
        }

        pub fn session_activity(&self) -> companion_host::pty_session::Activity {
            self.session.activity()
        }

        pub fn master_fd(&self) -> RawFd {
            self.master.as_raw_fd()
        }

        /// Single non-blocking write. Returns bytes written, `Ok(0)` if the
        /// buffer is full (WouldBlock), or an error for a real failure.
        pub fn write(&mut self, data: &[u8]) -> Result<usize> {
            // One nonblocking syscall shares the fault linearization fence.
            // EINTR retries release it, so a fault cannot starve behind retries.
            loop {
                match self.write_once(data) {
                    Ok(count) => return Ok(count),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(0),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(anyhow!("Failed to write to PTY: {error}")),
                }
            }
        }

        fn write_once(&self, data: &[u8]) -> std::io::Result<usize> {
            iced::advanced::subscription::Event::with_native_input_write(|| {
                // SAFETY: master is still owned and data is readable for the syscall.
                let count =
                    unsafe { libc::write(self.master_fd(), data.as_ptr().cast(), data.len()) };
                if count < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(count as usize)
                }
            })
            .unwrap_or_else(|| {
                Err(std::io::Error::other(
                    "native input overloaded; restart required",
                ))
            })
        }

        pub(crate) fn child_is_running(&self) -> bool {
            self.lifecycle == ChildLifecycle::Running
                && matches!(Self::observe_child_exit(self.child_pid), Ok(None))
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
                    self.master_fd(),
                    libc::TIOCSWINSZ,
                    (&win_size) as *const _ as *mut libc::c_void,
                ) < 0
                {
                    return Err(anyhow!("Failed to resize PTY"));
                }
            }
            Ok(())
        }

        /// The child's real exit status once reaped (negative = `-signal`),
        /// reaping via [`Self::is_alive`] first when the state is still open.
        /// Task terminals report this to the task reducer on PTY EOF.
        pub fn exited_code(&mut self) -> Option<i32> {
            let _ = self.is_alive();
            self.exit_code_cached
        }

        #[cfg(test)]
        pub(crate) fn cached_exit_code(&self) -> Option<i32> {
            self.exit_code_cached
        }

        /// Observe a child's exit status without consuming it (`waitid` with
        /// `WNOWAIT`). `Ok(None)` means still running; `Ok(Some(code))` means
        /// exited — decoded like a wait status, negative for `-signal` — while
        /// remaining reappable by the owning [`Pty`]. Keeping the exited
        /// leader as a zombie preserves ownership of its numeric
        /// PID/process-group identity until group cleanup has run; reaping
        /// first would make a later `kill(-pid, ...)` vulnerable to PID/PGID
        /// reuse. The PTY reader thread uses this to probe liveness without
        /// ever becoming a second reaper.
        pub fn observe_child_exit(child_pid: i32) -> std::io::Result<Option<i32>> {
            loop {
                match jterm_core::process::observe_child_exit(child_pid, true) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => {
                        return result.map(|exit| {
                            exit.map(|outcome| outcome.negative_signal_code().unwrap_or(-1))
                        });
                    }
                }
            }
        }

        /// The direct child is known to be exited but deliberately unreaped.
        /// Kill its still-owned private process group first — background jobs
        /// the shell left behind must not survive a natural leader exit — then
        /// consume the original leader status exactly once. SIGKILL cannot
        /// change a status that is already waitable, so a successful exit
        /// keeps its real code.
        fn finish_observed_exit(&mut self) -> std::io::Result<i32> {
            if let Some(code) = self.exit_code_cached {
                return Ok(code);
            }
            // The wait owner retains the zombie anchor through this bounded
            // drain, including jobs in other process groups of the session.
            self.session.drain_after_exit()?;
            // SAFETY: WNOWAIT kept child_pid unreaped, so its process-group ID
            // cannot be recycled between this signal and the waitpid below.
            let killed = unsafe { libc::kill(-self.child_pid, libc::SIGKILL) };
            if killed < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    // Preserve the zombie ownership anchor and retry rather
                    // than publishing completion while a group we failed to
                    // terminate may still be active.
                    return Err(error);
                }
            }

            let mut status = 0;
            let result = loop {
                // SAFETY: the observed child is waitable and still owned by
                // this Pty. is_alive/terminate serialize on `&mut self`, and
                // the detached reaper only runs after TerminationStarted, so
                // no other path can wait on this child concurrently.
                let result = unsafe { libc::waitpid(self.child_pid, &mut status, 0) };
                if result >= 0 {
                    break result;
                }
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    // Already reaped elsewhere despite the anchor; the status
                    // is unknown and must not masquerade as a clean exit.
                    self.exit_code_cached = Some(-1);
                    self.lifecycle = ChildLifecycle::Reaped;
                    return Ok(-1);
                }
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(error);
                }
            };
            if result != self.child_pid {
                return Err(std::io::Error::other(
                    "waitpid returned no status for an observed PTY exit",
                ));
            }
            let code = jterm_core::process::decode_wait_status(status)
                .and_then(jterm_core::process::ChildExitOutcome::negative_signal_code)
                .unwrap_or(-1);
            self.exit_code_cached = Some(code);
            self.lifecycle = ChildLifecycle::Reaped;
            Ok(code)
        }

        pub fn is_alive(&mut self) -> bool {
            // A detached reaper owns TerminationStarted children. Never issue a
            // second waitpid (or later signal) for a pid once teardown begins.
            if self.lifecycle != ChildLifecycle::Running || self.exit_code_cached.is_some() {
                return false;
            }

            match Self::observe_child_exit(self.child_pid) {
                Ok(None) => true,
                Ok(Some(_)) => match self.finish_observed_exit() {
                    Ok(_) => false,
                    Err(error) => {
                        log::warn!("[PTY] could not finish observed PTY child exit: {error}");
                        true
                    }
                },
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                    // Already reaped elsewhere. The status is unknown, so
                    // stop polling without reporting a successful task exit.
                    self.exit_code_cached = Some(-1);
                    self.lifecycle = ChildLifecycle::Reaped;
                    false
                }
                Err(error) => {
                    // Conservatively keep the process live: an unrelated
                    // waitid failure is not evidence that the PID was reaped.
                    log::warn!("[PTY] waitid(WNOWAIT) failed while checking PTY child: {error}");
                    true
                }
            }
        }

        pub fn terminate(&mut self) -> Result<()> {
            if self.begin_termination() {
                let child_pid = self.child_pid;
                std::thread::spawn(move || {
                    std::thread::sleep(TERMINATION_GRACE);
                    Self::kill_and_reap(child_pid);
                });
            }
            Ok(())
        }

        fn begin_termination(&mut self) -> bool {
            // Neither a completed natural exit nor a previously delegated
            // termination can target this numeric PID again.
            if self.lifecycle != ChildLifecycle::Running || !self.is_alive() {
                return false;
            }
            self.lifecycle = ChildLifecycle::TerminationStarted;
            self.exit_code_cached = Some(-libc::SIGTERM);
            // SAFETY: this live child is a session/process-group leader and
            // its PID is still owned by this Pty.
            unsafe {
                let _ = libc::kill(-self.child_pid, libc::SIGHUP);
                let _ = libc::kill(self.child_pid, libc::SIGTERM);
            }
            true
        }

        fn kill_and_reap(child_pid: i32) {
            // Keep the zombie PID/PGID anchor until the final group signal.
            // An ECHILD from observation means another owner consumed it.
            if let Err(error) = Self::observe_child_exit(child_pid) {
                if error.raw_os_error() != Some(libc::ECHILD) {
                    log::warn!("could not observe terminating PTY child: {error}");
                }
                return;
            }
            // SAFETY: the owned leader has not been reaped, and waitpid
            // targets only that direct child after signaling.
            unsafe {
                let _ = libc::kill(-child_pid, libc::SIGKILL);
                let _ = libc::kill(child_pid, libc::SIGKILL);
                let mut status = 0;
                loop {
                    if libc::waitpid(child_pid, &mut status, 0) >= 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                    {
                        break;
                    }
                }
            }
        }
    }

    const TERMINATION_GRACE: Duration = Duration::from_millis(50);

    impl AsFd for Pty {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.master.as_fd()
        }
    }

    impl jterm_core::session_actor::SessionTransport for Pty {
        fn read(
            &mut self,
            buffer: &mut [u8],
        ) -> Result<jterm_core::session_actor::ReadOutcome, String> {
            jterm_core::pty_reader::read_master(self.as_fd(), buffer)
                .map_err(|error| error.to_string())
        }

        fn write(
            &mut self,
            data: &[u8],
        ) -> Result<jterm_core::session_actor::WriteOutcome, String> {
            use jterm_core::session_actor::WriteOutcome;
            match Pty::write(self, data).map_err(|error| error.to_string())? {
                0 => Ok(WriteOutcome::WouldBlock),
                count => Ok(WriteOutcome::Written(count)),
            }
        }

        fn resize(&mut self, cols: usize, rows: usize) -> Result<(), String> {
            Pty::resize(self, cols, rows).map_err(|error| error.to_string())
        }

        fn is_alive(&mut self) -> bool {
            self.child_is_running()
        }

        fn try_reap(&mut self) -> Option<i32> {
            self.exited_code()
        }

        fn wait_timeout(&mut self, timeout_ms: u64) -> Result<i32, String> {
            let started = Instant::now();
            let timeout = Duration::from_millis(timeout_ms);
            loop {
                if let Some(status) = self.try_reap() {
                    return Ok(status);
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err("PTY child exit is not ready".into());
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
        }

        fn observe_before_reap_pid(&self) -> Option<i32> {
            Some(self.child_pid)
        }

        fn write_control_byte(&mut self, byte: u8) -> bool {
            matches!(self.write_once(&[byte]), Ok(1))
        }

        fn input_admitted() -> bool {
            !iced::advanced::subscription::Event::input_overloaded()
        }

        fn termination_grace(&self) -> Duration {
            TERMINATION_GRACE
        }

        fn signal_terminate(&mut self) {
            self.begin_termination();
        }

        fn force_kill_and_reap(&mut self) {
            if self.lifecycle == ChildLifecycle::TerminationStarted {
                Self::kill_and_reap(self.child_pid);
                self.lifecycle = ChildLifecycle::Reaped;
            }
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.terminate();
        }
    }

    #[inline]
    fn pty_dimension(value: usize) -> u16 {
        value.clamp(1, u16::MAX as usize) as u16
    }

    #[cfg(test)]
    mod lifecycle_tests {
        use super::*;

        #[cfg(target_os = "linux")]
        #[test]
        fn empty_command_argv_does_not_leak_pty_fds() {
            const HELPER: &str = "FROST_TEST_EMPTY_ARGV_FDS";
            if std::env::var_os(HELPER).is_none() {
                // Isolate the fd count from other concurrently running tests.
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "empty_command_argv_does_not_leak_pty_fds",
                        "--test-threads=1",
                    ])
                    .env(HELPER, "1")
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "isolated fd test failed: {}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                return;
            }
            let count_fds = || std::fs::read_dir("/proc/self/fd").unwrap().count();
            let before = count_fds();
            for _ in 0..8 {
                let error = Pty::new_with_cwd(80, 24, Some("/"), None, Some("/bin/sh"), Some(&[]))
                    .err()
                    .expect("empty command argv must fail");
                assert!(error.to_string().contains("command argv must not be empty"));
            }
            assert_eq!(count_fds(), before, "invalid argv leaked PTY descriptors");
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn version_probe_does_not_wait_for_an_escaped_stdout_holder() {
            use std::os::unix::fs::PermissionsExt;

            let root = std::env::temp_dir().join(format!(
                "frost-probe-escaped-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let script = root.join("jsh");
            // setsid deliberately escapes the owned group while keeping the
            // output pipe open. This tests a bounded probe, not containment of
            // escaped sessions. The fixture exits on its own after 1.2 seconds.
            std::fs::write(
                &script,
                concat!(
                    "#!/bin/sh\n",
                    "setsid /bin/sh -c 'printf ready > \"$1\"; sleep 1.2' ",
                    "probe-child \"$0.ready\" &\n",
                    "while [ ! -e \"$0.ready\" ]; do sleep 0.01; done\n",
                    "printf 'jsh 0.1.0\\n'\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let started = Instant::now();
            let banner = jsh_version_banner(&script);
            let elapsed = started.elapsed();
            let fixture_started = script.with_extension("ready").exists();
            let _ = std::fs::remove_dir_all(root);
            assert!(fixture_started, "setsid fixture did not start");
            assert!(banner.is_some_and(|banner| banner.starts_with("jsh ")));
            assert!(
                elapsed < Duration::from_secs(1),
                "version probe exceeded its deadline waiting for an escaped stdout holder: {elapsed:?}"
            );
        }

        #[test]
        fn terminate_cleans_descendants_after_leader_exits_during_grace_period() {
            let root = std::env::temp_dir().join(format!(
                "frost-pty-terminate-descendants-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let ready = root.join("ready");
            let leader_exited = root.join("leader-exited");
            let escaped = root.join("descendant-escaped");
            let argv = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                // Non-interactive sh leaves the background job in the leader's
                // group. Synchronize after its traps are installed, then make
                // the leader exit immediately on TERM while its child ignores
                // both graceful signals. The child is deliberately bounded so
                // a failing regression cannot leave a long-running orphan.
                concat!(
                    "trap '' HUP; trap 'printf exited > \"$2\"; exit 0' TERM; ",
                    "(trap '' HUP TERM; printf ready > \"$1\"; ",
                    "sleep 0.6; printf escaped > \"$3\") & ",
                    "while :; do :; done"
                )
                .to_string(),
                "frost-terminate-descendant-test".to_string(),
                ready.to_string_lossy().into_owned(),
                leader_exited.to_string_lossy().into_owned(),
                escaped.to_string_lossy().into_owned(),
            ];
            let mut pty = Pty::new_with_cwd(80, 24, Some("/"), None, None, Some(&argv))
                .expect("spawn termination test shell");
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.exists() {
                assert!(
                    Instant::now() < deadline,
                    "descendant did not install its traps"
                );
                std::thread::sleep(Duration::from_millis(1));
            }

            pty.terminate().expect("terminate the leader");
            pty.terminate().expect("repeated terminate is harmless");
            assert_eq!(pty.lifecycle, ChildLifecycle::TerminationStarted);
            // Do not reap here: the detached thread is the sole wait owner.
            std::thread::sleep(Duration::from_millis(850));
            let leader_handled_term = leader_exited.exists();
            let descendant_survived = escaped.exists();
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let observed = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pty.child_pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            let wait_error = std::io::Error::last_os_error();
            let _ = std::fs::remove_dir_all(root);
            assert_eq!(observed, -1, "detached reaper did not reap the leader");
            assert_eq!(wait_error.raw_os_error(), Some(libc::ECHILD));
            assert!(
                leader_handled_term,
                "leader did not handle TERM before SIGKILL"
            );
            assert!(
                !descendant_survived,
                "same-group descendant survived after the leader exited during termination"
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

        #[test]
        fn exited_leader_cannot_leave_same_group_descendants_running() {
            let root = std::env::temp_dir().join(format!(
                "frost-pty-descendant-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let escaped_marker = root.join("descendant-escaped");
            let argv = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "(trap '' HUP TERM; sleep 0.6; printf escaped > \"$1\") & exit 0".to_string(),
                "frost-descendant-test".to_string(),
                escaped_marker.to_string_lossy().into_owned(),
            ];
            let mut pty = Pty::new_with_cwd(80, 24, Some("/"), None, None, Some(&argv))
                .expect("spawn descendant test shell");

            let deadline = Instant::now() + Duration::from_secs(2);
            while pty.is_alive() {
                assert!(Instant::now() < deadline, "leader did not exit in time");
                std::thread::sleep(Duration::from_millis(10));
            }
            // Observing the natural exit killed the group and reaped the
            // leader exactly once, preserving its real status.
            assert_eq!(pty.exited_code(), Some(0));
            assert_eq!(pty.lifecycle, ChildLifecycle::Reaped);

            // The old reap-first path published the exit while the background
            // child remained alive. Give that child enough time to prove it
            // escaped; the cleaned private process group can never create
            // this marker.
            std::thread::sleep(Duration::from_millis(850));
            assert!(!escaped_marker.exists());
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn an_externally_reaped_child_has_unknown_not_successful_status() {
            let argv = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 7".to_string(),
            ];
            let mut pty = Pty::new_with_cwd(80, 24, Some("/"), None, None, Some(&argv))
                .expect("spawn externally reaped test child");
            // Model a competing reaper consuming the status. ECHILD cannot
            // establish success, and task terminals must not report it as 0.
            let mut status = 0;
            loop {
                let result = unsafe { libc::waitpid(pty.child_pid, &mut status, 0) };
                if result == pty.child_pid {
                    break;
                }
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EINTR)
                );
            }
            assert!(!pty.is_alive());
            assert_eq!(pty.exited_code(), Some(-1));
            assert_eq!(pty.lifecycle, ChildLifecycle::Reaped);
        }

        #[test]
        fn observe_child_exit_never_consumes_the_wait_status() {
            let argv = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 0.2; exit 7".to_string(),
            ];
            let mut pty = Pty::new_with_cwd(80, 24, Some("/"), None, None, Some(&argv))
                .expect("spawn observe test shell");
            let child_pid = pty.get_child_pid();

            // While the child runs, observation reports liveness without
            // disturbing a later reap.
            assert_eq!(Pty::observe_child_exit(child_pid).unwrap(), None);

            let deadline = Instant::now() + Duration::from_secs(2);
            let observed = loop {
                match Pty::observe_child_exit(child_pid).unwrap() {
                    Some(code) => break code,
                    None => {
                        assert!(Instant::now() < deadline, "child did not exit in time");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            };
            assert_eq!(observed, 7);

            // WNOWAIT left the status in place: is_alive still reaps the real
            // code instead of finding an empty wait queue.
            assert!(!pty.is_alive());
            assert_eq!(pty.exited_code(), Some(7));
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

        pub fn session_activity(&self) -> companion_host::pty_session::Activity {
            companion_host::pty_session::Activity::Unknown
        }

        pub fn terminate(&mut self) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }
    }
}

#[cfg(unix)]
pub use unix_pty::Pty;

/// Absolute shell path a pane would spawn, exposed for task provenance:
/// [`SemanticCommandContext`](crate::agent_task::SemanticCommandContext)
/// records this identity so validation replays the source command through the
/// same shell rather than a later hot-reloaded configuration.
#[cfg(unix)]
pub(crate) fn resolved_shell_identity(
    configured_shell: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    unix_pty::choose_shell(configured_shell, cwd).ok()
}

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
        assert!(error
            .downcast_ref::<jterm_core::native_pty_launch::PtyLaunchError>()
            .is_some_and(jterm_core::native_pty_launch::PtyLaunchError::failed_working_directory));
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
