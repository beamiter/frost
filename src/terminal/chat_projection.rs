//! Chat is a projection of native cells, never an input recorder or executor.
use super::*;
use crate::chat_mode::{resolve_surface, ChatRole, ChatRow, ChatSessionState, ChatSurface};

// The compact shared composer reserves two real editable rows initially.
// Occupied/multiline input still grows from actual cells and cursor extent.
const CHAT_MIN_INPUT_ROWS: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChatPlanKey {
    command_bubbles: bool,
    prompt: Option<usize>,
    keep_end: usize,
    companion_rows: usize,
    running_command: Option<(usize, usize)>,
    // Right alignment depends on command cell widths. A primary-screen app
    // can CUP into an earlier command and return within one parser batch, so
    // lifecycle/row identity alone cannot validate that cached geometry.
    live_command_versions: Vec<(RawRowId, u64)>,
}

impl TerminalState {
    /// Current parser authority, deliberately independent of the agent's
    /// empty-input gate: a real shell prompt may contain a multiline draft.
    pub(crate) fn chat_session_state(&self) -> ChatSessionState {
        let prompt = matches!(self.current_zone_state, ZoneState::CommandStarted(_, _));
        let running = matches!(self.current_zone_state, ZoneState::OutputStarted(_, _, _));
        ChatSessionState {
            shell_integration: prompt || running,
            prompt_ready: prompt && !self.prompt_submission_pending && !self.prompt_cancel_pending,
            command_running: running,
            alternate_screen: self.use_alt_buffer,
        }
    }

    pub(crate) fn chat_surface(&self) -> ChatSurface {
        resolve_surface(self.chat_session_state())
    }

    /// Enhanced keyboard protocols encode Enter without a literal CR. Reuse
    /// the existing pending bit after the native write is accepted so a
    /// second button event cannot submit the same prompt again before OSC C.
    pub(crate) fn note_chat_submit(&mut self) {
        if matches!(self.current_zone_state, ZoneState::CommandStarted(_, _)) {
            self.prompt_submission_pending = true;
        }
    }

    /// Observe each parsed control before a later control in the same read can
    /// undo it. This lifecycle-only latch contains no input or command bytes.
    pub(super) fn note_chat_native_takeover(&mut self) {
        if self.use_alt_buffer || self.chat_native_takeover {
            return;
        }
        if let ZoneState::OutputStarted(_, _, output_start) = self.current_zone_state {
            self.chat_native_takeover = self.is_mouse_enabled()
                || !self.kitty_graphics.get_placements().is_empty()
                || self.scrollback.len() + self.cursor_row
                    < output_start.saturating_sub(self.pending_zone_row_trim);
        }
    }

    pub(super) fn chat_plan_key(&self, policy: &ProjectionPolicy) -> Option<ChatPlanKey> {
        if !policy.chat || self.use_alt_buffer {
            return None;
        }
        let surface = self.chat_surface();
        // Paint authority is broader than input authority. A shell may split
        // D, A and B across reads, and accepted Enter precedes C. Keep its
        // proven transcript/composer geometry through those ordinary gaps;
        // chat_surface still independently disables Submit and native input
        // affordances until their real parser boundary arrives.
        let completed_start = matches!(self.current_zone_state, ZoneState::Idle)
            .then(|| self.command_zones.back())
            .flatten()
            .filter(|zone| {
                !zone.rows_evicted
                    && zone.start_mark_seen
                    && zone.completion_provenance
                        == crate::block_mode::CompletionProvenance::ShellReported
            })
            .map(|zone| zone.prompt_start);
        if !matches!(surface, ChatSurface::Prompt | ChatSurface::Running)
            && self.live_prompt_row().is_none()
            && completed_start.is_none()
        {
            return None;
        }
        // Primary-screen TUIs also own the native geometry. Their mouse/image
        // surfaces and cursor-addressed rewrites must not inherit Chat chrome.
        if self.chat_native_takeover
            || self.is_mouse_enabled()
            || !self.kitty_graphics.get_placements().is_empty()
        {
            return None;
        }
        if let ZoneState::OutputStarted(_, _, output_start) = self.current_zone_state {
            if self.scrollback.len() + self.cursor_row < output_start {
                return None;
            }
        }
        let total = self.scrollback.len() + self.grid.rows();
        let prompt = self.live_prompt_row();
        if prompt.is_some_and(|start| {
            self.raw_row_id_at_absolute(start)
                .is_none_or(|row| !self.spacing_prompt_rows.contains(&row))
        }) {
            return None;
        }

        let keep_end = if let Some(start) = prompt
            .or_else(|| self.running_zone_start())
            .or(completed_start)
        {
            let occupied = self
                .grid
                .iter()
                .enumerate()
                .rev()
                .find(|(row, cells)| {
                    self.grid.row_wrapped[*row]
                        || cells.iter().any(|cell| *cell != TerminalCell::default())
                })
                .map_or(self.scrollback.len(), |(row, _)| {
                    self.scrollback.len() + row + 1
                });
            occupied
                .max(self.scrollback.len() + self.cursor_row + 1)
                .max(self.active_app_extent_row().map_or(0, |row| row + 1))
                .max(start.saturating_add(CHAT_MIN_INPUT_ROWS))
                .min(total)
        } else {
            total
        };
        // A prompt taller than the viewport must retain the native viewport
        // and cursor scrolling rather than hiding rows in a tiny composer.
        if prompt.is_some_and(|start| keep_end.saturating_sub(start) + 2 >= self.grid.rows()) {
            return None;
        }
        let companion_rows = if let Some(start) = prompt {
            policy.organism_rows.min(
                self.grid
                    .rows()
                    .saturating_sub(keep_end.saturating_sub(start) + 3),
            )
        } else {
            0
        };
        let running_command = match self.current_zone_state {
            ZoneState::OutputStarted(start, _, end)
                if start < end
                    && self.current_output_start_col == Some(0)
                    && self
                        .raw_row_id_at_absolute(start)
                        .is_some_and(|row| self.spacing_prompt_rows.contains(&row)) =>
            {
                Some((start, end))
            }
            _ => None,
        };
        Some(ChatPlanKey {
            command_bubbles: policy.command_bubbles,
            prompt,
            keep_end,
            companion_rows,
            running_command,
            live_command_versions: (0..self.grid.rows())
                .filter_map(|row| {
                    let absolute = self.scrollback.len() + row;
                    if running_command.is_some_and(|(start, end)| (start..end).contains(&absolute))
                    {
                        return Some((self.grid.row_ids[row], self.row_versions[row]));
                    }
                    let index = self
                        .command_zones
                        .partition_point(|zone| zone.prompt_start <= absolute)
                        .checked_sub(1)?;
                    let zone = &self.command_zones[index];
                    (zone.command.is_some() && zone.output_start.is_some_and(|end| absolute < end))
                        .then_some((self.grid.row_ids[row], self.row_versions[row]))
                })
                .collect(),
        })
    }

    pub(super) fn chat_key_is_current(&self, key: &ChatPlanKey) -> bool {
        let mut policy = ProjectionPolicy::new();
        policy.chat = true;
        policy.organism_rows = key.companion_rows;
        self.chat_plan_key(&policy).as_ref() == Some(key)
    }

    /// Only default terminal tail cells may be moved out of a short command's
    /// capsule. Colored/styled blanks, wide continuations and links remain.
    fn chat_row_content_end(&self, absolute: usize) -> usize {
        let content_end = |cells: &[TerminalCell]| {
            cells
                .iter()
                .rposition(|cell| *cell != TerminalCell::default())
                .map_or(0, |col| col + 1)
        };
        if absolute < self.scrollback.len() {
            content_end(&self.scrollback[absolute].decompress())
        } else {
            self.grid
                .iter()
                .nth(absolute - self.scrollback.len())
                .map_or(0, content_end)
        }
    }

    pub(super) fn chat_projection_plan(
        &self,
        mut plan: ProjectionPlan,
        key: &ChatPlanKey,
        revision: u64,
    ) -> ProjectionPlan {
        let cols = plan.cols;
        let total = self.scrollback.len() + self.grid.rows();
        let active_start = self.running_zone_start().or(key.prompt).unwrap_or(total);
        let zones: Vec<_> = self
            .command_zones
            .iter()
            .filter(|zone| !zone.rows_evicted)
            .collect();
        let mut rows = Vec::with_capacity(plan.rows.len() + zones.len() * 2 + 8);
        let mut roles = Vec::with_capacity(rows.capacity());
        let mut source = std::mem::take(&mut plan.rows).into_iter().peekable();
        let mut next_group = 1;
        let mut dock_start = None;

        // Unclassified pre-integration data remains native and full width.
        while let Some(row) = source.peek() {
            let absolute = row
                .row_source
                .map(|s| s.raw_absolute_row)
                .or_else(|| row.raw_slices.first().map(|s| s.source.absolute_row))
                .unwrap_or(0);
            let zone_index = zones
                .partition_point(|zone| zone.prompt_start <= absolute)
                .checked_sub(1);
            let (role, start, end) = if absolute >= active_start {
                if let Some((start, end)) = key.running_command.filter(|(_, end)| absolute < *end) {
                    (ChatRole::Command, start, end)
                } else {
                    (
                        if key.prompt.is_some() {
                            ChatRole::Composer
                        } else {
                            ChatRole::Output
                        },
                        key.running_command.map_or(active_start, |(_, end)| end),
                        key.keep_end,
                    )
                }
            } else if let Some(index) = zone_index {
                let zone = zones[index];
                let end = zones
                    .get(index + 1)
                    .map_or(active_start, |z| z.prompt_start)
                    .min(active_start);
                let command_end = zone.output_start.unwrap_or(zone.prompt_start);
                let standalone = zone.command.is_some()
                    && zone.output_start_col == 0
                    && self
                        .raw_row_id_at_absolute(zone.prompt_start)
                        .is_some_and(|id| self.spacing_prompt_rows.contains(&id));
                if standalone && absolute < command_end {
                    (ChatRole::Command, zone.prompt_start, command_end.min(end))
                } else {
                    (
                        ChatRole::Output,
                        zone.prompt_start.max(command_end.min(absolute)),
                        end,
                    )
                }
            } else {
                (
                    ChatRole::Unclassified,
                    0,
                    zones.first().map_or(active_start, |zone| zone.prompt_start),
                )
            };
            let mut group = Vec::new();
            // Only whole, unambiguously owned projected rows may be moved.
            while let Some(row) = source.peek() {
                let first = row
                    .row_source
                    .map(|s| s.raw_absolute_row)
                    .or_else(|| row.raw_slices.first().map(|s| s.source.absolute_row))
                    .unwrap_or(0);
                if first >= end || (!group.is_empty() && first < start) {
                    break;
                }
                if first >= key.keep_end {
                    break;
                }
                group.push(source.next().expect("peeked source row"));
            }
            if group.is_empty() {
                // Ambiguous, overlapping or trimmed ranges never discard real
                // rows; only a proven empty tail after the composer is omitted.
                let row = source.next().expect("peeked source row");
                if absolute < key.keep_end {
                    rows.push(row);
                    roles.push(ChatRow::default());
                }
                continue;
            }
            let next = group[0].row_source.map(|s| s.raw_row);
            if role == ChatRole::Composer {
                dock_start = Some(rows.len());
                if let Some(next) = next {
                    if key.companion_rows > 0 {
                        for line in 0..key.companion_rows {
                            rows.push(Self::chat_synthetic_row(next));
                            roles.push(ChatRow {
                                role: ChatRole::Companion,
                                group: next_group,
                                status_line: Some((line, key.companion_rows)),
                                ..ChatRow::default()
                            });
                        }
                        next_group += 1;
                    }
                    rows.push(Self::chat_synthetic_row(next));
                    roles.push(ChatRow {
                        role: ChatRole::Spacer,
                        ..ChatRow::default()
                    });
                }
            } else if !rows.is_empty() && role != ChatRole::Unclassified {
                if let Some(next) = next {
                    rows.push(Self::chat_synthetic_row(next));
                    roles.push(ChatRow {
                        role: ChatRole::Spacer,
                        ..ChatRow::default()
                    });
                }
            }
            let safe_command = role == ChatRole::Command
                && group.iter().all(|row| {
                    row.raw_slices
                        .iter()
                        .all(|s| s.source.absolute_row >= start && s.source.absolute_row < end)
                });
            let mut left = 0;
            if safe_command && key.command_bubbles {
                let mut max_end = 0;
                for row in &mut group {
                    for slice in &mut row.raw_slices {
                        let content = self.chat_row_content_end(slice.source.absolute_row);
                        slice.len = slice
                            .len
                            .min(content.saturating_sub(slice.source.col_start));
                        max_end = max_end.max(slice.view_col_start + slice.len);
                    }
                    row.raw_slices.retain(|slice| slice.len > 0);
                }
                left = crate::chat_mode::command_left(cols, max_end);
                for row in &mut group {
                    for slice in &mut row.raw_slices {
                        slice.view_col_start += left;
                    }
                }
            }
            let status_zone = (role == ChatRole::Output && absolute < active_start)
                .then(|| zone_index.map(|index| zones[index].id))
                .flatten();
            // Reserve the same host-only role header while output is still
            // running. Adding it only after OSC D moves every native output
            // row down at completion, even when no terminal cells changed.
            let role_header = role == ChatRole::Output && key.command_bubbles;
            if role_header {
                if let Some(next) = next {
                    for line in 0..2 {
                        rows.push(Self::chat_synthetic_row(next));
                        roles.push(ChatRow {
                            role: ChatRole::Output,
                            group: next_group,
                            top: line == 0,
                            role_header: line == 0,
                            ..ChatRow::default()
                        });
                    }
                }
            }
            let len = group.len();
            for (index, row) in group.into_iter().enumerate() {
                rows.push(row);
                roles.push(ChatRow {
                    role,
                    role_header: false,
                    group: next_group,
                    left_col: left,
                    top: index == 0 && !role_header,
                    bottom: index + 1 == len && role != ChatRole::Composer && status_zone.is_none(),
                    app_eligible: absolute >= active_start && role != ChatRole::Command,
                    status_line: None,
                    status_zone: None,
                });
            }
            if let (Some(zone_id), Some(next)) = (status_zone, next) {
                rows.push(Self::chat_synthetic_row(next));
                roles.push(ChatRow {
                    role: ChatRole::Output,
                    group: next_group,
                    bottom: true,
                    status_zone: Some(zone_id),
                    ..ChatRow::default()
                });
            }
            if role == ChatRole::Composer {
                if let Some(next) = next {
                    rows.push(Self::chat_synthetic_row(next));
                    roles.push(ChatRow {
                        role: ChatRole::Composer,
                        group: next_group,
                        bottom: true,
                        ..ChatRow::default()
                    });
                }
            }
            next_group += 1;
        }
        plan.rows = rows;
        plan.chat_rows = roles;
        plan.chat_dock_start = dock_start;
        plan.policy_revision = revision;
        plan.raw_slice_count = plan.rows.iter().map(|row| row.raw_slices.len()).sum();
        plan.spacer_count = plan
            .rows
            .iter()
            .filter(|row| row.kind != ProjectedRowKind::Raw)
            .count();
        plan.rebuild_raw_row_placements();
        plan
    }

    fn chat_synthetic_row(next: RawRowId) -> ProjectionPlanRow {
        ProjectionPlanRow {
            raw_slices: SmallVec::new(),
            row_source: None,
            wrapped: false,
            kind: ProjectedRowKind::BlockSpacer { next },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ProjectionPolicy {
        let mut policy = ProjectionPolicy::new();
        policy.set_chat(true);
        policy.set_native_chrome(false, 3);
        policy
    }

    fn prompt(terminal: &mut TerminalState) {
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    }

    fn command(terminal: &mut TerminalState, command: &str, output: &str) {
        prompt(terminal);
        terminal.process_input(command.as_bytes());
        terminal.process_input(b"\r\n\x1b]133;C\x07");
        terminal.process_input(output.as_bytes());
        terminal.process_input(b"\r\n\x1b]133;D;0\x07");
    }

    fn snapshot(
        terminal: &mut TerminalState,
        state: &mut ProjectionViewState,
    ) -> std::sync::Arc<ProjectedViewport> {
        terminal.get_projected_viewport_with_state(true, &policy(), state)
    }

    #[test]
    fn chat_lifecycle_uses_current_parser_authority_and_native_input() {
        let mut terminal = TerminalState::new(80, 24);
        assert_eq!(terminal.chat_surface(), ChatSurface::Passthrough);
        terminal.process_input(b"\x1b]133;B\x07");
        assert_eq!(terminal.chat_surface(), ChatSurface::Passthrough);
        prompt(&mut terminal);
        terminal.note_user_input(b"unfinished command");
        terminal.process_input(b"unfinished command");
        assert_eq!(terminal.chat_surface(), ChatSurface::Prompt);
        terminal.note_user_input(b"\r");
        assert_eq!(terminal.chat_surface(), ChatSurface::Passthrough);
        terminal.process_input(b"\r\n\x1b]133;C\x07Password: ");
        assert_eq!(terminal.chat_surface(), ChatSurface::Running);
        let command_before = terminal.current_command_text.clone();
        terminal.note_user_input(b"private input\r");
        assert_eq!(terminal.current_command_text, command_before);
        assert!(!terminal.chat_surface().allows_submit_affordance());
        terminal.process_input(b"\x1b[?1049h");
        assert_eq!(terminal.chat_surface(), ChatSurface::Fullscreen);
        terminal.process_input(b"\x1b[?1049l\x1b]133;D;0\x07");
        assert_eq!(terminal.chat_surface(), ChatSurface::Passthrough);
        prompt(&mut terminal);
        assert_eq!(terminal.chat_surface(), ChatSurface::Prompt);
    }

    #[test]
    fn chat_interrupt_is_native_input_and_does_not_fabricate_completion() {
        let mut terminal = TerminalState::new(80, 24);
        prompt(&mut terminal);
        terminal.process_input(b"sleep 60\r\n\x1b]133;C\x07");
        let generation = terminal.watch_generation().unwrap();
        terminal.note_user_input(b"\x03");
        assert_eq!(terminal.chat_surface(), ChatSurface::Running);
        assert_eq!(terminal.watch_generation(), Some(generation));
        assert!(terminal.command_zones.is_empty());
        terminal.process_input(b"^C\r\n\x1b]133;D;130\x07");
        prompt(&mut terminal);
        terminal.process_input(b"next\r\n\x1b]133;C\x07");
        assert_ne!(terminal.watch_generation(), Some(generation));
    }

    #[test]
    fn chat_shared_row_prompt_is_not_separated_from_real_output() {
        let mut terminal = TerminalState::new(40, 18);
        terminal.process_input(b"output prefix ");
        prompt(&mut terminal);
        terminal.process_input(b"draft");
        let view = snapshot(&mut terminal, &mut ProjectionViewState::new());
        assert!(view.chat_rows().is_empty());
        assert_eq!(view.cells(), terminal.get_visible_cells().as_ref());
    }

    #[test]
    fn chat_preserves_wide_command_and_link_origins() {
        let mut terminal = TerminalState::new(40, 20);
        command(
            &mut terminal,
            "echo 中文",
            "\x1b]8;;https://example.com\x07链接\x1b]8;;\x07",
        );
        prompt(&mut terminal);
        let view = snapshot(&mut terminal, &mut ProjectionViewState::new());
        for (row, cells) in view.cells().iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                if cell.flags.wide() {
                    assert!(cells[col + 1].flags.wide_continuation());
                    let origin = view.view_to_raw(ViewportCell { row, col }).unwrap();
                    assert_eq!(
                        view.raw_range_to_view(origin, 2),
                        Some(ViewportCell { row, col })
                    );
                }
                if cell.hyperlink != 0 {
                    assert!(view.view_to_raw(ViewportCell { row, col }).is_some());
                    assert_eq!(view.chat_rows()[row].role, ChatRole::Output);
                }
            }
        }
    }

    #[test]
    fn chat_enhanced_enter_cannot_rearm_submit_before_new_prompt() {
        let mut terminal = TerminalState::new(80, 24);
        prompt(&mut terminal);
        terminal.note_user_input(b"\x1b[13u");
        terminal.note_chat_submit();
        assert!(!terminal.chat_surface().allows_submit_affordance());
        terminal.process_input(b"\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        assert!(!terminal.chat_surface().allows_submit_affordance());
        prompt(&mut terminal);
        assert!(terminal.chat_surface().allows_submit_affordance());
    }

    #[test]
    fn block_docks_the_native_prompt_and_keeps_completed_commands_left_aligned() {
        let mut terminal = TerminalState::new(40, 18);
        command(&mut terminal, "echo hello", "hello");
        prompt(&mut terminal);
        let grid = terminal.grid.to_vec();
        let cursor = terminal.get_cursor_pos();
        let mut policy = policy();
        let mut state = ProjectionViewState::new();
        for bubbles in [false, true, false] {
            policy.set_command_bubbles(bubbles);
            let view = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
            let command = view
                .chat_rows()
                .iter()
                .find(|row| row.role == ChatRole::Command)
                .unwrap();
            assert_eq!(command.left_col > 0, bubbles);
            assert!(view
                .chat_rows()
                .iter()
                .any(|row| row.role == ChatRole::Composer));
            assert_eq!(terminal.grid.to_vec(), grid);
            assert_eq!(terminal.get_cursor_pos(), cursor);
            for (row, cells) in view.cells().iter().enumerate() {
                for (col, cell) in cells.iter().enumerate() {
                    if !cell.character.is_whitespace() {
                        assert!(view.view_to_raw(ViewportCell { row, col }).is_some());
                    }
                }
            }
        }
    }

    #[test]
    fn chat_short_command_is_right_aligned_and_output_keeps_full_width_and_origins() {
        let mut terminal = TerminalState::new(40, 18);
        command(&mut terminal, "echo hello", "hello");
        prompt(&mut terminal);
        let grid = terminal.grid.to_vec();
        let mut state = ProjectionViewState::new();
        let view = snapshot(&mut terminal, &mut state);
        let command = view
            .chat_rows()
            .iter()
            .position(|r| r.role == ChatRole::Command)
            .unwrap();
        let output_header = view
            .chat_rows()
            .iter()
            .position(|r| r.role == ChatRole::Output && r.role_header)
            .unwrap();
        assert!(
            view.view_to_raw(ViewportCell {
                row: output_header,
                col: 0,
            })
            .is_none(),
            "role header is host chrome, never native output"
        );
        let output = view
            .chat_rows()
            .iter()
            .enumerate()
            .find_map(|(row, role)| {
                (role.role == ChatRole::Output
                    && view.view_to_raw(ViewportCell { row, col: 0 }).is_some())
                .then_some(row)
            })
            .unwrap();
        assert!(view.chat_rows()[command].left_col > 15);
        assert_eq!(view.chat_rows()[output].left_col, 0);
        assert_eq!(view.cells()[output][0].character, 'h');
        assert_eq!(terminal.grid.to_vec(), grid);
        for (row, cells) in view.cells().iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                if *cell != TerminalCell::default() {
                    let origin = view
                        .view_to_raw(ViewportCell { row, col })
                        .expect("real glyph origin");
                    assert_eq!(view.raw_to_view(origin), Some(ViewportCell { row, col }));
                    let raw = terminal.retained_absolute_for_raw(origin.row).unwrap();
                    assert_eq!(
                        terminal.grid[raw - terminal.scrollback.len()][origin.col],
                        *cell
                    );
                }
            }
        }
        terminal.select_line_in_projection(&view, command);
        assert_eq!(terminal.copy_selection().unwrap().trim(), "$ echo hello");
    }

    #[test]
    fn chat_dock_is_fixed_while_history_scrolls_and_copies_native_draft() {
        let mut terminal = TerminalState::new(40, 14);
        for index in 0..12 {
            command(
                &mut terminal,
                &format!("echo {index}"),
                &format!("output {index}"),
            );
        }
        prompt(&mut terminal);
        terminal.process_input(b"draft");
        let mut state = ProjectionViewState::new();
        let live = snapshot(&mut terminal, &mut state);
        let raw = terminal
            .raw_cell_origin_at_absolute(terminal.scrollback.len() + terminal.cursor_row, 2)
            .unwrap();
        let live_cell = live.raw_to_view(raw).unwrap();
        state.scroll(5, &live);
        let history = snapshot(&mut terminal, &mut state);
        assert!(history.scroll_offset() > 0);
        assert_eq!(history.raw_to_view(raw), Some(live_cell));
        assert_eq!(
            history.view_document_row(live_cell.row),
            live.view_document_row(live_cell.row)
        );
        assert_ne!(history.document_start(), live.document_start());
        terminal.select_word_in_projection(&history, live_cell.row, live_cell.col);
        assert_eq!(terminal.copy_selection().as_deref(), Some("draft"));
        assert_eq!(terminal.grid.cols(), 40);
        assert_eq!(terminal.grid.rows(), 14);
    }

    #[test]
    fn chat_synthetic_rows_have_no_terminal_origin_or_selection() {
        let mut terminal = TerminalState::new(40, 14);
        prompt(&mut terminal);
        let view = snapshot(&mut terminal, &mut ProjectionViewState::new());
        let mut count = 0;
        for (row, kind) in view.row_kinds().iter().enumerate() {
            if !matches!(kind, ProjectedRowKind::Raw) {
                count += 1;
                for col in 0..40 {
                    assert!(view.view_to_raw(ViewportCell { row, col }).is_none());
                }
                terminal.start_selection_in_projection(&view, (row, 0), SelectionMode::Normal);
                assert!(terminal.copy_selection().is_none());
            }
        }
        assert!(count > 3);
    }

    #[test]
    fn chat_drag_across_scrolled_history_and_dock_never_copies_hidden_rows() {
        let mut terminal = TerminalState::new(40, 18);
        for index in 0..12 {
            command(
                &mut terminal,
                "echo",
                &if index == 11 {
                    "HIDDEN_SENTINEL".to_string()
                } else {
                    format!("visible {index}")
                },
            );
        }
        prompt(&mut terminal);
        terminal.process_batch(b"draft");
        let mut state = ProjectionViewState::new();
        let live = snapshot(&mut terminal, &mut state);
        state.scroll(8, &live);
        let view = snapshot(&mut terminal, &mut state);
        let history = view
            .chat_rows()
            .iter()
            .enumerate()
            .find_map(|(row, meta)| {
                (meta.role == ChatRole::Output && view.cells()[row][0].character == 'v')
                    .then_some(row)
            })
            .unwrap();
        let dock = view
            .chat_rows()
            .iter()
            .position(|meta| meta.role == ChatRole::Composer)
            .unwrap();
        assert!(!view.cells().iter().any(|row| row
            .iter()
            .map(|cell| cell.character)
            .collect::<String>()
            .contains("HIDDEN_SENTINEL")));
        for count in 1..=3 {
            for (from, to) in [((history, 0), (dock, 4)), ((dock, 2), (history, 4))] {
                match count {
                    2 => terminal.select_word_in_projection(&view, from.0, from.1),
                    3 => terminal.select_line_in_projection(&view, from.0),
                    _ => terminal.start_selection_in_projection(&view, from, SelectionMode::Normal),
                }
                let before = terminal.copy_selection();
                match count {
                    2 => terminal.extend_word_selection_in_projection(&view, to.0, to.1),
                    3 => terminal.extend_line_selection_in_projection(&view, to.0),
                    _ => terminal.update_selection_in_projection(&view, to),
                }
                let after = terminal.copy_selection();
                assert_eq!(after, before, "click count={count}, from={from:?}");
                assert!(!after.unwrap_or_default().contains("HIDDEN_SENTINEL"));
            }
        }
        terminal.start_selection_in_projection(&view, (dock, 2), SelectionMode::Normal);
        terminal.update_selection_in_projection(&view, (dock, 6));
        assert_eq!(terminal.copy_selection().as_deref(), Some("draft"));
    }

    #[test]
    fn chat_preserves_multiline_draft_completion_extent_and_styled_blanks() {
        let mut terminal = TerminalState::new(30, 20);
        prompt(&mut terminal);
        terminal.process_input(
            b"for x in one two; do\r\n> echo $x\r\ncompletion-a completion-b\x1b[1A\r",
        );
        let dimensions = (terminal.grid.cols(), terminal.grid.rows());
        let mut state = ProjectionViewState::new();
        for _ in 0..4 {
            let view = snapshot(&mut terminal, &mut state);
            let text: String = view
                .cells()
                .iter()
                .flatten()
                .map(|cell| cell.character)
                .collect();
            assert!(text.contains("completion-a"));
            assert!(text.contains("echo $x"));
            assert_eq!(dimensions, (terminal.grid.cols(), terminal.grid.rows()));
        }
        terminal.process_input(b"\x1b[15;1H\x1b[41m \x1b[0m\x1b[2;1H");
        let view = snapshot(&mut terminal, &mut state);
        assert!(view
            .cells()
            .iter()
            .flatten()
            .any(|cell| cell.background != Color::Default));
    }

    #[test]
    fn chat_initial_word_and_link_detection_do_not_cross_the_dock_seam() {
        let mut terminal = TerminalState::new(40, 24);
        for _ in 0..5 {
            command(&mut terminal, "old", "old output");
        }
        command(
            &mut terminal,
            "url",
            &format!("https://example.com/{}", "a".repeat(130)),
        );
        prompt(&mut terminal);
        terminal.process_batch(b"/current/path");
        let raw_wraps = terminal.grid.row_wrapped.clone();
        let detector = crate::link::LinkDetector::new(crate::link::LinkDetectionConfig::default());
        let mut state = ProjectionViewState::new();
        let initial = snapshot(&mut terminal, &mut state);
        let mut checked_seam = false;
        let mut checked_contiguous = false;
        for offset in 0..initial.max_scroll_offset() {
            state.set_offset(offset, &initial);
            let view = snapshot(&mut terminal, &mut state);
            let plan = terminal.projection_plan_cache.as_ref().unwrap().1.clone();
            for row in 0..view.cells().len() - 1 {
                let Some(document) = view.view_document_row(row) else {
                    continue;
                };
                if !plan.rows[document].wrapped {
                    continue;
                }
                let next = view.view_document_row(row + 1);
                let contiguous = next == Some(document + 1)
                    && matches!(view.row_kinds()[row + 1], ProjectedRowKind::Raw);
                if contiguous {
                    assert!(view.row_wrapped()[row]);
                    checked_contiguous = true;
                } else {
                    assert!(!view.row_wrapped()[row]);
                    terminal.select_word_in_projection(&view, row, 5);
                    assert!(!terminal
                        .copy_selection()
                        .unwrap_or_default()
                        .contains("/current/path"));
                    checked_seam = true;
                }
            }
            let links = detector
                .detect_links_in_visible_cells_with_wrapping(view.cells(), view.row_wrapped());
            assert!(links.iter().all(
                |link| !(link.text.contains("https://") && link.text.contains("/current/path"))
            ));
        }
        assert!(checked_seam && checked_contiguous);
        assert_eq!(terminal.grid.row_wrapped, raw_wraps);
    }

    #[test]
    fn chat_origin_span_rejects_outside_columns_before_offset_arithmetic() {
        let terminal = TerminalState::new(40, 20);
        let raw_row = terminal.raw_row_id_at_absolute(0).unwrap();
        let span = OriginSpan {
            view_row: 0,
            view_col_start: 5,
            view_col_end: 10,
            raw_row,
            raw_col_start: 10,
        };
        for col in [0, usize::MAX] {
            assert_eq!(span.view_to_raw(ViewportCell { row: 0, col }), None);
            assert_eq!(span.raw_to_view(RawCellOrigin { row: raw_row, col }), None);
        }
        let raw = RawCellOrigin {
            row: raw_row,
            col: 10,
        };
        assert_eq!(span.view_to_raw(span.raw_to_view(raw).unwrap()), Some(raw));
    }

    #[test]
    fn chat_search_maps_shifted_command_history_and_fixed_dock_through_exact_origins() {
        let mut terminal = TerminalState::new(40, 24);
        for _ in 0..12 {
            command(&mut terminal, "needle cmd", "needle output");
        }
        prompt(&mut terminal);
        terminal.process_batch(b"needle draft");
        let mut state = ProjectionViewState::new();
        let live = snapshot(&mut terminal, &mut state);
        state.scroll(8, &live);
        let view = snapshot(&mut terminal, &mut state);
        let mut roles = Vec::new();
        for (row, cells) in view.cells().iter().enumerate() {
            let line: String = cells.iter().map(|cell| cell.character).collect();
            if let Some(col) = line.find("needle") {
                let raw = view.view_to_raw(ViewportCell { row, col }).unwrap();
                let absolute = terminal.retained_absolute_for_raw(raw.row).unwrap();
                let mapped = crate::project_search_match(
                    &terminal,
                    &view,
                    &crate::search::SearchMatch {
                        line: absolute,
                        col_start: raw.col,
                        col_end: raw.col + 6,
                    },
                )
                .unwrap();
                assert_eq!(
                    (mapped.line, mapped.col_start, mapped.col_end),
                    (row, col, col + 6)
                );
                roles.push(view.chat_rows()[row].role);
            }
        }
        for role in [ChatRole::Command, ChatRole::Output, ChatRole::Composer] {
            assert!(roles.contains(&role));
        }
    }

    #[test]
    fn chat_d_only_gap_preserves_history_anchor_before_the_next_prompt() {
        let mut terminal = TerminalState::new(40, 18);
        for _ in 0..20 {
            command(&mut terminal, "echo", "history output");
        }
        prompt(&mut terminal);
        terminal.process_batch(b"run\r\n\x1b]133;C\x07live output");
        let mut state = ProjectionViewState::new();
        let live = snapshot(&mut terminal, &mut state);
        // Host role headers add display rows without adding native history.
        // Select a proven historical viewport rather than a fixed display
        // distance that can stop among the current live-grid rows.
        state.set_offset(live.max_scroll_offset(), &live);
        let history = snapshot(&mut terminal, &mut state);
        let first_raw = |view: &ProjectedViewport| {
            view.provenance
                .row_sources
                .iter()
                .flatten()
                .next()
                .unwrap()
                .raw_row
        };
        let anchor = first_raw(&history);
        assert!(terminal.retained_absolute_for_raw(anchor).unwrap() < terminal.scrollback.len(), "fixture must anchor genuine history, not a live-grid row that cannot become native viewport top");
        terminal.process_batch(b"\r\n\x1b]133;D;0\x07");
        let between = snapshot(&mut terminal, &mut state);
        assert_eq!(terminal.chat_surface(), ChatSurface::Passthrough);
        assert_eq!(between.mode(), ProjectionMode::Transformed);
        assert!(!between.chat_rows().is_empty());
        assert_eq!(
            first_raw(&between),
            anchor,
            "D-only completion must retain the same rendered transcript"
        );
        let parked_offset = terminal.scroll_offset;
        let cursor_origin = |terminal: &TerminalState| {
            let (row, col) = terminal.get_cursor_pos();
            terminal
                .raw_cell_origin_at_absolute(terminal.scrollback_len() + row, col)
                .unwrap()
        };
        assert!(between.raw_to_view(cursor_origin(&terminal)).is_none());
        assert!(!terminal.is_mapped_cursor_visible(&between, false));
        // The actual cursor is absent from this historical viewport. Its
        // visibility is controlled by the projection, without moving the
        // native terminal's independent scroll coordinate during a shell gap.
        assert!(between.scroll_offset() > 0);

        prompt(&mut terminal);
        let restored = snapshot(&mut terminal, &mut state);
        assert_eq!(first_raw(&restored), anchor);
        let mapped = restored.raw_to_view(cursor_origin(&terminal)).unwrap();
        assert_eq!(restored.chat_rows()[mapped.row].role, ChatRole::Composer);
        assert_eq!(terminal.scroll_offset, parked_offset);
        assert!(terminal.is_mapped_cursor_visible(&restored, true));
        assert!(!terminal.is_mapped_cursor_visible(&restored, false));

        for (control, visible) in [(b"\x1b[?25l".as_slice(), false), (b"\x1b[?25h", true)] {
            terminal.process_batch(control);
            let view = snapshot(&mut terminal, &mut state);
            assert_eq!(first_raw(&view), anchor);
            assert_eq!(terminal.scroll_offset, parked_offset);
            assert_eq!(view.raw_to_view(cursor_origin(&terminal)), Some(mapped));
            assert_eq!(terminal.is_mapped_cursor_visible(&view, true), visible);
            assert!(!terminal.is_mapped_cursor_visible(&view, false));
        }
    }

    #[test]
    fn chat_cursor_gate_preserves_native_block_and_alt_visibility() {
        let mut terminal = TerminalState::new(40, 18);
        for _ in 0..20 {
            command(&mut terminal, "echo", "history output");
        }
        prompt(&mut terminal);
        terminal.scroll(8);
        assert!(terminal.scroll_offset > 0);
        let normal = terminal.get_projected_viewport(false);
        assert!(normal.chat_rows().is_empty());
        assert!(!terminal.is_mapped_cursor_visible(&normal, true));
        let mut block_policy = ProjectionPolicy::new();
        block_policy.set_block_spacing(true);
        let block = terminal.get_projected_viewport_with_state(
            true,
            &block_policy,
            &mut ProjectionViewState::new(),
        );
        assert_eq!(block.mode(), ProjectionMode::Transformed);
        assert!(block.chat_rows().is_empty());
        assert!(!terminal.is_mapped_cursor_visible(&block, true));

        // Actual primary-screen takeover must not acquire the Chat exception.
        terminal.process_batch(b"cat\r\n\x1b]133;C\x07\x1b[?1000h");
        let primary = snapshot(&mut terminal, &mut ProjectionViewState::new());
        assert!(primary.chat_rows().is_empty());
        assert!(terminal.scroll_offset > 0);
        assert!(!terminal.is_mapped_cursor_visible(&primary, true));

        terminal.process_batch(b"\x1b[?1049hSCREEN");
        let alt = snapshot(&mut terminal, &mut ProjectionViewState::new());
        assert_eq!(alt.mode(), ProjectionMode::Bypass);
        assert!(alt.chat_rows().is_empty());
        assert!(terminal.is_cursor_visible());
        assert!(terminal.is_mapped_cursor_visible(&alt, true));
        assert!(!terminal.is_mapped_cursor_visible(&alt, false));
        terminal.process_batch(b"\x1b[?25l");
        assert!(!terminal.is_mapped_cursor_visible(&alt, true));
        terminal.process_batch(b"\x1b[?25h");
        assert!(terminal.is_mapped_cursor_visible(&alt, true));
    }

    #[test]
    fn chat_running_interactive_and_fullscreen_keep_native_cells() {
        let mut terminal = TerminalState::new(30, 10);
        prompt(&mut terminal);
        terminal.process_input(b"cat\r\n\x1b]133;C\x07live input");
        let mut state = ProjectionViewState::new();
        let view = snapshot(&mut terminal, &mut state);
        assert!(view
            .chat_rows()
            .iter()
            .all(|row| row.role != ChatRole::Composer));
        assert!(view
            .chat_rows()
            .iter()
            .any(|row| row.role == ChatRole::Command && row.left_col > 0 && !row.app_eligible));
        assert!(view
            .chat_rows()
            .iter()
            .any(|row| row.role == ChatRole::Output && row.left_col == 0 && row.app_eligible));
        let cursor = terminal
            .raw_cell_origin_at_absolute(
                terminal.scrollback.len() + terminal.cursor_row,
                terminal.cursor_col,
            )
            .unwrap();
        assert!(view.raw_to_view(cursor).is_some());
        terminal.process_input(b"\x1b[?1049hSCREEN");
        let alt = snapshot(&mut terminal, &mut state);
        assert_eq!(alt.mode(), ProjectionMode::Bypass);
        assert!(alt.chat_rows().is_empty());
        assert_eq!(alt.cells(), terminal.get_visible_cells().as_ref());
        terminal.process_input(b"\x1b[?1049l\r\n\x1b]133;D;0\x07");
        prompt(&mut terminal);
        assert!(snapshot(&mut terminal, &mut state)
            .chat_rows()
            .iter()
            .any(|r| r.role == ChatRole::Composer));
    }

    #[test]
    fn chat_primary_screen_mouse_and_cursor_takeover_restore_native_geometry() {
        let mut terminal = TerminalState::new(40, 20);
        command(&mut terminal, "first", "answer");
        prompt(&mut terminal);
        terminal.process_input(b"cat\r\n\x1b]133;C\x07live");
        let mut state = ProjectionViewState::new();
        assert!(!snapshot(&mut terminal, &mut state).chat_rows().is_empty());
        terminal.process_input(b"\x1b[?1000h");
        let native = snapshot(&mut terminal, &mut state);
        assert!(native.chat_rows().is_empty());
        assert_eq!(native.cells(), terminal.get_visible_cells().as_ref());
        assert!(terminal.projection_plan_cache.is_none());
        terminal.process_input(b"\x1b[?1000l");
        assert!(snapshot(&mut terminal, &mut state).chat_rows().is_empty());
        terminal.process_input(b"\x1b[1;1Hrewrite");
        let native = snapshot(&mut terminal, &mut state);
        assert!(native.chat_rows().is_empty());
        assert_eq!(native.cells(), terminal.get_visible_cells().as_ref());
        terminal.process_input(b"\x1b[6;1H\x1b]133;D;0\x07");
        prompt(&mut terminal);
        assert!(snapshot(&mut terminal, &mut state)
            .chat_rows()
            .iter()
            .any(|r| r.role == ChatRole::Composer));
    }

    #[test]
    fn chat_primary_screen_graphics_preserve_full_native_surface() {
        let mut terminal = TerminalState::new(40, 20);
        prompt(&mut terminal);
        terminal.process_input(
            b"image\r\n\x1b]133;C\x07\x1b_Gf=32,s=1,v=1,a=T,i=51,q=2;AQIDBA==\x1b\\",
        );
        assert!(!terminal.kitty_graphics.get_placements().is_empty());
        let mut state = ProjectionViewState::new();
        let native = snapshot(&mut terminal, &mut state);
        assert!(native.chat_rows().is_empty());
        assert_eq!(native.cells(), terminal.get_visible_cells().as_ref());
        terminal.process_input(b"\x1b_Ga=d,d=I,i=51,q=2\x1b\\\r\n\x1b]133;D;0\x07");
        prompt(&mut terminal);
        assert!(terminal.kitty_graphics.get_placements().is_empty());
        assert!(snapshot(&mut terminal, &mut state)
            .chat_rows()
            .iter()
            .any(|r| r.role == ChatRole::Composer));
    }

    #[test]
    fn chat_transient_takeover_is_latched_until_the_next_prompt() {
        for takeover in [
            b"\x1b[?1000h\x1b[?1000l".as_slice(),
            b"\x1b[s\x1b[1;1H\x1b[u",
            b"\x1b_Gf=32,s=1,v=1,a=T,i=51,q=2;AQIDBA==\x1b\\\x1b_Ga=d,d=I,i=51,q=2\x1b\\",
        ] {
            let mut terminal = TerminalState::new(40, 20);
            command(&mut terminal, "first", "answer");
            prompt(&mut terminal);
            terminal.process_batch(b"app\r\n\x1b]133;C\x07live");
            let mut state = ProjectionViewState::new();
            assert!(!snapshot(&mut terminal, &mut state).chat_rows().is_empty());
            terminal.process_batch(takeover);
            assert!(terminal.chat_native_takeover);
            assert!(!terminal.is_mouse_enabled());
            assert!(terminal.kitty_graphics.get_placements().is_empty());
            assert!(snapshot(&mut terminal, &mut state).chat_rows().is_empty());
            terminal.process_batch(b"more output");
            assert!(snapshot(&mut terminal, &mut state).chat_rows().is_empty());
            terminal
                .process_batch(b"\x1b]133;B\x07\x1b[?1049h\x1b]133;A\x07\x1b]133;B\x07\x1b[?1049l");
            assert!(
                terminal.chat_native_takeover,
                "stray B and alternate-screen marks have no authority"
            );
            terminal.process_batch(b"\r\n\x1b]133;D;0\x07");
            prompt(&mut terminal);
            assert!(!terminal.chat_native_takeover);
            assert!(snapshot(&mut terminal, &mut state)
                .chat_rows()
                .iter()
                .any(|r| r.role == ChatRole::Composer));
        }
    }

    #[test]
    fn chat_replans_command_width_after_same_batch_cursor_rewrite() {
        let mut terminal = TerminalState::new(40, 20);
        command(&mut terminal, "first", "answer");
        prompt(&mut terminal);
        let mut state = ProjectionViewState::new();
        let before = snapshot(&mut terminal, &mut state);
        let before_left = before
            .chat_rows()
            .iter()
            .find(|row| row.role == ChatRole::Command)
            .unwrap()
            .left_col;
        terminal.process_batch(b"\x1b[s\x1b[1;1Hrewritten long command\x1b[u");
        let after = snapshot(&mut terminal, &mut state);
        let command_row = after
            .chat_rows()
            .iter()
            .position(|row| row.role == ChatRole::Command)
            .unwrap();
        assert!(after.chat_rows()[command_row].left_col < before_left);
        let text: String = after.cells()[command_row]
            .iter()
            .map(|cell| cell.character)
            .collect();
        assert!(text.contains("rewritten long command"));
        for (col, cell) in after.cells()[command_row].iter().enumerate() {
            if cell.character != ' ' {
                let origin = after
                    .view_to_raw(ViewportCell {
                        row: command_row,
                        col,
                    })
                    .unwrap();
                assert_eq!(
                    after.raw_to_view(origin),
                    Some(ViewportCell {
                        row: command_row,
                        col
                    })
                );
            }
        }
    }

    #[test]
    fn chat_missing_boundaries_and_oversized_prompt_fail_closed() {
        let mut terminal = TerminalState::new(20, 6);
        terminal.process_input(b"unintegrated shell $ ");
        let plain = terminal.get_visible_cells();
        let view = snapshot(&mut terminal, &mut ProjectionViewState::new());
        assert!(view.chat_rows().is_empty());
        assert_eq!(view.cells(), plain.as_ref());
        prompt(&mut terminal);
        terminal.process_input(b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
        let view = snapshot(&mut terminal, &mut ProjectionViewState::new());
        assert!(view.chat_rows().is_empty());
        assert_eq!(view.cells(), terminal.get_visible_cells().as_ref());
    }

    #[test]
    fn chat_wide_output_is_not_wrapped_to_a_bubble_width() {
        let mut terminal = TerminalState::new(40, 20);
        let output = "0123456789".repeat(4);
        command(&mut terminal, "wide", &output);
        prompt(&mut terminal);
        let view = snapshot(&mut terminal, &mut ProjectionViewState::new());
        assert!(view.cells().iter().any(|row| row
            .iter()
            .map(|cell| cell.character)
            .collect::<String>()
            == output));
        assert_eq!(terminal.grid.cols(), 40);
    }
    #[derive(Debug, PartialEq, Eq)]
    struct SessionParityIdentityMismatch {
        fixture: String,
        chunking: String,
        checkpoint: usize,
        field: String,
        expected: String,
        observed: String,
    }

    fn replay_shared_session_parity_corpus(
        fixtures: Vec<jterm_core::session_parity::Fixture>,
        chunkings: &[jterm_core::session_parity::Chunking],
    ) -> Result<(), Box<SessionParityIdentityMismatch>> {
        use jterm_core::session_parity::{chunks, Completion, Expected};
        for fixture in fixtures {
            for &chunking in chunkings {
                let mut terminal = TerminalState::new(80, 24);
                let mut completed = Vec::new();
                let mut identities = std::collections::HashMap::new();
                let mut view_state = ProjectionViewState::new();
                for (step_index, step) in fixture.steps.iter().enumerate() {
                    let context = format!("{} {chunking:?} checkpoint {step_index}", fixture.id);
                    for bytes in chunks(step.pty.as_bytes(), chunking) {
                        terminal.process_input(bytes);
                    }
                    completed.extend(terminal.take_completed_commands().into_iter().map(|event| {
                        Completion {
                            id: event.id.expect("fixture supplies execution identity"),
                            command: event.command,
                            exit_code: event.exit_code,
                        }
                    }));
                    let running_id = terminal
                        .is_command_running()
                        .then(|| terminal.current_command_id.clone())
                        .flatten();
                    if let Some(id) = &running_id {
                        let generation = terminal.watch_generation().expect("running generation");
                        if let Some(previous) = identities.insert(id.clone(), generation) {
                            assert_eq!(previous, generation, "{context}: local identity changed");
                        }
                    }
                    let draft_text = step.expected.draft_text.as_ref().map(|draft| {
                        let row: String = terminal.grid.to_vec()[terminal.cursor_row]
                            .iter()
                            .filter(|cell| !cell.flags.wide_continuation())
                            .map(|cell| cell.character)
                            .collect();
                        // Ignore only native grid padding; the corpus retains the prompt space.
                        assert_eq!(
                            terminal.cursor_col,
                            draft
                                .chars()
                                .map(crate::char_width::cached_char_width)
                                .sum::<usize>(),
                            "{context}: draft cursor"
                        );
                        row.trim_end().to_owned()
                    });
                    let mut expected = step.expected.clone();
                    expected.draft_text =
                        expected.draft_text.map(|text| text.trim_end().to_owned());
                    let observed = Expected {
                        draft_text,
                        surface: format!("{:?}", terminal.chat_surface()).to_lowercase(),
                        completed: completed.clone(),
                        running_id,
                    };
                    // Keep all non-identity checks strict: an unrelated setup/state
                    // failure must never satisfy the negative golden test.
                    let mut non_identity = observed.clone();
                    for (actual, wanted) in
                        non_identity.completed.iter_mut().zip(&expected.completed)
                    {
                        actual.id.clone_from(&wanted.id);
                    }
                    assert_eq!(non_identity, expected, "{context}: non-identity fields");
                    for (index, (actual, wanted)) in observed
                        .completed
                        .iter()
                        .zip(&expected.completed)
                        .enumerate()
                    {
                        if actual.id != wanted.id {
                            return Err(Box::new(SessionParityIdentityMismatch {
                                fixture: fixture.id.clone(),
                                chunking: format!("{chunking:?}"),
                                checkpoint: step_index,
                                field: format!("completed[{index}].id"),
                                expected: wanted.id.clone(),
                                observed: actual.id.clone(),
                            }));
                        }
                    }
                    assert_eq!(observed, expected, "{context}");
                    // Project the same native model in all modes. No recorder,
                    // command execution, or shell editor is introduced by Chat.
                    let grid = terminal.grid.to_vec();
                    let cursor = (terminal.cursor_row, terminal.cursor_col);
                    let zones = format!("{:?}", terminal.command_zones);
                    let command = (
                        terminal.current_command_id.clone(),
                        terminal.current_command_text.clone(),
                    );
                    let generation = terminal.watch_generation();
                    for mode in [2, 0, 1, 2] {
                        let mut policy = ProjectionPolicy::new();
                        policy.set_native_chrome(mode == 1, 3);
                        policy.set_chat(mode == 2);
                        let _view = terminal.get_projected_viewport_with_state(
                            mode != 0,
                            &policy,
                            &mut view_state,
                        );
                        assert_eq!(
                            terminal.grid.to_vec(),
                            grid,
                            "{context}: mode {mode} changed cells"
                        );
                        assert_eq!(
                            (terminal.cursor_row, terminal.cursor_col),
                            cursor,
                            "{context}"
                        );
                        assert_eq!(format!("{:?}", terminal.command_zones), zones, "{context}");
                        assert_eq!(
                            (
                                terminal.current_command_id.clone(),
                                terminal.current_command_text.clone()
                            ),
                            command,
                            "{context}"
                        );
                        assert_eq!(terminal.watch_generation(), generation, "{context}");
                        assert!(terminal.take_completed_commands().is_empty(), "{context}");
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn shared_session_parity_corpus_uses_native_owner_and_projection() {
        use jterm_core::session_parity::{fixtures, CHUNKINGS};
        replay_shared_session_parity_corpus(fixtures(), &CHUNKINGS)
            .expect("production replay must match the unmodified shared corpus");
    }

    #[test]
    fn shared_session_parity_rejects_wrong_completion_identity_golden() {
        use jterm_core::session_parity::{fixtures, CHUNKINGS};
        let original = fixtures()
            .into_iter()
            .find(|fixture| fixture.id == "identity_dedup")
            .expect("shared corpus must contain identity_dedup");
        assert_eq!(original.steps[3].expected.completed.len(), 1);
        assert_eq!(original.steps[3].expected.completed[0].id, "identity_dedup");
        for chunking in CHUNKINGS {
            // A passing control also exercises all later duplicate-D checkpoints.
            replay_shared_session_parity_corpus(vec![original.clone()], &[chunking])
                .expect("unmodified identity fixture must pass");
            let mut mutated = original.clone();
            mutated.steps[3].expected.completed[0].id = "unrelated".to_owned();
            // Only a structured mismatch from the actual production replay counts.
            // Panics, setup failures, and unrelated fields fail this test normally.
            let mismatch = replay_shared_session_parity_corpus(vec![mutated], &[chunking])
                .expect_err("the real replay must consume the mutated completion identity");
            assert_eq!(
                *mismatch,
                SessionParityIdentityMismatch {
                    fixture: "identity_dedup".to_owned(),
                    chunking: format!("{chunking:?}"),
                    checkpoint: 3,
                    field: "completed[0].id".to_owned(),
                    expected: "unrelated".to_owned(),
                    observed: "identity_dedup".to_owned(),
                }
            );
        }
        // This proves same-owner execution identity sensitivity, not cross-session routing.
    }
}
