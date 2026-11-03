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
        },
        policy,
        state,
    )
}

fn resize_pass(
    app: &mut Frost,
    policy: &ProjectionPolicy,
    state: &mut ProjectionViewState,
) -> bool {
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
    println!("PASS: SSH prompt, multiline edits, resize convergence, Block/Chat, short windows and font zoom");
}
