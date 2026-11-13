//! Production bounded scanner policy. Historical scanner fixtures stay frozen.
use super::*;

fn ready() -> TerminalState {
    let mut terminal = TerminalState::new(40, 8);
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07before");
    assert!(terminal.idle_background_capture_active());
    terminal
}

#[test]
fn bounded_private_strings_never_leak_and_capture_recovers_in_same_feed() {
    for intro in *b"]P_X^" {
        let mut packet = vec![27, intro];
        packet.extend_from_slice(b"SECRET");
        packet.resize(MAX_PENDING_ESCAPE + 3, b'x');
        packet.extend_from_slice(b"\x1b[2J\x1bc\x1b[6n\x1b]52;c;SECRET\x1b\\Z");
        for cut in [
            0,
            2,
            MAX_PENDING_ESCAPE,
            MAX_PENDING_ESCAPE + 2,
            packet.len() - 2,
            packet.len(),
        ] {
            let mut terminal = ready();
            terminal.process_input(&packet[..cut]);
            terminal.process_input(&packet[cut..]);
            assert_eq!(
                terminal.parser.rejected_count(),
                1,
                "intro={intro} cut={cut}"
            );
            assert_eq!(terminal.parser.retained_bytes(), 0);
            assert!(terminal.get_output().is_empty());
            assert!(terminal.titles.window_title.is_empty());
            let raw = terminal
                .idle_background_output
                .as_ref()
                .unwrap()
                .raw_bytes();
            assert_eq!(raw, b"beforeZ", "intro={intro} cut={cut}");
            terminal.process_input(b"\x1b]133;A\x07");
            let zone = terminal.command_zones.back().expect("real background zone");
            let (output, truncated) = zone.captured_output.as_ref().unwrap();
            assert_eq!(output, "beforeZ");
            assert!(!truncated);
        }
    }
}

#[test]
fn bounded_discard_bel_and_cancellation_preserve_capture_privacy() {
    for intro in *b"P_X^" {
        for end in [b"\x18".as_slice(), b"\x1a", b"\x1b\\"] {
            let mut terminal = ready();
            let mut attack = vec![27, intro];
            attack.resize(MAX_PENDING_ESCAPE + 3, b'x');
            terminal.process_input(&attack);
            terminal.process_input(b"\x07SECRET\x1b]133;C\x07");
            assert_eq!(
                terminal
                    .idle_background_output
                    .as_ref()
                    .unwrap()
                    .raw_bytes(),
                b"before"
            );
            assert!(matches!(
                terminal.current_zone_state,
                ZoneState::CommandStarted(_, _)
            ));
            for byte in end {
                terminal.process_input(&[*byte]);
            }
            terminal.process_input(b"Z");
            assert_eq!(
                terminal
                    .idle_background_output
                    .as_ref()
                    .unwrap()
                    .raw_bytes(),
                b"beforeZ"
            );
            assert!(terminal.get_output().is_empty());
        }
    }
}

#[test]
fn bounded_osc8_rejection_clears_only_active_link() {
    let mut terminal = TerminalState::new(40, 8);
    terminal.process_input(b"\x1b]8;;https://safe.test\x1b\\A");
    let old = terminal.grid[0][0].hyperlink_id;
    assert!(!old.is_none());
    let mut attack = b"\x1b]8;;SECRET".to_vec();
    attack.resize(MAX_PENDING_ESCAPE + 3, b'x');
    terminal.process_input(&attack);
    assert!(terminal.current_hyperlink.is_none());
    terminal.process_input(b"\x1b\\B");
    assert_eq!(terminal.grid[0][0].hyperlink_id, old);
    assert!(terminal.grid[0][1].hyperlink_id.is_none());
    assert!(terminal.get_output().is_empty());
}

#[test]
fn bounded_normal_stream_matches_legacy_at_every_split() {
    let bytes = b"hello\x1b[31mX\x1b[0m\x1b]0;title\x07\x1b]8;;https://safe.test\x1b\\L\x1b]8;;\x1b\\\r\n\xe4\xb8\xad\x1b[?1049hALT\x1b[?1049l\x1b[6n\x1b_Gf=32,s=1,v=1,a=t,i=56;AQIDBA==\x1b\\";
    for cut in 0..=bytes.len() {
        let mut strict = ready();
        let mut legacy = ready();
        legacy.parser = Parser::with_profile(
            ParserLimits {
                pending_bytes: MAX_PENDING_ESCAPE,
                kitty_recovery_bytes: jterm_core::kitty_graphics::MAX_CONTROL_BYTES,
            },
            ParserProfile::FrostLegacyV1,
        );
        for part in [&bytes[..cut], &bytes[cut..]] {
            strict.process_input(part);
            legacy.process_input(part);
        }
        for row in 0..8 {
            assert_eq!(strict.grid[row], legacy.grid[row], "cut={cut}");
        }
        assert_eq!(strict.get_cursor_pos(), legacy.get_cursor_pos());
        assert_eq!(strict.titles.window_title, legacy.titles.window_title);
        assert_eq!(strict.get_output(), legacy.get_output());
        assert_eq!(
            strict
                .idle_background_output
                .as_ref()
                .map(|p| p.raw_bytes()),
            legacy
                .idle_background_output
                .as_ref()
                .map(|p| p.raw_bytes()),
            "capture cut={cut}"
        );
        assert_eq!(strict.parser.rejected_count(), 0);
    }
}

#[test]
fn bounded_exact_payload_limit_and_plus_one_are_consumer_partition_invariant() {
    for intro in *b"]P_X^" {
        for payload_len in [MAX_PENDING_ESCAPE, MAX_PENDING_ESCAPE + 1] {
            let mut packet = vec![27, intro];
            packet.resize(payload_len + 2, b'x');
            packet.extend_from_slice(b"\x1b\\Z");
            for cut in [0, 2, MAX_PENDING_ESCAPE + 1, packet.len() - 2, packet.len()] {
                let mut terminal = ready();
                terminal.process_input(&packet[..cut]);
                assert!(terminal.parser.retained_bytes() <= MAX_PENDING_ESCAPE + 3);
                terminal.process_input(&packet[cut..]);
                assert_eq!(
                    terminal.parser.rejected_count(),
                    u64::from(payload_len > MAX_PENDING_ESCAPE),
                    "intro={intro} payload={payload_len} cut={cut}"
                );
                assert_eq!(terminal.parser.retained_bytes(), 0);
                assert_eq!(
                    terminal
                        .idle_background_output
                        .as_ref()
                        .unwrap()
                        .raw_bytes(),
                    b"beforeZ"
                );
                assert!(terminal.get_output().is_empty());
            }
        }
    }
}

#[test]
fn bounded_small_kitty_can_sub_do_not_reflect_old_error_reply() {
    for cancel in [0x18, 0x1a] {
        let mut packet = b"\x1b_Gi=53,q=0;SECRET".to_vec();
        packet.push(cancel);
        packet.push(b'Z');
        for cut in 0..=packet.len() {
            let mut terminal = ready();
            terminal.process_input(&packet[..cut]);
            terminal.process_input(&packet[cut..]);
            assert!(
                terminal.get_output().is_empty(),
                "cancel={cancel} cut={cut}"
            );
            assert_eq!(
                terminal
                    .idle_background_output
                    .as_ref()
                    .unwrap()
                    .raw_bytes(),
                b"beforeZ"
            );
            assert_eq!(terminal.parser.retained_bytes(), 0);
        }
    }
}

#[test]
fn bounded_rejected_kitty_cannot_resume_a_previous_chunked_image() {
    for attack in [
        b"\x1b_Gi=34;bad\x18".to_vec(),
        b"\x1b_Gi=34;bad\x1a".to_vec(),
        b"\x1b_Gi=34;bad\x1b[31m".to_vec(),
        {
            let mut v = b"\x1b_Gi=34;".to_vec();
            v.resize(MAX_PENDING_ESCAPE + 3, b'x');
            v.extend_from_slice(b"\x1b\\");
            v
        },
    ] {
        for cut in [0, 2, attack.len() - 1, attack.len()] {
            let mut terminal = TerminalState::new(8, 2);
            terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=t,i=34,q=2,m=1;AQID\x1b\\");
            assert!(terminal.kitty_graphics.get_image(34).is_none());
            terminal.process_input(&attack[..cut]);
            terminal.process_input(&attack[cut..]);
            assert!(terminal.get_output().is_empty());
            terminal.process_input(b"\x1b_Gm=0;BA==\x1b\\");
            assert!(
                terminal.kitty_graphics.get_image(34).is_none(),
                "rejected input must invalidate pending transfer"
            );
        }
    }
}

#[test]
fn bounded_non_apc_boundary_preserves_transfer_and_abort_preserves_completed_images() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=T,i=33,q=2;AQIDBA==\x1b\\");
    assert!(terminal.kitty_graphics.get_image(33).is_some());
    let completed_placements = terminal.kitty_graphics.get_placements().len();
    terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=t,i=34,q=2,m=1;AQID\x1b\\");
    terminal.process_input(b"\x1bPignored\x18\x1b]0;title\x07");
    terminal.process_input(b"\x1b_Gm=0;BA==\x1b\\");
    assert!(terminal.kitty_graphics.get_image(34).is_some());
    terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=t,i=35,q=2,m=1;AQID\x1b\\");
    terminal.process_input(b"\x1b_Gignored\x18");
    assert!(terminal.kitty_graphics.get_image(33).is_some());
    assert!(terminal.kitty_graphics.get_image(34).is_some());
    assert_eq!(
        terminal.kitty_graphics.get_placements().len(),
        completed_placements
    );
    terminal.process_input(b"\x1b_Gm=0;BA==\x1b\\");
    assert!(terminal.kitty_graphics.get_image(35).is_none());
}

// These assertions run through the real host dispatch and screen, not just
// parser tokens. In FrostLegacyV1 the private prefix consumes one of the 128
// retained parameter bytes; excess parameter bytes are consumed, not executed.
#[test]
fn bounded_csi_capacity_reaches_host_without_suffix_execution() {
    for zeros in [127, 128, 129] {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'0', zeros));
        bytes.extend_from_slice(b"1mZ\x1b[0m\x1b[6n");
        for cut in 0..=bytes.len() {
            let mut terminal = TerminalState::new(40, 8);
            terminal.process_input(&bytes[..cut]);
            terminal.process_input(&bytes[cut..]);
            assert_eq!(
                terminal.grid[0][0].character, 'Z',
                "zeros={zeros} cut={cut}"
            );
            assert_eq!(
                terminal.grid[0][0].flags.bold(),
                zeros == 127,
                "zeros={zeros} cut={cut}"
            );
            assert_eq!(
                terminal.get_output(),
                b"\x1b[1;2R",
                "zeros={zeros} cut={cut}"
            );
            assert_eq!(terminal.parser.retained_bytes(), 0);
        }
    }
    for zeros in [126, 127, 128] {
        let mut bytes = b"\x1b[?".to_vec();
        bytes.extend(std::iter::repeat_n(b'0', zeros));
        bytes.extend_from_slice(b"1hZ\x1b[6n");
        for cut in 0..=bytes.len() {
            let mut terminal = TerminalState::new(40, 8);
            terminal.process_input(&bytes[..cut]);
            terminal.process_input(&bytes[cut..]);
            assert_eq!(
                terminal.is_application_cursor_keys(),
                zeros == 126,
                "zeros={zeros} cut={cut}"
            );
            assert_eq!(terminal.grid[0][0].character, 'Z');
            assert_eq!(terminal.get_output(), b"\x1b[1;2R");
            assert_eq!(terminal.parser.retained_bytes(), 0);
        }
    }
}

#[test]
fn bounded_cancelled_csi_cannot_change_host_color_or_cursor_mode() {
    for cancel in [0x18, 0x1a] {
        let mut bytes = b"\x1b[31".to_vec();
        bytes.push(cancel);
        bytes.extend_from_slice(b"m\x1b[?1");
        bytes.push(cancel);
        bytes.extend_from_slice(b"hZ\x1b[6n");
        for cut in 0..=bytes.len() {
            let mut terminal = TerminalState::new(40, 8);
            terminal.process_input(&bytes[..cut]);
            terminal.process_input(&bytes[cut..]);
            assert!(!terminal.is_application_cursor_keys());
            for (col, character) in ['m', 'h', 'Z'].into_iter().enumerate() {
                assert_eq!(
                    terminal.grid[0][col].character, character,
                    "cancel={cancel} cut={cut}"
                );
                assert_eq!(terminal.grid[0][col].foreground, Color::Default);
            }
            assert_eq!(terminal.get_output(), b"\x1b[1;4R");
            assert_eq!(terminal.parser.retained_bytes(), 0);
        }
    }
}
