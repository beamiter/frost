//! Real production parser/planner/selection methods without the GUI test target.
#![allow(dead_code)]
pub(crate) use jterm_core::char_width;
#[path = "../src/ansi.rs"]
mod ansi;
#[path = "../src/block_mode.rs"]
mod block_mode;
#[path = "../src/chat_mode.rs"]
mod chat_mode;
#[path = "../src/debug.rs"]
mod debug;
#[path = "../src/fold_preview.rs"]
mod fold_preview;
#[path = "../src/immersive_command.rs"]
mod immersive_command;
#[path = "../src/kitty_graphics.rs"]
mod kitty_graphics;
#[path = "../src/link.rs"]
mod link;
#[path = "../src/organism_activity.rs"]
mod organism_activity;
#[path = "../src/organism_watch.rs"]
mod organism_watch;
#[path = "../src/review_text.rs"]
mod review_text;
#[path = "../src/search.rs"]
mod search;
#[path = "../src/terminal.rs"]
mod terminal;
use terminal::{ProjectionPolicy, ProjectionViewState, SelectionMode, TerminalState};

fn main() {
    for bubbles in [false, true] {
        let mut terminal = TerminalState::new(80, 24);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hello\r\n\x1b]133;C\x07hello world\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07draft");
        let mut policy = ProjectionPolicy::new();
        policy.set_chat(true);
        policy.set_native_chrome(false, 3);
        policy.set_command_bubbles(bubbles);
        let mut state = ProjectionViewState::new();
        let view = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        for (needle, expected) in [
            ("$ echo hello", "echo"),
            ("hello world", "hello"),
            ("$ draft", "draft"),
        ] {
            let (row, col) = view
                .cells()
                .iter()
                .enumerate()
                .find_map(|(row, cells)| {
                    let text: String = cells.iter().map(|cell| cell.character).collect();
                    text.find(needle)
                        .map(|start| (row, start + needle.find(expected).unwrap()))
                })
                .expect("real command, output and draft must be visible");
            terminal.start_selection_in_projection(&view, (row, col), SelectionMode::Normal);
            terminal.update_selection_in_projection(&view, (row, col + expected.len() - 1));
            assert_eq!(
                terminal.copy_selection().as_deref(),
                Some(expected),
                "drag, bubbles={bubbles}"
            );
            terminal.select_word_in_projection(&view, row, col + 1);
            assert_eq!(
                terminal.copy_selection().as_deref(),
                Some(expected),
                "word, bubbles={bubbles}"
            );
            terminal.select_line_in_projection(&view, row);
            assert!(
                terminal.copy_selection().unwrap().contains(needle),
                "line, bubbles={bubbles}"
            );
        }
        terminal.process_input(b"\x1b[?1049h");
        terminal.update_selection_in_projection(&view, (0, 0));
        assert!(
            terminal.copy_selection().is_none(),
            "stale source must remain rejected"
        );
    }
    println!("PASS: real parser, Block/Chat layouts, command/output/draft drag/word/line selection and stale-source rejection");
}
