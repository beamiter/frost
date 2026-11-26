//! Pre-convergence Frost CSI dispatcher and transaction loops.
use super::*;
use std::cell::RefCell;
impl TerminalState {
    fn frozen_csi_dispatch(
        &mut self,
        params: &[u16],
        raw_params: &[u8],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        let first = params.first().copied();
        if private_prefix == Some(b'>') && intermediates.is_empty() && first == Some(4) {
            if matches!(cmd, 'f' | 'm') {
                observe(self, "modifier");
            }
            match cmd {
                'f' => {
                    crate::debug_log!(
                        "[XTFMTKEYS] formatOtherKeys={} previous={}",
                        params.get(1).copied().unwrap_or(0),
                        self.vt.protocol.xterm_format_other_keys()
                    );
                }
                'm' => {
                    crate::debug_log!(
                        "[XTMODKEYS] modifyOtherKeys={} previous={}",
                        params.get(1).copied().unwrap_or(0),
                        self.vt.protocol.xterm_modify_other_keys()
                    );
                }
                _ => {}
            }
        }
        if cmd.is_ascii() {
            use jterm_engine::semantics::{dispatch_frost_legacy_csi, Dispatch, Effect};
            if let Dispatch::Handled(effects) = dispatch_frost_legacy_csi(
                &mut self.vt.screen,
                &mut self.vt.protocol,
                raw_params,
                params,
                cmd as u8,
                private_prefix,
                intermediates,
            ) {
                for effect in effects {
                    observe(self, "local");
                    match effect {
                        Effect::Reply(bytes) => {
                            if cmd == 'c' {
                                crate::debug_log!(
                                    "[DA] device attributes request prefix={private_prefix:?}"
                                );
                            }
                            self.output_buffer.extend(bytes);
                        }
                        Effect::GlobalBackground(color) => self.global_bg = color,
                        Effect::Row(mutation) => self.mark_row_dirty(mutation.row),
                        Effect::CursorReposition { .. } | Effect::Bell => {
                            unreachable!("Frost local CSI produced an unexpected effect")
                        }
                    }
                }
                return;
            }
        }
        if let Some(operation) = jterm_engine::display_erase::prepare_display_erase(
            jterm_engine::display_erase::DisplayEraseProfile::FrostLegacy,
            &self.vt.screen,
            params,
            cmd,
            private_prefix,
            intermediates,
        ) {
            self.frozen_csi_display_erase(operation);
            return;
        }
        if let Some(operation) = jterm_engine::region_ops::prepare_region_operation(
            jterm_engine::region_ops::RegionOpsProfile::FrostLegacy,
            &self.vt.screen,
            params,
            cmd,
        ) {
            self.frozen_csi_region_operation(operation);
            return;
        }
        if let Some(command) = jterm_engine::mode_mutation::prepare_native_mode_command(
            jterm_engine::mode_mutation::NativeModeProfile::FrostLegacy,
            params,
            cmd,
            private_prefix,
        ) {
            self.frozen_csi_mode_command(command);
            return;
        }
        if let Some(mut repeat) =
            jterm_engine::graphic_write::RepeatGraphics::prepare(&self.vt.screen, params, cmd)
        {
            while let Some(operation) = repeat.next(
                jterm_engine::graphic_write::GraphicProfile::FrostLegacy,
                &self.vt.screen,
                self.vt.protocol.modes().contains(&7),
                crate::char_width::cached_char_width,
            ) {
                self.frozen_csi_graphic_write(operation);
            }
            return;
        }
        match cmd {
            't' => {
                if private_prefix.is_none() && intermediates.is_empty() {
                    observe(self, "window");
                    self.handle_window_ops(params);
                }
            }
            'n' if private_prefix == Some(b'?')
                && intermediates.is_empty()
                && first == Some(996) =>
            {
                // The actual default background is a host paint property.
                let scheme = if self.default_background_is_dark() {
                    1
                } else {
                    2
                };
                let response = format!("\x1b[?997;{scheme}n");
                observe(self, "reply");
                self.output_buffer.extend(response.as_bytes());
            }
            'q' if private_prefix == Some(b'>')
                && intermediates.is_empty()
                && first.unwrap_or(0) == 0 =>
            {
                observe(self, "version");
                crate::debug_log!("[XTVERSION] report terminal version request");
                self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
            }
            _ => {}
        }
    }
    fn frozen_csi_region_operation(
        &mut self,
        mut operation: jterm_engine::region_ops::RegionOperation,
    ) {
        use jterm_engine::region_ops::RegionStep;
        loop {
            let allow_alt_archive = self.vt.sync_output.active && !self.inline_cli_owner_active();
            let Some(step) = operation.step(
                &mut self.vt.screen,
                allow_alt_archive,
                RawRowId::allocate_process_unique,
            ) else {
                break;
            };
            observe(self, "region");
            match step {
                RegionStep::Scroll {
                    mutation,
                    frost_capture,
                    allow_alt_scrollback,
                    ..
                } => {
                    self.mark_rows_dirty(mutation.top, mutation.bottom);
                    if let Some(line) = frost_capture {
                        self.push_scrollback_compressed_with_options(line, allow_alt_scrollback);
                        if !self.vt.screen.use_alt_buffer
                            && mutation.bottom + 1 == self.vt.screen.grid.rows()
                        {
                            self.full_screen_scroll_revision =
                                self.full_screen_scroll_revision.wrapping_add(1);
                        }
                    }
                }
                RegionStep::Lines { dirty, .. } => {
                    self.mark_rows_dirty(*dirty.start(), *dirty.end())
                }
            }
        }
    }
    fn frozen_csi_display_erase(&mut self, operation: jterm_engine::display_erase::DisplayErase) {
        use jterm_engine::display_erase::{DisplayErase, DisplayEraseStep};
        match operation {
            DisplayErase::Rows(mut operation) => {
                while let Some(step) = operation.step(&mut self.vt.screen) {
                    observe(self, "erase");
                    match step {
                        DisplayEraseStep::ScreenErased => self.inline_cli_screen_erased(),
                        DisplayEraseStep::Mutations(effects) => {
                            for effect in effects {
                                self.mark_row_dirty(effect.row);
                            }
                        }
                        DisplayEraseStep::DirtyRows(rows) => {
                            self.mark_rows_dirty(*rows.start(), *rows.end())
                        }
                    }
                }
            }
            DisplayErase::Screen => {
                observe(self, "erase-screen");
                self.clear_screen_no_home()
            }
            DisplayErase::SavedLines => {
                observe(self, "erase-saved");
                self.erase_saved_lines()
            }
            DisplayErase::Ignored => {}
        }
    }
    fn frozen_csi_mode_command(
        &mut self,
        mut command: jterm_engine::mode_mutation::NativeModeCommand<'_>,
    ) {
        while let Some(effect) = command.step(&mut self.vt.screen, &mut self.vt.protocol, false) {
            observe(self, "mode");
            self.apply_mode_effect(effect);
            #[cfg(test)]
            mode_command_oracles::observe(self);
        }
    }
    fn frozen_csi_graphic_write(
        &mut self,
        mut operation: jterm_engine::graphic_write::GraphicWrite,
    ) {
        use jterm_engine::graphic_write::GraphicStep;
        while let Some(effect) = operation.step(&mut self.vt.screen) {
            observe(self, "graphic");
            match effect {
                GraphicStep::Wrap => {
                    #[cfg(test)]
                    graphic_write_oracles::observe(self, "wrap");
                    self.move_vertical(VerticalMotion::Wrap);
                }
                GraphicStep::ObserveIdle { columns, nonblank } => {
                    #[cfg(test)]
                    graphic_write_oracles::observe(self, "idle");
                    self.note_idle_background_cells(columns.start, columns.end, nonblank);
                }
                GraphicStep::Composed(mutation) => {
                    #[cfg(test)]
                    graphic_write_oracles::observe(self, "compose");
                    self.mark_row_dirty(mutation.row);
                }
                GraphicStep::Written { mutation, columns } => {
                    #[cfg(test)]
                    graphic_write_oracles::observe(self, "write");
                    self.mark_row_dirty(mutation.row);
                    self.mark_command_echo_extent(columns.start, columns.end);
                }
            }
        }
    }
}

type Snapshot = std::collections::BTreeMap<String, serde_json::Value>;
thread_local! { static TRACE: RefCell<Option<Vec<(String,Snapshot)>>> = const { RefCell::new(None) }; }
pub(super) fn observe(t: &mut TerminalState, event: &str) {
    if TRACE.with(|trace| trace.borrow().is_none()) {
        return;
    }
    let snapshot = parser_oracle_tests::snapshot(t);
    TRACE.with(|trace| {
        trace
            .borrow_mut()
            .as_mut()
            .unwrap()
            .push((event.to_owned(), snapshot))
    });
}
fn capture(f: impl FnOnce()) -> Vec<(String, Snapshot)> {
    TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    f();
    TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
fn seed(cols: usize, bits: usize) -> TerminalState {
    let mut t = TerminalState::new(cols, 3);
    t.process_input(b"old\r\nhistory\r\nmore\r\nlast");
    if bits & 1 != 0 {
        t.enter_alternate_screen(1049);
    }
    t.vt.screen.cursor_row = 2;
    t.vt.screen.cursor_col = cols - 1;
    t.vt.screen.pending_wrap = bits & 2 != 0;
    t.vt.screen.insert_mode = bits & 4 != 0;
    t.vt.screen.scroll_region_top = 1;
    t.vt.screen.scroll_region_bottom = 2;
    t.vt.screen.last_printed_char = Some('q');
    t.viewport_pixel_width = 71;
    t.viewport_pixel_height = 41;
    t.titles.window_title = "window".into();
    t.titles.icon_title = "icon".into();
    t
}
#[test]
fn shared_frost_csi_execution_matches_frozen_mixed_state_and_callback_order() {
    let cases: &[(&[u16], &[u8], char)] = &[
        (&[5], b"", 'n'),
        (&[6], b"", 'n'),
        (&[31, 44], b"31;44", 'm'),
        (&[0], b"", 'J'),
        (&[1], b"", 'J'),
        (&[2], b"", 'S'),
        (&[2], b"", 'T'),
        (&[2], b"", 'L'),
        (&[1], b"", 'M'),
        (&[3], b"", 'b'),
        (&[6, 1049, 2026], b"", 'h'),
        (&[1049, 6], b"", 'l'),
        (&[4, 2], b"", 'm'),
        (&[4, 1], b"", 'f'),
        (&[22, 0], b"", 't'),
        (&[18], b"", 't'),
        (&[23, 0], b"", 't'),
        (&[996], b"", 'n'),
        (&[0], b"", 'q'),
        (&[2026], b"", 'l'),
        (&[2], b"", 'J'),
        (&[3], b"", 'J'),
        (&[1, 2], b"", 'c'),
    ];
    for cols in [1, 2, 7] {
        for bits in 0..8 {
            for prefix in [None, Some(b'?'), Some(b'>'), Some(b'=')] {
                for intermediate in [&b""[..], b" ", b"!"] {
                    let mut a = seed(cols, bits);
                    let mut e = seed(cols, bits);
                    for &(params, raw, cmd) in cases {
                        let actual = capture(|| {
                            a.handle_escape_sequence(params, raw, cmd, prefix, intermediate)
                        });
                        let expected = capture(|| {
                            e.frozen_csi_dispatch(params, raw, cmd, prefix, intermediate)
                        });
                        assert_eq!(actual,expected,"cols={cols} bits={bits} cmd={cmd} params={params:?} prefix={prefix:?} intermediates={intermediate:?}");
                        assert_eq!(
                            parser_oracle_tests::snapshot(&mut a),
                            parser_oracle_tests::snapshot(&mut e)
                        );
                    }
                }
            }
        }
    }
}
