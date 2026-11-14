//! Independent wire expectations through Frost's production host adapter.
use super::TerminalState;

#[test]
fn shared_mouse_all_mode_combinations_preserve_encoding_precedence() {
    let modes = [1000, 1002, 1003, 1005, 1006, 1015];
    for mask in 0..64 {
        let mut terminal = TerminalState::new(20, 5);
        for (bit, mode) in modes.into_iter().enumerate() {
            if mask & (1 << bit) != 0 {
                terminal.modes.insert(mode);
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
    terminal.modes.insert(1000);
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        b"\x1b[M\xff\xff\xff"
    );
    terminal.modes.insert(1005);
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        b"\x1b[M\xc4\x9f\xdf\xbf\xdf\xbf"
    );
    terminal.modes.insert(1015);
    assert_eq!(
        terminal
            .get_mouse_report(255, usize::MAX, usize::MAX)
            .unwrap(),
        format!("\x1b[287;{0};{0}M", usize::MAX).as_bytes()
    );
    terminal.modes.insert(1006);
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
        1, 4, 6, 7, 25, 47, 66, 1001, 1004, 1047, 1048, 1049, 2004, 2026, 2031, 5522,
    ] {
        terminal.modes.insert(mode);
    }
    assert!(terminal.get_mouse_report(0, 0, 0).is_none());
    assert!(terminal.get_mouse_release_report(0, 0, 0).is_none());
    assert!(terminal.modes.contains(&4));
    assert!(terminal.modes.contains(&6));
}

#[test]
fn shared_mouse_utf8_button_boundaries_are_valid_through_host_adapter() {
    let mut terminal = TerminalState::new(20, 5);
    terminal.modes.insert(1000);
    terminal.modes.insert(1005);
    for (button, wire) in [
        (95, &b"\x1b[M\x7f!!"[..]),
        (96, &b"\x1b[M\xc2\x80!!"[..]),
        (127, &b"\x1b[M\xc2\x9f!!"[..]),
        (128, &b"\x1b[M\xc2\xa0!!"[..]),
        (223, &b"\x1b[M\xc3\xbf!!"[..]),
        (224, &b"\x1b[M\xc4\x80!!"[..]),
        (255, &b"\x1b[M\xc4\x9f!!"[..]),
    ] {
        let report = terminal.get_mouse_report(button, 0, 0).unwrap();
        assert_eq!(report, wire, "button={button}");
        assert!(std::str::from_utf8(&report).is_ok());
        assert_eq!(
            terminal.get_mouse_release_report(button, 0, 0).unwrap(),
            b"\x1b[M#!!"
        );
    }
}
