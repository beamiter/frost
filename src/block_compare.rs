//! Read-only command comparison and exact selection export from native Review.

use super::{block_export, block_review, BlockReview, Frost, Message, ToastKind};
use block_export::{SessionExportFormat, SessionExportSnapshot};
use iced::Task;
use jterm_core::output_compare::{compare, OutputSnapshot};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static COMPARISON_BUSY: AtomicBool = AtomicBool::new(false);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(crate) struct ComparisonState {
    pub request_id: u64,
    pub ready: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ComparisonResult {
    pub session_id: usize,
    pub ids: Vec<u64>,
    pub request_id: u64,
    pub body: Result<String, String>,
}

struct ComparisonPermit;

impl ComparisonPermit {
    fn acquire() -> Option<Self> {
        COMPARISON_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ComparisonPermit {
    fn drop(&mut self) {
        COMPARISON_BUSY.store(false, Ordering::Release);
    }
}

fn comparison_body(snapshot: &SessionExportSnapshot) -> Result<String, String> {
    if snapshot.blocks.len() != 2 || snapshot.blocks[0].id == snapshot.blocks[1].id {
        return Err("Select exactly two different retained blocks to compare.".into());
    }
    if snapshot.blocks.iter().any(|block| {
        !block.command_truncated
            && block
                .command
                .as_deref()
                .is_none_or(|command| command.trim().is_empty())
    }) {
        return Err("Choose two command blocks; background output is not a command result.".into());
    }
    if let Some(block) = snapshot
        .blocks
        .iter()
        .find(|block| block.output_unavailable)
    {
        return Err(format!(
            "Block #{} no longer has retained output. No comparison was made; unavailable output is not empty output.",
            block.id
        ));
    }
    let before = &snapshot.blocks[0];
    let after = &snapshot.blocks[1];
    let comparison = compare(
        OutputSnapshot {
            text: &before.output,
            truncated: before.output_truncated,
        },
        OutputSnapshot {
            text: &after.output,
            truncated: after.output_truncated,
        },
    );
    let mut body = format!(
        "Pane session {} · earlier → later\n\n",
        snapshot.pane_session_id
    );
    for (label, block) in [("Before", before), ("After", after)] {
        body.push_str(&format!(
            "{label} · block #{}\nCommand: {}{}\nDirectory: {}\nExit: {} · duration: {}\nFinished (Unix ms): {}\nCompletion: {}\n\n",
            block.id,
            block_review::preview(block.command.as_deref().unwrap_or("Command unavailable"), 4096),
            if block.command_truncated { " [command truncated]" } else { "" },
            block_review::preview(block.cwd.as_deref().unwrap_or("Not reported"), 2048),
            block.exit_code.map(|code| code.to_string()).unwrap_or_else(|| "Not reported".into()),
            block.duration_ms.map(|ms| format!("{ms} ms")).unwrap_or_else(|| "Not reported".into()),
            block.finished_at_ms.map(|ms| ms.to_string()).unwrap_or_else(|| "Not reported".into()),
            super::block_mode::lifecycle_detail(block.start_mark_seen, block.completion_provenance),
        ));
    }
    body.push_str(&comparison.summary());
    body.push_str("\n\n");
    body.push_str(&comparison.text);
    Ok(body)
}

impl Frost {
    /// A Review footer acts on its original exact set. It cannot quietly turn
    /// a clipped 128-record preview into a successful partial-selection export.
    pub(crate) fn block_review_sources_current(&self, review: &BlockReview) -> bool {
        self.sessions.get(self.active).is_some_and(|session| {
            if session.id != review.session_id
                || !self.config.is_block_mode()
                || session.terminal.is_alt_buffer_active()
                || review.ids.is_empty()
                || review.ids.len() > block_export::MAX_SELECTED_BLOCKS
                || session.block_selection.len() != review.ids.len()
            {
                return false;
            }
            let ids: Vec<_> = session
                .terminal
                .command_zones
                .iter()
                .filter(|zone| session.block_selection.contains(zone.id))
                .map(|zone| zone.id)
                .collect();
            ids == review.ids
        })
    }

    fn selection_export_snapshot(
        &self,
        from_review: bool,
    ) -> Result<SessionExportSnapshot, String> {
        let session = self.sessions.get(self.active).ok_or("No active pane.")?;
        let ids = if from_review {
            let review = self
                .block_review
                .as_ref()
                .ok_or("Reopen the selection review.")?;
            if !self.block_review_sources_current(review) {
                return Err("Selection changed or a source was evicted. Review the complete selection again.".into());
            }
            review.ids.clone()
        } else {
            let ids: Vec<_> = session
                .terminal
                .command_zones
                .iter()
                .filter(|zone| session.block_selection.contains(zone.id))
                .map(|zone| zone.id)
                .collect();
            if ids.len() != session.block_selection.len() {
                return Err(
                    "A selected block was evicted. Select the retained blocks again.".into(),
                );
            }
            ids
        };
        block_export::snapshot_selected(&session.terminal, session.id, &ids)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn block_export_selected_task(
        &mut self,
        format: SessionExportFormat,
        from_review: bool,
    ) -> Task<Message> {
        if !self.ensure_block_action_available("Selected block export") {
            return Task::none();
        }
        if self.block_export_in_flight {
            self.push_toast("A block export is already in progress", ToastKind::Info);
            return Task::none();
        }
        match self.selection_export_snapshot(from_review) {
            Ok(snapshot) => self.start_block_export(snapshot, format, "selected blocks"),
            Err(error) => {
                self.push_toast(
                    format!("Could not export selection: {error}"),
                    ToastKind::Warning,
                );
                Task::none()
            }
        }
    }

    pub(crate) fn block_compare_selected_task(&mut self, from_review: bool) -> Task<Message> {
        if !self.ensure_block_action_available("Compare selected output") {
            return Task::none();
        }
        if self
            .sessions
            .get(self.active)
            .is_none_or(|session| session.block_selection.len() != 2)
        {
            self.push_toast(
                "Select exactly two completed blocks to compare their output.",
                ToastKind::Info,
            );
            return Task::none();
        }
        let Some(permit) = ComparisonPermit::acquire() else {
            self.push_toast(
                "An output comparison is already in progress.",
                ToastKind::Info,
            );
            return Task::none();
        };
        let snapshot = match self.selection_export_snapshot(from_review) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.push_toast(
                    format!("Could not compare selection: {error}"),
                    ToastKind::Warning,
                );
                return Task::none();
            }
        };
        let session_id = snapshot.pane_session_id;
        let ids: Vec<_> = snapshot.blocks.iter().map(|block| block.id).collect();
        let request_id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        self.block_review = Some(BlockReview {
            session_id,
            ids: ids.clone(),
            title: format!("Compare output · #{} → #{}", ids[0], ids[1]),
            body: "Comparing the two retained output snapshots…".into(),
            commands: None,
            command_error: None,
            bracketed: false,
            comparison: Some(ComparisonState {
                request_id,
                ready: false,
            }),
        });
        Task::perform(
            async move {
                let body = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    comparison_body(&snapshot)
                })
                .await
                .unwrap_or_else(|error| Err(format!("Comparison worker failed: {error}")));
                ComparisonResult {
                    session_id,
                    ids,
                    request_id,
                    body,
                }
            },
            Message::BlockComparisonFinished,
        )
    }

    pub(crate) fn finish_block_comparison(&mut self, result: ComparisonResult) {
        let Some(review) = self.block_review.as_mut() else {
            return;
        };
        let Some(state) = review.comparison.as_mut() else {
            return;
        };
        if state.request_id != result.request_id
            || review.session_id != result.session_id
            || review.ids != result.ids
        {
            return;
        }
        state.ready = result.body.is_ok();
        review.body = result.body.unwrap_or_else(|error| error);
    }

    pub(crate) fn copy_block_comparison(&self) -> Task<Message> {
        let Some(review) = self
            .block_review
            .as_ref()
            .filter(|review| review.comparison.as_ref().is_some_and(|state| state.ready))
        else {
            return Task::none();
        };
        // This is an immutable, display-safe report. No current terminal state
        // is read and no terminal input path is entered by the copy action.
        iced::clipboard::write(format!("{}\n\n{}", review.title, review.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalState;

    fn terminal_pair() -> TerminalState {
        let mut terminal = TerminalState::new(80, 12);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07check\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=check\x07old\r\n\x1b]133;D;exit_code=1;cwd_url=%2Fwork\x07",
        );
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07check\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=check\x07new\r\n\x1b]133;D;exit_code=0;cwd_url=%2Fwork\x07",
        );
        terminal
    }

    #[test]
    fn compare_real_finalized_snapshots_in_terminal_order_without_touching_sources() {
        let terminal = terminal_pair();
        let before: Vec<_> = terminal
            .command_zones
            .iter()
            .map(|zone| zone.captured_output.clone())
            .collect();
        let snapshot = block_export::snapshot_selected(&terminal, 42, &[1, 0]).unwrap();
        let report = comparison_body(&snapshot).unwrap();
        assert!(
            report.find("Before · block #0").unwrap() < report.find("After · block #1").unwrap()
        );
        assert!(report.contains("-old\n"));
        assert!(report.contains("+new\n"));
        assert!(report.contains("Directory: /work"));
        assert_eq!(
            before,
            terminal
                .command_zones
                .iter()
                .map(|zone| zone.captured_output.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_output_refuses_comparison_and_partial_capture_is_disclosed() {
        let mut terminal = terminal_pair();
        terminal.command_zones[0].captured_output = None;
        terminal.command_zones[0].captured_output_evicted = true;
        let missing = block_export::snapshot_selected(&terminal, 42, &[0, 1]).unwrap();
        assert!(comparison_body(&missing)
            .unwrap_err()
            .contains("not empty output"));
        terminal.command_zones[0].captured_output = Some(("old".into(), true));
        let partial = block_export::snapshot_selected(&terminal, 42, &[0, 1]).unwrap();
        assert!(comparison_body(&partial)
            .unwrap()
            .contains("partial captured output"));
    }

    #[test]
    fn compare_requires_two_distinct_records_and_keeps_empty_capture_valid() {
        let terminal = terminal_pair();
        let mut snapshot = block_export::snapshot_selected(&terminal, 42, &[0, 1]).unwrap();
        snapshot.blocks[0].output.clear();
        snapshot.blocks[1].output.clear();
        assert!(comparison_body(&snapshot)
            .unwrap()
            .contains("are identical"));
        snapshot.blocks[1].id = snapshot.blocks[0].id;
        assert!(comparison_body(&snapshot).is_err());
        snapshot.blocks.pop();
        assert!(comparison_body(&snapshot).is_err());
    }

    #[test]
    fn background_is_not_a_command_but_truncated_command_metadata_is_preserved() {
        let terminal = terminal_pair();
        let mut snapshot = block_export::snapshot_selected(&terminal, 42, &[0, 1]).unwrap();
        snapshot.blocks[0].command = None;
        assert!(comparison_body(&snapshot)
            .unwrap_err()
            .contains("background"));
        snapshot.blocks[0].command_truncated = true;
        let report = comparison_body(&snapshot).unwrap();
        assert!(report.contains("Command unavailable [command truncated]"));
    }
}
