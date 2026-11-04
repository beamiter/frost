//! Runs the real parser/projection and input budget through repeated PTY resizes.
//! Window/config ownership is a fixture; no SSH server or native display is used.
use std::{collections::HashMap, sync::Arc};
use terminal::{ProjectedViewport, ProjectionPolicy, ProjectionViewState, TerminalState};

struct Config {
    chat: bool,
}
impl Config {
    fn is_card_mode(&self) -> bool {
        true
    }
    fn is_chat_mode(&self) -> bool {
        self.chat
    }
    fn is_block_mode(&self) -> bool {
        !self.chat
    }
}
struct Recovery;
impl Recovery {
    fn banner_height(&self, _: f32) -> f32 {
        0.0
    }
}
struct Session {
    id: usize,
    terminal: TerminalState,
    projection: Arc<ProjectedViewport>,
    cursor: (usize, usize),
    // Bytes the remote shell writes after a SIGWINCH at its prompt.
    winch_redraw: Option<&'static [u8]>,
}
impl Session {
    fn transcript_read_only(&self) -> bool {
        false
    }
    fn refresh(&mut self, policy: &ProjectionPolicy, state: &mut ProjectionViewState) {
        self.projection = self
            .terminal
            .get_projected_viewport_with_state(true, policy, state);
        let (row, col) = self.terminal.get_cursor_pos();
        let origin = self
            .terminal
            .raw_cell_origin_at_absolute(self.terminal.scrollback_len() + row, col)
            .unwrap();
        let cursor = self.projection.raw_to_view(origin).unwrap();
        self.cursor = (cursor.row, cursor.col);
    }
}
struct Frost {
    config: Config,
    sessions: Vec<Session>,
    active: usize,
    vim9_tabs: HashMap<usize, ()>,
    metrics: Metrics,
    startup_recovery: Recovery,
    layout: glass_layout::Layout,
    input_dock_hold: Option<InputDockHold>,
    input_dock_hold_generation: u64,
    // The harness stands in for the expiry timer update() schedules.
    pending_expiry: Option<u64>,
}
impl Frost {
    fn is_split(&self) -> bool {
        false
    }
    fn reference_layout(&self) -> glass_layout::Layout {
        self.layout
    }
    fn block_reading_bar_height(&self) -> f32 {
        0.0
    }
    fn chrome_font_size(&self) -> f32 {
        14.0
    }
    // @input-metrics@
}

fn fixture(
    chat: bool,
    height: f32,
    font: f32,
    draft_rows: usize,
) -> (Frost, ProjectionPolicy, ProjectionViewState) {
    let layout = glass_layout::Layout::with_panels(1000.0, height, None, None, false, false);
    let metrics = Metrics::new(font, 1.0, 2.0, true);
    let raw = layout.native_budget(0.0, 0.0, None).raw;
    let (cols, rows) = metrics.grid_size(raw.width - terminal_view::SCROLLBAR_WIDTH, raw.height);
    let mut terminal = TerminalState::new(cols, rows);
    // Local shell starts SSH, then the remote integrated shell emits its prompt.
    terminal.process_input(
        b"\x1b]133;A\x07local$ \x1b]133;B\x07ssh robot\r\n\x1b]133;C\x07Connected\r\n",
    );
    terminal.process_input(b"\x1b]133;A\x07robot$ \x1b]133;B\x07");
    for _ in 1..draft_rows {
        terminal.process_input(b"line\r\n");
    }
    terminal.process_input(b"draft");
    let mut policy = ProjectionPolicy::new();
    policy.set_chat(true);
    policy.set_command_bubbles(chat);
    let mut state = ProjectionViewState::new();
    let projection = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
    let mut session = Session {
        id: 1,
        terminal,
        projection,
        cursor: (0, 0),
        winch_redraw: None,
    };
    session.refresh(&policy, &mut state);
    (
        Frost {
            config: Config { chat },
            sessions: vec![session],
            active: 0,
            vim9_tabs: HashMap::new(),
            metrics,
            startup_recovery: Recovery,
            layout,
            input_dock_hold: None,
            input_dock_hold_generation: 0,
            pending_expiry: None,
        },
        policy,
        state,
    )
}

/// jsh-remote's bash fallback (`integration.bash`) on a host jsh cannot be
/// deployed to: PS1 is wrapped in A/B, PS0 is C, PROMPT_COMMAND prints D and
/// OSC 7. Unlike jsh, the prompt is a single row.
const BASH_PROMPT: &[u8] = b"\x1b]133;A\x07\x1b]0;ubuntu@ubuntu: ~\x07\x1b[01;32mubuntu@ubuntu\x1b[00m:\x1b[01;34m~\x1b[00m$ \x1b]133;B\x07";
/// readline's redisplay after SIGWINCH: CR, clear the line, repaint PS1.
const BASH_WINCH: &[u8] = b"\r\x1b[K\x1b]133;A\x07\x1b]0;ubuntu@ubuntu: ~\x07\x1b[01;32mubuntu@ubuntu\x1b[00m:\x1b[01;34m~\x1b[00m$ \x1b]133;B\x07";

fn bash_fixture(
    chat: bool,
    height: f32,
    font: f32,
) -> (Frost, ProjectionPolicy, ProjectionViewState) {
    let (mut app, policy, mut state) = fixture(chat, height, font, 1);
    let session = &mut app.sessions[0];
    let mut terminal = TerminalState::new(
        session.terminal.get_dimensions().0,
        session.terminal.get_dimensions().1,
    );
    terminal.process_input(
        b"\x1b]133;A\x07local$ \x1b]133;B\x07ssh robot\r\n\x1b]133;C\x07Connected\r\n",
    );
    terminal.process_input(b"\x1b]7;file://ubuntu/home/ubuntu\x07");
    terminal.process_input(BASH_PROMPT);
    session.terminal = terminal;
    session.winch_redraw = Some(BASH_WINCH);
    session.refresh(&policy, &mut state);
    (app, policy, state)
}

/// Run one remote command whose output scrolls the screen, so the next
/// one-row prompt lands on the last PTY row.
fn bash_command_fills_screen(
    app: &mut Frost,
    policy: &ProjectionPolicy,
    state: &mut ProjectionViewState,
) {
    let session = &mut app.sessions[0];
    let rows = session.terminal.get_dimensions().1;
    session.terminal.process_input(b"ls -la\r\n\x1b]133;C\x07");
    for line in 0..rows * 2 {
        session
            .terminal
            .process_input(format!("-rw-r--r-- 1 ubuntu ubuntu 220 file{line}\r\n").as_bytes());
    }
    session
        .terminal
        .process_input(b"\x1b]133;D;0\x07\x1b]7;file://ubuntu/home/ubuntu\x07");
    session.terminal.process_input(BASH_PROMPT);
    session.refresh(policy, state);
}

fn resize_pass(
    app: &mut Frost,
    policy: &ProjectionPolicy,
    state: &mut ProjectionViewState,
) -> bool {
    // update() syncs the hold before the PTY budget, after every message.
    if let Some(generation) = app.sync_input_dock_hold() {
        app.pending_expiry = Some(generation);
    }
    let input = app
        .reference_input_metrics()
        .map(|metrics| metrics.input_height);
    let raw = app.layout.native_budget(0.0, 0.0, input).raw;
    let dimensions = app
        .metrics
        .grid_size(raw.width - terminal_view::SCROLLBAR_WIDTH, raw.height);
    let session = &mut app.sessions[0];
    let changed = dimensions != session.terminal.get_dimensions();
    if changed {
        session.terminal.on_resize(dimensions.0, dimensions.1);
        // readline repaints only while it owns the prompt, not under a command.
        if let Some(redraw) = session
            .winch_redraw
            .filter(|_| session.terminal.live_prompt_row().is_some())
        {
            session.terminal.process_input(redraw);
        }
    }
    session.refresh(policy, state);
    changed
}

fn assert_settles(app: &mut Frost, policy: &ProjectionPolicy, state: &mut ProjectionViewState) {
    let mut trace = Vec::new();
    for _ in 0..12 {
        let changed = resize_pass(app, policy, state);
        trace.push((
            app.sessions[0].terminal.get_dimensions().1,
            app.reference_input_metrics().map(|m| m.input_height),
        ));
        if !changed {
            return;
        }
    }
    panic!("input/PTY resize oscillation: {trace:?}");
}

fn main() {
    // This previously alternated forever between a 180px dock and no dock.
    for chat in [false, true] {
        let (mut app, policy, mut state) = fixture(chat, 600.0, 14.0, 14);
        assert_settles(&mut app, &policy, &mut state);
        assert!(
            app.reference_input_metrics().is_none(),
            "oversized prompt must stay native"
        );
    }
    for chat in [false, true] {
        for height in [320.0, 480.0, 600.0, 800.0] {
            for font in [10.0, 14.0, 24.0, 36.0] {
                for rows in [1, 2, 3, 4, 6, 10, 14, 20] {
                    let (mut app, policy, mut state) = fixture(chat, height, font, rows);
                    assert_settles(&mut app, &policy, &mut state);
                    // Cursor-only redraws must not start another resize cycle.
                    let dimensions = app.sessions[0].terminal.get_dimensions();
                    app.sessions[0]
                        .terminal
                        .process_input(b"\x1b[?25l\x1b[?25h");
                    app.sessions[0].refresh(&policy, &mut state);
                    assert!(!resize_pass(&mut app, &policy, &mut state));
                    assert_eq!(app.sessions[0].terminal.get_dimensions(), dimensions);
                }
            }
        }
        let (mut app, policy, mut state) = fixture(chat, 600.0, 14.0, 1);
        assert_settles(&mut app, &policy, &mut state);
        assert!(
            app.reference_input_metrics().is_some(),
            "short prompt still docks"
        );
        let first_height = app.reference_input_metrics().unwrap().input_height;
        app.sessions[0]
            .terminal
            .process_input(b"\r\nsecond\r\nthird\r\nfourth");
        app.sessions[0].refresh(&policy, &mut state);
        assert_settles(&mut app, &policy, &mut state);
        assert!(app.reference_input_metrics().unwrap().input_height > first_height);
        for _ in 0..12 {
            app.sessions[0].terminal.process_input(b"\r\nmore input");
        }
        app.sessions[0].refresh(&policy, &mut state);
        assert_settles(&mut app, &policy, &mut state);
        assert!(app.reference_input_metrics().is_none());
        // Remote command completion restores a new short prompt after fallback.
        app.sessions[0].terminal.process_input(
            b"\r\n\x1b]133;C\x07output\r\n\x1b]133;D;0\x07\x1b]133;A\x07robot$ \x1b]133;B\x07",
        );
        app.sessions[0].refresh(&policy, &mut state);
        assert_settles(&mut app, &policy, &mut state);
        assert_eq!(
            app.reference_input_metrics().unwrap().input_height,
            first_height
        );
    }
    // A one-row bash prompt measured one row on the last PTY row but two rows
    // when a blank row sat below it. Docking moves it between those states, so
    // after a command filled the screen the input alternated heights forever.
    for chat in [false, true] {
        for height in (320..=900).step_by(3) {
            for font in [10.0, 13.0, 14.0, 24.0] {
                let (mut app, policy, mut state) = bash_fixture(chat, height as f32, font);
                assert_settles(&mut app, &policy, &mut state);
                let Some(connected) = app.reference_input_metrics() else {
                    continue;
                };
                bash_command_fills_screen(&mut app, &policy, &mut state);
                assert_settles(&mut app, &policy, &mut state);
                let filled = app
                    .reference_input_metrics()
                    .expect("one-row bash prompt still docks");
                assert_eq!(
                    filled.input_height, connected.input_height,
                    "prompt position must not change the input height ({height}px, {font}pt)"
                );
                // Typing at the bottom prompt redraws only the edited cells.
                app.sessions[0].terminal.process_input(b"ls");
                app.sessions[0].refresh(&policy, &mut state);
                assert!(!resize_pass(&mut app, &policy, &mut state));
            }
        }
    }
    // A quick command keeps the input strip from Enter to its next prompt,
    // including a shell that writes D and the next A/B in separate reads.
    for chat in [false, true] {
        for height in [320.0, 480.0, 683.0, 704.0, 800.0] {
            for font in [10.0, 13.0, 24.0] {
                let (mut app, policy, mut state) = bash_fixture(chat, height, font);
                assert_settles(&mut app, &policy, &mut state);
                let Some(docked) = app.reference_input_metrics() else {
                    continue;
                };
                let dimensions = app.sessions[0].terminal.get_dimensions();
                let rows = dimensions.1;
                let step =
                    |app: &mut Frost, state: &mut ProjectionViewState, bytes: &[u8], what: &str| {
                        app.sessions[0].terminal.process_input(bytes);
                        app.sessions[0].refresh(&policy, state);
                        assert!(
                            !resize_pass(app, &policy, state),
                            "{what} resized the PTY ({height}px, {font}pt)"
                        );
                        assert_eq!(app.sessions[0].terminal.get_dimensions(), dimensions);
                        assert_eq!(
                            app.reference_input_metrics().map(|m| m.input_height),
                            Some(docked.input_height),
                            "{what} changed the input height"
                        );
                    };
                step(&mut app, &mut state, b"ls -la", "typing");
                step(&mut app, &mut state, b"\r\n", "accepted Enter");
                step(&mut app, &mut state, b"\x1b]133;C\x07", "command start");
                let expiry = app
                    .pending_expiry
                    .take()
                    .expect("command start arms the hold");
                for line in 0..rows * 2 {
                    step(
                        &mut app,
                        &mut state,
                        format!("drwxr-xr-x {line}\r\n").as_bytes(),
                        "output",
                    );
                }
                step(
                    &mut app,
                    &mut state,
                    b"\x1b]133;D;0\x07\x1b]7;file://ubuntu/home/ubuntu\x07",
                    "D before A",
                );
                step(&mut app, &mut state, BASH_PROMPT, "next prompt");
                // The quick command's timer fires after its prompt docked.
                app.expire_input_dock_hold(expiry);
                assert!(!resize_pass(&mut app, &policy, &mut state));
                assert!(app.reference_input_metrics().is_some());

                // A long command collapses the input once its hold expires.
                step(
                    &mut app,
                    &mut state,
                    b"sleep 10\r\n\x1b]133;C\x07",
                    "command start",
                );
                let expiry = app.pending_expiry.take().unwrap();
                app.expire_input_dock_hold(expiry);
                assert!(
                    resize_pass(&mut app, &policy, &mut state),
                    "expiry returns the rows"
                );
                assert!(app.reference_input_metrics().is_none());
                app.sessions[0]
                    .terminal
                    .process_input(b"\x1b]133;D;0\x07\x1b]7;file://ubuntu/home/ubuntu\x07");
                app.sessions[0].terminal.process_input(BASH_PROMPT);
                app.sessions[0].refresh(&policy, &mut state);
                assert_settles(&mut app, &policy, &mut state);
                assert_eq!(
                    app.reference_input_metrics().map(|m| m.input_height),
                    Some(docked.input_height)
                );

                // A fullscreen application gets the pane immediately.
                let docked_rows = app.sessions[0].terminal.get_dimensions().1;
                app.sessions[0]
                    .terminal
                    .process_input(b"vim\r\n\x1b]133;C\x07\x1b[?1049h\x1b[H\x1b[2J");
                app.sessions[0].refresh(&policy, &mut state);
                assert!(resize_pass(&mut app, &policy, &mut state));
                assert!(app.reference_input_metrics().is_none());
                assert!(app.sessions[0].terminal.get_dimensions().1 > docked_rows);
            }
        }
    }
    println!("PASS: SSH prompt, multiline edits, resize convergence, Block/Chat, short windows and font zoom");
    println!(
        "PASS: a quick command keeps the input strip and PTY size from Enter to its next prompt"
    );
    println!("PASS: one-row bash prompt keeps one input height mid-screen and on the last PTY row");
}
