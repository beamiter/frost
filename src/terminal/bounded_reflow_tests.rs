use super::*;

fn assert_tail(source: &[ScrollbackLine], cols: usize, cap: usize) {
    let reference = TerminalState::reflow_lines(source, cols, &TerminalCell::default());
    let tail = reference.len().saturating_sub(cap);
    BOUNDED_REFLOW_PEAK.with(|p| p.set((0, 0, 0)));
    let actual = TerminalState::reflow_lines_bounded(
        source.iter().cloned(),
        cols,
        &TerminalCell::default(),
        cap,
    );
    assert_eq!(actual.len(), reference.len() - tail);
    for (a, b) in actual.iter().zip(&reference[tail..]) {
        assert_eq!(a.decompress(), b.decompress());
        assert_eq!(a.is_wrapped, b.is_wrapped);
        assert_eq!(a.cols, b.cols);
        assert_eq!(a.projected_active_len, b.projected_active_len);
        assert_eq!(
            a.projected_wide_continuations,
            b.projected_wide_continuations
        );
        assert!(a.raw_row_id.is_tracked());
        assert!(!source.iter().any(|s| s.raw_row_id == a.raw_row_id));
    }
    let peak = BOUNDED_REFLOW_PEAK.with(|p| p.get());
    assert!(
        peak.0
            <= source
                .iter()
                .map(|s| usize::from(s.cols))
                .max()
                .unwrap_or(0)
    );
    assert!(peak.1 <= cols);
    assert!(peak.2 <= cap);
}

#[test]
fn bounded_reflow_real_output_wide_to_narrow_never_materializes_full_expansion() {
    let mut terminal = TerminalState::new(64, 2);
    terminal.set_max_scrollback(64);
    for _ in 0..66 {
        terminal.process_input(&[b'x'; 64]);
    }
    assert_eq!(terminal.scrollback.len(), 64);
    let source: Vec<_> = terminal.scrollback.iter().cloned().collect();
    let old = TerminalState::reflow_lines(&source, 1, &TerminalCell::default());
    // Moderate concrete regression: old scratch materializes4096 output rows
    // before normalize discards4032 of them, despite retention64.
    assert_eq!(old.len(), 4096);
    assert_tail(&source, 1, 64);
    terminal.on_resize(1, 2);
    BOUNDED_REFLOW_PEAK.with(|p| p.set((0, 0, 0)));
    assert!(terminal.normalize_scrollback_width());
    assert_eq!(terminal.scrollback.len(), 64);
    assert_eq!(BOUNDED_REFLOW_PEAK.with(|p| p.get()), (64, 1, 64));
    for (a, b) in terminal.scrollback.iter().zip(&old[4032..]) {
        assert_eq!(a.decompress(), b.decompress());
        assert_eq!(a.is_wrapped, b.is_wrapped);
    }
}

#[test]
fn bounded_reflow_matches_frozen_algorithm_for_wide_empty_style_and_wraps() {
    let plain = TerminalCell::default();
    let mut a = plain;
    a.character = 'a';
    let mut body = plain;
    body.character = '界';
    body.flags.set_wide(true);
    let mut continuation = plain;
    continuation.flags.set_wide_continuation(true);
    let mut painted = plain;
    painted.background = Color::Red;
    let mut linked = plain;
    linked.hyperlink_id = HyperlinkId::from_raw(1);
    let patterns = [
        vec![],
        vec![plain; 4],
        vec![a; 5],
        vec![a, body, continuation, a],
        vec![body, continuation],
        vec![body],
        vec![continuation, a],
        vec![painted, linked, plain],
        vec![continuation; 3],
    ];
    for first in &patterns {
        for second in &patterns {
            for third in &patterns {
                for wrapmask in 0..8 {
                    let source: Vec<_> = [first, second, third]
                        .into_iter()
                        .enumerate()
                        .map(|(i, c)| ScrollbackLine::compress(c, wrapmask & (1 << i) != 0))
                        .collect();
                    for cols in 1..=6 {
                        for cap in [0, 1, 2, 8, 64] {
                            assert_tail(&source, cols, cap);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_reflow_long_wrapped_group_scratch_does_not_scale_with_group() {
    let cell = TerminalCell {
        character: 'x',
        ..TerminalCell::default()
    };
    let source: Vec<_> = (0..256)
        .map(|_| ScrollbackLine::compress(&[cell; 128], true))
        .collect();
    assert_tail(&source, 3, 16);
    assert_eq!(BOUNDED_REFLOW_PEAK.with(|p| p.get()), (128, 3, 16));
}

#[test]
fn bounded_normalize_keeps_live_grid_anchor_shift_and_finalized_zone_identity() {
    let mut terminal = TerminalState::new(16, 8);
    terminal.set_max_scrollback(16);
    terminal.process_input(
        b"\x1b]133;A\x07$ \x1b]133;B\x07echo\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07",
    );
    let id = terminal.command_zones.back().unwrap().id;
    let output = terminal.zone_output_text(id);
    assert!(output.as_deref().is_some_and(|text| text.contains("out")));
    for _ in 0..12 {
        terminal.process_input(b"abcdefghijklmnop\r\n");
    }
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=T,i=44,q=2;AQIDBA==\x1b\\");
    let old = terminal.scrollback.len();
    let (prompt, command) = match terminal.current_zone_state {
        ZoneState::CommandStarted(p, c) => (p, c),
        _ => panic!("expected live prompt"),
    };
    assert!(prompt >= old);
    let image_row = terminal.kitty_graphics.get_placements()[0].buffer_row;
    assert!(image_row >= old);
    terminal.on_resize(2, 8);
    assert!(terminal.normalize_scrollback_width());
    let new = terminal.scrollback.len();
    assert_eq!(new, 16);
    assert!(new > old);
    match terminal.current_zone_state {
        ZoneState::CommandStarted(p, c) => {
            assert_eq!(p, new + prompt - old);
            assert_eq!(c, new + command - old);
        }
        _ => panic!("live prompt identity changed"),
    }
    let zone = terminal.command_zones.iter().find(|z| z.id == id).unwrap();
    assert!(zone.rows_evicted);
    assert_eq!(terminal.zone_output_text(id), output);
    assert_eq!(
        terminal.kitty_graphics.get_placements()[0].buffer_row,
        new + image_row - old
    );
}
