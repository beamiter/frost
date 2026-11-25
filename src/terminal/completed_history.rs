use super::{Arc, CommandZone, TerminalState, MAX_COMMAND_ZONES};
use jterm_core::completed_history::{Card, Checkpoint, Output, MAX_RETAINED_TEXT_BYTES};
use std::io;

impl TerminalState {
    pub(super) fn mark_completed_history_changed(&mut self) {
        self.completed_history_revision = self.completed_history_revision.saturating_add(1);
    }

    pub fn completed_history_revision(&self) -> u64 {
        self.completed_history_revision
    }

    pub fn completed_history_checkpoint(&self, pane: &str) -> io::Result<Checkpoint> {
        let next_sequence = self
            .next_zone_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("completed card ID space exhausted"))?;
        let mut remaining = MAX_RETAINED_TEXT_BYTES;
        let mut cards = Vec::new();
        for zone in self.command_zones.iter().rev() {
            let Some(source) = zone.output_source.clone() else {
                continue;
            };
            let mut card = Card {
                sequence: zone
                    .id
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("completed card ID space exhausted"))?,
                shell_id: zone
                    .source_shell_id
                    .clone()
                    .unwrap_or_else(|| format!("local:{}", zone.id)),
                shell_session_id: None,
                shell_sequence: None,
                shell_started_at_ms: None,
                command: zone.command.clone(),
                command_exact: zone.command_exact,
                command_truncated: zone.command_truncated,
                cwd: zone
                    .cwd
                    .clone()
                    .filter(|cwd| jterm_core::execution_journal::is_valid_jsh_cwd(cwd)),
                cwd_after: None,
                exit_code: zone.exit_code,
                duration_ms: zone.duration_ms,
                started_at_ms: None,
                finished_at_ms: zone.finished_at_ms,
                start_mark_seen: zone.start_mark_seen,
                completion: zone.completion_provenance.into(),
                source,
                output: None,
            };
            let Some(payload_budget) = remaining.checked_sub(card.retained_bytes()) else {
                break;
            };
            card.output = if let Some((text, truncated)) = zone.captured_output.as_ref() {
                // Frost exposes the retained length and a cap flag.
                Output::capture(
                    text,
                    *truncated,
                    text.len() as u64,
                    zone.captured_annotations.as_deref(),
                    payload_budget,
                )
            } else if !zone.captured_output_evicted && !card.source.execution.head_dropped {
                Output::capture("", false, 0, None, payload_budget)
            } else {
                None
            };
            remaining = payload_budget - card.output.as_ref().map_or(0, Output::retained_bytes);
            cards.push(card);
        }
        cards.reverse();
        Checkpoint::new(pane.to_owned(), next_sequence, cards)
    }

    pub fn restore_completed_history(&mut self, checkpoint: Checkpoint) -> io::Result<()> {
        let (next_sequence, cards) = checkpoint.into_records();
        if self.next_zone_id != 0
            || !self.command_zones.is_empty()
            || cards.len() > MAX_COMMAND_ZONES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid completed Frost pane history",
            ));
        }
        self.next_zone_id = next_sequence - 1;
        for card in cards {
            let captured_output_evicted = card.output.is_none();
            let (captured_output, captured_annotations) = card
                .output
                .filter(|output| !output.text.is_empty())
                .map_or((None, None), |output| {
                    let attributes = output
                        .annotations
                        .and_then(|annotations| annotations.restore(&output.text))
                        .map(Arc::new);
                    (Some((output.text, output.truncated)), attributes)
                });
            let output_preview = captured_output
                .as_ref()
                .filter(|(_, capped)| !capped)
                .map_or_else(String::new, |(text, _)| {
                    crate::fold_preview::from_plain(text)
                });
            let zone = CommandZone {
                id: card.sequence - 1,
                prompt_start: 0,
                command_start: None,
                output_start: None,
                output_start_col: 0,
                output_end: None,
                exit_code: card.exit_code,
                command: card.command,
                duration_ms: card.duration_ms,
                finished_at_ms: card.finished_at_ms,
                command_truncated: card.command_truncated,
                command_exact: card.command_exact,
                cwd: card.cwd,
                captured_output,
                captured_annotations,
                output_source: Some(card.source),
                source_shell_id: Some(card.shell_id),
                output_preview,
                captured_output_evicted,
                start_mark_seen: card.start_mark_seen,
                completion_provenance: card.completion.into(),
                rows_evicted: true,
            };
            self.captured_output_bytes += zone.captured_bytes();
            self.command_zones.push_back(zone);
        }
        // Cached pane projection predates restored metadata, even though no
        // live terminal rows have changed or were supplied by the checkpoint.
        self.bump_history_revision();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_card_keeps_its_source_without_reattaching_old_rows() {
        const EXECUTION: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07source proof\r\n\x1b]133;C;id=proof;cmdline_url=source%20proof\x07\x1b[1;38;2;31;171;193mvisible\x1b[0m\r\n\x1b]133;D;0;id=proof\x07";
        let mut source = TerminalState::new(80, 24);
        source.process_input(EXECUTION);
        let old_id = source.command_zones[0].id;
        let old_source = source.command_zones[0].output_source.clone();
        assert!(old_source.is_some());
        let checkpoint = source.completed_history_checkpoint("pane").unwrap();

        let mut restored = TerminalState::new(80, 24);
        restored.restore_completed_history(checkpoint).unwrap();
        assert!(restored.take_completed_commands().is_empty());
        assert!(restored.command_zones[0].rows_evicted);
        assert_eq!(restored.command_zones[0].id, old_id);
        assert_eq!(restored.command_zones[0].output_source, old_source);
        let zone = &restored.command_zones[0];
        let annotations = zone.captured_annotations.as_ref().unwrap();
        let output = &zone.captured_output.as_ref().unwrap().0;
        assert_eq!(&output[annotations.spans[0].bytes.clone()], "visible");
        assert_eq!(
            annotations.spans[0].style.foreground,
            super::super::Color::Rgb(31, 171, 193)
        );
        assert!(annotations.spans[0].style.flags.bold());
        assert_eq!(
            restored.zone_output_text(old_id).unwrap().trim_end(),
            "visible"
        );
        assert_eq!(restored.zone_output_line_row(old_id, 1), None);

        restored.process_input(EXECUTION);
        assert_eq!(restored.command_zones.len(), 2);
        assert_ne!(restored.command_zones[1].output_source, old_source);
        assert_ne!(restored.command_zones[1].id, old_id);
        assert_eq!(restored.take_completed_commands().len(), 1);
    }
}
