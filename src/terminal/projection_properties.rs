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

// Independent dense source-token model; no production plan/mapping input.
#[derive(Clone, Copy, Debug)]
struct DenseCollapseRange {
    zone: u64,
    start: (usize, usize),
    end: (usize, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DenseCollapseRow {
    Raw {
        source_row: usize,
        start: usize,
        end: usize,
        wrapped: bool,
    },
    Summary {
        zone: u64,
        hidden_rows: usize,
    },
}

fn dense_collapse_rows(
    original: &[Vec<TerminalCell>],
    wrapped: &[bool],
    ranges: &[DenseCollapseRange],
) -> Vec<DenseCollapseRow> {
    assert!(!original.is_empty());
    let width = original[0].len();
    assert!(width > 0 && wrapped.len() == original.len());
    for row in original {
        assert_eq!(row.len(), width);
        for (col, cell) in row.iter().enumerate() {
            assert!(!cell.character.is_control());
            assert!(!(cell.flags.wide() && cell.flags.wide_continuation()));
            if cell.flags.wide() {
                assert!(col + 1 < width && row[col + 1].flags.wide_continuation());
            }
            if cell.flags.wide_continuation() {
                assert!(col > 0 && row[col - 1].flags.wide());
            }
        }
    }
    let mut owners = vec![vec![None; width]; original.len()];
    let mut previous_end = (0, 0);
    let mut zones = BTreeSet::new();
    for range in ranges {
        assert!(range.start.0 < original.len() && range.end.0 < original.len());
        assert!(range.start.1 <= width && range.end.1 <= width);
        let start = range.start.0 * width + range.start.1;
        let end = range.end.0 * width + range.end.1;
        assert!(start < end, "collapse must own at least one dense cell");
        assert!(
            range.start >= previous_end,
            "admitted tuple intervals must not overlap"
        );
        assert!(zones.insert(range.zone));
        for &(row, col) in &[range.start, range.end] {
            assert!(col == width || !original[row][col].flags.wide_continuation());
        }
        previous_end = range.end;
        for (row, labels) in owners.iter_mut().enumerate() {
            for (col, owner) in labels.iter_mut().enumerate() {
                if (start..end).contains(&(row * width + col)) {
                    assert!(owner.replace(range.zone).is_none());
                }
            }
        }
    }
    let mut document = Vec::new();
    let mut emitted = BTreeSet::new();
    for (row, labels) in owners.iter().enumerate() {
        let mut start = 0;
        while start < width {
            let owner = labels[start];
            let end = (start + 1..width)
                .find(|&col| labels[col] != owner)
                .unwrap_or(width);
            if let Some(zone) = owner {
                if emitted.insert(zone) {
                    let hidden_rows = owners
                        .iter()
                        .filter(|line| line.contains(&Some(zone)))
                        .count();
                    document.push(DenseCollapseRow::Summary { zone, hidden_rows });
                }
            } else {
                document.push(DenseCollapseRow::Raw {
                    source_row: row,
                    start,
                    end,
                    wrapped: end == width && wrapped[row],
                });
            }
            start = end;
        }
    }
    document
}

fn dense_visible_point(
    rows: &[DenseCollapseRow],
    source: (usize, usize),
) -> Option<(usize, usize)> {
    rows.iter().enumerate().find_map(|(index, row)| match row {
        DenseCollapseRow::Raw {
            source_row,
            start,
            end,
            ..
        } if *source_row == source.0 && (*start..*end).contains(&source.1) => {
            Some((index, source.1))
        }
        _ => None,
    })
}

fn dense_collapse_text(
    rows: &[DenseCollapseRow],
    original: &[Vec<TerminalCell>],
    anchor: (usize, usize),
    active: (usize, usize),
    mode: SelectionMode,
) -> String {
    let (first, last) = (anchor.min(active), anchor.max(active));
    let mut fragments = Vec::new();
    for (index, row) in rows.iter().enumerate().take(last.0 + 1).skip(first.0) {
        if let DenseCollapseRow::Raw {
            source_row,
            start,
            end,
            wrapped,
        } = row
        {
            let text: String = (*start..*end)
                .filter(|&col| match mode {
                    SelectionMode::Normal => (first..=last).contains(&(index, col)),
                    SelectionMode::Block => {
                        (anchor.1.min(active.1)..=anchor.1.max(active.1)).contains(&col)
                    }
                })
                .filter_map(|col| {
                    let cell = original[*source_row][col];
                    (!cell.flags.wide_continuation()).then_some(cell.character)
                })
                .collect();
            // An empty rectangle intersection is still a traversed raw row.
            fragments.push((index, *wrapped, text));
        }
    }
    let mut output = String::new();
    for (index, fragment) in fragments.iter().enumerate() {
        if let Some(previous) = index.checked_sub(1).map(|i| &fragments[i]) {
            let summary_gap = rows[previous.0 + 1..fragment.0]
                .iter()
                .any(|row| matches!(row, DenseCollapseRow::Summary { .. }));
            if mode == SelectionMode::Block || !previous.1 || summary_gap {
                output.push('\n');
            }
        }
        output.push_str(&fragment.2);
    }
    output
}

fn dense_ascii(rows: &[&str]) -> Vec<Vec<TerminalCell>> {
    rows.iter()
        .map(|row| {
            row.chars()
                .map(|character| TerminalCell {
                    character,
                    ..Default::default()
                })
                .collect()
        })
        .collect()
}

type DenseLiteralText<'a> = ((usize, usize), (usize, usize), SelectionMode, &'a str);

fn check_dense_collapse_case(
    name: &str,
    original: Vec<Vec<TerminalCell>>,
    wrapped: &[bool],
    ranges: &[DenseCollapseRange],
    literal_rows: &[DenseCollapseRow],
    literal_texts: &[DenseLiteralText<'_>],
) {
    let width = original[0].len();
    assert!(original.iter().all(|row| row.len() == width));
    assert_eq!(wrapped.len(), original.len());
    let expected = dense_collapse_rows(&original, wrapped, ranges);
    assert_eq!(
        expected, literal_rows,
        "{name}: model must match hand-written geometry"
    );
    for &(a, b, mode, text) in literal_texts {
        let a = dense_visible_point(&expected, a).unwrap();
        let b = dense_visible_point(&expected, b).unwrap();
        assert_eq!(
            dense_collapse_text(&expected, &original, a, b, mode),
            text,
            "{name}: model must match hand-written text"
        );
    }
    let mut terminal = TerminalState::new(width, original.len());
    for (row, cells) in original.iter().enumerate() {
        terminal.grid.set_row(row, cells.clone());
        terminal.grid.row_wrapped[row] = wrapped[row];
    }
    let ids: Vec<_> = (0..original.len())
        .map(|row| terminal.grid.raw_row_id(row).unwrap())
        .collect();
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), ids.len());
    let resolved: Vec<_> = ranges
        .iter()
        .map(|range| ResolvedCollapse {
            range: FinishedOutputRange {
                zone_id: range.zone,
                start: RawCellBoundary {
                    row: ids[range.start.0],
                    col: range.start.1,
                },
                end: RawCellBoundary {
                    row: ids[range.end.0],
                    col: range.end.1,
                },
            },
            start_absolute: range.start.0,
            end_absolute: range.end.0,
        })
        .collect();
    let plan = terminal
        .identity_projection_plan(width)
        .splice_collapses(&resolved, 42);
    assert_eq!(
        plan.rows.len(),
        expected.len(),
        "{name}: document row count"
    );
    let view = viewport(&terminal, &plan, 0, plan.rows.len());
    for (row, model) in expected.iter().enumerate() {
        match model {
            DenseCollapseRow::Raw { wrapped, .. } => {
                assert_eq!(plan.rows[row].kind, ProjectedRowKind::Raw, "{name}");
                assert_eq!(
                    plan.rows[row].wrapped, *wrapped,
                    "{name}: fragment hard/soft boundary"
                );
            }
            DenseCollapseRow::Summary { zone, hidden_rows } => match plan.rows[row].kind {
                ProjectedRowKind::CollapsedSummary {
                    key,
                    hidden_range,
                    hidden_display_rows,
                } => {
                    assert_eq!(key.zone_id, *zone, "{name}");
                    assert_eq!(key.policy_revision, 42);
                    assert_eq!(
                        hidden_display_rows, *hidden_rows,
                        "{name}: hidden physical row count"
                    );
                    assert_eq!(
                        hidden_range,
                        resolved
                            .iter()
                            .find(|range| range.range.zone_id == *zone)
                            .unwrap()
                            .range
                    );
                }
                other => panic!("{name}: expected summary, got {other:?}"),
            },
        }
        for (col, _) in original[0].iter().enumerate() {
            let source = match model {
                DenseCollapseRow::Raw {
                    source_row,
                    start,
                    end,
                    ..
                } if (*start..*end).contains(&col) => Some(*source_row),
                _ => None,
            };
            let origin = source.map(|source_row| RawCellOrigin {
                row: ids[source_row],
                col,
            });
            assert_eq!(
                view.view_to_raw(ViewportCell { row, col }),
                origin,
                "{name}: doc={row}:{col}"
            );
            let cell = source.map_or(TerminalCell::default(), |source_row| {
                original[source_row][col]
            });
            assert_eq!(
                view.cells()[row][col],
                cell,
                "{name}: materialized doc={row}:{col}"
            );
        }
    }
    let mut visible = Vec::new();
    for (source_row, id) in ids.iter().copied().enumerate() {
        for col in 0..width {
            let point = dense_visible_point(&expected, (source_row, col));
            let origin = RawCellOrigin { row: id, col };
            assert_eq!(
                plan.selection_point_for_anchor(ProjectedSelectionAnchor::Cell(origin)),
                point,
                "{name}: selectable source={source_row}:{col}"
            );
            if let Some(point) = point {
                visible.push(point);
            } else {
                let owner = ranges
                    .iter()
                    .find(|range| {
                        let position = source_row * width + col;
                        (range.start.0 * width + range.start.1..range.end.0 * width + range.end.1)
                            .contains(&position)
                    })
                    .unwrap();
                let summary = expected.iter().position(|row| matches!(row, DenseCollapseRow::Summary { zone, .. } if *zone == owner.zone)).unwrap();
                assert_eq!(
                    plan.document_row_for_anchor(ProjectedTopAnchor::RawCell(origin)),
                    Some(summary),
                    "{name}: hidden ownership"
                );
            }
        }
    }
    if visible.is_empty() {
        assert_eq!(
            copy_projected_cells(
                &plan,
                &selected_all(&plan),
                &terminal.scrollback,
                &terminal.grid
            ),
            Some(String::new()),
            "{name}: all hidden"
        );
    }
    for &a in &visible {
        for &b in &visible {
            for mode in [SelectionMode::Normal, SelectionMode::Block] {
                let mut selection = selected_all(&plan);
                selection.anchor.point = a;
                selection.active.point = b;
                selection.mode = mode;
                let expected_text = dense_collapse_text(&expected, &original, a, b, mode);
                assert_eq!(
                    copy_projected_cells(&plan, &selection, &terminal.scrollback, &terminal.grid),
                    Some(expected_text),
                    "{name}: endpoints={a:?}..{b:?} mode={mode:?}"
                );
            }
        }
    }
}

#[test]
fn partial_collapse_dense_model_matches_literal_counterexamples() {
    use DenseCollapseRow::{Raw, Summary};
    let raw = |source_row, start, end, wrapped| Raw {
        source_row,
        start,
        end,
        wrapped,
    };
    let range = |zone, start, end| DenseCollapseRange { zone, start, end };
    check_dense_collapse_case(
        "single-row partial",
        dense_ascii(&["ABCDEF"]),
        &[false],
        &[range(1, (0, 2), (0, 4))],
        &[
            raw(0, 0, 2, false),
            Summary {
                zone: 1,
                hidden_rows: 1,
            },
            raw(0, 4, 6, false),
        ],
        &[
            ((0, 0), (0, 5), SelectionMode::Normal, "AB\nEF"),
            ((0, 1), (0, 4), SelectionMode::Block, "B\nE"),
        ],
    );
    check_dense_collapse_case(
        "multi-row partial",
        dense_ascii(&["ABCDEF", "GHIJKL", "MNOPQR"]),
        &[true, true, false],
        &[range(2, (1, 1), (2, 3))],
        &[
            raw(0, 0, 6, true),
            raw(1, 0, 1, false),
            Summary {
                zone: 2,
                hidden_rows: 2,
            },
            raw(2, 3, 6, false),
        ],
        &[((0, 4), (2, 5), SelectionMode::Normal, "EFG\nPQR")],
    );
    check_dense_collapse_case(
        "hidden soft continuation",
        dense_ascii(&["ABCDEF", "GHIJKL", "MNOPQR"]),
        &[true, true, false],
        &[range(3, (1, 0), (1, 6))],
        &[
            raw(0, 0, 6, true),
            Summary {
                zone: 3,
                hidden_rows: 1,
            },
            raw(2, 0, 6, false),
        ],
        &[((0, 4), (2, 2), SelectionMode::Normal, "EF\nMNO")],
    );
    check_dense_collapse_case(
        "adjacent distinct zones",
        dense_ascii(&["ABCDEFGH"]),
        &[false],
        &[range(7, (0, 1), (0, 3)), range(8, (0, 3), (0, 6))],
        &[
            raw(0, 0, 1, false),
            Summary {
                zone: 7,
                hidden_rows: 1,
            },
            Summary {
                zone: 8,
                hidden_rows: 1,
            },
            raw(0, 6, 8, false),
        ],
        &[((0, 0), (0, 7), SelectionMode::Normal, "A\nGH")],
    );
    check_dense_collapse_case(
        "all hidden including blanks",
        dense_ascii(&["ABCDEF", "      "]),
        &[true, false],
        &[range(9, (0, 0), (1, 6))],
        &[Summary {
            zone: 9,
            hidden_rows: 2,
        }],
        &[],
    );
    check_dense_collapse_case(
        "hidden prefix",
        dense_ascii(&["ABCDEF"]),
        &[false],
        &[range(10, (0, 0), (0, 2))],
        &[
            Summary {
                zone: 10,
                hidden_rows: 1,
            },
            raw(0, 2, 6, false),
        ],
        &[((0, 2), (0, 5), SelectionMode::Normal, "CDEF")],
    );
    check_dense_collapse_case(
        "hidden soft suffix",
        dense_ascii(&["ABCDEF", "GHIJKL"]),
        &[true, false],
        &[range(11, (0, 2), (0, 6))],
        &[
            raw(0, 0, 2, false),
            Summary {
                zone: 11,
                hidden_rows: 1,
            },
            raw(1, 0, 6, false),
        ],
        &[((0, 0), (1, 5), SelectionMode::Normal, "AB\nGHIJKL")],
    );
    check_dense_collapse_case(
        "end at next row zero",
        dense_ascii(&["ABCDEF", "GHIJKL"]),
        &[false, false],
        &[range(12, (0, 4), (1, 0))],
        &[
            raw(0, 0, 4, false),
            Summary {
                zone: 12,
                hidden_rows: 1,
            },
            raw(1, 0, 6, false),
        ],
        &[((0, 2), (1, 1), SelectionMode::Normal, "CD\nGH")],
    );
    check_dense_collapse_case(
        "empty rectangle intersection remains a traversed fragment",
        dense_ascii(&["ABCDEF", "GHIJKL", "MNOPQR"]),
        &[false, false, false],
        &[range(14, (1, 0), (1, 4))],
        &[
            raw(0, 0, 6, false),
            Summary {
                zone: 14,
                hidden_rows: 1,
            },
            raw(1, 4, 6, false),
            raw(2, 0, 6, false),
        ],
        &[((0, 3), (2, 3), SelectionMode::Block, "D\n\nP")],
    );
    check_dense_collapse_case(
        "start at previous row width",
        dense_ascii(&["ABCDEF", "GHIJKL"]),
        &[true, false],
        &[range(15, (0, 6), (1, 2))],
        &[
            raw(0, 0, 6, true),
            Summary {
                zone: 15,
                hidden_rows: 1,
            },
            raw(1, 2, 6, false),
        ],
        &[((0, 4), (1, 5), SelectionMode::Normal, "EF\nIJKL")],
    );
    check_dense_collapse_case(
        "summary before selection does not break later soft continuation",
        dense_ascii(&["ABCDEF", "GHIJKL"]),
        &[true, false],
        &[range(16, (0, 0), (0, 2))],
        &[
            Summary {
                zone: 16,
                hidden_rows: 1,
            },
            raw(0, 2, 6, true),
            raw(1, 0, 6, false),
        ],
        &[((0, 4), (1, 3), SelectionMode::Normal, "EFGHIJ")],
    );
    let mut wide = dense_ascii(&["A中 BCD🙂 "]);
    for col in [1, 6] {
        wide[0][col].flags.set_wide(true);
        wide[0][col + 1].flags.set_wide_continuation(true);
    }
    check_dense_collapse_case(
        "whole wide pair hidden",
        wide,
        &[false],
        &[range(13, (0, 1), (0, 3))],
        &[
            raw(0, 0, 1, false),
            Summary {
                zone: 13,
                hidden_rows: 1,
            },
            raw(0, 3, 8, false),
        ],
        &[
            ((0, 0), (0, 7), SelectionMode::Normal, "A\nBCD🙂"),
            ((0, 7), (0, 7), SelectionMode::Normal, ""),
        ],
    );
}

#[test]
#[should_panic(expected = "collapse must own at least one dense cell")]
fn dense_collapse_model_rejects_zero_cell_seam_interval() {
    dense_collapse_rows(
        &dense_ascii(&["ABCDEF", "GHIJKL"]),
        &[true, false],
        &[DenseCollapseRange {
            zone: 1,
            start: (0, 6),
            end: (1, 0),
        }],
    );
}

#[test]
#[should_panic(expected = "admitted tuple intervals must not overlap")]
fn dense_collapse_model_rejects_token_disjoint_tuple_overlap() {
    dense_collapse_rows(
        &dense_ascii(&["ABCDEF", "GHIJKL"]),
        &[true, false],
        &[
            DenseCollapseRange {
                zone: 1,
                start: (0, 0),
                end: (1, 0),
            },
            DenseCollapseRange {
                zone: 2,
                start: (0, 6),
                end: (1, 1),
            },
        ],
    );
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

const PARTIAL_SELECTION_SEED: u64 = 0x4652_4f53_5450_4152;
const PARTIAL_SELECTION_CASES: usize = 2048;
const PARTIAL_SELECTION_WIDTHS: [usize; 11] = [1, 2, 3, 4, 7, 13, 31, 80, 255, 256, 1024];

// Construct hard/soft row boundaries from the original ledger. The eager
// glyph packer does not inspect plans, slices, mappings or materialized cells.
fn eager_history_rows(raw: &[Raw], width: usize) -> Vec<(Vec<Token>, bool)> {
    let mut result = Vec::new();
    let mut start = 0;
    for (index, row) in raw.iter().enumerate() {
        if !row.wrapped || index + 1 == raw.len() {
            let rows = eager_rows(&raw[start..=index], width);
            let count = rows.len();
            result.extend(
                rows.into_iter()
                    .enumerate()
                    .map(|(i, cells)| (cells, i + 1 < count)),
            );
            start = index + 1;
        }
    }
    result
}

fn eager_selected_text(
    rows: &[(Vec<Token>, bool)],
    width: usize,
    anchor: (usize, usize),
    active: (usize, usize),
    mode: SelectionMode,
) -> String {
    let (first, last) = (anchor.min(active), anchor.max(active));
    let mut text = String::new();
    for (row, (tokens, wrapped)) in rows.iter().enumerate().take(last.0 + 1).skip(first.0) {
        for col in 0..width {
            let selected = match mode {
                SelectionMode::Normal => (first..=last).contains(&(row, col)),
                SelectionMode::Block => {
                    (anchor.1.min(active.1)..=anchor.1.max(active.1)).contains(&col)
                }
            };
            if !selected {
                continue;
            }
            if tokens.is_empty() {
                // An empty logical line owns a selectable blank row. Padding
                // at the end of a nonempty history line owns no raw cells.
                text.push(' ');
            } else if let Some((_, cell)) = tokens.get(col) {
                if !cell.flags.wide_continuation() {
                    text.push(cell.character);
                }
            }
        }
        if row != last.0 && (mode == SelectionMode::Block || !wrapped) {
            text.push('\n');
        }
    }
    text
}

fn partial_selection_fixture(case: usize) -> (TerminalState, Vec<Raw>) {
    let mut rng = Rng(PARTIAL_SELECTION_SEED.wrapping_add(case as u64));
    let mut terminal = TerminalState::new(16, 2);
    let mut raw = Vec::new();
    for index in 0..(6 + rng.pick(7)) {
        let length = 1 + rng.pick(64);
        let kind = if index < 3 { index } else { rng.pick(5) };
        let mut cells = match kind {
            0 | 1 => vec![TerminalCell::default(); length],
            2 => vec![
                TerminalCell {
                    hyperlink: 1 + rng.pick(u16::MAX as usize) as u16,
                    ..Default::default()
                };
                length
            ],
            _ => corpus(&mut rng),
        };
        if kind == 1 {
            // Compression retains these cells, while projection deliberately
            // trims their style-only spaces. Every case has an empty hard row
            // and an empty wrapped prefix, not just nonempty random text.
            for cell in &mut cells {
                cell.foreground = Color::Rgb(17, 83, 201);
                cell.flags.set_blink(true);
                cell.flags.set_underline(UnderlineStyle::Curly);
            }
        } else if kind >= 3 {
            for cell in &mut cells {
                if cell.character != ' ' && rng.pick(3) == 0 {
                    cell.hyperlink = 1 + rng.pick(u16::MAX as usize) as u16;
                    cell.flags.set_blink(true);
                }
            }
        }
        let wrapped = match index {
            0 | 2 => false,
            1 => true,
            _ => rng.pick(2) == 0,
        };
        let line = ScrollbackLine::compress(&cells, wrapped);
        assert_eq!(
            line.decompress(),
            cells,
            "selection case={case} row={index}"
        );
        raw.push(Raw {
            id: line.raw_row_id(),
            cells,
            wrapped,
        });
        terminal.scrollback.push_back(line);
    }
    (terminal, raw)
}

#[test]
fn seeded_partial_history_selections_match_independent_eager_text() {
    let replay = std::env::var("FROST_PROJECTION_SELECTION_CASE")
        .ok()
        .map(|s| s.parse::<usize>().unwrap());
    let cases = replay.map_or(0..PARTIAL_SELECTION_CASES, |case| case..case + 1);
    let mut checks = 0;
    for case in cases {
        let width = PARTIAL_SELECTION_WIDTHS[case % PARTIAL_SELECTION_WIDTHS.len()];
        let (terminal, raw) = partial_selection_fixture(case);
        let expected_rows = eager_history_rows(&raw, width);
        let plan = ProjectionPlan::identity(
            terminal
                .scrollback
                .iter()
                .enumerate()
                .map(|(i, row)| row.layout(i)),
            [],
            width,
        );
        assert_eq!(plan.rows.len(), expected_rows.len(), "case={case}");
        let full = viewport(&terminal, &plan, 0, expected_rows.len());
        for (row, (tokens, wrapped)) in expected_rows.iter().enumerate() {
            assert_eq!(full.row_wrapped()[row], *wrapped, "case={case} row={row}");
            for col in 0..width {
                let expected = tokens.get(col);
                assert_eq!(
                    full.view_to_raw(ViewportCell { row, col }),
                    expected.map(|(origin, _)| *origin),
                    "case={case} row={row} col={col}"
                );
                assert_eq!(
                    full.cells()[row][col],
                    expected.map_or(TerminalCell::default(), |(_, cell)| *cell),
                    "case={case} row={row} col={col}"
                );
            }
        }
        let mut rng = Rng(PARTIAL_SELECTION_SEED ^ case as u64);
        for sample in 0..16 {
            let anchor = (rng.pick(expected_rows.len()), rng.pick(width));
            let active = (rng.pick(expected_rows.len()), rng.pick(width));
            for mode in [SelectionMode::Normal, SelectionMode::Block] {
                let expected = eager_selected_text(&expected_rows, width, anchor, active, mode);
                for (start, end) in [(anchor, active), (active, anchor)] {
                    let mut selection = selected_all(&plan);
                    selection.anchor.point = start;
                    selection.active.point = end;
                    selection.mode = mode;
                    assert_eq!(
                        copy_projected_cells(
                            &plan,
                            &selection,
                            &terminal.scrollback,
                            &terminal.grid,
                        )
                        .as_deref(),
                        Some(expected.as_str()),
                        "seed={PARTIAL_SELECTION_SEED:#x} case={case} width={width} sample={sample} anchor={start:?} active={end:?} mode={mode:?}"
                    );
                    checks += 1;
                }
            }
        }
    }
    println!("partial history selection properties: seed={PARTIAL_SELECTION_SEED:#x}, checks={checks}, widths={PARTIAL_SELECTION_WIDTHS:?}; replay with FROST_PROJECTION_SELECTION_CASE");
}

#[test]
fn partial_history_selection_keeps_blank_rows_and_soft_wraps_distinct() {
    let mut terminal = TerminalState::new(3, 2);
    terminal.scrollback.push_back(ascii_row("AB", true));
    let mut wide = vec![TerminalCell::default(); 4];
    for (cell, character) in wide.iter_mut().zip(['C', '中', ' ', 'D']) {
        cell.character = character;
    }
    wide[1].flags.set_wide(true);
    wide[2].flags.set_wide_continuation(true);
    terminal
        .scrollback
        .push_back(ScrollbackLine::compress(&wide, false));
    terminal.scrollback.push_back(ascii_row("    ", false));
    terminal.scrollback.push_back(ascii_row("E   ", false));
    let plan = ProjectionPlan::identity(
        terminal
            .scrollback
            .iter()
            .enumerate()
            .map(|(i, row)| row.layout(i)),
        [],
        3,
    );
    // At width 3: ABC (soft), 中·D (hard), a blank hard row, then E.
    // These expectations are hand-written, independent of either oracle.
    for (anchor, active, mode, expected) in [
        ((0, 2), (3, 0), SelectionMode::Normal, "C中D\n   \nE"),
        ((3, 0), (0, 2), SelectionMode::Normal, "C中D\n   \nE"),
        ((0, 1), (3, 2), SelectionMode::Block, "BC\nD\n  \n"),
        ((3, 2), (0, 1), SelectionMode::Block, "BC\nD\n  \n"),
        ((1, 1), (1, 1), SelectionMode::Normal, ""),
        ((2, 1), (2, 2), SelectionMode::Normal, "  "),
        ((3, 1), (3, 2), SelectionMode::Normal, ""),
    ] {
        let mut selection = selected_all(&plan);
        selection.anchor.point = anchor;
        selection.active.point = active;
        selection.mode = mode;
        assert_eq!(
            copy_projected_cells(&plan, &selection, &terminal.scrollback, &terminal.grid)
                .as_deref(),
            Some(expected),
            "anchor={anchor:?} active={active:?} mode={mode:?}"
        );
    }
}
// Independent diagnostic for live-grid selection with inserted host chrome.
// Endpoints always belong to original raw cells; chrome-only selection and
// spare-tail trimming are deliberately outside this corpus.
const LIVE_CHROME_SEED: u64 = 0x4652_4f53_544c_4956;
const LIVE_CHROME_CASES: usize = 1024;
const LIVE_CHROME_WIDTHS: [usize; 11] = [1, 2, 3, 4, 7, 13, 31, 80, 255, 256, 1024];

fn live_grid_expected_text(
    original: &[Vec<TerminalCell>],
    wrapped: &[bool],
    anchor: (usize, usize),
    active: (usize, usize),
    mode: SelectionMode,
) -> String {
    let (first, last) = (anchor.min(active), anchor.max(active));
    let mut output = String::new();
    for (row, cells) in original.iter().enumerate().take(last.0 + 1).skip(first.0) {
        for (col, cell) in cells.iter().enumerate() {
            let selected = match mode {
                SelectionMode::Normal => (first..=last).contains(&(row, col)),
                SelectionMode::Block => {
                    (anchor.1.min(active.1)..=anchor.1.max(active.1)).contains(&col)
                }
            };
            if selected && !cell.flags.wide_continuation() {
                output.push(cell.character);
            }
        }
        if row != last.0 && (matches!(mode, SelectionMode::Block) || !wrapped[row]) {
            output.push('\n');
        }
    }
    output
}

#[test]
fn seeded_live_grid_raw_endpoint_copy_is_invariant_under_legal_chrome() {
    let replay = std::env::var("FROST_LIVE_CHROME_CASE")
        .ok()
        .map(|value| value.parse::<usize>().expect("valid case index"));
    assert!(replay.is_none_or(|case| case < LIVE_CHROME_CASES));
    let mut checked = 0;
    let mut accepted_boundaries = 0;
    let mut rejected_boundaries = 0;
    for case in 0..LIVE_CHROME_CASES {
        if replay.is_some_and(|selected| selected != case) {
            continue;
        }
        let mut rng = Rng(LIVE_CHROME_SEED.wrapping_add(case as u64));
        let width = LIVE_CHROME_WIDTHS[case % LIVE_CHROME_WIDTHS.len()];
        let rows = 2 + rng.pick(5);
        let mut terminal = TerminalState::new(width, rows);
        let mut original = Vec::new();
        let mut wrapped = Vec::new();
        let mut boundaries = Vec::new();
        let mut expected_document = Vec::new();
        let mut document_row_for_raw = Vec::new();
        for row in 0..rows {
            let mut cells = vec![TerminalCell::default(); width];
            let mut col = 0;
            while col < width {
                let kind = rng.pick(7);
                if kind == 0 && col + 1 < width {
                    cells[col].character = if rng.pick(2) == 0 { '中' } else { '🙂' };
                    cells[col].flags.set_wide(true);
                    cells[col + 1].flags.set_wide_continuation(true);
                    col += 2;
                    continue;
                }
                if kind == 1 {
                    cells[col].character = 'é';
                } else if kind >= 4 {
                    cells[col].character = char::from(b'a' + rng.pick(26) as u8);
                } else if kind == 2 {
                    cells[col].background = Color::Indexed(17);
                    cells[col].flags.set_bold(true);
                    cells[col].hyperlink = 1;
                }
                col += 1;
            }
            terminal.grid.set_row(row, cells.clone());
            original.push(cells);
            let soft = row + 1 < rows && rng.pick(2) == 0;
            terminal.grid.row_wrapped[row] = soft;
            wrapped.push(soft);
            let chrome_rows = 1 + rng.pick(3);
            boundaries.push(ChromeBoundary {
                next: terminal.grid.raw_row_id(row).unwrap(),
                kind: ChromeKind::Organism,
                rows: chrome_rows,
            });
            // Native chrome is admitted only before a standalone raw row.
            // Fixed grid rows are never joined or reflowed; a soft predecessor
            // is the only failed admission condition in this fixture domain.
            if row == 0 || !wrapped[row - 1] {
                expected_document.extend(std::iter::repeat_n(None, chrome_rows));
                accepted_boundaries += 1;
            } else {
                rejected_boundaries += 1;
            }
            document_row_for_raw.push(expected_document.len());
            expected_document.push(Some(row));
        }
        let plan = terminal
            .identity_projection_plan(width)
            .splice_native_chrome(&boundaries, 0, rows);
        assert_eq!(
            plan.rows.len(),
            expected_document.len(),
            "case={case} width={width}"
        );
        let view = viewport(&terminal, &plan, 0, plan.rows.len());
        for (document_row, raw_row) in expected_document.iter().enumerate() {
            for (col, _) in original[0].iter().enumerate() {
                let expected_origin = raw_row.map(|row| RawCellOrigin {
                    row: terminal.grid.raw_row_id(row).unwrap(),
                    col,
                });
                assert_eq!(
                    view.view_to_raw(ViewportCell {
                        row: document_row,
                        col
                    }),
                    expected_origin,
                    "case={case} width={width} document_row={document_row} col={col}"
                );
                if let Some(row) = raw_row {
                    assert_eq!(view.cells()[document_row][col], original[*row][col]);
                }
            }
        }
        for sample in 0..16 {
            let anchor = (rng.pick(rows), rng.pick(width));
            let active = (rng.pick(rows), rng.pick(width));
            for mode in [SelectionMode::Normal, SelectionMode::Block] {
                let expected = live_grid_expected_text(&original, &wrapped, anchor, active, mode);
                for (a, b) in [(anchor, active), (active, anchor)] {
                    let mut selection = selected_all(&plan);
                    selection.anchor.point = (document_row_for_raw[a.0], a.1);
                    selection.active.point = (document_row_for_raw[b.0], b.1);
                    selection.mode = mode;
                    assert_eq!(copy_projected_cells(&plan, &selection, &terminal.scrollback, &terminal.grid),
                               Some(expected.clone()),
                               "seed={LIVE_CHROME_SEED:#x} case={case} width={width} sample={sample} a={a:?} b={b:?} mode={mode:?}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, if replay.is_some() { 64 } else { 65_536 });
    if replay.is_none() {
        assert!(accepted_boundaries > 0 && rejected_boundaries > 0);
    }
    eprintln!("live-grid/chrome seed={LIVE_CHROME_SEED:#x} checks={checked} accepted_boundaries={accepted_boundaries} rejected_boundaries={rejected_boundaries}");
}
