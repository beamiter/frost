//! Independent wire expectations through Frost's production host adapter.
use super::TerminalState;

#[test]
fn mouse_mode_round_trip_in_one_batch_retires_queued_scroll() {
    let mut terminal = TerminalState::new(20, 5);
    let initial = terminal.projection_scroll_guard.snapshot();
    terminal.process_batch(b"\x1b[?1000;1006h\x1b[?1006;1000l");
    assert!(!terminal.is_mouse_enabled());
    let updated = terminal.projection_scroll_guard.snapshot();
    assert_ne!(initial, updated);

    terminal.process_batch(b"\x1b[?1006;1000l");
    assert_eq!(updated, terminal.projection_scroll_guard.snapshot());
}

#[test]
fn shared_mouse_all_mode_combinations_preserve_encoding_precedence() {
    let modes = [1000, 1002, 1003, 1005, 1006, 1015];
    for mask in 0..64 {
        let mut terminal = TerminalState::new(20, 5);
        for (bit, mode) in modes.into_iter().enumerate() {
            if mask & (1 << bit) != 0 {
                terminal.process_input(format!("\x1b[?{mode}h").as_bytes());
            }
        }
        let (press, release): (Option<&[u8]>, Option<&[u8]>) = if mask & 7 == 0 {
            (None, None)
        } else if mask & 16 != 0 {
            (Some(b"\x1b[<2;300;11M"), Some(b"\x1b[<2;300;11m"))
        } else if mask & 32 != 0 {
            (Some(b"\x1b[34;300;11M"), Some(b"\x1b[35;300;11M"))
        } else if mask & 8 != 0 {
            (Some(b"\x1b[M\x22\xc5\x8c+"), Some(b"\x1b[M#\xc5\x8c+"))
        } else {
            (Some(b"\x1b[M\x22\xff+"), Some(b"\x1b[M#\xff+"))
        };
        assert_eq!(
            terminal.get_mouse_report(2, 299, 10).as_deref(),
            press,
            "mask={mask}"
        );
        assert_eq!(
            terminal.get_mouse_release_report(2, 299, 10).as_deref(),
            release,
            "mask={mask}"
        );
    }
}

#[test]
fn shared_mouse_extreme_coordinates_and_buttons_never_wrap() {
    let mut terminal = TerminalState::new(20, 5);
    terminal.process_input(b"\x1b[?1000h");
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        b"\x1b[M\xff\xff\xff"
    );
    terminal.process_input(b"\x1b[?1005h");
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        b"\x1b[M\xc4\x9f\xdf\xbf\xdf\xbf"
    );
    terminal.process_input(b"\x1b[?1015h");
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        format!("\x1b[287;{0};{0}M", usize::MAX).as_bytes()
    );
    terminal.process_input(b"\x1b[?1006h");
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        format!("\x1b[<255;{0};{0}M", usize::MAX).as_bytes()
    );
    assert_eq!(
        terminal
            .get_mouse_release_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        format!("\x1b[<255;{0};{0}m", usize::MAX).as_bytes()
    );
}

#[test]
fn unrelated_host_modes_cannot_enable_mouse_reports() {
    let mut terminal = TerminalState::new(20, 5);
    for mode in [
        1, 6, 7, 25, 47, 66, 1001, 1004, 1047, 1048, 1049, 2004, 2026, 2031, 5522,
    ] {
        terminal.process_input(format!("\x1b[?{mode}h").as_bytes());
    }
    assert!(terminal.get_mouse_report(0, 0, 0).is_none());
    assert!(terminal.get_mouse_release_report(0, 0, 0).is_none());
    terminal.process_input(b"\x1b[4h");
    assert!(terminal.vt.screen.insert_mode);
    assert!(terminal.vt.screen.origin_mode);
}

#[test]
fn shared_mouse_utf8_button_boundaries_are_valid_through_host_adapter() {
    let mut terminal = TerminalState::new(20, 5);
    terminal.process_input(b"\x1b[?1000h");
    terminal.process_input(b"\x1b[?1005h");
    for (button, wire, release) in [
        (95, &b"\x1b[M\x7f!!"[..], &b"\x1b[M?!!"[..]),
        (96, &b"\x1b[M\xc2\x80!!"[..], &b"\x1b[M#!!"[..]),
        (127, &b"\x1b[M\xc2\x9f!!"[..], &b"\x1b[M?!!"[..]),
        (128, &b"\x1b[M\xc2\xa0!!"[..], &b"\x1b[M#!!"[..]),
        (223, &b"\x1b[M\xc3\xbf!!"[..], &b"\x1b[M?!!"[..]),
        (224, &b"\x1b[M\xc4\x80!!"[..], &b"\x1b[M#!!"[..]),
        (255, &b"\x1b[M\xc4\x9f!!"[..], &b"\x1b[M?!!"[..]),
    ] {
        let report = terminal.get_mouse_report(button, 0, 0).unwrap();
        assert_eq!(report, wire, "button={button}");
        assert!(std::str::from_utf8(&report).is_ok());
        assert_eq!(
            terminal.get_mouse_release_report(button, 0, 0).unwrap(),
            release
        );
    }
}

#[test]
fn shared_mouse_release_preserves_each_modifier_and_discards_legacy_button_bits() {
    // Independent expected Cb bytes: no production encoder computes these values.
    for (mods, legacy, decimal) in [
        (0, b'#', 35),
        (4, b'\'', 39),
        (8, b'+', 43),
        (12, b'/', 47),
        (16, b'3', 51),
        (20, b'7', 55),
        (24, b';', 59),
        (28, b'?', 63),
    ] {
        for base in [0, 1, 2, 32, 64, 128, 192] {
            let mut terminal = TerminalState::new(20, 5);
            terminal.process_input(b"\x1b[?1002h");
            for mode in [0, 1005, 1015, 1006] {
                if mode != 0 {
                    terminal.process_input(format!("\x1b[?{mode}h").as_bytes());
                }
                let expected = match mode {
                    1006 => format!("\x1b[<{};1;1m", base | mods).into_bytes(),
                    1015 => format!("\x1b[{decimal};1;1M").into_bytes(),
                    _ => vec![27, b'[', b'M', legacy, b'!', b'!'],
                };
                assert_eq!(
                    terminal
                        .get_mouse_release_report(base | mods, 0, 0)
                        .unwrap(),
                    expected,
                    "mode={mode}, base={base}, mods={mods}"
                );
            }
        }
    }
}
