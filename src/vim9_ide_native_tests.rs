//! Headless integration of the production Frost PTY adapter with actual Vim.
//! This does not mock editing, sockets, the parser, or child-exit observation.

use super::*;
use std::fs;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("frost-vim9-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn pump(session: &mut Session) {
    session.flush_write_queue();
    let mut bytes = [0u8; 16384];
    for _ in 0..32 {
        // SAFETY: this test owns a live Session and its nonblocking master fd;
        // the stack buffer is writable for the advertised byte count.
        let n = unsafe { libc::read(session.master_fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if n <= 0 { break; }
        session.terminal.process_batch(&bytes[..n as usize]);
        session.flush_responses();
        session.refresh();
    }
}

#[track_caller]
fn wait_for(session: &mut Session, editor: &Vim9Session, mut ready: impl FnMut(&Vim9Snapshot) -> bool) -> Vim9Snapshot {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        pump(session);
        let state = editor.snapshot();
        if ready(&state) { return state; }
        assert!(Instant::now() < until, "Real Vim did not reach the expected state: {state:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn child_exited_without_reaping(session: &Session) -> bool {
    // SAFETY: zero initialization is valid for siginfo_t, and waitid writes
    // only that structure. WNOWAIT leaves reaping to the owning production Pty.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let result = libc::waitid(libc::P_PID, session.pty.get_child_pid() as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT);
        assert_eq!(result, 0, "waitid: {}", std::io::Error::last_os_error());
        info.si_pid() != 0
    }
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires actual Vim 9, a local PTY and loopback sockets"]
fn real_vim9_native_pty() {
    let scratch = Scratch::new();
    let first = scratch.0.join("source.txt");
    let special = scratch.0.join("odd |%# file.txt");
    fs::write(&first, "original\nsecond\n").unwrap();
    fs::write(&special, "first\nsecond\n").unwrap();
    let config = Config {
        block_mode: false,
        terminal_mode: Some(config::TerminalMode::Normal),
        disable_alt_screen: false,
        ascii_organism_enabled: false,
        ..Config::default()
    };
    let editor = Vim9Session::start(&first).expect("Vim9 must be installed for the explicit native test");
    let mut session = editor_session(&editor, &config, true).unwrap();
    wait_for(&mut session, &editor, |s| s.connected && s.file == first.to_str().unwrap() && s.mode == "n");
    assert!(session.write_pty(b"0inative \x1b"));
    wait_for(&mut session, &editor, |s| s.modified && s.mode == "n");
    editor.send(Vim9Command::Quit).unwrap();
    let refused = wait_for(&mut session, &editor, |s| s.pending == 0 && s.error.is_some());
    assert!(refused.error.as_ref().unwrap().contains("E37") || refused.error.as_ref().unwrap().contains("E162"));
    assert!(!child_exited_without_reaping(&session), "dirty Vim must remain alive");
    assert_eq!(fs::read_to_string(&first).unwrap(), "original\nsecond\n");
    editor.send(Vim9Command::Save).unwrap();
    wait_for(&mut session, &editor, |s| s.pending == 0 && !s.modified);
    assert_eq!(fs::read_to_string(&first).unwrap(), "native original\nsecond\n");

    editor.send(Vim9Command::Open { path: special.to_str().unwrap().into(), line: Some(2), column: Some(3) }).unwrap();
    wait_for(&mut session, &editor, |s| s.file == special.to_str().unwrap() && s.line == 2 && s.column == 3 && s.pending == 0);
    assert!(session.write_pty(b"iX\x1b"));
    wait_for(&mut session, &editor, |s| s.modified && s.mode == "n");
    editor.send(Vim9Command::SaveAll).unwrap();
    wait_for(&mut session, &editor, |s| s.pending == 0 && !s.modified);
    assert_eq!(fs::read_to_string(&special).unwrap(), "first\nseXcond\n");

    assert!(session.write_pty(b":set errorformat=%f:%l:%c:%m\r"));
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(250) { pump(&mut session); std::thread::sleep(Duration::from_millis(5)); }
    editor.send(Vim9Command::Build { command: "printf 'source.txt:2:1:fixture failure\\n'; exit 3".into() }).unwrap();
    let built = wait_for(&mut session, &editor, |s| !s.build.running && s.build.exit_code == Some(3));
    assert_eq!(built.diagnostics.len(), 1);
    assert_eq!(built.diagnostics[0].line, 2);
    assert_eq!(built.diagnostics[0].filename, first.to_str().unwrap());
    assert!(built.build.output.iter().any(|line| line.contains("fixture failure")));

    editor.send(Vim9Command::Search { query: "native original".into() }).unwrap();
    let found = wait_for(&mut session, &editor, |s| !s.search.running && s.search.query == "native original" && s.search.count == 1);
    assert_eq!(found.diagnostics[0].filename, first.to_str().unwrap());
    editor.send(Vim9Command::Quit).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !child_exited_without_reaping(&session) {
        assert!(Instant::now() < until, "saved Vim must exit after native Quit");
        pump(&mut session);
        std::thread::sleep(Duration::from_millis(10));
    }
    // The production Pty destructor is still the only reaper. No process kill
    // or synthetic PTY hangup was used to make this exit assertion pass.
}
