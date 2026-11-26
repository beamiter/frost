//! Frozen pre-extraction Frost RIS body.
use super::*;
use jterm_engine::screen::{default_tab_stops, TabStopOrigin};
use jterm_engine::StyleFlags;
impl TerminalState {
    fn frozen_ris(&mut self) {
        self.projection_scroll_guard.revoke();
        self.watch_batch.boundary = true;
        self.watch_batch.interrupted = true;
        if self.vt.screen.use_alt_buffer {
            self.leave_alternate_screen(1049);
        }
        self.record_reset_interrupted_agent_command();
        self.vt.screen.current_fg = Color::Default;
        self.vt.screen.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.vt.screen.current_flags = StyleFlags::default();
        self.vt.screen.g0_charset = Charset::Ascii;
        self.vt.screen.g1_charset = Charset::Ascii;
        self.vt.screen.active_charset = Charset::Ascii;
        self.vt.screen.scroll_region_top = 0;
        self.vt.screen.scroll_region_bottom = self.vt.screen.grid.rows().saturating_sub(1);
        self.vt.screen.tab_stops = default_tab_stops(self.vt.screen.grid.row_len(), TabStopOrigin::Clear);
        self.vt.screen.saved_state = None;
        let mut modes = TerminalModes::default();
        modes.insert(25);
        modes.insert(7);
        *self.vt.protocol.modes_mut() = modes;
        self.vt.screen.insert_mode = false;
        self.vt.screen.origin_mode = false;
        self.scroll_offset = 0;
        self.vt.screen.pending_wrap = false;
        // xterm RIS also discards saved lines and resets cursor style, dynamic
        // colors, keyboard-protocol state, selection, and any open hyperlink.
        if !self.scrollback.is_empty() {
            self.bump_history_revision();
        }
        self.scrollback.clear();
        self.pending_saved_line_purge = 0;
        self.projected_saved_lines_held = false;
        self.provisional_alt_snapshot = None;
        if self
            .command_zones
            .iter()
            .any(|zone| zone.output_source.is_some())
        {
            self.mark_completed_history_changed();
        }
        self.command_zones.clear();
        self.pending_zone_row_trim = 0;
        self.finished_output_provenance.clear();
        self.captured_output_bytes = 0;
        // RIS destroys the rows the undo stash points at; it cannot rebuild
        // blocks whose text is gone.
        self.cleared_blocks = None;
        self.current_zone_state = ZoneState::default();
        self.inline_cli_command_scope = None;
        self.inline_cli_launch_floor = self.inline_cli_launch_floor.map(|_| 0);
        self.chat_native_takeover = false;
        self.current_command_start_col = None;
        self.current_command_extent_row = None;
        self.current_output_start_col = None;
        self.current_output_start_row_id = None;
        self.current_output_extent_row = None;
        self.current_output_extent_col = None;
        self.current_output_extent_row_id = None;
        self.agent_prompt_input_tainted = false;
        self.prompt_submission_pending = false;
        self.prompt_cancel_pending = false;
        self.idle_prompt_input_dirty = false;
        self.pending_prompt_typeahead = false;
        self.idle_background_output = None;
        self.armed_agent_execution = None;
        self.active_agent_execution = None;
        self.current_command_text = None;
        self.current_command_exact = false;
        self.current_command_id = None;
        self.current_command_start_id = None;
        self.current_command_start_lifecycle = None;
        self.running_output_source = None;
        self.current_command_started_at = None;
        self.last_archived_screen_snapshot.clear();
        self.last_synced_primary_screen_snapshot.clear();
        self.vt.screen.cursor_shape = CursorShape::default();
        self.colors.reset_dynamic();
        // RIS clears both screen-local Kitty stacks. Frost historically
        // retains the global xterm keyboard settings across this reset.
        self.vt.protocol.reset_keyboard_enhancement();
        self.vt.protocol.switch_screen();
        self.vt.protocol.reset_keyboard_enhancement();
        self.vt.protocol.switch_screen();
        self.clear_text_selection();
        self.vt.screen.current_hyperlink = HyperlinkId::NONE;
        self.osc8.clear();
        // RIS resets the graphics namespace with the text screen: neither a
        // visible placement nor a half-finished upload may survive it. The
        // parser-side state of an in-flight transfer goes with it.
        self.kitty_graphics.clear();
        self.vt.parser.reset();
        self.clear_screen();
    }
}

#[test]
fn ris_matches_frozen_host_reset() {
    for seed in 0..16 {
        let make = || {
            let mut t = TerminalState::new(7, 3);
            t.process_input(b"old\r\nhistory\r\nmore\r\nlast\x1b[31;44m\x1b[2;3r\x1b[4h\x1b[?6h\x1b[>4;2m\x1b[=17u");
            if seed & 1 != 0 {
                t.process_input(b"\x1b[?1049hALT");
            }
            if seed & 2 != 0 {
                t.process_input(b"\x1b[?2026h");
            }
            if seed & 4 != 0 {
                t.process_input(b"\x1b]8;;https://example.test\x1b\\link");
            }
            if seed & 8 != 0 {
                t.vt.screen.pending_wrap = true;
            }
            t
        };
        let mut actual = make();
        let mut reference = make();
        actual.full_reset();
        reference.frozen_ris();
        assert_eq!(
            parser_oracle_tests::snapshot(&mut actual),
            parser_oracle_tests::snapshot(&mut reference),
            "seed={seed}"
        );
        assert_eq!(
            format!("{:?}", actual.vt.protocol),
            format!("{:?}", reference.vt.protocol)
        );
    }
}
