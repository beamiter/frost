//! Immutable original-scanner oracle, captured before the shared parser adapter.
//! This is NOT a comparison of the new parser with another copy of itself.
//! Every feed partition has its own recorded prefix/final expectations: Frost's
//! original scanner intentionally has some partition-sensitive behavior.
use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const ORIGINAL_SOURCE_SHA256: &str =
    "feab8952b2dda2876e7288673c9e30cb7b8f8f8a5ac8065c795e4b1f703fb990";
const REGISTRATION: &str = "#[cfg(test)]\nmod parser_oracle_tests;\n";

fn fingerprint(bytes: &[u8]) -> Value {
    let hash = bytes.iter().fold(14695981039346656037u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(1099511628211)
    });
    json!({"bytes": bytes.len(), "fnv1a64": format!("{hash:016x}")})
}

fn lane(value: impl std::fmt::Debug) -> Value {
    let text = format!("{value:?}");
    if text.len() <= 192 {
        Value::String(text)
    } else {
        fingerprint(text.as_bytes())
    }
}

// Keep carry bytes bounded in the fixture, including the 1 MiB cases. Scan
// offsets and the complete active UTF-8 bytes remain independently observable.
// After extraction only these field reads may be mapped to parser.snapshot().
fn parser_pending_snapshot(t: &TerminalState, result: &mut BTreeMap<String, Value>) {
    let pending = t.parser.snapshot();
    for (name, bytes) in [
        ("pending_escape", &pending.pending_escape),
        ("pending_apc", &pending.pending_apc),
        ("pending_osc", &pending.pending_osc),
        ("pending_dcs", &pending.pending_string),
    ] {
        result.insert(
            name.to_owned(),
            if bytes.len() <= 96 {
                lane(bytes)
            } else {
                fingerprint(bytes)
            },
        );
    }
    result.insert(
        "pending_scan_offsets".into(),
        lane((
            pending.pending_apc_scan_from,
            pending.pending_osc_scan_from,
            pending.pending_string_scan_from,
        )),
    );
    result.insert(
        "pending_apc_discard".into(),
        lane((
            pending.discarding_oversized_apc,
            pending.discarding_apc_prev_escape,
        )),
    );
    result.insert(
        "utf8".into(),
        lane((
            &pending.utf8_buf[..pending.utf8_len as usize],
            pending.utf8_len,
            (pending.utf8_len > 0).then_some(pending.utf8_expected),
        )),
    );
}

fn snapshot(t: &mut TerminalState) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    macro_rules! fields {
        ($($field:ident),* $(,)?) => {$(out.insert(stringify!($field).into(), lane(&t.$field));)*};
    }
    out.insert("window_title".into(), lane(&t.titles.window_title));
    out.insert("icon_title".into(), lane(&t.titles.icon_title));
    out.insert("title_stack".into(), lane(&t.titles.title_stack));
    out.insert("dynamic_fg".into(), lane(t.colors.dynamic_fg));
    out.insert("dynamic_bg".into(), lane(t.colors.dynamic_bg));
    out.insert(
        "dynamic_cursor_color".into(),
        lane(t.colors.dynamic_cursor_color),
    );
    out.insert("dynamic_palette".into(), lane(t.colors.dynamic_palette));
    fields!(
        current_working_dir,
        organism_idle_cwd,
        current_fg,
        current_bg,
        current_flags,
        global_bg,
        current_hyperlink,
        osc8_hyperlinks,
        output_buffer,
        modes,
        keyboard_enhancement_flags,
        keyboard_enhancement_stack,
        alt_keyboard_enhancement_flags,
        alt_keyboard_enhancement_stack,
        xterm_modify_other_keys,
        xterm_format_other_keys,
        pending_clipboard_requests,
        pending_paste_password,
        pending_osc52_clipboard_set,
        pending_osc52_clipboard_queries,
        pending_notifications,
        sync_output_active,
        osc_reply_terminator,
        current_zone_state,
        next_zone_id,
        current_command_start_col,
        current_command_extent_row,
        current_output_start_col,
        current_output_extent_row,
        current_output_extent_col,
        agent_prompt_input_tainted,
        prompt_submission_pending,
        prompt_cancel_pending,
        chat_native_takeover,
        idle_prompt_input_dirty,
        pending_prompt_typeahead,
        agent_prompt_generation,
        current_command_text,
        current_command_exact,
        current_command_id,
        current_command_start_id,
        current_command_start_lifecycle,
        current_command_cwd,
        current_command_truncated,
        consumed_execution_ids,
        captured_output_bytes,
        pending_zone_row_trim
    );
    parser_pending_snapshot(t, &mut out);
    out.insert(
        "cursor".into(),
        lane((
            t.cursor_row,
            t.cursor_col,
            t.cursor_shape,
            t.pending_wrap,
            t.last_printed_char,
        )),
    );
    out.insert(
        "screen".into(),
        lane((
            t.use_alt_buffer,
            t.saved_cursor_row,
            t.saved_cursor_col,
            t.alt_cursor_row,
            t.alt_cursor_col,
            t.scroll_region_top,
            t.scroll_region_bottom,
            t.scroll_offset,
        )),
    );
    out.insert(
        "charsets_tabs".into(),
        lane((t.g0_charset, t.g1_charset, t.active_charset, &t.tab_stops)),
    );
    for (name, saved) in [
        ("saved_cursor", &t.saved_cursor),
        ("saved_primary", &t.saved_primary_screen_state),
    ] {
        out.insert(
            name.into(),
            lane(saved.as_ref().map(|s| {
                (
                    (s.row, s.col, s.fg, s.bg, s.flags),
                    (s.g0, s.g1, s.active, s.origin_mode, s.pending_wrap),
                )
            })),
        );
    }
    out.insert(
        "saved_primary_colors".into(),
        lane((
            t.saved_primary_global_bg,
            t.saved_primary_cursor_shape,
            t.saved_primary_colors.dynamic_fg,
            t.saved_primary_colors.dynamic_bg,
            t.saved_primary_colors.dynamic_cursor_color,
            t.saved_primary_colors.dynamic_palette,
        )),
    );
    out.insert("grid_cells".into(), lane(t.grid.to_vec()));
    out.insert("alt_grid_cells".into(), lane(t.alt_grid.to_vec()));
    out.insert(
        "grid_text".into(),
        lane(
            t.grid
                .to_vec()
                .iter()
                .map(|r| r.iter().map(|c| c.character).collect::<String>())
                .collect::<Vec<_>>(),
        ),
    );
    out.insert(
        "wraps".into(),
        lane((t.grid.row_wrapped(), t.alt_grid.row_wrapped())),
    );
    out.insert(
        "history_cells".into(),
        lane(
            t.scrollback
                .iter()
                .map(|r| (r.decompress(), r.is_wrapped))
                .collect::<Vec<_>>(),
        ),
    );
    out.insert(
        "versions".into(),
        lane((
            t.grid_version,
            &t.row_versions,
            t.history_revision,
            t.full_screen_scroll_revision,
            t.scrollback_pushes,
        )),
    );
    out.insert(
        "watch".into(),
        lane((t.watch_generation, t.watch_generation(), t.watch_batch())),
    );

    // Stable raw IDs come from a process-global allocator. Compare retained
    // absolute-row references instead, while preserving absent/stale anchors.
    let row = |id: RawRowId| t.retained_absolute_for_raw(id);
    out.insert(
        "output_origins".into(),
        lane((
            t.current_output_start_row_id.map(row),
            t.current_output_extent_row_id.map(row),
            t.spacing_prompt_rows
                .iter()
                .map(|id| row(*id))
                .collect::<Vec<_>>(),
        )),
    );
    let mut provenance = t
        .finished_output_provenance
        .iter()
        .map(|(id, p)| {
            (
                *id,
                (row(p.range.start.row), p.range.start.col),
                (row(p.range.end.row), p.range.end.col),
                p.row_ids.iter().map(|id| row(*id)).collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    provenance.sort_by_key(|p| p.0);
    out.insert("finished_provenance".into(), lane(provenance));
    out.insert(
        "background_geometry".into(),
        lane(t.idle_background_output.as_ref().map(|b| {
            (
                (
                    b.initial_col,
                    b.columns,
                    b.start_row,
                    b.start_row_id.map(row),
                    b.start_col,
                ),
                (
                    b.last_row,
                    b.last_row_id.map(row),
                    b.last_col_end,
                    b.anchor_started,
                    b.rows_evicted,
                ),
                (b.raw_len, b.raw_truncated),
            )
        })),
    );
    out.insert(
        "background_raw".into(),
        t.idle_background_output
            .as_ref()
            .map(|b| {
                let bytes = b.raw_bytes();
                if bytes.len() <= 192 {
                    lane(bytes)
                } else {
                    fingerprint(&bytes)
                }
            })
            .unwrap_or(Value::Null),
    );
    out.insert(
        "background_chunks".into(),
        lane(t.idle_background_output.as_ref().map(|b| {
            b.raw_chunks
                .iter()
                .map(|c| (c.start, c.bytes.len(), c.splittable_ascii))
                .collect::<Vec<_>>()
        })),
    );
    out.insert(
        "zones".into(),
        lane(
            t.command_zones
                .iter()
                .cloned()
                .map(|mut z| {
                    z.duration_ms = None;
                    z.finished_at_ms = None;
                    z
                })
                .collect::<Vec<_>>(),
        ),
    );
    out.insert(
        "zone_exports".into(),
        lane(
            t.command_zones
                .iter()
                .map(|z| (z.id, t.zone_output_export_capped(z.id)))
                .collect::<Vec<_>>(),
        ),
    );
    out.insert(
        "completed_commands".into(),
        lane(
            t.pending_completed_commands
                .iter()
                .cloned()
                .map(|mut c| {
                    c.duration_ms = None;
                    c
                })
                .collect::<Vec<_>>(),
        ),
    );
    out.insert(
        "graphics_count".into(),
        lane(t.kitty_graphics.image_count()),
    );
    out.insert(
        "graphics_placements".into(),
        lane(t.kitty_graphics.get_placements()),
    );
    out.insert(
        "graphics_images".into(),
        lane([9, 41, 42, 99].map(|id| {
            t.kitty_graphics
                .get_image(id)
                .map(|i| (i.format, i.width, i.height, &i.data))
        })),
    );

    // Probe both identity and collapsed projections at each boundary. Do not
    // serialize global IDs, clock values, image generations, or cache handles.
    let mut policy = ProjectionPolicy::new();
    for z in &t.command_zones {
        policy.collapse(z.id);
    }
    for (name, projected) in [
        ("identity_projection", t.get_projected_viewport(true)),
        (
            "collapsed_projection",
            t.get_projected_viewport_with_policy(true, &policy, 0),
        ),
    ] {
        let origins = (0..projected.cells().len())
            .map(|r| {
                (0..projected.cells()[r].len())
                    .map(|c| {
                        projected
                            .view_to_raw(ViewportCell { row: r, col: c })
                            .map(|p| (t.retained_absolute_for_raw(p.row), p.col))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        out.insert(format!("{name}_cells"), lane(projected.cells()));
        out.insert(format!("{name}_origins"), lane(origins));
        out.insert(
            format!("{name}_layout"),
            lane((
                projected.mode,
                projected.document_rows,
                projected.document_start,
                projected.top_padding,
                projected.row_wrapped(),
                &projected.effective_collapsed,
            )),
        );
    }
    out
}

fn short_cases() -> Vec<(String, Vec<u8>)> {
    let mut cases: Vec<(String, Vec<u8>)> = [
        ("text_controls", b"ab\tC\x08D\rE\nF\x0bG\x0cH\x7fZ".as_slice()),
        ("utf8_valid", "a界e\u{301}🙂z".as_bytes()),
        ("utf8_overlong", b"A\xe0\x80\x80Z"),
        ("utf8_interrupted", b"A\xe2\x82AZ"),
        ("utf8_invalid_mixed", b"\xed\xa0\x80\xf4\x90\x80\x80\xe2A\xc0\xff\xf0\x9f\x1bcZ"),
        ("charset_escape_escape", b"a\x1b(\x1b[31mX\x1b(BZ"),
        ("charset_multidesignators", b"\x1b(0lqk\x1b)0\x0ex\x0f\x1b(Ba\x1b*0q\x1b+0q\x1b-0q\x1b.0q\x1b/0q\x1b%GZ\x1b#8Q\x1b( 0q"),
        ("unknown_escape_c0_high", b"A\x1b\nB\x1b\rC\x1b\tD\x1b\x18E\x1b\xffF\x1b\xe2\x82\xacG\x1bNZ"),
        ("csi_empty_fields", b"\x1b[;3H\x1b[;31;mA\x1b[1;;4H\x1b[;;mB\x1b[9999999999C\x1b[6n"),
        ("sgr_colon", b"\x1b[38:2::1:2:3mA\x1b[48:5:67mB\x1b[4:3;58:2::7:8:9mC\x1b[0mD"),
        ("csi_embedded_c0", b"abc\x1b[1\r\n;3\x08\t\x0b\x0c\x0e\x0f\x00mX\x1b[6n"),
        ("csi_cancel", b"\x1b[31\x18mA\x1b[32\x1amB\x1b[33\x1b[34mC\x1b[12\xfftail\x1b[0mZ"),
        ("osc_title_colors", b"\x1b]0;title\x07\x1b]2;other\x1b\\\x1b]10;#010203\x07\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]4;1;rgb:11/22/33\x07"),
        ("osc8_clipboard", b"\x1b]8;id=test;https://example.test/a\x1b\\link\x1b]8;;\x07\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x1b\\"),
        ("modes_replies", b"\x1b[?7l\x1b[?1;25;1004;2004h\x1b[5n\x1b[6n\x1b[c\x1b[>c\x1b[=17u\x1b[?u\x1b[>4;2m\x1b[?2026hX\x1b[?2026l"),
        ("alt_screen_cursor", b"A\x1b7\x1b[31m\x1b[?1049hALT\x1b[>8u\x1b[?1049l\x1b8Z\x1b[6n"),
        ("kitty_transfer", b"\x1b_Ga=T,f=32,s=1,v=1,i=41;AQIDBA==\x1b\\\x1b_Ga=q,i=99;\x1b\\Z"),
        ("kitty_multipart", b"\x1b_Ga=T,f=32,s=1,v=1,i=42,m=1;AQID\x1b\\\x1b_Gm=0;BA==\x1b\\Z"),
        ("watch_markers", b"\x1b]133;C\x07\x1b]133;A\x07$ \x1b]133;B\x07echo x\x1b]133;C\x07x\x1b]133;D;0\x07\x1b]133;A\x07"),
        ("shell_lifecycle", b"\x1b]133;A;cwd=/tmp\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C;id=abc;session_id=s1;seq=1;started_at_ms=100;cmdline_url=echo%20hi\x07hi\r\n\x1b]133;D;0;id=abc;duration_ms=7\x07"),
        ("background_privacy", b"\x1b]133;A\x07$ \x1b]133;B\x07visible\x1b]0;private-title\x07\x1bPprivate-dcs\x1b\\\x1bXprivate-sos\x1b\\\x1b^private-pm\x1b\\\x1b_Gprivate-apc\x1b\\\r\x1b[2Ctail\x1b]133;D;17\x07\x1b]133;A\x07"),
        ("background_utf8_erase", b"\x1b]133;A\x07$ \x1b]133;B\x07A\xe0\x80\x80\xe2\x82AZ\r\x1b[3Xok\r\n\x1b]133;A\x07"),
        ("incomplete_csi", b"A\x1b[38:2::1\r\n:2:"),
        ("incomplete_osc", b"A\x1b]0;unfinished\x1b"),
    ].into_iter().map(|(n,b)| (n.to_owned(),b.to_vec())).collect();
    for count in [127, 128, 129] {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b';', count));
        bytes.extend_from_slice(b"31mX\x1b[6n");
        cases.push((format!("csi_parameter_bytes_{count}"), bytes));
    }
    for (name, opener) in [
        ("osc", b"\x1b]0;".as_slice()),
        ("dcs", b"\x1bP"),
        ("sos", b"\x1bX"),
        ("pm", b"\x1b^"),
        ("apc", b"\x1b_G"),
    ] {
        for (ending, suffix) in [
            ("st", b"hidden\x1b\\Z".as_slice()),
            ("can", b"hidden\x18Z"),
            ("sub", b"hidden\x1aZ"),
            ("ris", b"hidden\x1bcZ"),
            ("escape_restart", b"hidden\x1b[31mZ"),
        ] {
            cases.push((format!("string_{name}_{ending}"), [opener, suffix].concat()));
        }
    }
    cases
}

fn record(
    name: &str,
    bytes: &[u8],
    ends: Vec<usize>,
    states: &mut Vec<Value>,
    state_ids: &mut BTreeMap<String, usize>,
) -> Value {
    let mut terminal = TerminalState::new(24, 6);
    // This frozen pre-extraction oracle includes old unsafe overflow/reply
    // semantics. Keep its scope explicitly historical; production bounded
    // behavior is covered separately and must not rewrite these fixtures.
    terminal.parser = jterm_engine::parser::Parser::with_profile(
        jterm_engine::parser::ParserLimits {
            pending_bytes: MAX_PENDING_ESCAPE,
            kitty_recovery_bytes: jterm_core::kitty_graphics::MAX_CONTROL_BYTES,
        },
        jterm_engine::parser::ParserProfile::FrostLegacyV1,
    );
    terminal.max_scrollback = 32;
    let mut start = 0;
    let mut snapshots = Vec::new();
    for &end in &ends {
        assert!(end >= start && end <= bytes.len());
        terminal.process_batch(&bytes[start..end]);
        let value = serde_json::to_value(snapshot(&mut terminal)).unwrap();
        let key = serde_json::to_string(&value).unwrap();
        let index = *state_ids.entry(key).or_insert_with(|| {
            let id = states.len();
            states.push(value);
            id
        });
        snapshots.push(index);
        start = end;
    }
    assert_eq!(start, bytes.len());
    json!({"case":name,"ends":ends,"states":snapshots})
}

fn captured_oracle() -> Value {
    let mut states = Vec::new();
    let mut state_ids = BTreeMap::new();
    let mut records = Vec::new();
    let mut inputs = Vec::new();
    for (name, bytes) in short_cases() {
        inputs.push(json!({"name":name,"input":fingerprint(&bytes)}));
        for cut in 0..=bytes.len() {
            records.push(record(
                &name,
                &bytes,
                vec![cut, bytes.len()],
                &mut states,
                &mut state_ids,
            ));
        }
        records.push(record(
            &name,
            &bytes,
            (1..=bytes.len()).collect(),
            &mut states,
            &mut state_ids,
        ));
        for chunk in [2, 7] {
            let ends = (chunk..bytes.len())
                .step_by(chunk)
                .chain(std::iter::once(bytes.len()))
                .collect();
            records.push(record(&name, &bytes, ends, &mut states, &mut state_ids));
        }
    }
    // Selected cuts only for giant packets: complete vs buffered, the exact
    // limit +/- 1, split ST, abandonment and following valid recovery output.
    for (kind, opener) in [
        ("osc", b"\x1b]0;".as_slice()),
        ("dcs", b"\x1bP"),
        ("sos", b"\x1bX"),
        ("pm", b"\x1b^"),
        ("apc", b"\x1b_Gi=9,q=1;"),
    ] {
        for len in [
            MAX_PENDING_ESCAPE - 1,
            MAX_PENDING_ESCAPE,
            MAX_PENDING_ESCAPE + 1,
            MAX_PENDING_ESCAPE + 33,
        ] {
            let name = format!("limit_{kind}_{len}");
            let mut bytes = opener.to_vec();
            bytes.resize(len, b'A');
            bytes.extend_from_slice(b"\x1b\\\x1b[32mRECOVER\x1b[0m");
            inputs.push(json!({"name":name,"input":fingerprint(&bytes)}));
            let mut cuts = vec![0, 1, 2, opener.len(), len - 1, len, len + 1, bytes.len()];
            cuts.sort_unstable();
            cuts.dedup();
            for cut in cuts {
                records.push(record(
                    &name,
                    &bytes,
                    vec![cut, bytes.len()],
                    &mut states,
                    &mut state_ids,
                ));
            }
            records.push(record(
                &name,
                &bytes,
                vec![2, len - 1, len, len + 1, bytes.len()],
                &mut states,
                &mut state_ids,
            ));
        }
    }
    json!({"format":1,"original_source_sha256":ORIGINAL_SOURCE_SHA256,"inputs":inputs,"records":records,"snapshots":states})
}

fn verify_original_source_before_capture() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let source = include_str!("../terminal.rs").replace(REGISTRATION, "");
    assert!(source.contains("fn process_input_inner") && source.contains("pending_apc: Vec<u8>"));
    let mut command = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    command
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let output = command.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap(),
        ORIGINAL_SOURCE_SHA256,
        "capture requires the immutable original scanner, not a shared-parser result"
    );
}

#[test]
fn historical_legacy_parser_oracle_original_scanner() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser/frost-original-scanner-oracle.json");
    let capture = std::env::var_os("FROST_CAPTURE_PARSER_ORACLE").is_some();
    if capture {
        verify_original_source_before_capture();
        assert!(!path.exists(), "frozen fixture must never be overwritten");
    }
    let actual = captured_oracle();
    if capture {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(&serde_json::to_vec(&actual).unwrap())
            .unwrap();
        eprintln!(
            "Captured {} records, {} snapshots, {} cases",
            actual["records"].as_array().unwrap().len(),
            actual["snapshots"].as_array().unwrap().len(),
            actual["inputs"].as_array().unwrap().len()
        );
    } else {
        let expected: Value = serde_json::from_slice(
            &std::fs::read(&path)
                .expect("original scanner fixture must be captured before extraction"),
        )
        .unwrap();
        assert_eq!(
            actual["original_source_sha256"],
            expected["original_source_sha256"]
        );
        assert_eq!(
            actual["inputs"], expected["inputs"],
            "oracle corpus changed"
        );
        let a = actual["records"].as_array().unwrap();
        let b = expected["records"].as_array().unwrap();
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            assert_eq!(a["case"], b["case"]);
            assert_eq!(a["ends"], b["ends"]);
            assert_eq!(
                a["states"].as_array().unwrap().len(),
                b["states"].as_array().unwrap().len()
            );
            for (feed, (ai, bi)) in a["states"]
                .as_array()
                .unwrap()
                .iter()
                .zip(b["states"].as_array().unwrap())
                .enumerate()
            {
                let actual = &actual["snapshots"][ai.as_u64().unwrap() as usize];
                let expected = &expected["snapshots"][bi.as_u64().unwrap() as usize];
                assert_eq!(
                    actual.as_object().unwrap().keys().collect::<Vec<_>>(),
                    expected.as_object().unwrap().keys().collect::<Vec<_>>()
                );
                for (lane, want) in expected.as_object().unwrap() {
                    assert_eq!(
                        &actual[lane], want,
                        "case {} ends {} feed {} lane {}",
                        a["case"], a["ends"], feed, lane
                    );
                }
            }
        }
    }
}
