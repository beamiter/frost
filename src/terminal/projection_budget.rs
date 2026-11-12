//! Allocation-free preflight. Counts are structural limits, not process RSS.
use super::{CompressedLineData, ScrollbackLine};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionResource {
    SourceRows,
    WideContinuations,
    PlanRows,
    RawSlices,
    SourceBytes,
    DecodedCells,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionUnavailable {
    BudgetExceeded {
        resource: ProjectionResource,
        limit: usize,
    },
    ArithmeticOverflow,
}

pub(super) type RawAdmissionCache = ((u64, u64, u64, usize, usize, usize), Result<()>);
pub(super) type AdmissionCache = (super::ProjectionPlanCacheKey, u64, usize, bool, Result<()>);

type Result<T> = std::result::Result<T, ProjectionUnavailable>;

#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub source_rows: usize,
    pub wide: usize,
    pub rows: usize,
    pub slices: usize,
    pub source_bytes: usize,
    pub decoded_cells: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            source_rows: 101_024,
            wide: 1_048_576,
            rows: 131_072,
            slices: 262_144,
            source_bytes: 32 * 1024 * 1024,
            decoded_cells: 1_048_576,
        }
    }
}

pub(super) fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or(ProjectionUnavailable::ArithmeticOverflow)
}
pub(super) fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b)
        .ok_or(ProjectionUnavailable::ArithmeticOverflow)
}
pub(super) fn check(value: usize, limit: usize, resource: ProjectionResource) -> Result<()> {
    if value > limit {
        Err(ProjectionUnavailable::BudgetExceeded { resource, limit })
    } else {
        Ok(())
    }
}

#[derive(Default, Debug)]
pub(super) struct Estimate {
    pub source_rows: usize,
    pub wide: usize,
    pub rows: usize,
    pub slices: usize,
    pub source_bytes: usize,
    pub decoded_cells: usize,
}

/// Reads only cached geometry and encoded length. Never clones/decompresses a
/// row, and never walks a rejected logical line once per output cell.
/// `raw_materialization` additionally charges actual source widths and encoded
/// bytes; trimmed style-only blanks still cost their complete encoded storage.
pub(super) fn history<'a>(
    lines: impl IntoIterator<Item = &'a ScrollbackLine>,
    cols: usize,
    limits: Limits,
    raw_materialization: bool,
) -> Result<Estimate> {
    if cols == 0 {
        return Err(ProjectionUnavailable::ArithmeticOverflow);
    }
    let mut result = Estimate::default();
    let mut group_cells = 0usize;
    let mut group_wide = 0usize;
    let mut group_rows = 0usize;
    // Upper bound with at most one lost column at each wide-aware boundary.
    // It is deliberately conservative: budget exhaustion never clips mappings.
    // Exact geometry construction remains separately charged before each push.
    for line in lines {
        result.source_rows = add(result.source_rows, 1)?;
        check(
            result.source_rows,
            limits.source_rows,
            ProjectionResource::SourceRows,
        )?;
        let wide = line.projected_wide_continuations.len();
        result.wide = add(result.wide, wide)?;
        check(
            result.wide,
            limits.wide,
            ProjectionResource::WideContinuations,
        )?;
        let data_bytes = match &line.data {
            CompressedLineData::Plain(text, _) => text.len(),
            CompressedLineData::Encoded(data) => data.len(),
        };
        result.source_bytes = add(
            result.source_bytes,
            add(
                std::mem::size_of::<ScrollbackLine>(),
                add(data_bytes, mul(wide, std::mem::size_of::<u16>())?)?,
            )?,
        )?;
        result.decoded_cells = add(result.decoded_cells, usize::from(line.cols))?;
        if raw_materialization {
            check(
                result.source_bytes,
                limits.source_bytes,
                ProjectionResource::SourceBytes,
            )?;
            check(
                result.decoded_cells,
                limits.decoded_cells,
                ProjectionResource::DecodedCells,
            )?;
        }
        group_cells = add(group_cells, usize::from(line.projected_active_len))?;
        group_wide = add(group_wide, wide)?;
        group_rows = add(group_rows, 1)?;
        let visible_cells = group_cells
            .checked_sub(group_wide)
            .ok_or(ProjectionUnavailable::ArithmeticOverflow)?;
        let lower = add(visible_cells / cols, usize::from(visible_cells % cols != 0))?;
        check(
            add(result.rows, lower)?,
            limits.rows,
            ProjectionResource::PlanRows,
        )?;
        if !line.is_wrapped {
            finish_group(
                &mut result,
                group_cells,
                group_wide,
                group_rows,
                cols,
                limits,
            )?;
            group_cells = 0;
            group_wide = 0;
            group_rows = 0;
        }
    }
    if group_rows != 0 {
        finish_group(
            &mut result,
            group_cells,
            group_wide,
            group_rows,
            cols,
            limits,
        )?;
    }
    Ok(result)
}

fn finish_group(
    result: &mut Estimate,
    cells: usize,
    wide: usize,
    source_rows: usize,
    cols: usize,
    limits: Limits,
) -> Result<()> {
    let rows = if cells == 0 {
        1
    } else if cols == 1 {
        cells
            .checked_sub(wide)
            .ok_or(ProjectionUnavailable::ArithmeticOverflow)?
    } else {
        let width = if wide == 0 { cols } else { cols - 1 };
        add(cells / width, usize::from(!cells.is_multiple_of(width)))?
    };
    result.rows = add(result.rows, rows)?;
    check(result.rows, limits.rows, ProjectionResource::PlanRows)?;
    // Each new output boundary can split at most one existing physical slice.
    result.slices = add(result.slices, add(rows, source_rows)?)?;
    check(result.slices, limits.slices, ProjectionResource::RawSlices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalCell;
    #[test]
    fn arithmetic_never_saturates_into_an_accepted_budget() {
        assert_eq!(
            add(usize::MAX, 1),
            Err(ProjectionUnavailable::ArithmeticOverflow)
        );
        assert_eq!(
            mul(usize::MAX, 2),
            Err(ProjectionUnavailable::ArithmeticOverflow)
        );
    }
    #[test]
    fn exact_single_column_budget_and_next_row_rejected_before_expansion() {
        let line = ScrollbackLine::compress(
            &[TerminalCell {
                character: 'x',
                ..TerminalCell::default()
            }; 8],
            false,
        );
        let limits = Limits {
            rows: 8,
            ..Limits::default()
        };
        assert_eq!(history([&line], 1, limits, false).unwrap().rows, 8);
        assert!(matches!(
            history([&line, &line], 1, limits, false),
            Err(ProjectionUnavailable::BudgetExceeded {
                resource: ProjectionResource::PlanRows,
                ..
            })
        ));
    }
    #[test]
    fn trimmed_styled_blanks_cannot_bypass_source_decode_budget() {
        let mut blank = TerminalCell::default();
        blank.flags.set_bold(true);
        let line = ScrollbackLine::compress(&[blank; 32], false);
        assert_eq!(line.projected_active_len, 0);
        let limits = Limits {
            decoded_cells: 31,
            ..Limits::default()
        };
        assert!(matches!(
            history([&line], 8, limits, true),
            Err(ProjectionUnavailable::BudgetExceeded {
                resource: ProjectionResource::DecodedCells,
                ..
            })
        ));
    }
}

impl super::TerminalState {
    pub fn projection_unavailable(&self) -> Option<ProjectionUnavailable> {
        self.projection_failure
    }

    pub fn projection_interaction_allowed(&self, viewport: &super::ProjectedViewport) -> bool {
        self.projection_unavailable().is_none()
            && viewport.unavailable.is_none()
            && viewport.view_revision != 0
            && self
                .projected_viewport_cache
                .as_ref()
                .is_some_and(|(_, current)| current.view_revision == viewport.view_revision)
            && (viewport.mode != super::ProjectionMode::Transformed
                || self.projection_selection_revision(viewport).is_some())
            && viewport.source_revision.grid == self.grid_version
            && viewport.source_revision.history == self.history_revision
            && viewport.source_revision.row_identity == self.grid.identity_revision
            && viewport.source_revision.alternate_screen == self.use_alt_buffer
    }

    /// Empty unavailable snapshots deliberately contain no live/raw cells:
    /// inline scope and Chat privacy must survive a rejected projection.
    pub(super) fn unavailable_viewport(
        &mut self,
        reason: ProjectionUnavailable,
    ) -> std::sync::Arc<super::ProjectedViewport> {
        self.projection_failure = Some(reason);
        self.projection_plan_cache = None;
        self.projected_viewport_cache = None;
        self.visible_cells_cache = None;
        self.clear_text_selection();
        std::sync::Arc::new(super::ProjectedViewport {
            unavailable: Some(reason),
            cells: std::sync::Arc::new(super::VisibleRows::materialized(Vec::new())),
            row_wrapped: Default::default(),
            row_kinds: Default::default(),
            provenance: std::sync::Arc::new(super::ProjectedProvenance {
                origin_spans: Vec::new(),
                row_sources: Vec::new(),
            }),
            raw_span_index: Default::default(),
            raw_row_index: Default::default(),
            source_revision: super::ProjectionSourceRevision {
                grid: self.grid_version,
                history: self.history_revision,
                row_identity: self.grid.identity_revision,
                alternate_screen: self.use_alt_buffer,
            },
            view_revision: 0,
            identity_fast_path: false,
            mode: super::ProjectionMode::Transformed,
            scroll_offset: 0,
            policy_revision: 0,
            policy_ids: Default::default(),
            block_spacing: false,
            document_rows: 0,
            document_start: 0,
            top_padding: 0,
            effective_collapsed: Default::default(),
            plan_revision: 0,
            chat_rows: Default::default(),
            document_row_map: Default::default(),
            canvas_head: false,
        })
    }
}

impl super::TerminalState {
    pub(super) fn plan_preflight(
        &self,
        cols: usize,
        policy: &super::ProjectionPolicy,
    ) -> Result<()> {
        let limits = Limits::current();
        let estimate = history(self.scrollback.iter(), cols, limits, false)?;
        check(
            add(estimate.source_rows, self.grid.rows())?,
            limits.source_rows,
            ProjectionResource::SourceRows,
        )?;
        let grid_wide = self.grid.iter().try_fold(0usize, |count, row| {
            add(
                count,
                row.iter()
                    .filter(|cell| cell.flags.wide_continuation())
                    .count(),
            )
        })?;
        check(
            add(estimate.wide, grid_wide)?,
            limits.wide,
            ProjectionResource::WideContinuations,
        )?;
        let base_rows = add(estimate.rows, self.grid.rows())?;
        let base_slices = add(estimate.slices, self.grid.rows())?;
        let collapses = self.resolved_collapses(policy);
        // resolved_collapses rejects every overlapping component. Disjoint
        // intervals can split existing slices only at their two endpoints;
        // complete interior slices disappear rather than multiply.
        let cuts = mul(collapses.len(), 2)?;
        let slices = add(base_slices, cuts)?;
        check(slices, limits.slices, ProjectionResource::RawSlices)?;
        let owners = collapses.iter().try_fold(0usize, |count, collapse| {
            let span = collapse
                .end_absolute
                .checked_sub(collapse.start_absolute)
                .ok_or(ProjectionUnavailable::ArithmeticOverflow)?;
            add(count, add(span, 1)?)
        })?;
        check(owners, limits.slices, ProjectionResource::RawSlices)?;
        let mut extra = add(cuts, collapses.len())?;
        extra = add(
            extra,
            self.block_spacing_boundaries(policy.block_spacing).len(),
        )?;
        for boundary in self.native_chrome_boundaries(policy) {
            extra = add(extra, boundary.rows)?;
        }
        if let Some(chat) = self.chat_plan_key(policy) {
            // Ownership intervals are ordered and disjoint: at most command
            // + output per completed zone, then active command/composer and
            // an unclassified prefix. Ambiguous singleton rows add no chrome.
            let groups = add(mul(self.command_zones.len(), 2)?, 4)?;
            extra = add(extra, add(mul(groups, 5)?, chat.companion_rows)?)?;
        }
        check(
            add(base_rows, extra)?,
            limits.rows,
            ProjectionResource::PlanRows,
        )
    }

    pub(super) fn raw_preflight(&self, rows: usize, cols: usize) -> Result<()> {
        if self.scroll_offset == 0 {
            return Ok(());
        }
        let limits = Limits::current();
        let mut start = self
            .scrollback
            .len()
            .saturating_sub(add(self.scroll_offset, rows)?);
        while start > 0 && self.scrollback[start - 1].is_wrapped {
            start -= 1;
        }
        let estimate = history(self.scrollback.iter().skip(start), cols, limits, true)?;
        check(
            mul(estimate.rows, cols)?,
            limits.decoded_cells,
            ProjectionResource::DecodedCells,
        )
    }
}

impl Limits {
    fn current() -> Self {
        #[cfg(test)]
        {
            TEST_LIMITS.with(std::cell::Cell::get)
        }
        #[cfg(not(test))]
        {
            Self::default()
        }
    }
}
#[cfg(test)]
thread_local! { static TEST_LIMITS: std::cell::Cell<Limits> = std::cell::Cell::new(Limits::default()); }

impl super::TerminalState {
    pub(super) fn cached_projection_preflight(
        &mut self,
        block_mode: bool,
        policy: &super::ProjectionPolicy,
    ) -> Result<()> {
        let key = self.projection_plan_cache_key(self.grid.cols().max(1), policy);
        if let Some((old, grid, offset, mode, result)) = &self.projection_budget_cache {
            if old == &key
                && *grid == self.grid_version
                && *offset == self.scroll_offset
                && *mode == block_mode
            {
                return *result;
            }
        }
        #[cfg(test)]
        PREFLIGHT_SCANS.with(|count| count.set(count.get() + 1));
        let result = if block_mode
            && !self.use_alt_buffer
            && !policy.is_identity()
            && !(policy.chat && self.chat_plan_key(policy).is_none())
        {
            self.plan_preflight(self.grid.cols().max(1), policy)
        } else {
            self.cached_raw_preflight(self.grid.rows(), self.grid.cols().max(1))
        };
        self.projection_budget_cache = Some((
            key,
            self.grid_version,
            self.scroll_offset,
            block_mode,
            result,
        ));
        result
    }
}

#[cfg(test)]
mod production_tests {
    use super::*;
    use crate::terminal::{
        ProjectionPolicy, ProjectionViewState, TerminalCell, TerminalState, ViewportCell,
    };
    struct Restore(Limits);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_LIMITS.with(|slot| slot.set(self.0));
        }
    }
    fn limit(limits: Limits) -> Restore {
        Restore(TEST_LIMITS.with(|slot| slot.replace(limits)))
    }
    fn history_terminal() -> TerminalState {
        let mut terminal = TerminalState::new(8, 2);
        terminal.process_input(b"abcdefgh\r\nijklmnop\r\nqrstuvwx\r\n");
        terminal
    }
    #[test]
    fn unavailable_is_empty_and_cannot_be_cleared_by_auxiliary_raw_query() {
        let _restore = limit(Limits {
            rows: 12,
            ..Limits::default()
        });
        let mut terminal = history_terminal();
        let policy = ProjectionPolicy {
            retain_identity_canvas: true,
            ..ProjectionPolicy::new()
        };
        let mut state = ProjectionViewState::default();
        let old = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(old.unavailable().is_none());
        terminal.start_selection_in_projection(
            &old,
            (0, 0),
            crate::terminal::SelectionMode::Normal,
        );
        terminal.on_resize(1, 2);
        let rejected = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(rejected.unavailable().is_some());
        assert!(format!("{rejected:?}").contains("unavailable: Some(BudgetExceeded"));
        assert!(rejected.cells().is_empty());
        assert_eq!(rejected.view_to_raw(ViewportCell { row: 0, col: 0 }), None);
        assert!(!terminal.projection_interaction_allowed(&old));
        assert_eq!(terminal.copy_selection(), None);
        assert!(state.top_anchor.is_none());
        assert!(state.last_plan_key.is_none());
        assert!(terminal
            .get_projected_viewport(false)
            .unavailable()
            .is_some());
        assert!(terminal.get_visible_cells().is_empty());
        assert!(terminal.projection_unavailable().is_some());
        terminal.on_resize(8, 2);
        let recovered = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(recovered.unavailable().is_none());
        assert!(!terminal.projection_interaction_allowed(&old));
        assert!(terminal.projection_interaction_allowed(&recovered));
    }
    #[test]
    fn raw_scrolled_materialization_uses_real_width_not_trimmed_content() {
        let _restore = limit(Limits {
            decoded_cells: 7,
            ..Limits::default()
        });
        let mut terminal = TerminalState::new(8, 2);
        let mut cell = TerminalCell::default();
        cell.flags.set_bold(true);
        terminal
            .scrollback
            .push_back(ScrollbackLine::compress(&[cell; 8], false));
        terminal.scroll_offset = 1;
        let view = terminal.get_projected_viewport(false);
        assert!(matches!(
            view.unavailable(),
            Some(ProjectionUnavailable::BudgetExceeded {
                resource: ProjectionResource::DecodedCells,
                ..
            })
        ));
        assert!(view.cells().is_empty());
    }
    #[test]
    fn source_clone_bytes_checked_before_raw_clone() {
        let _restore = limit(Limits {
            source_bytes: 1,
            ..Limits::default()
        });
        let mut terminal = history_terminal();
        terminal.scroll_offset = 1;
        assert!(matches!(
            terminal.get_projected_viewport(false).unavailable(),
            Some(ProjectionUnavailable::BudgetExceeded {
                resource: ProjectionResource::SourceBytes,
                ..
            })
        ));
    }
    #[test]
    fn policy_change_invalidates_old_frame_without_raw_source_change() {
        let mut terminal = history_terminal();
        let old = terminal.get_projected_viewport_with_policy(
            true,
            &ProjectionPolicy {
                retain_identity_canvas: true,
                ..ProjectionPolicy::new()
            },
            0,
        );
        let new = terminal.get_projected_viewport_with_policy(
            true,
            &ProjectionPolicy {
                retain_identity_canvas: true,
                block_spacing: true,
                ..ProjectionPolicy::new()
            },
            0,
        );
        assert!(old.unavailable().is_none() && new.unavailable().is_none());
        assert!(!terminal.projection_interaction_allowed(&old));
        assert!(terminal.projection_interaction_allowed(&new));
    }
}

#[cfg(test)]
impl super::TerminalState {
    /// Scoped test injection only; construct the terminal inside `action` so
    /// no budget certificate from an earlier limit can be reused.
    pub(crate) fn with_projection_row_budget<T>(rows: usize, action: impl FnOnce() -> T) -> T {
        struct Restore(Limits);
        impl Drop for Restore {
            fn drop(&mut self) {
                TEST_LIMITS.with(|slot| slot.set(self.0));
            }
        }
        let _restore = Restore(TEST_LIMITS.with(|slot| {
            let old = slot.get();
            slot.set(Limits { rows, ..old });
            old
        }));
        action()
    }
}

#[cfg(test)]
thread_local! { static PREFLIGHT_SCANS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)}; }

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::terminal::{ProjectionPolicy, TerminalState};
    #[test]
    fn unchanged_success_and_failure_are_cached_and_source_changes_recheck() {
        TerminalState::with_projection_row_budget(12, || {
            let mut terminal = TerminalState::new(8, 2);
            terminal.process_input(b"abcdefgh\r\nijklmnop\r\nqrstuvwx\r\n");
            let policy = ProjectionPolicy {
                retain_identity_canvas: true,
                ..ProjectionPolicy::new()
            };
            let start = PREFLIGHT_SCANS.with(std::cell::Cell::get);
            assert!(terminal
                .get_projected_viewport_with_policy(true, &policy, 0)
                .unavailable()
                .is_none());
            assert!(terminal
                .get_projected_viewport_with_policy(true, &policy, 0)
                .unavailable()
                .is_none());
            assert_eq!(PREFLIGHT_SCANS.with(std::cell::Cell::get), start + 1);
            terminal.on_resize(1, 2);
            assert!(terminal
                .get_projected_viewport_with_policy(true, &policy, 0)
                .unavailable()
                .is_some());
            assert!(terminal
                .get_projected_viewport_with_policy(true, &policy, 0)
                .unavailable()
                .is_some());
            assert_eq!(PREFLIGHT_SCANS.with(std::cell::Cell::get), start + 2);
            terminal.on_resize(8, 2);
            assert!(terminal
                .get_projected_viewport_with_policy(true, &policy, 0)
                .unavailable()
                .is_none());
            assert_eq!(PREFLIGHT_SCANS.with(std::cell::Cell::get), start + 3);
        });
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;
    use crate::terminal::TerminalCell;
    #[test]
    fn hundred_thousand_normal_rows_fit_metadata_only_budget() {
        let line = ScrollbackLine::compress(
            &[TerminalCell {
                character: 'x',
                ..TerminalCell::default()
            }; 80],
            false,
        );
        let result = history(
            std::iter::repeat_n(&line, 100_000),
            80,
            Limits::default(),
            false,
        )
        .unwrap();
        assert_eq!(result.rows, 100_000);
        assert_eq!(result.slices, 200_000);
        // Decoded cells exceed raw materialization budget, but metadata plans
        // must not be rejected on a cell array they never allocate.
        assert_eq!(result.decoded_cells, 8_000_000);
    }
    #[test]
    fn hundred_million_expansion_is_rejected_after_small_metadata_prefix() {
        let line = ScrollbackLine::compress(
            &[TerminalCell {
                character: 'x',
                ..TerminalCell::default()
            }; 1024],
            true,
        );
        let visited = std::cell::Cell::new(0);
        let result = history(
            std::iter::repeat_n(&line, 100_000).inspect(|_| visited.set(visited.get() + 1)),
            1,
            Limits::default(),
            false,
        );
        assert!(matches!(
            result,
            Err(ProjectionUnavailable::BudgetExceeded {
                resource: ProjectionResource::PlanRows,
                ..
            })
        ));
        assert_eq!(visited.get(), 129);
    }
}

impl super::TerminalState {
    pub(super) fn checked_materialize_projection(
        &self,
        plan: &super::ProjectionPlan,
        requested_scroll_offset: usize,
        rows: usize,
        cols: usize,
        canvas_head: bool,
    ) -> Result<super::MaterializedProjection> {
        let limits = Limits::current();
        let offset = requested_scroll_offset.min(plan.rows.len().saturating_sub(rows));
        let dock_start = plan.chat_dock_start.unwrap_or(plan.rows.len());
        let full_dock = plan
            .rows
            .len()
            .checked_sub(dock_start)
            .ok_or(ProjectionUnavailable::ArithmeticOverflow)?;
        let dock_rows = full_dock.min(rows);
        let rows = add(rows, usize::from(canvas_head))?;
        let capacity = rows.saturating_sub(dock_rows);
        let end = dock_start.saturating_sub(offset);
        let start = end.saturating_sub(capacity);
        let padding = usize::from(canvas_head && end < capacity);
        let visible = end - start;
        let gap = rows.saturating_sub(add(add(padding, visible)?, dock_rows)?);
        let output_rows = add(add(add(padding, visible)?, gap)?, full_dock)?;
        check(output_rows, limits.rows, ProjectionResource::PlanRows)?;
        check(
            mul(output_rows, cols)?,
            limits.decoded_cells,
            ProjectionResource::DecodedCells,
        )?;
        let mut decoded = 0;
        let mut slices = 0;
        // No source cloning/decompression until every referenced slice has
        // been charged. Repeated references conservatively pay again; hence
        // both cache size and decode work are bounded without a preflight set.
        for row in plan.rows[start..end]
            .iter()
            .chain(plan.rows[dock_start..].iter())
        {
            for slice in &row.raw_slices {
                slices = add(slices, 1)?;
                check(slices, limits.slices, ProjectionResource::RawSlices)?;
                if let Some(source) = self.scrollback.get(slice.source.absolute_row) {
                    decoded = add(decoded, usize::from(source.cols))?;
                    check(
                        decoded,
                        limits.decoded_cells,
                        ProjectionResource::DecodedCells,
                    )?;
                }
            }
        }
        Ok(self.materialize_projection_plan_with_margin(
            plan,
            requested_scroll_offset,
            rows - usize::from(canvas_head),
            cols,
            canvas_head,
        ))
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::terminal::{ProjectionPolicy, ProjectionViewState, TerminalCell, TerminalState};
    #[test]
    fn metadata_plan_can_fit_but_actual_visible_decode_fails_closed() {
        let prior = TEST_LIMITS.with(|slot| {
            slot.replace(Limits {
                decoded_cells: 64,
                ..Limits::default()
            })
        });
        struct Reset(Limits);
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_LIMITS.with(|slot| slot.set(self.0));
            }
        }
        let _reset = Reset(prior);
        let mut terminal = TerminalState::new(8, 2);
        let policy = ProjectionPolicy {
            retain_identity_canvas: true,
            ..ProjectionPolicy::new()
        };
        let mut state = ProjectionViewState::default();
        let old = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        for i in 0..8 {
            let mut cells = [TerminalCell::default(); 32];
            cells[0].character = 'x';
            // Empty default tails still decompress to all32 physical cells.
            terminal
                .scrollback
                .push_back(ScrollbackLine::compress(&cells, i != 7));
        }
        terminal.history_revision += 1;
        // Show the history group rather than live-grid-only bottom rows.
        state.follow_bottom = false;
        state.offset_from_bottom = 2;
        let view = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(matches!(
            view.unavailable(),
            Some(ProjectionUnavailable::BudgetExceeded {
                resource: ProjectionResource::DecodedCells,
                ..
            })
        ));
        assert!(view.cells().is_empty());
        assert!(state.top_anchor.is_none() && state.last_plan_key.is_none());
        assert!(!terminal.projection_interaction_allowed(&old));
        assert_eq!(terminal.copy_selection(), None);
        assert_eq!(state.offset_from_bottom, 2);
        state.invalidate_unavailable(); // Same final-surface cleanup as the UI.
        assert_eq!(state.offset_from_bottom, 2);
        let scans = PREFLIGHT_SCANS.with(std::cell::Cell::get);
        for _ in 0..3 {
            let idle = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
            assert!(idle.unavailable().is_some() && idle.cells().is_empty());
            assert_eq!(state.offset_from_bottom, 2);
            assert!(!state.reading_hold);
        }
        assert_eq!(PREFLIGHT_SCANS.with(std::cell::Cell::get), scans);
        // Same source and policy, but Block disabled: the transformed failure
        // must not veto an explicitly requested native live-grid surface.
        let source = (terminal.grid_version, terminal.history_revision);
        let raw = terminal.get_projected_viewport_with_state(false, &policy, &mut state);
        assert!(raw.unavailable().is_none());
        assert!(terminal.projection_interaction_allowed(&raw));
        assert_eq!((terminal.grid_version, terminal.history_revision), source);
        // Re-enabling the exact rejected transform must remain unavailable,
        // not reuse the intervening raw frame as a privacy fallback.
        let rejected_again = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(rejected_again.unavailable().is_some());
        assert!(!terminal.projection_interaction_allowed(&raw));
        assert_eq!(state.offset_from_bottom, 2);
        // A narrower target needs fewer original rows per visible frame; the
        // old numeric offset is clamped only after this new frame succeeds.
        terminal.on_resize(1, 2);
        let recovered = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(recovered.unavailable().is_none());
        assert!(terminal.projection_interaction_allowed(&recovered));
        assert!(!terminal.projection_interaction_allowed(&old));
    }
}

#[cfg(test)]
mod transform_bound_tests {
    use super::*;
    use crate::terminal::{ProjectionPolicy, TerminalState};
    #[test]
    fn accepted_budget_bounds_actual_collapse_spacing_chrome_and_chat_outputs() {
        let old = TEST_LIMITS.with(std::cell::Cell::get);
        struct Reset(Limits);
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_LIMITS.with(|s| s.set(self.0));
            }
        }
        let _reset = Reset(old);
        for cols in [2, 3, 8, 16] {
            let mut terminal = TerminalState::new(cols, 6);
            for n in 0..6 {
                terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo\r\n\x1b]133;C\x07");
                terminal.process_input(if n % 2 == 0 {
                    "a界bcdefgh\r\n".as_bytes()
                } else {
                    b"short\r\n"
                });
                terminal.process_input(b"\x1b]133;D;0\x07");
            }
            terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            for mode in 0..4 {
                let mut policy = ProjectionPolicy {
                    retain_identity_canvas: true,
                    ..ProjectionPolicy::new()
                };
                policy.set_block_spacing(mode >= 1);
                policy.set_native_chrome(mode == 2, 2);
                policy.set_chat(mode == 3);
                for zone in terminal.command_zones.iter().step_by(2) {
                    policy.collapse(zone.id);
                }
                let actual = terminal.collapsed_projection_plan(cols, &policy);
                for cap in [8, 32, 64, 128, 256, 512, 1024] {
                    TEST_LIMITS.with(|s| {
                        s.set(Limits {
                            rows: cap,
                            slices: cap * 2,
                            ..Limits::default()
                        })
                    });
                    if terminal.plan_preflight(cols, &policy).is_ok() {
                        assert!(
                            actual.rows.len() <= cap,
                            "mode {mode} cols {cols} cap {cap}"
                        );
                        assert!(
                            actual.raw_slice_count <= cap * 2,
                            "mode {mode} cols {cols} cap {cap}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod authority_tests {
    use crate::terminal::{ProjectionPolicy, TerminalState};
    #[test]
    fn unregistered_and_old_source_snapshots_are_not_live_authority() {
        let mut terminal = TerminalState::new(8, 2);
        let policy = ProjectionPolicy {
            retain_identity_canvas: true,
            ..ProjectionPolicy::new()
        };
        let current = terminal.get_projected_viewport_with_policy(true, &policy, 0);
        assert!(terminal.projection_interaction_allowed(&current));
        let mut unregistered = current.as_ref().clone();
        unregistered.view_revision += 1;
        assert!(!terminal.projection_interaction_allowed(&unregistered));
        let prior_grid_revision = terminal.grid_version;
        // Production PTY ingestion goes through process_batch, which advances
        // the host paint/source revision before parsing. process_input is the
        // lower-level parser test primitive and deliberately does not do so.
        terminal.process_batch(b"x");
        assert_eq!(terminal.grid[0][0].character, 'x');
        assert_ne!(terminal.grid_version, prior_grid_revision);
        assert!(!terminal.projection_interaction_allowed(&current));
        // Immutable geometry remains readable; this does not authorize a live
        // operation against the rewritten grid.
        let _ = current.view_to_raw(crate::terminal::ViewportCell { row: 0, col: 0 });
    }
}

impl super::TerminalState {
    pub(super) fn cached_raw_preflight(&mut self, rows: usize, cols: usize) -> Result<()> {
        let key = (
            self.grid_version,
            self.history_revision,
            self.grid.identity_revision,
            rows,
            cols,
            self.scroll_offset,
        );
        if let Some((old, result)) = &self.raw_projection_budget_cache {
            if *old == key {
                return *result;
            }
        }
        let result = self.raw_preflight(rows, cols);
        self.raw_projection_budget_cache = Some((key, result));
        result
    }
}
