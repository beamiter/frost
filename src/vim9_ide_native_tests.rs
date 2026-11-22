//! Headless integration of the production Frost PTY adapter with actual Vim.
//! This does not mock editing, sockets, the parser, or child-exit observation.

use super::*;
use std::fs;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("frost-vim9-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        // An ancestor (including /tmp) may itself be a checkout. Project
        // discovery must be bounded by this test's isolated fixture.
        fs::create_dir(path.join(".git")).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pump(session: &mut Session) {
    for _ in 0..32 {
        let Ok(event) = session.pty.events().try_recv() else {
            break;
        };
        if let crate::SessionEvent::Output(bytes) = event {
            session.terminal.process_batch(bytes.as_bytes());
            session.flush_responses();
            session.refresh();
        }
    }
}

#[track_caller]
fn wait_for(
    session: &mut Session,
    editor: &Vim9Session,
    mut ready: impl FnMut(&Vim9Snapshot) -> bool,
) -> Vim9Snapshot {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        pump(session);
        let state = editor.snapshot();
        if ready(&state) {
            return state;
        }
        assert!(
            Instant::now() < until,
            "Real Vim did not reach the expected state: {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn child_exited(session: &Session) -> bool {
    session
        .pty
        .inspect_transport(|pty| pty.cached_exit_code())
        .is_some()
}

#[cfg(target_os = "linux")]
fn diagnostic_file_matches(path: &Path, expected: &[u8]) -> Option<bool> {
    use std::io::Read;

    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take((expected.len() + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes == expected)
}

#[cfg(target_os = "linux")]
fn quit_error_diagnostic(error: &str) -> (&'static str, Option<u16>) {
    match error {
        "Vim is busy with a command, operator or completion; finish it before this action" => {
            return ("vim-busy", None);
        }
        "A Vim workbench plugin is waiting for input; finish it before another IDE action" => {
            return ("vim-workbench-busy", None);
        }
        "A save is waiting for Vim to finish input; wait for it before another IDE action" => {
            return ("vim-save-pending", None);
        }
        _ => {}
    }
    if let Some(reason) = error.strip_prefix("Vim9 bridge disconnected: ") {
        return if matches!(
            reason,
            "Invalid native Vim buffer paint frame"
                | "Editor state exceeds the 1 MiB frame limit"
                | "Unterminated editor frame exceeds the 1 MiB limit"
                | "Editor response has no request id"
        ) {
            ("bridge-frame-rejected", None)
        } else {
            ("bridge-disconnected", None)
        };
    }
    if error.starts_with("Vim9 bridge: ") {
        return ("bridge-listener", None);
    }
    // Never print raw text, even on unexpected error origins. Only retain a
    // numeric Vim code from a conventional exception prefix.
    let candidate = error
        .strip_prefix("Vim:")
        .or_else(|| {
            error
                .strip_prefix("Vim(")?
                .split_once("):")
                .map(|(_, rest)| rest)
        })
        .unwrap_or(error);
    let code = candidate
        .strip_prefix('E')
        .and_then(|s| s.split_once(':'))
        .map(|(code, _)| code)
        .filter(|code| (2..=5).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|code| code.parse().ok());
    if code.is_some() {
        return ("vim-error-code", code);
    }
    ("unclassified", None)
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
    let editor = Vim9Session::start(Vim9EntryRequest::user_action(first.clone()))
        .expect("Vim9 must be installed for the explicit native test");
    assert_eq!(editor.root(), scratch.0);
    let mut session = editor_session(&editor, &config, true).unwrap();
    wait_for(&mut session, &editor, |s| {
        s.connected && s.file == first.to_str().unwrap() && s.mode == "n"
    });
    assert!(session.write_pty(b"0inative \x1b"));
    wait_for(&mut session, &editor, |s| s.modified && s.mode == "n");
    let quit_request = editor.send(Vim9Command::Quit).unwrap();
    let refused = wait_for(&mut session, &editor, |s| {
        s.pending == 0 && s.error.is_some()
    });
    let quit_error = refused.error.as_deref().unwrap();
    assert!(
        quit_error.contains("E37") || quit_error.contains("E162"),
        "dirty Quit request {quit_request}: diagnostic={:?}, connected={}, modified={}, normal_mode={}, remaining_commands={}, child_alive={:?}, fixture_file_unchanged={:?}",
        quit_error_diagnostic(quit_error),
        refused.connected,
        refused.modified,
        refused.mode == "n",
        refused.pending,
        !child_exited(&session),
        diagnostic_file_matches(&first, b"original\nsecond\n"),
    );
    assert!(!child_exited(&session), "dirty Vim must remain alive");
    assert_eq!(fs::read_to_string(&first).unwrap(), "original\nsecond\n");
    editor.send(Vim9Command::Save).unwrap();
    wait_for(&mut session, &editor, |s| s.pending == 0 && !s.modified);
    assert_eq!(
        fs::read_to_string(&first).unwrap(),
        "native original\nsecond\n"
    );

    editor
        .send(Vim9Command::Open {
            path: special.to_str().unwrap().into(),
            line: Some(2),
            column: Some(3),
        })
        .unwrap();
    wait_for(&mut session, &editor, |s| {
        s.file == special.to_str().unwrap() && s.line == 2 && s.column == 3 && s.pending == 0
    });
    assert!(session.write_pty(b"iX\x1b"));
    wait_for(&mut session, &editor, |s| s.modified && s.mode == "n");
    editor.send(Vim9Command::SaveAll).unwrap();
    wait_for(&mut session, &editor, |s| s.pending == 0 && !s.modified);
    assert_eq!(fs::read_to_string(&special).unwrap(), "first\nseXcond\n");

    assert!(session.write_pty(b":set errorformat=%f:%l:%c:%m\r"));
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(250) {
        pump(&mut session);
        std::thread::sleep(Duration::from_millis(5));
    }
    editor
        .send(Vim9Command::Build {
            command: "printf 'source.txt:2:1:fixture failure\\n'; exit 3".into(),
        })
        .unwrap();
    let built = wait_for(&mut session, &editor, |s| {
        !s.build.running && s.build.exit_code == Some(3)
    });
    assert_eq!(built.diagnostics.len(), 1);
    assert_eq!(built.diagnostics[0].line, 2);
    assert_eq!(built.diagnostics[0].filename, first.to_str().unwrap());
    assert!(built
        .build
        .output
        .iter()
        .any(|line| line.contains("fixture failure")));

    editor
        .send(Vim9Command::Search {
            query: "native original".into(),
        })
        .unwrap();
    let found = wait_for(&mut session, &editor, |s| {
        !s.search.running && s.search.query == "native original" && s.search.count == 1
    });
    assert_eq!(found.diagnostics[0].filename, first.to_str().unwrap());
    editor.send(Vim9Command::Quit).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !child_exited(&session) {
        assert!(
            Instant::now() < until,
            "saved Vim must exit after native Quit"
        );
        pump(&mut session);
        std::thread::sleep(Duration::from_millis(10));
    }
    // The production Pty destructor is still the only reaper. No process kill
    // or synthetic PTY hangup was used to make this exit assertion pass.
}

#[test]
#[ignore = "requires actual Vim 9, local PTYs and loopback sockets"]
fn embedded_body_first_focus_and_active_clicks_preserve_modified_vim_mouse_gestures() {
    let scratch = Scratch::new();
    let program = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("vim"))
        .find(|path| path.is_file())
        .expect("Vim must be installed");
    let mut editors = Vec::new();
    let mut logs = Vec::new();
    for index in 0..2 {
        let file = scratch.0.join(format!("pane-{index}.txt"));
        fs::write(&file, "first row\nsecond row\nthird row\n").unwrap();
        let log = scratch.0.join(format!("mouse-{index}.log"));
        let ready = scratch.0.join(format!("ready-{index}"));
        let args: Vec<OsString> = [
            "--vim-shell".into(),
            program.as_os_str().to_owned(),
            "--".into(),
            "-u".into(),
            "NONE".into(),
            "-i".into(),
            "NONE".into(),
            "-n".into(),
            file.as_os_str().to_owned(),
        ]
        .into();
        let request = vim9::shell_vim_request(&args).unwrap().unwrap();
        let mut config = Config::default();
        config.vim9.native_buffer = false;
        let ownership = std::sync::Arc::new(parking_lot::Mutex::new(Default::default()));
        let (mut ide, _) = Ide::new(
            request,
            config,
            None,
            ownership,
            Size::new(800.0, 600.0),
            None,
        );
        ide.embedded = true;
        let controller = ide.editor.as_ref().unwrap().clone();
        wait_for(ide.session.as_mut().unwrap(), &controller, |state| {
            state.connected && state.mode == "n"
        });
        let mut setup = String::from(":set nocompatible mouse=a ttymouse=sgr mousetime=1 nowrap\r");
        for (prefix, label) in [("C", "ctrl"), ("M", "alt")] {
            for (event, phase) in [
                ("LeftMouse", "press"),
                ("LeftDrag", "drag"),
                ("LeftRelease", "release"),
            ] {
                setup.push_str(&format!(":nnoremap <{prefix}-{event}> <Cmd>call writefile(['{label}_{phase}'], '{}', 'a')<CR>\r", log.display()));
            }
        }
        setup.push_str(&format!(
            ":call writefile(['ready'], '{}')\r",
            ready.display()
        ));
        assert!(ide.session.as_mut().unwrap().write_pty(setup.as_bytes()));
        wait_for(ide.session.as_mut().unwrap(), &controller, |_| {
            ready.exists()
        });
        pump(ide.session.as_mut().unwrap());
        assert!(ide.session.as_ref().unwrap().terminal.is_mouse_enabled());
        ide.focus_embedded(index == 0);
        editors.push(ide);
        logs.push(log);
    }
    let valid_entry = editors[1].editor.as_ref().unwrap().entry_id();
    let stale_message = editors[0].body_mouse_message(MouseInput::Release {
        col: 1,
        row: 1,
        button: MouseButton::Left,
    });
    assert!(!editors[1].accepts_body_mouse(editors[0].editor.as_ref().map(Vim9Session::entry_id)));
    let _ = editors[1].update(stale_message);
    assert!(
        !editors[1].focused,
        "a queued event from a replaced editor must not activate its successor"
    );
    let _ = editors[1].form(FormKind::Goto);
    assert!(
        !editors[1].accepts_body_mouse(Some(valid_entry)),
        "a host form must block before workspace focus"
    );
    assert!(super::super::visible_embedded_body_session(
        &super::super::PaneTree::Leaf(1),
        202,
        editors[1].accepts_body_mouse(Some(valid_entry)),
        [(1, 202)]
    )
    .is_none());
    editors[1].form = None;
    let layout = super::super::PaneTree::Split {
        axis: super::super::Axis::Vertical,
        children: vec![
            super::super::PaneTree::Leaf(0),
            super::super::PaneTree::Leaf(1),
        ],
        ratios: vec![0.5, 0.5],
    };
    for (current_modifiers, expected_lines) in [
        (keyboard::Modifiers::CTRL, 3usize),
        (keyboard::Modifiers::ALT, 6usize),
    ] {
        let mut press = MouseInput::Press {
            col: 1,
            row: 1,
            button: MouseButton::Left,
            shift: false,
            alt: true,
            ctrl: false,
            count: 1,
            finalized: false,
            link_eligible: false,
            app_eligible: true,
            block: None,
            block_zone_id: None,
            link: false,
            link_revision: 0,
            x: 8.0,
            y: 20.0,
        };
        super::super::sync_embedded_body_modifiers(&mut press, current_modifiers);
        let target =
            super::super::visible_embedded_body_session(&layout, 202, true, [(0, 101), (1, 202)])
                .unwrap();
        // This is the workspace's production order: focus first, then route
        // the original widget press exactly once to its stable source editor.
        editors[target].set_workspace_modifiers(current_modifiers);
        editors[target].focus_embedded(true);
        let message = editors[target].body_mouse_message(press);
        editors[target].set_workspace_modifiers(keyboard::Modifiers::empty());
        let _ = editors[target].update(message);
        assert!(
            editors[target].modifiers.is_empty(),
            "an old click must not roll back the latest keyboard state"
        );
        editors[target].set_workspace_modifiers(current_modifiers);
        dispatch_body_mouse(
            &mut editors[target],
            MouseInput::Drag {
                col: 2,
                row: 1,
                count: 1,
            },
        );
        dispatch_body_mouse(
            &mut editors[target],
            MouseInput::Release {
                col: 2,
                row: 1,
                button: MouseButton::Left,
            },
        );
        let controller = editors[target].editor.as_ref().unwrap().clone();
        wait_for(
            editors[target].session.as_mut().unwrap(),
            &controller,
            |_| {
                fs::read_to_string(&logs[target])
                    .is_ok_and(|text| text.lines().count() >= expected_lines)
            },
        );
        assert!(!editors[target]
            .session
            .as_ref()
            .unwrap()
            .terminal
            .has_text_selection());
        assert!(editors[target].gestures[MouseButton::Left.slot()].is_none());
        assert!(
            !logs[0].exists(),
            "the previously active pane must not receive any part of this gesture"
        );
    }
    // The workspace opens an overlay after an accepted app press. Closing
    // that gesture must use its original cell and emit exactly one release;
    // a physical release arriving after the overlay closes is then inert.
    editors[1].set_workspace_modifiers(keyboard::Modifiers::CTRL);
    let press = MouseInput::Press {
        col: 1,
        row: 1,
        button: MouseButton::Left,
        shift: false,
        alt: false,
        ctrl: true,
        count: 1,
        finalized: false,
        link_eligible: false,
        app_eligible: true,
        block: None,
        block_zone_id: None,
        link: false,
        link_revision: 0,
        x: 8.0,
        y: 20.0,
    };
    dispatch_body_mouse(&mut editors[1], press);
    editors[1].cancel_body_pointer();
    editors[1].cancel_body_pointer();
    dispatch_body_mouse(
        &mut editors[1],
        MouseInput::Release {
            col: 99,
            row: 29,
            button: MouseButton::Left,
        },
    );
    let controller = editors[1].editor.as_ref().unwrap().clone();
    wait_for(editors[1].session.as_mut().unwrap(), &controller, |_| {
        fs::read_to_string(&logs[1]).is_ok_and(|text| text.lines().count() >= 8)
    });
    assert_eq!(
        fs::read_to_string(&logs[1]).unwrap(),
        "ctrl_press\nctrl_drag\nctrl_release\nalt_press\nalt_drag\nalt_release\nctrl_press\nctrl_release\n"
    );
    assert!(editors[1].gestures.iter().all(Option::is_none));
    assert!(!logs[0].exists());
}

fn dispatch_body_mouse(ide: &mut Ide, input: MouseInput) {
    let message = ide.body_mouse_message(input);
    let _ = ide.update(message);
}
