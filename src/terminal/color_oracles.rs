//! Frost color replay observer adapted only for the current color owner read paths.
//! Expected values are the immutable, independently sealed original-production stream.
//! Every Debug lane retains the complete value, never fingerprints/truncation.
use super::*;
use serde_json::{json, Value};
fn lane(value: impl std::fmt::Debug) -> Value {
    Value::String(format!("{value:?}"))
}
fn zones(values: &VecDeque<CommandZone>) -> Value {
    lane(
        values
            .iter()
            .cloned()
            .map(|mut z| {
                // Preserve presence; normalize wall-clock magnitudes only on the clone.
                z.finished_at_ms = z.finished_at_ms.map(|_| 0);
                z
            })
            .collect::<Vec<_>>(),
    )
}
fn full_snapshot(t: &TerminalState) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    macro_rules! fields { ($($f:ident),* $(,)?) => {$(out.insert(stringify!($f).into(), lane(&t.$f));)*}; }
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
    out.insert(
        "saved_primary_dynamic_fg".into(),
        lane(t.saved_primary_colors.dynamic_fg),
    );
    out.insert(
        "saved_primary_dynamic_bg".into(),
        lane(t.saved_primary_colors.dynamic_bg),
    );
    out.insert(
        "saved_primary_dynamic_cursor_color".into(),
        lane(t.saved_primary_colors.dynamic_cursor_color),
    );
    out.insert(
        "saved_primary_dynamic_palette".into(),
        lane(t.saved_primary_colors.dynamic_palette),
    );
    fields!(
        scrollback,
        selection,
        projected_selection,
        scroll_offset,
        pending_saved_line_purge,
        scrollback_pushes,
        full_screen_scroll_revision,
        provisional_alt_snapshot,
        max_scrollback,
        inline_cli_launch_floor,
        inline_cli_command_scope,
        use_alt_buffer,
        disable_alt_screen,
        viewport_pixel_width,
        viewport_pixel_height,
        cursor_row,
        cursor_col,
        saved_cursor_row,
        saved_cursor_col,
        saved_primary_global_bg,
        saved_primary_cursor_shape,
        tab_stops,
        last_printed_char,
        pending_wrap,
        alt_cursor_row,
        alt_cursor_col,
        cursor_shape,
        current_fg,
        current_bg,
        current_flags,
        current_working_dir,
        organism_idle_cwd,
        global_bg,
        scroll_region_top,
        scroll_region_bottom,
        g0_charset,
        g1_charset,
        active_charset,
        ime_enabled,
        preedit_text,
        preedit_selection,
        modes,
        output_buffer,
        keyboard_enhancement_flags,
        keyboard_enhancement_stack,
        alt_keyboard_enhancement_flags,
        alt_keyboard_enhancement_stack,
        xterm_modify_other_keys,
        xterm_format_other_keys,
        pending_clipboard_requests,
        pending_paste_password,
        grid_version,
        row_versions,
        visible_cells_cache,
        frame_revision,
        frame_layout,
        original_visible_cells_cache,
        history_revision,
        projected_viewport_cache,
        projection_plan_cache,
        next_projected_view_revision,
        next_projection_plan_revision,
        current_hyperlink,
        osc8_hyperlinks,
        sync_output_active,
        last_archived_screen_snapshot,
        last_synced_primary_screen_snapshot,
        pending_osc52_clipboard_set,
        pending_osc52_clipboard_queries,
        next_zone_id,
        current_zone_state,
        spacing_prompt_rows,
        watch_generation,
        watch_batch,
        current_command_start_col,
        current_command_extent_row,
        current_output_start_col,
        current_output_start_row_id,
        current_output_extent_row,
        current_output_extent_col,
        current_output_extent_row_id,
        agent_prompt_input_tainted,
        prompt_submission_pending,
        prompt_cancel_pending,
        chat_native_takeover,
        idle_prompt_input_dirty,
        pending_prompt_typeahead,
        idle_background_output,
        agent_prompt_generation,
        armed_agent_execution,
        active_agent_execution,
        current_command_text,
        current_command_exact,
        theme_default_colors,
        theme_ansi_colors,
        osc_reply_terminator,
        pending_notifications,
        current_command_id,
        current_command_start_id,
        current_command_start_lifecycle,
        pending_zone_row_trim,
        consumed_execution_ids,
        current_command_cwd,
        current_command_truncated,
        captured_output_bytes,
    );
    for (name, g) in [("grid", &t.grid), ("alt_grid", &t.alt_grid)] {
        out.insert(
            name.into(),
            lane((
                g.rows(),
                g.cols(),
                g.to_vec(),
                g.row_ids(),
                g.row_wrapped(),
                g.identity_revision,
            )),
        );
    }
    for (name, s) in [
        ("saved_cursor", &t.saved_cursor),
        ("saved_primary_screen_state", &t.saved_primary_screen_state),
    ] {
        out.insert(
            name.into(),
            lane(s.as_ref().map(|s| {
                (
                    (s.row, s.col, s.fg, s.bg, s.flags),
                    (s.g0, s.g1, s.active, s.origin_mode, s.pending_wrap),
                )
            })),
        );
    }
    out.insert("parser".into(), lane(t.parser.snapshot()));
    out.insert(
        "sync_output_start".into(),
        json!(t.sync_output_start.is_some()),
    );
    out.insert(
        "current_command_started_at".into(),
        json!(t.current_command_started_at.is_some()),
    );
    let mut links = t.osc8_hyperlink_keys.iter().collect::<Vec<_>>();
    links.sort_by_key(|(_, id)| **id);
    out.insert("osc8_hyperlink_keys".into(), lane(links));
    let mut provenance = t.finished_output_provenance.iter().collect::<Vec<_>>();
    provenance.sort_by_key(|(id, _)| **id);
    out.insert("finished_output_provenance".into(), lane(provenance));
    out.insert("command_zones".into(), zones(&t.command_zones));
    out.insert(
        "pending_completed_commands".into(),
        lane(&t.pending_completed_commands),
    );
    out.insert(
        "kitty_graphics".into(),
        t.kitty_graphics.title_oracle_snapshot(),
    );
    out.insert("cleared_blocks".into(),t.cleared_blocks.as_ref().map(|s| {
        let mut p=s.provenance.iter().collect::<Vec<_>>();p.sort_by_key(|(id,_)|**id);
        json!({"scrollback":lane(&s.scrollback),"grid_rows":lane(&s.grid_rows),"zones":zones(&s.zones),"provenance":lane(p),"captured_output_bytes":s.captured_output_bytes,"placements":lane(&s.placements),"images":lane(&s.images)})
    }).unwrap_or(Value::Null));
    out
}

struct ObservedSeed {
    terminal: TerminalState,
    retained_rows: Arc<VisibleRows>,
    retained_view: Arc<ProjectedViewport>,
}
fn retained_snapshot(seed: &ObservedSeed) -> BTreeMap<String, Value> {
    let mut out = full_snapshot(&seed.terminal);
    out.insert("retained_rows".into(), lane(&seed.retained_rows));
    out.insert("retained_view".into(), lane(&seed.retained_view));
    out.insert(
        "retained_rows_is_current".into(),
        json!(seed
            .terminal
            .visible_cells_cache
            .as_ref()
            .is_some_and(|(_, _, rows, _)| Arc::ptr_eq(rows, &seed.retained_rows))),
    );
    out.insert(
        "retained_view_is_current".into(),
        json!(seed
            .terminal
            .projected_viewport_cache
            .as_ref()
            .is_some_and(|(_, view)| Arc::ptr_eq(view, &seed.retained_view))),
    );
    out
}
fn seed(which: usize) -> ObservedSeed {
    let mut t = TerminalState::new(6, 5);
    t.max_scrollback = 8;
    // First seed only in the dedicated worker must start the global ID domain.
    if which == 0 && std::env::var_os("FROST_COLOR_ALLOCATOR_FIRST").is_some() {
        assert_eq!(t.grid.row_ids()[0], RawRowId::new(1));
        assert_eq!(t.alt_grid.row_ids()[0], RawRowId::new(6));
        std::env::remove_var("FROST_COLOR_ALLOCATOR_FIRST");
    }
    // Actual production batches establish history, rich cells, output provenance,
    // a completed command, and an idle prompt without a ticking active command.
    t.process_batch(b"history0\r\nhistory1\r\nhistory2\r\nhistory3\r\nhistory4\r\nhistory5\r\n");
    t.process_batch(b"\x1b]133;A;cwd=/tmp\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C;id=title-seed;session_id=title-session;seq=1;started_at_ms=100;cmdline_url=echo%20hi\x07hi\r\n\x1b]133;D;0;id=title-seed;duration_ms=7\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    t.process_batch(
        "\x1b[31;44;1m界Z\x1b[0m\x1b]8;id=title;https://example.test/title\x07link\x1b]8;;\x07"
            .as_bytes(),
    );
    t.process_batch(
        b"\x1b]10;#123456\x07\x1b]11;#234567\x07\x1b]12;#345678\x07\x1b]4;3;#456789\x07",
    );
    // Establish primary overrides through real OSC, never assign inactive stash.
    for i in 0..256u16 {
        let input = format!(
            "\x1b]4;{i};#{:02x}{:02x}{:02x}\x07",
            i as u8,
            (i as u8).wrapping_add(31),
            (i as u8).wrapping_add(71)
        );
        t.process_batch(input.as_bytes());
    }
    if which & 1 != 0 {
        t.process_batch(b"\x1b[?1049hALT\x1b]10;#a1b2c3\x07\x1b]11;#c3b2a1\x07\x1b]12;#102938\x07\x1b]4;0;#abcdef;255;#fedcba\x07");
        assert_ne!(
            t.colors.dynamic_palette,
            t.saved_primary_colors.dynamic_palette
        );
        assert_ne!(t.colors.dynamic_bg, t.saved_primary_colors.dynamic_bg);
    } else {
        assert!(t
            .saved_primary_colors
            .dynamic_palette
            .iter()
            .all(Option::is_none));
    }
    if which & 2 != 0 {
        t.process_batch(b"\x1b[?2026h");
    }
    t.titles.window_title = format!("seed-{which}");
    t.titles.icon_title = format!("icon-{which}");
    t.titles.title_stack = match which {
        0 => vec![],
        1 => vec![(Some("icon-only".into()), None)],
        2 => vec![(None, Some("window-only".into()))],
        _ => (0..16)
            .map(|n| (Some(format!("i{n}")), Some(format!("w{n}"))))
            .collect(),
    };
    if which >= 2 {
        let poison = format!(
            "a\x07\x1b\u{009b}\u{202e}\u{200b}\u{fffd}{}TAIL",
            "界".repeat(257)
        );
        t.titles.window_title = poison.clone();
        t.titles.icon_title = poison.clone();
        if which == 3 {
            t.titles.title_stack[15] = (Some(poison.clone()), Some(poison));
        } else {
            t.titles
                .title_stack
                .push((Some(poison.clone()), Some(poison)));
        }
    }
    t.process_batch(
        b"\x1b_Ga=T,f=32,s=1,v=1,i=41;AQIDBA==\x1b\\\x1b_Ga=T,f=32,s=1,v=1,i=42,m=1;AQID\x1b\\",
    );
    t.armed_agent_execution = Some(ArmedAgentExecution {
        generation: 17,
        prompt_generation: t.agent_prompt_generation,
        command: "approved-sentinel".into(),
    });
    t.active_agent_execution = Some(ActiveAgentExecution {
        generation: 19,
        execution_id: Some("active-sentinel".into()),
    });
    t.selection = Some(Selection {
        anchor: (0, 0),
        active: (1, 2),
        mode: SelectionMode::Normal,
    });
    t.preedit_text = "IME界".into();
    t.preedit_selection = Some(0..3);
    t.ime_enabled = true;
    t.pending_notifications
        .push(("title-sentinel".into(), "keep".into()));
    t.pending_osc52_clipboard_set = Some("clipboard-sentinel".into());
    t.pending_paste_password = Some("password-sentinel-not-a-real-secret".into());
    t.pending_clipboard_requests.push(ClipboardReadRequest {
        kind: ClipboardReadKind::MimeData("text/plain".into()),
    });
    t.set_default_colors(
        (10, 20, 30),
        if which < 2 {
            (40, 50, 60)
        } else {
            (240, 230, 220)
        },
        (70, 80, 90),
    );
    if which != 0 {
        t.set_ansi_colors(std::array::from_fn(|i| (i as u8, 2 * i as u8, 3 * i as u8)));
    }
    t.output_buffer.extend_from_slice(b"reply-sentinel");
    // Seed-only explicit renderer call; no getter occurs during observation.
    assert!(
        t.command_zones.iter().any(|z| z.duration_ms == Some(7)),
        "seed must preserve explicit shell duration"
    );
    let retained_rows = t.get_visible_cells();
    let retained_view = t.get_projected_viewport(true);
    if which >= 2 {
        let mut policy = ProjectionPolicy::new();
        for z in &t.command_zones {
            policy.collapse(z.id);
        }
        let view = t.get_projected_viewport_with_policy(true, &policy, 0);
        if which == 2 {
            assert!(
                !view.effective_collapsed.is_empty(),
                "primary seed2 must have a real effective collapse"
            );
        }
        if which == 3 {
            assert!(
                view.effective_collapsed.is_empty(),
                "alternate seed3 intentionally takes identity fallback"
            );
        }
        let points = (0..view.row_view().len())
            .flat_map(|r| (0..6).map(move |c| (r, c)))
            .filter_map(|pos| TerminalState::projected_selection_point(&view, pos))
            .collect::<Vec<_>>();
        let anchor = *points.first().expect("real projected endpoint");
        let active = *points.last().expect("real projected endpoint");
        assert!(anchor.point != active.point, "nontrivial real selection");
        // Both endpoints are minted by production's actual mapping, not fabricated
        // from grid coordinates. Normalized wide-cell columns follow production.
        t.projected_selection = Some(TerminalState::new_projected_selection(
            &view,
            view.plan_revision,
            anchor,
            active,
            SelectionMode::Normal,
        ));
        t.selection = None;
        // Synthetic retained undo sentinel, not a user-visible clear operation.
        t.cleared_blocks = Some(ClearedBlocksSnapshot {
            scrollback: t.scrollback.iter().cloned().collect(),
            grid_rows: vec![],
            zones: t.command_zones.clone(),
            provenance: t.finished_output_provenance.clone(),
            captured_output_bytes: t.captured_output_bytes,
            placements: t.kitty_graphics.get_placements().to_vec(),
            images: t
                .kitty_graphics
                .get_image(41)
                .map(|i| vec![(41, i.clone())])
                .unwrap_or_default(),
        });
    }
    ObservedSeed {
        terminal: t,
        retained_rows,
        retained_view,
    }
}

fn ends(n: usize, schedule: usize, bulk: bool) -> Vec<usize> {
    if schedule == 0 {
        return vec![n];
    }
    if schedule == 1 {
        return (if bulk { 17 } else { 1 }..n)
            .step_by(if bulk { 17 } else { 1 })
            .chain(std::iter::once(n))
            .collect();
    }
    if schedule > 5 {
        return vec![schedule - 5, n];
    }
    let mut x = (schedule - 1) as u64;
    let mut at = 0;
    let mut out = vec![];
    while at < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        at = (at + 1 + (x % if bulk { 64 } else { 23 }) as usize).min(n);
        out.push(at);
    }
    out
}

// Replay observes production/candidate through the same original emitter.
// This routine only emits actual states, never computes expected states.

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum InputOperation {
    FeedHex(String),
    SetDefaultColors([[u8; 3]; 3]),
    SetAnsiColors([[u8; 3]; 16]),
}
fn rgb(x: [u8; 3]) -> (u8, u8, u8) {
    (x[0], x[1], x[2])
}
fn host_operation(t: &mut TerminalState, op: InputOperation) {
    match op {
        InputOperation::SetDefaultColors(v) => {
            t.set_default_colors(rgb(v[0]), rgb(v[1]), rgb(v[2]))
        }
        InputOperation::SetAnsiColors(v) => t.set_ansi_colors(v.map(rgb)),
        InputOperation::FeedHex(_) => panic!("feed must use scheduled process_batch path"),
    }
}
// Emit exact fixed operation IDs. No independent output model or expected writing.
fn emit_actual<W: std::io::Write>(writer: &mut W, corpus: &Value) {
    let mut count = 0;
    for which in 0..4 {
        let t = seed(which);
        serde_json::to_writer(
            &mut *writer,
            &json!([format!("seed_{which}"), retained_snapshot(&t)]),
        )
        .unwrap();
        writeln!(writer).unwrap();
        count += 1;
    }
    for c in corpus["cases"].as_array().unwrap() {
        for which in 0..4 {
            for schedule in 0..c["schedules"].as_u64().unwrap() as usize {
                let mut t = seed(which);
                for (op_index, value) in c["operations"].as_array().unwrap().iter().enumerate() {
                    let op: InputOperation = serde_json::from_value(value.clone()).unwrap();
                    match op {
                        InputOperation::FeedHex(h) => {
                            assert!(h.is_ascii() && h.len() % 2 == 0);
                            let bytes = (0..h.len())
                                .step_by(2)
                                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
                                .collect::<Vec<_>>();
                            let mut start = 0;
                            for (feed, end) in
                                ends(bytes.len(), schedule, c["partition_policy"] == "bulk")
                                    .into_iter()
                                    .enumerate()
                            {
                                t.terminal.process_batch(&bytes[start..end]);
                                start = end;
                                serde_json::to_writer(&mut *writer,&json!([format!("{}_seed_{which}_schedule_{schedule}_op_{op_index}_feed_{feed}",c["id"].as_str().unwrap()),retained_snapshot(&t)])).unwrap();
                                writeln!(writer).unwrap();
                                count += 1;
                            }
                        }
                        host => {
                            host_operation(&mut t.terminal, host);
                            serde_json::to_writer(
                                &mut *writer,
                                &json!([
                                    format!(
                                        "{}_seed_{which}_schedule_{schedule}_op_{op_index}_host",
                                        c["id"].as_str().unwrap()
                                    ),
                                    retained_snapshot(&t)
                                ]),
                            )
                            .unwrap();
                            writeln!(writer).unwrap();
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(count, corpus["expected_record_count"].as_u64().unwrap());
}

#[path = "color_replay_transport/mod.rs"]
mod color_replay_transport;
#[test]
#[ignore = "isolated private replay worker, parent wrapper only"]
fn replay_actual_worker() {
    color_replay_transport::worker::child(|bytes, writer| {
        let corpus: Value = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
        emit_actual(&mut std::io::BufWriter::new(writer), &corpus);
        Ok(())
    })
    .unwrap();
}
#[test]
fn frozen_original_color_full_state() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color_golden");
    let scratch = std::env::temp_dir().join(format!(
        "frost-color-replay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&scratch).unwrap();
    let actual = color_replay_transport::worker::run(&fixture.join("corpus.json"), &scratch)
        .unwrap_or_else(|e| panic!("worker failed: {e}; retained {}", scratch.display()));
    color_replay_transport::compare(&fixture.join("frost-color-original.jsonl.zst"), &actual)
        .unwrap_or_else(|e| panic!("golden failed: {e}; retained {}", scratch.display()));
    std::fs::remove_dir_all(scratch).unwrap();
}
