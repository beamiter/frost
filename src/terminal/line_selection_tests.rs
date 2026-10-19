//! Deterministic whole-line drags over declared raw-cell ledgers.
//! Shared with the dependency-free, production-method extraction runner.
use super::*;

const WIDTH: usize = 8;

fn fixture(text: &[&str]) -> TerminalState {
    let mut terminal = TerminalState::new(WIDTH, 4);
    for line in text {
        assert!(line.chars().count() <= WIDTH);
        let mut cells = vec![TerminalCell::default(); WIDTH];
        for (cell, character) in cells.iter_mut().zip(line.chars()) {
            cell.character = character;
        }
        terminal
            .scrollback
            .push_back(ScrollbackLine::compress(&cells, false));
    }
    terminal
}

// Geometry is declared directly from the input ledger. This fixture does not
// use the production planner, selection mapping or copied text as an oracle.
fn plan(
    terminal: &TerminalState,
    text: &[&str],
    layout: &[Option<usize>],
    revision: u64,
) -> ProjectionPlan {
    let mut rows = Vec::new();
    let mut raw_rows = Vec::new();
    for (document_row, entry) in layout.iter().enumerate() {
        let Some(absolute_row) = *entry else {
            rows.push(ProjectionPlanRow {
                raw_slices: Default::default(),
                row_source: None,
                wrapped: false,
                kind: ProjectedRowKind::BlockSpacer {
                    next: terminal.scrollback[0].raw_row_id(),
                },
            });
            continue;
        };
        let id = terminal.scrollback[absolute_row].raw_row_id();
        let len = text[absolute_row].chars().count();
        let raw_slices = (len > 0)
            .then_some(RawSlice {
                view_col_start: 0,
                source: RawSliceSource {
                    absolute_row,
                    col_start: 0,
                },
                origin: Some(RawSliceOrigin {
                    row: id,
                    col_start: 0,
                }),
                len,
                narrow_wide_body: false,
            })
            .into_iter()
            .collect();
        rows.push(ProjectionPlanRow {
            raw_slices,
            row_source: Some(RowSource {
                raw_row: id,
                raw_absolute_row: absolute_row,
            }),
            wrapped: false,
            kind: ProjectedRowKind::Raw,
        });
        raw_rows.push(RawRowPlacement {
            absolute_row,
            raw_row: id,
            first_view_row: Some(document_row),
            last_view_row: Some(document_row),
        });
    }
    ProjectionPlan {
        cols: WIDTH,
        rows,
        raw_rows,
        raw_slice_count: text.len(),
        spacer_count: layout.len() - text.len(),
        policy_revision: revision,
        effective_collapsed: BTreeSet::new(),
        resolved_collapses: Vec::new(),
        plan_revision: revision,
        chat_rows: Vec::new(),
        chat_dock_start: None,
    }
}

fn install(terminal: &mut TerminalState, plan: ProjectionPlan) {
    terminal.reanchor_projected_selection(&plan);
    // Fill the real cache-key guards from fixture storage. No cache validation
    // function is replaced in the isolated runner.
    let key = ProjectionPlanCacheKey {
        history_revision: terminal.history_revision,
        history_len: terminal.scrollback.len(),
        scrollback_pushes: 0,
        full_screen_scroll_revision: 0,
        grid_row_ids: Default::default(),
        row_identity_revision: terminal.grid.identity_revision,
        rows: terminal.grid.rows(),
        cols: WIDTH,
        row_wrapped: terminal.grid.row_wrapped.iter().copied().collect(),
        policy_revision: plan.policy_revision,
        policy_ids: Default::default(),
        block_spacing: false,
        spacing_boundaries: Default::default(),
        chrome_boundaries: Default::default(),
        chrome_tail_rows: 0,
        next_zone_id: terminal.next_zone_id,
        zone_count: terminal.command_zones.len(),
        provenance_count: terminal.finished_output_provenance.len(),
        chat: None,
    };
    terminal.projection_plan_cache = Some((key, std::sync::Arc::new(plan)));
}

fn viewport(terminal: &TerminalState, offset: usize) -> ProjectedViewport {
    let plan = &terminal.projection_plan_cache.as_ref().unwrap().1;
    let m = terminal.materialize_projection_plan(plan, offset, terminal.grid.rows(), WIDTH);
    let (raw_span_index, raw_row_index) = TerminalState::projection_indexes(&m.provenance);
    ProjectedViewport {
        cells: std::sync::Arc::new(m.cells),
        row_wrapped: std::sync::Arc::new(m.row_wrapped),
        row_kinds: std::sync::Arc::new(m.row_kinds),
        provenance: std::sync::Arc::new(m.provenance),
        raw_span_index: std::sync::Arc::new(raw_span_index),
        raw_row_index: std::sync::Arc::new(raw_row_index),
        source_revision: ProjectionSourceRevision {
            grid: 0,
            history: terminal.history_revision,
            row_identity: terminal.grid.identity_revision,
            alternate_screen: false,
        },
        view_revision: plan.plan_revision,
        identity_fast_path: false,
        mode: ProjectionMode::Transformed,
        scroll_offset: m.scroll_offset,
        policy_revision: plan.policy_revision,
        policy_ids: std::sync::Arc::from([]),
        block_spacing: false,
        document_rows: plan.rows.len(),
        document_start: m.document_start,
        top_padding: m.top_padding,
        effective_collapsed: std::sync::Arc::new(BTreeSet::new()),
        plan_revision: plan.plan_revision,
        chat_rows: std::sync::Arc::new(m.chat_rows),
        document_row_map: std::sync::Arc::new(m.document_row_map),
    }
}

fn row_of(terminal: &TerminalState, view: &ProjectedViewport, raw: usize) -> usize {
    view.raw_to_view(RawCellOrigin {
        row: terminal.scrollback[raw].raw_row_id(),
        col: 0,
    })
    .unwrap()
    .row
}

fn assert_drag(terminal: &TerminalState, text: &[&str], origin: usize, target: usize) {
    let expected = text[origin.min(target)..=origin.max(target)].join("\n");
    assert_eq!(
        terminal.copy_projected_selection().as_deref(),
        Some(expected.as_str()),
        "whole original line must survive drag from raw row {origin} to {target}"
    );
    let selected = terminal.projected_selection.as_ref().unwrap();
    let expected_col = if target < origin {
        text[origin].chars().count() - 1
    } else {
        0
    };
    let (actual_row, actual_col) = match selected.anchor.anchor {
        ProjectedSelectionAnchor::Cell(cell) => (cell.row, cell.col),
        ProjectedSelectionAnchor::Row { row, col } => (row, col),
    };
    assert_eq!(
        (actual_row, actual_col),
        (terminal.scrollback[origin].raw_row_id(), expected_col),
        "the fixed endpoint must belong to the original raw line, never the current target"
    );
}

#[test]
fn projected_line_drag_long_short_reverse_keeps_original_line() {
    let text = ["UP", "ABCDEFGH", "XY"];
    let mut terminal = fixture(&text);
    let p = plan(&terminal, &text, &[None, Some(0), Some(1), Some(2)], 1);
    install(&mut terminal, p);
    let view = viewport(&terminal, 0);
    terminal.select_line_in_projection(&view, row_of(&terminal, &view, 1));
    for target in [2, 0] {
        terminal.extend_line_selection_in_projection(&view, row_of(&terminal, &view, target));
        assert_drag(&terminal, &text, 1, target);
    }
}

#[test]
fn projected_line_drag_repeated_reversals_and_same_row_keep_raw_anchor() {
    let text = ["UP", "ABCDEFGH", "X"];
    let mut terminal = fixture(&text);
    let p = plan(&terminal, &text, &[None, Some(0), Some(1), Some(2)], 1);
    install(&mut terminal, p);
    let view = viewport(&terminal, 0);
    terminal.select_line_in_projection(&view, row_of(&terminal, &view, 1));
    for target in [2, 0, 2, 1, 0, 1, 2, 0] {
        terminal.extend_line_selection_in_projection(&view, row_of(&terminal, &view, target));
        assert_drag(&terminal, &text, 1, target);
    }
}

#[test]
fn projected_line_drag_plan_chrome_reorder_preserves_original_raw_anchor() {
    let text = ["UP", "ABCDEFGH", "XY"];
    let mut terminal = fixture(&text);
    let p = plan(&terminal, &text, &[None, Some(0), Some(1), Some(2)], 1);
    install(&mut terminal, p);
    let view = viewport(&terminal, 0);
    terminal.select_line_in_projection(&view, row_of(&terminal, &view, 1));
    terminal.extend_line_selection_in_projection(&view, row_of(&terminal, &view, 2));
    for (revision, layout, target) in [
        (2, [Some(0), Some(1), None, Some(2)], 0),
        (3, [Some(0), None, Some(1), Some(2)], 2),
        (4, [None, Some(0), Some(1), Some(2)], 0),
    ] {
        let p = plan(&terminal, &text, &layout, revision);
        install(&mut terminal, p);
        let view = viewport(&terminal, 0);
        terminal.extend_line_selection_in_projection(&view, row_of(&terminal, &view, target));
        assert_drag(&terminal, &text, 1, target);
    }
}

#[test]
fn projected_line_drag_origin_outside_viewport_still_uses_full_raw_line() {
    let text = ["UP", "ABCDEFGH", "mid1", "mid2", "mid3", "XY"];
    let mut terminal = fixture(&text);
    let layout: Vec<_> = std::iter::once(None)
        .chain((0..text.len()).map(Some))
        .collect();
    let p = plan(&terminal, &text, &layout, 1);
    install(&mut terminal, p);
    let top = viewport(&terminal, 3);
    terminal.select_line_in_projection(&top, row_of(&terminal, &top, 1));
    let bottom = viewport(&terminal, 0);
    assert!(bottom
        .raw_to_view(RawCellOrigin {
            row: terminal.scrollback[1].raw_row_id(),
            col: 0
        })
        .is_none());
    terminal.extend_line_selection_in_projection(&bottom, row_of(&terminal, &bottom, 5));
    assert_drag(&terminal, &text, 1, 5);
    terminal.extend_line_selection_in_projection(&top, row_of(&terminal, &top, 0));
    assert_drag(&terminal, &text, 1, 0);
}

#[test]
fn projected_line_drag_stale_revision_still_fails_closed() {
    let text = ["UP", "ABCDEFGH", "XY"];
    let mut terminal = fixture(&text);
    let p = plan(&terminal, &text, &[None, Some(0), Some(1), Some(2)], 1);
    install(&mut terminal, p);
    let stale = viewport(&terminal, 0);
    terminal.select_line_in_projection(&stale, row_of(&terminal, &stale, 1));
    let mut p = plan(&terminal, &text, &[Some(0), None, Some(1), Some(2)], 2);
    // A source-driven rebuild may keep the policy revision unchanged. This
    // isolates the viewport's plan-revision guard from the policy guard.
    p.policy_revision = stale.policy_revision();
    install(&mut terminal, p);
    terminal.extend_line_selection_in_projection(&stale, 0);
    assert!(terminal.projected_selection.is_none());
    assert!(terminal.copy_projected_selection().is_none());
}

#[test]
fn projected_line_drag_joined_source_rows_reanchors_each_real_boundary() {
    let text = ["UP", "ABCD", "EFGH", "XY"];
    let mut terminal = fixture(&text);
    let mut p = plan(
        &terminal,
        &text,
        &[None, Some(0), Some(1), Some(2), Some(3)],
        1,
    );
    // A legal history reflow row consists of two independently tagged slices.
    // Declare the joined geometry directly; the literal expected line is not
    // derived from production placement or endpoint functions.
    let right = p.rows.remove(3);
    p.rows[2]
        .raw_slices
        .extend(right.raw_slices.into_iter().map(|mut slice| {
            slice.view_col_start += 4;
            slice
        }));
    for raw in &mut p.raw_rows {
        let row = match raw.absolute_row {
            0 => 1,
            1 | 2 => 2,
            3 => 3,
            _ => unreachable!(),
        };
        raw.first_view_row = Some(row);
        raw.last_view_row = Some(row);
    }
    install(&mut terminal, p);
    let view = viewport(&terminal, 0);
    terminal.select_line_in_projection(&view, row_of(&terminal, &view, 1));
    assert_eq!(
        terminal.copy_projected_selection().as_deref(),
        Some("ABCDEFGH")
    );
    terminal.extend_line_selection_in_projection(&view, row_of(&terminal, &view, 3));
    assert_eq!(
        terminal.copy_projected_selection().as_deref(),
        Some("ABCDEFGH\nXY")
    );

    for (revision, target) in [(2_u64, 0), (3, 3), (4, 0)] {
        let mut p = terminal
            .projection_plan_cache
            .as_ref()
            .unwrap()
            .1
            .as_ref()
            .clone();
        let spacer = p
            .rows
            .iter()
            .position(|row| matches!(row.kind, ProjectedRowKind::BlockSpacer { .. }))
            .unwrap();
        let host = p.rows.remove(spacer);
        p.rows
            .insert(if revision.is_multiple_of(2) { 2 } else { 0 }, host);
        for raw in &mut p.raw_rows {
            let row = p
                .rows
                .iter()
                .position(|row| {
                    row.raw_slices
                        .iter()
                        .any(|slice| slice.origin.is_some_and(|origin| origin.row == raw.raw_row))
                })
                .unwrap();
            raw.first_view_row = Some(row);
            raw.last_view_row = Some(row);
        }
        p.plan_revision = revision;
        p.policy_revision = revision;
        install(&mut terminal, p);
        let view = viewport(&terminal, 0);
        terminal.extend_line_selection_in_projection(&view, row_of(&terminal, &view, target));
        let (expected_text, expected_raw, expected_col) = if target == 0 {
            ("UP\nABCDEFGH", 2, 3)
        } else {
            ("ABCDEFGH\nXY", 1, 0)
        };
        assert_eq!(
            terminal.copy_projected_selection().as_deref(),
            Some(expected_text)
        );
        assert_eq!(
            terminal.projected_selection.as_ref().unwrap().anchor.anchor,
            ProjectedSelectionAnchor::Cell(RawCellOrigin {
                row: terminal.scrollback[expected_raw].raw_row_id(),
                col: expected_col,
            })
        );
    }
}

#[test]
fn projected_line_drag_source_revision_change_still_fails_closed() {
    let text = ["UP", "ABCDEFGH", "XY"];
    let mut terminal = fixture(&text);
    let p = plan(&terminal, &text, &[None, Some(0), Some(1), Some(2)], 1);
    install(&mut terminal, p);
    let stale = viewport(&terminal, 0);
    terminal.select_line_in_projection(&stale, row_of(&terminal, &stale, 1));
    terminal.history_revision += 1;
    terminal.extend_line_selection_in_projection(&stale, 0);
    assert!(terminal.projected_selection.is_none());
    assert!(terminal.copy_projected_selection().is_none());
}
