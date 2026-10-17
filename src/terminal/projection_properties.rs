//! Seeded properties over semantic cells and plans, never escape input.
//! This module is also used by scripts/test-projection-properties.py.
use super::*;

const SEED: u64 = 0x4652_4f53_5452_3032;
const WIDTHS: [usize; 12] = [1, 2, 3, 4, 5, 7, 8, 13, 16, 31, 64, 80];
pub(super) const CASES: usize = 10_000;
const STEPS: usize = 16;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Clone, Debug)]
pub(super) enum Op {
    Resize(usize),
    Toggle(usize),
    Mode(usize),
    Scroll(usize),
}

#[derive(Clone)]
pub(super) struct Raw {
    pub(super) id: RawRowId,
    pub(super) cells: Vec<TerminalCell>,
    wrapped: bool,
}

type Token = (RawCellOrigin, TerminalCell);

fn corpus(rng: &mut Rng) -> Vec<TerminalCell> {
    let mut cells = Vec::new();
    for _ in 0..(4 + rng.pick(7)) {
        let mut cell = TerminalCell::default();
        match rng.pick(7) {
            0 | 1 => {
                cell.character = if rng.pick(2) == 0 { '中' } else { '🙂' };
                cell.flags.set_wide(true);
                cells.push(cell);
                let mut continuation = TerminalCell::default();
                continuation.flags.set_wide_continuation(true);
                cells.push(continuation);
                continue;
            }
            // The native cell stores one scalar. These are the NFC results of
            // e+acute and A+ring; this is NOT a grapheme/parser-input test.
            2 => cell.character = if rng.pick(2) == 0 { 'é' } else { 'Å' },
            3 => {
                cell.background = Color::Indexed(17);
                cell.flags.set_italic(true);
            }
            4 => {
                cell.foreground = Color::Red;
                cell.flags.set_bold(true);
            }
            _ => cell.character = char::from(b'a' + rng.pick(26) as u8),
        }
        cells.push(cell);
    }
    // Both retained background spaces and structurally trimmed styled spaces.
    cells.push(TerminalCell {
        background: Color::Blue,
        ..Default::default()
    });
    cells.push(TerminalCell {
        foreground: Color::Red,
        ..Default::default()
    });
    cells.extend([TerminalCell::default(); 2]);
    cells
}

pub(super) fn fixture(case: usize) -> (TerminalState, Vec<Raw>, Vec<ResolvedCollapse>, Vec<Op>) {
    let mut rng = Rng(SEED.wrapping_add(case as u64));
    let mut terminal = TerminalState::new(16, 3);
    let mut raw = Vec::new();
    for index in 0..12 {
        let cells = corpus(&mut rng);
        let wrapped = index % 3 != 2 && rng.pick(2) == 0;
        let line = ScrollbackLine::compress(&cells, wrapped);
        assert_eq!(
            line.decompress(),
            cells,
            "compression must retain all styles"
        );
        raw.push(Raw {
            id: line.raw_row_id(),
            cells,
            wrapped,
        });
        terminal.scrollback.push_back(line);
    }
    let collapses = [2, 5, 8]
        .map(|index| ResolvedCollapse {
            range: FinishedOutputRange {
                zone_id: index as u64 + 1,
                start: RawCellBoundary {
                    row: raw[index].id,
                    col: 0,
                },
                end: RawCellBoundary {
                    row: raw[index + 1].id,
                    col: raw[index + 1].cells.len(),
                },
            },
            start_absolute: index,
            end_absolute: index + 1,
        })
        .to_vec();
    let ops = (0..STEPS)
        .map(|step| match if step < 4 { step } else { rng.pick(4) } {
            0 => Op::Resize(WIDTHS[rng.pick(WIDTHS.len())]),
            1 => Op::Toggle(rng.pick(collapses.len())),
            2 => Op::Mode(rng.pick(3)),
            _ => Op::Scroll(rng.pick(512)),
        })
        .collect();
    (terminal, raw, collapses, ops)
}

// Independent eager oracle: expand tagged input cells, discard only structural
// tails, then pack whole glyphs. Never reads plan slices, layout metadata or a
// production mapping function.
fn eager_rows(raw: &[Raw], width: usize) -> Vec<Vec<Token>> {
    let mut result = Vec::new();
    let mut logical = Vec::new();
    for (index, row) in raw.iter().enumerate() {
        let mut end = row.cells.len();
        while end > 0 {
            let c = row.cells[end - 1];
            if c.character != ' '
                || c.background != Color::Default
                || c.flags.wide()
                || c.flags.wide_continuation()
                || c.hyperlink != 0
            {
                break;
            }
            end -= 1;
        }
        logical.extend(
            row.cells[..end]
                .iter()
                .enumerate()
                .map(|(col, cell)| (RawCellOrigin { row: row.id, col }, *cell)),
        );
        if row.wrapped && index + 1 < raw.len() {
            continue;
        }
        let mut output = Vec::new();
        let mut cursor = 0;
        while cursor < logical.len() {
            let mut token = logical[cursor];
            let wide = token.1.flags.wide();
            let glyph_len = if wide { 2 } else { 1 };
            if width == 1 {
                token.1.flags.set_wide(false);
                result.push(vec![token]);
            } else {
                if output.len() + glyph_len > width {
                    result.push(std::mem::take(&mut output));
                }
                output.extend_from_slice(&logical[cursor..cursor + glyph_len]);
                if output.len() == width {
                    result.push(std::mem::take(&mut output));
                }
            }
            cursor += glyph_len;
        }
        if !output.is_empty() || logical.is_empty() {
            result.push(output);
        }
        logical.clear();
    }
    result
}

fn hidden(origin: RawCellOrigin, raw: &[Raw], collapses: &[ResolvedCollapse]) -> bool {
    let row = raw.iter().position(|r| r.id == origin.row).unwrap();
    collapses.iter().any(|c| {
        row >= c.start_absolute
            && row <= c.end_absolute
            && (row > c.start_absolute || origin.col >= c.range.start.col)
            && (row < c.end_absolute || origin.col < c.range.end.col)
    })
}

fn viewport(
    terminal: &TerminalState,
    plan: &ProjectionPlan,
    offset: usize,
    rows: usize,
) -> ProjectedViewport {
    let m = terminal.materialize_projection_plan(plan, offset, rows, plan.cols);
    let (spans, row_index) = TerminalState::projection_indexes(&m.provenance);
    ProjectedViewport {
        cells: m.cells.into(),
        row_wrapped: m.row_wrapped.into(),
        row_kinds: m.row_kinds.into(),
        provenance: std::sync::Arc::new(m.provenance),
        raw_span_index: spans.into(),
        raw_row_index: row_index.into(),
        source_revision: ProjectionSourceRevision {
            grid: 1,
            history: 1,
            row_identity: 1,
            alternate_screen: false,
        },
        view_revision: 1,
        identity_fast_path: false,
        mode: ProjectionMode::Transformed,
        scroll_offset: m.scroll_offset,
        policy_revision: plan.policy_revision,
        policy_ids: Vec::new().into(),
        block_spacing: false,
        document_rows: plan.rows.len(),
        document_start: m.document_start,
        top_padding: m.top_padding,
        effective_collapsed: plan.effective_collapsed.clone().into(),
        plan_revision: plan.plan_revision,
        chat_rows: m.chat_rows.into(),
        document_row_map: m.document_row_map.into(),
    }
}

fn selected_all(plan: &ProjectionPlan) -> ProjectedSelection {
    let anchor = ProjectedSelectionAnchor::Row {
        row: RawRowId::UNTRACKED,
        col: 0,
    };
    ProjectedSelection {
        plan_revision: plan.plan_revision,
        plan_cols: plan.cols,
        hidden: plan.effective_collapsed.clone().into(),
        anchor: ProjectedSelectionEndpoint {
            point: (0, 0),
            anchor,
        },
        active: ProjectedSelectionEndpoint {
            point: (plan.rows.len() - 1, plan.cols - 1),
            anchor,
        },
        mode: SelectionMode::Normal,
    }
}

fn check_case(case: usize, limit: usize) {
    let (terminal, raw, ranges, ops) = fixture(case);
    let mut width = 16;
    let mut mask = [false; 3];
    let mut mode = 0;
    let mut offset = 0;
    for (step, op) in ops.iter().take(limit).enumerate() {
        match *op {
            Op::Resize(w) => width = w,
            Op::Toggle(i) => mask[i] = !mask[i],
            Op::Mode(m) => mode = m,
            Op::Scroll(n) => offset = n,
        }
        let context = format!(
            "seed={SEED:#x} case={case} prefix={} ops={:?}",
            step + 1,
            &ops[..=step]
        );
        let active: Vec<_> = ranges
            .iter()
            .enumerate()
            .filter(|(i, _)| mask[*i] && mode != 0)
            .map(|(_, c)| *c)
            .collect();
        // Semantic mode 0 is raw; 1 is collapsed/spaced; 2 adds host chrome.
        // App mode eligibility and cache admission are tested separately.
        let mut plan = ProjectionPlan::identity(
            terminal
                .scrollback
                .iter()
                .enumerate()
                .map(|(i, r)| r.layout(i)),
            [],
            width,
        )
        .splice_collapses(&active, step as u64 + 1);
        if mode != 0 {
            plan = plan.splice_block_spacing(&raw.iter().map(|r| r.id).collect::<Vec<_>>());
        }
        if mode == 2 {
            plan = plan.splice_native_chrome(
                &raw.iter()
                    .map(|r| ChromeBoundary {
                        next: r.id,
                        kind: ChromeKind::Organism,
                        rows: 2,
                    })
                    .collect::<Vec<_>>(),
                0,
                7,
            );
        }
        plan.plan_revision = step as u64 + 1;
        let eager = eager_rows(&raw, width);
        let expected: Vec<_> = eager
            .iter()
            .flatten()
            .filter(|(o, _)| !hidden(*o, &raw, &active))
            .copied()
            .collect();
        // Inspect every row once: content tags come from the fixture, not from
        // round-tripping two potentially identically broken mapping functions.
        let full = viewport(&terminal, &plan, 0, plan.rows.len());
        let mut actual = Vec::new();
        for (row, cells) in full.cells().iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                let view = ViewportCell { row, col };
                if let Some(origin) = full.view_to_raw(view) {
                    actual.push((origin, *cell));
                    assert_eq!(full.raw_to_view(origin), Some(view), "{context}");
                } else {
                    assert_eq!(*cell, TerminalCell::default(), "{context}: chrome/padding");
                }
            }
        }
        assert_eq!(
            actual, expected,
            "{context}: independently tagged cell order"
        );
        if mode == 0 {
            assert_eq!(plan.rows.len(), eager.len(), "{context}");
            for (row, tokens) in eager.iter().enumerate() {
                for (col, (origin, cell)) in tokens.iter().enumerate() {
                    assert_eq!(
                        full.view_to_raw(ViewportCell { row, col }),
                        Some(*origin),
                        "{context}"
                    );
                    assert_eq!(full.cells()[row][col], *cell, "{context}");
                }
            }
        }
        // Check persistent raw anchors through every resize/collapse/mode step.
        for origin in eager.iter().flatten().map(|(o, _)| *o) {
            if hidden(origin, &raw, &active) {
                let row = plan
                    .document_row_for_anchor(ProjectedTopAnchor::RawCell(origin))
                    .expect(&context);
                let owner = active
                    .iter()
                    .find(|c| hidden(origin, &raw, &[**c]))
                    .unwrap();
                assert!(
                    matches!(plan.rows[row].kind, ProjectedRowKind::CollapsedSummary { key, .. } if key.zone_id == owner.range.zone_id),
                    "{context}"
                );
            } else {
                let point = plan
                    .selection_point_for_anchor(ProjectedSelectionAnchor::Cell(origin))
                    .expect(&context);
                assert_eq!(
                    full.view_to_raw(ViewportCell {
                        row: point.0,
                        col: point.1
                    }),
                    Some(origin),
                    "{context}: identity jump"
                );
            }
        }
        let visible = viewport(&terminal, &plan, offset, 7);
        for row in 0..7 {
            for col in 0..width {
                let point = ViewportCell { row, col };
                if let Some(origin) = visible.view_to_raw(point) {
                    assert_eq!(visible.raw_to_view(origin), Some(point), "{context}");
                    let source = expected
                        .iter()
                        .find(|(o, _)| *o == origin)
                        .expect(&context)
                        .1;
                    assert_eq!(visible.cells()[row][col], source, "{context}");
                }
            }
        }
        // Exact pure text oracle from validated dense rows. Synthetic rows
        // contribute no bytes; a hidden segment forces a hard boundary.
        let mut text = String::new();
        let mut pending = false;
        for row in 0..full.cells().len() {
            match full.row_kinds()[row] {
                ProjectedRowKind::CollapsedSummary { .. } => {
                    pending = !text.ends_with('\n');
                }
                ProjectedRowKind::Raw => {
                    if pending {
                        text.push('\n');
                        pending = false;
                    }
                    let mut had_cell = false;
                    for col in 0..width {
                        if full.view_to_raw(ViewportCell { row, col }).is_some() {
                            had_cell = true;
                            let cell = full.cells()[row][col];
                            if !cell.flags.wide_continuation() {
                                text.push(cell.character);
                            }
                        }
                    }
                    if !had_cell && full.view_row_absolute(row).is_some() {
                        text.extend(std::iter::repeat_n(' ', width));
                    }
                    if row + 1 < full.cells().len() && !full.row_wrapped()[row] {
                        text.push('\n');
                        pending = false;
                    }
                }
                _ => {}
            }
        }
        let copied = copy_projected_cells(
            &plan,
            &selected_all(&plan),
            &terminal.scrollback,
            &terminal.grid,
        );
        assert_eq!(
            copied.as_deref(),
            Some(text.as_str()),
            "{context}: copy includes chrome/hidden content"
        );
    }
}

#[test]
fn seeded_projection_ten_thousand_sequences() {
    let replay = std::env::var("FROST_PROJECTION_CASE")
        .ok()
        .map(|s| s.parse::<usize>().unwrap());
    let limit = std::env::var("FROST_PROJECTION_STEPS")
        .ok()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(STEPS)
        .min(STEPS);
    let cases = replay.map_or(0..CASES, |case| case..case + 1);
    for case in cases {
        check_case(case, limit);
    }
    println!("projection properties: seed={SEED:#x}, cases={}, steps={limit}, widths={WIDTHS:?}; replay with FROST_PROJECTION_CASE and shrink prefix with FROST_PROJECTION_STEPS", if replay.is_some() { 1 } else { CASES });
}

#[test]
fn fixed_viewport_work_is_independent_of_retained_history() {
    let mut measurements = Vec::new();
    for depth in [128, 4096, 65_536] {
        let mut terminal = TerminalState::new(16, 7);
        let mut cells = vec![TerminalCell::default(); 16];
        cells[0].character = '中';
        cells[0].flags.set_wide(true);
        cells[1].flags.set_wide_continuation(true);
        cells[15].background = Color::Blue;
        for _ in 0..depth {
            terminal
                .scrollback
                .push_back(ScrollbackLine::compress(&cells, false));
        }
        PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|c| c.set(0));
        PROJECTION_HISTORY_LAYOUT_ROWS.with(|c| c.set(0));
        let plan = terminal.identity_projection_plan(16);
        assert_eq!(PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|c| c.get()), 0);
        assert_eq!(
            PROJECTION_HISTORY_LAYOUT_ROWS.with(|c| c.get()),
            depth,
            "cold plan is honestly linear"
        );
        let mut work = Vec::new();
        for offset in [7, depth / 2, depth - 7] {
            PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|c| c.set(0));
            PROJECTION_HISTORY_LAYOUT_ROWS.with(|c| c.set(0));
            PROJECTED_HISTORY_LAYOUT_BYTE_SCAN_COUNT.with(|c| c.set(0));
            PROJECTION_MATERIALIZED_ROWS.with(|c| c.set(0));
            PROJECTION_MATERIALIZED_SLICES.with(|c| c.set(0));
            let view = viewport(&terminal, &plan, offset, 7);
            assert_eq!(view.cells().len(), 7);
            let observed = (
                PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|c| c.get()),
                PROJECTION_HISTORY_LAYOUT_ROWS.with(|c| c.get()),
                PROJECTED_HISTORY_LAYOUT_BYTE_SCAN_COUNT.with(|c| c.get()),
                PROJECTION_MATERIALIZED_ROWS.with(|c| c.get()),
                PROJECTION_MATERIALIZED_SLICES.with(|c| c.get()),
            );
            assert_eq!(observed, (7, 0, 0, 7, 7));
            work.push(observed);
        }
        println!("history={depth}: cold_layout_rows={depth}, warm (decode/layout/bytes/rows/slices)={work:?}");
        measurements.push(work);
    }
    assert!(measurements.windows(2).all(|w| w[0] == w[1]));
}

fn ascii_row(text: &str, wrapped: bool) -> ScrollbackLine {
    ScrollbackLine::compress(
        &text
            .chars()
            .map(|character| TerminalCell {
                character,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
        wrapped,
    )
}

#[test]
fn selection_anchor_uses_projected_columns_after_join_and_split() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.scrollback.push_back(ascii_row("AB", true));
    terminal.scrollback.push_back(ascii_row("CDEF", false));
    let id = terminal.scrollback[1].raw_row_id();
    // Independent, hand-written expectations: raw C/E/F columns are 0/2/3.
    for (width, raw_col, expected) in [
        (8, 0, (0, 2)),
        (8, 3, (0, 5)),
        (2, 3, (2, 1)),
        (3, 2, (1, 1)),
    ] {
        let plan = ProjectionPlan::identity(
            terminal
                .scrollback
                .iter()
                .enumerate()
                .map(|(i, r)| r.layout(i)),
            [],
            width,
        );
        let origin = RawCellOrigin {
            row: id,
            col: raw_col,
        };
        assert_eq!(
            plan.selection_point_for_anchor(ProjectedSelectionAnchor::Cell(origin)),
            Some(expected)
        );
        let view = viewport(&terminal, &plan, 0, plan.rows.len());
        assert_eq!(
            view.view_to_raw(ViewportCell {
                row: expected.0,
                col: expected.1
            }),
            Some(origin)
        );
    }
}

#[test]
fn collapsed_prefix_selection_never_retargets_visible_suffix() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.scrollback.push_back(ascii_row("ABCDEFGH", false));
    let id = terminal.scrollback[0].raw_row_id();
    let collapse = ResolvedCollapse {
        range: FinishedOutputRange {
            zone_id: 1,
            start: RawCellBoundary { row: id, col: 0 },
            end: RawCellBoundary { row: id, col: 2 },
        },
        start_absolute: 0,
        end_absolute: 0,
    };
    let plan = ProjectionPlan::identity(
        terminal
            .scrollback
            .iter()
            .enumerate()
            .map(|(i, r)| r.layout(i)),
        [],
        8,
    )
    .splice_collapses(&[collapse], 1);
    for col in 0..2 {
        assert_eq!(
            plan.selection_point_for_anchor(ProjectedSelectionAnchor::Cell(RawCellOrigin {
                row: id,
                col
            })),
            None,
            "a hidden cell is not trailing raw whitespace"
        );
    }
    assert_eq!(
        copy_projected_cells(
            &plan,
            &selected_all(&plan),
            &terminal.scrollback,
            &terminal.grid
        ),
        Some("\nCDEFGH".to_owned())
    );
}

#[test]
fn materializer_decodes_each_visible_source_once_when_rows_join() {
    let mut terminal = TerminalState::new(80, 2);
    for i in 0..160 {
        terminal.scrollback.push_back(ascii_row("x", i != 159));
    }
    let plan = ProjectionPlan::identity(
        terminal
            .scrollback
            .iter()
            .enumerate()
            .map(|(i, r)| r.layout(i)),
        [],
        80,
    );
    PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|c| c.set(0));
    PROJECTION_HISTORY_LAYOUT_ROWS.with(|c| c.set(0));
    PROJECTION_MATERIALIZED_ROWS.with(|c| c.set(0));
    PROJECTION_MATERIALIZED_SLICES.with(|c| c.set(0));
    let view = viewport(&terminal, &plan, 0, 2);
    assert_eq!(view.cells().len(), 2);
    assert_eq!(PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|c| c.get()), 160);
    assert_eq!(PROJECTION_HISTORY_LAYOUT_ROWS.with(|c| c.get()), 0);
    assert_eq!(PROJECTION_MATERIALIZED_ROWS.with(|c| c.get()), 2);
    assert_eq!(PROJECTION_MATERIALIZED_SLICES.with(|c| c.get()), 160);
}

#[test]
fn selected_text_covers_live_wide_partial_reverse_block_and_chrome() {
    let mut terminal = TerminalState::new(8, 2);
    for (col, ch) in ['X', '中', ' ', 'Y', 'é'].into_iter().enumerate() {
        terminal.grid.get_mut(0, col).character = ch;
    }
    terminal.grid.get_mut(0, 1).flags.set_wide(true);
    terminal
        .grid
        .get_mut(0, 2)
        .flags
        .set_wide_continuation(true);
    for (col, ch) in "ABCDEFGH".chars().enumerate() {
        terminal.grid.get_mut(1, col).character = ch;
    }
    let id = terminal.grid.raw_row_id(1).unwrap();
    let plan = terminal.identity_projection_plan(8).splice_native_chrome(
        &[ChromeBoundary {
            next: id,
            kind: ChromeKind::Organism,
            rows: 2,
        }],
        0,
        2,
    );
    let mut selection = selected_all(&plan);
    for (start, end, mode, expected) in [
        ((0, 1), (0, 2), SelectionMode::Normal, "中"),
        ((0, 2), (0, 2), SelectionMode::Normal, ""),
        ((0, 4), (0, 1), SelectionMode::Normal, "中Yé"),
        ((0, 1), (3, 3), SelectionMode::Block, "中Y\nBCD"),
    ] {
        selection.anchor.point = start;
        selection.active.point = end;
        selection.mode = mode;
        assert_eq!(
            copy_projected_cells(&plan, &selection, &terminal.scrollback, &terminal.grid)
                .as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn hidden_guard_preserves_suffix_and_trailing_blank_fallback() {
    let mut terminal = TerminalState::new(12, 2);
    terminal
        .scrollback
        .push_back(ascii_row("ABCDEFGH    ", false));
    let id = terminal.scrollback[0].raw_row_id();
    let collapse = |end| ResolvedCollapse {
        range: FinishedOutputRange {
            zone_id: 1,
            start: RawCellBoundary { row: id, col: 0 },
            end: RawCellBoundary { row: id, col: end },
        },
        start_absolute: 0,
        end_absolute: 0,
    };
    let build = |end| {
        ProjectionPlan::identity(
            terminal
                .scrollback
                .iter()
                .enumerate()
                .map(|(i, r)| r.layout(i)),
            [],
            12,
        )
        .splice_collapses(&[collapse(end)], 1)
    };
    let plan = build(2);
    let view = viewport(&terminal, &plan, 0, plan.rows.len());
    for col in 2..8 {
        let origin = RawCellOrigin { row: id, col };
        let point = plan
            .selection_point_for_anchor(ProjectedSelectionAnchor::Cell(origin))
            .unwrap();
        assert_eq!(
            view.view_to_raw(ViewportCell {
                row: point.0,
                col: point.1
            }),
            Some(origin)
        );
    }
    assert_eq!(
        plan.selection_point_for_anchor(ProjectedSelectionAnchor::Cell(RawCellOrigin {
            row: id,
            col: 10
        })),
        Some((1, 10))
    );
    for col in 0..12 {
        assert_eq!(
            view.view_to_raw(ViewportCell { row: 0, col }),
            None,
            "summary has no raw ownership"
        );
    }
    let all_hidden = build(8);
    for col in 0..8 {
        assert_eq!(
            all_hidden.selection_point_for_anchor(ProjectedSelectionAnchor::Cell(RawCellOrigin {
                row: id,
                col
            })),
            None
        );
    }
    assert_eq!(
        copy_projected_cells(
            &all_hidden,
            &selected_all(&all_hidden),
            &terminal.scrollback,
            &terminal.grid
        ),
        Some(String::new())
    );
}

#[test]
fn wide_collapse_boundaries_and_adjacent_ranges_preserve_survivors() {
    let mut terminal = TerminalState::new(8, 2);
    let mut cells = vec![TerminalCell::default(); 8];
    for (col, ch) in ['A', '中', ' ', 'B', '🙂', ' ', 'C', 'D']
        .into_iter()
        .enumerate()
    {
        cells[col].character = ch;
    }
    for col in [1, 4] {
        cells[col].flags.set_wide(true);
        cells[col + 1].flags.set_wide_continuation(true);
    }
    terminal
        .scrollback
        .push_back(ScrollbackLine::compress(&cells, false));
    let id = terminal.scrollback[0].raw_row_id();
    for width in [1, 2, 3, 8] {
        let collapse = |zone_id, start, end| ResolvedCollapse {
            range: FinishedOutputRange {
                zone_id,
                start: RawCellBoundary {
                    row: id,
                    col: start,
                },
                end: RawCellBoundary { row: id, col: end },
            },
            start_absolute: 0,
            end_absolute: 0,
        };
        let plan = ProjectionPlan::identity(
            terminal
                .scrollback
                .iter()
                .enumerate()
                .map(|(i, r)| r.layout(i)),
            [],
            width,
        )
        .splice_collapses(&[collapse(1, 1, 3), collapse(2, 3, 6)], 1);
        let view = viewport(&terminal, &plan, 0, plan.rows.len());
        for col in [0, 6, 7] {
            let origin = RawCellOrigin { row: id, col };
            let point = plan
                .selection_point_for_anchor(ProjectedSelectionAnchor::Cell(origin))
                .unwrap();
            assert_eq!(
                view.view_to_raw(ViewportCell {
                    row: point.0,
                    col: point.1
                }),
                Some(origin)
            );
            assert_eq!(view.cells()[point.0][point.1], cells[col]);
        }
        for col in 1..6 {
            assert_eq!(
                plan.selection_point_for_anchor(ProjectedSelectionAnchor::Cell(RawCellOrigin {
                    row: id,
                    col
                })),
                None
            );
        }
        assert_eq!(
            plan.rows
                .iter()
                .filter(|r| matches!(r.kind, ProjectedRowKind::CollapsedSummary { .. }))
                .count(),
            2
        );
    }
}
