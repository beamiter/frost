//! Real Bash/readline state, through Frost's search reducers and split path.
//! No compositor is required; this does not test Iced widget focus or OS IME.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHILD: &str = "FROST_READLINE_TEST_CHILD";
const TEST: &str =
    "readline_preservation_tests::search_and_split_preserve_readline_draft_cursor_and_undo";

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Keep PTY ownership alive until its child is reaped, including on assertion
// failure. Production Drop intentionally uses an asynchronous UI-safe reaper;
// this short-lived test process must not exit before that cleanup completes.
struct TestApp(Frost);
impl std::ops::Deref for TestApp {
    type Target = Frost;
    fn deref(&self) -> &Frost {
        &self.0
    }
}
impl std::ops::DerefMut for TestApp {
    fn deref_mut(&mut self) -> &mut Frost {
        &mut self.0
    }
}
impl Drop for TestApp {
    fn drop(&mut self) {
        for session in &mut self.0.sessions {
            if session.pty.is_alive() {
                let pid = session.pty.get_child_pid();
                // The unreaped child still pins its PID and session PGID.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut any_alive = false;
            for session in &mut self.0.sessions {
                // Do not short-circuit: every Pty must observe/reap its child.
                any_alive |= session.pty.is_alive();
            }
            if !any_alive || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

// Run in a subprocess rather than mutating HOME/XDG in the parallel test runner.
#[test]
fn search_and_split_preserve_readline_draft_cursor_and_undo() {
    if let Some(root) = std::env::var_os(CHILD) {
        run_child(Path::new(&root));
        return;
    }
    assert!(
        Path::new("/bin/bash").is_file(),
        "this regression requires Bash/readline"
    );
    let root = Scratch(std::env::temp_dir().join(format!(
        "frost-readline-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, &root.0)
        .env("HOME", &root.0)
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_CACHE_HOME", root.0.join("cache"))
        .env("XDG_DATA_HOME", root.0.join("data"))
        .env("XDG_STATE_HOME", root.0.join("state"))
        .env("XDG_RUNTIME_DIR", &root.0)
        .env("INPUTRC", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .current_dir(&root.0)
        .spawn()
        .unwrap();
    // Each PTY observation is bounded below. Let the child unwind normally on
    // failure so TestApp can reap its shells; the CI job owns the outer timeout.
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "isolated readline regression failed: {status}"
    );
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn run_child(root: &Path) {
    let rc = root.join("bashrc");
    std::fs::write(&rc, include_str!("../scripts/readline-preservation.bash")).unwrap();
    let wrapper = root.join("fixture-shell");
    std::fs::write(
        &wrapper,
        format!(
        "#!/bin/sh\nexport FROST_READLINE_REPORT={}\nexec /bin/bash --noprofile --rcfile {} -i\n",
        quote(root), quote(&rc)
    ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    for mode in [config::TerminalMode::Block, config::TerminalMode::Chat] {
        let config = Config {
            shell: Some(wrapper.to_str().unwrap().to_owned()),
            restore_session: false,
            terminal_mode: Some(mode),
            ascii_organism_enabled: false,
            ..Config::default()
        };
        let make = || {
            let (mut app, _startup) = Frost::new(
                config.clone(),
                None,
                None,
                Arc::new(parking_lot::Mutex::new(
                    native_enter::NativeEnterOwnership::default(),
                )),
            );
            // This isolated fixture does not persist sessions or execute startup Tasks.
            app.is_first_instance = false;
            app.focused = true;
            TestApp(app)
        };
        let mut app = make();
        let mut control = make();
        wait_prompt(&mut app, 0);
        wait_prompt(&mut control, 0);
        // Two readline edit groups, with the cursor deliberately in the middle.
        // Never submit this line. Ctrl-_ below must undo the inserted Q.
        for target in [&mut app, &mut control] {
            send(target, 0, b"printf KEEPxyz");
            let _ = snapshot(target, 0, root);
            send(target, 0, b"\x02\x02\x02Q");
        }
        let before = snapshot(&mut app, 0, root);
        assert_eq!(before, snapshot(&mut control, 0, root));
        assert_eq!(before.1, "printf KEEPQxyz");
        assert_eq!(before.0, "printf KEEPQ".len());
        let original = (
            app.sessions[0].id,
            app.sessions[0].master_fd,
            app.sessions[0].pty.get_child_pid(),
            Arc::clone(&app.sessions[0].ime_identity),
        );
        let cursor = app.sessions[0].terminal.get_cursor_pos();
        let original_cols = app.sessions[0].terminal.grid.cols();
        assert!(app.sessions[0].terminal.scrollback_len() > 0);

        // Real command dispatch and reducers, not a source-text assertion.
        let _ = app
            .dispatch_command(keybindings::Command::SearchOpen)
            .unwrap();
        let _ = app.update(Message::SearchInput("needle".into()));
        assert!(!app.search.matches.is_empty());
        let _ = app
            .dispatch_command(keybindings::Command::SearchNext)
            .unwrap();
        let _ = app
            .dispatch_command(keybindings::Command::SearchClose)
            .unwrap();
        assert!(!app.search.is_open);
        assert_eq!(app.sessions[0].terminal.get_cursor_pos(), cursor);
        assert_eq!(snapshot(&mut app, 0, root), before);

        if config.is_block_mode() {
            let _ = app.update(Message::BlockBrowse);
            let open = app.block_search.as_ref().expect("Block search opened");
            let identity = BlockSearchBuildIdentity {
                session_id: open.session_id,
                epoch: open.epoch,
            };
            // Same production cache builder/completion reducer, without spawning
            // an executor solely to deliver this deterministic worker result.
            let build = block_mode::build_block_search_cache(
                Frost::block_search_source_snapshot(&app.sessions[0]),
                block_mode::BLOCK_SEARCH_CACHE_MAX_BYTES,
            );
            let _ = app.update(Message::BlockSearchCacheBuilt(identity, Ok(build)));
            let _ = app.update(Message::BlockSearchInput("needle".into()));
            let hit = app
                .block_search
                .as_ref()
                .unwrap()
                .hits
                .first()
                .unwrap()
                .clone();
            let _ = app.update(Message::BlockSearchAccept(hit));
            assert!(app.block_search.is_none());
            assert_eq!(snapshot(&mut app, 0, root), before);
        }

        let _ = app
            .dispatch_command(keybindings::Command::TerminalSplitVertical)
            .unwrap();
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.active, 1);
        assert!(app.sessions[0].terminal.grid.cols() < original_cols);
        assert!(app.history_reflow_sessions.contains(&original.0));
        wait_prompt(&mut app, 1);
        assert_eq!(
            snapshot(&mut app, 1, root),
            (0, String::new()),
            "new PTY must not inherit draft"
        );
        app.focus_prev_pane();
        assert_eq!(app.active, 0);
        let _ = app.update(Message::BlockGoLive);
        app.history_reflow_due = Some(Instant::now());
        let _ = app.update(Message::HistoryReflowTick);
        assert!(
            !app.history_reflow_sessions.contains(&original.0),
            "history normalization completed"
        );
        assert_eq!(app.sessions[0].id, original.0);
        assert_eq!(app.sessions[0].master_fd, original.1);
        assert_eq!(app.sessions[0].pty.get_child_pid(), original.2);
        assert!(Arc::ptr_eq(&app.sessions[0].ime_identity, &original.3));
        assert_eq!(
            snapshot(&mut app, 0, root),
            before,
            "resize must preserve readline cursor and draft"
        );

        send(&mut app, 0, b"\x1f");
        send(&mut control, 0, b"\x1f");
        let undone = snapshot(&mut app, 0, root);
        assert_eq!(
            undone,
            (11, "printf KEEPxyz".to_owned()),
            "Undo must remove only the inserted Q"
        );
        assert_eq!(
            undone,
            snapshot(&mut control, 0, root),
            "UI operations must preserve readline Undo"
        );
        assert_eq!(
            snapshot(&mut app, 1, root),
            (0, String::new()),
            "Undo stayed with original PTY"
        );
    }
}

fn send(app: &mut Frost, index: usize, bytes: &[u8]) {
    assert!(app.sessions[index].write_pty(bytes));
}

fn pump(app: &mut Frost) {
    let sources: Vec<_> = app.sessions.iter().map(|s| (s.id, s.master_fd)).collect();
    for (id, fd) in sources {
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // The normal subscription is not running in this reducer fixture.
        if unsafe { libc::poll(&mut ready, 1, 5) } > 0 && ready.revents & libc::POLLIN != 0 {
            let mut bytes = [0u8; 8192];
            let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            assert!(n > 0, "fixture PTY unexpectedly closed");
            let _ = app.update(Message::PtyOutput(
                id,
                fd,
                bytes[..n as usize].to_vec(),
                None,
            ));
        }
    }
    for session in &mut app.sessions {
        assert!(session.flush_write_queue());
    }
}

fn wait_prompt(app: &mut Frost, index: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pump(app);
        let terminal = &app.sessions[index].terminal;
        if !terminal.command_zones.is_empty() && terminal.live_prompt_row().is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "Bash OSC prompt did not arrive");
    }
}

fn snapshot(app: &mut Frost, index: usize, root: &Path) -> (usize, String) {
    let report = root.join(app.sessions[index].pty.get_child_pid().to_string());
    let _ = std::fs::remove_file(&report);
    send(app, index, b"\x18\x04");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pump(app);
        if let Ok(value) = std::fs::read_to_string(&report) {
            let (point, line) = value
                .trim_end_matches('\n')
                .split_once('\n')
                .unwrap_or_else(|| (value.trim(), ""));
            return (
                point.parse().expect("readline cursor position"),
                line.to_owned(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "readline snapshot binding did not run"
        );
    }
}
