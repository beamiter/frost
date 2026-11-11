//! Source-locked original Frost title observer; immutable expected values are external.
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
        saved_primary_dynamic_fg,
        saved_primary_dynamic_bg,
        saved_primary_dynamic_cursor_color,
        saved_primary_dynamic_palette,
        tab_stops,
        last_printed_char,
        pending_wrap,
        alt_cursor_row,
        alt_cursor_col,
        cursor_shape,
        current_fg,
        current_bg,
        current_flags,
        window_title,
        icon_title,
        title_stack,
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
        dynamic_fg,
        dynamic_bg,
        dynamic_cursor_color,
        dynamic_palette,
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
    if which == 0 && std::env::var_os("FROST_TITLE_ALLOCATOR_FIRST").is_some() {
        assert_eq!(t.grid.row_ids()[0], RawRowId::new(1));
        assert_eq!(t.alt_grid.row_ids()[0], RawRowId::new(6));
        std::env::remove_var("FROST_TITLE_ALLOCATOR_FIRST");
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
    if which & 1 != 0 {
        t.process_batch(b"\x1b[?1049hALT");
    }
    if which & 2 != 0 {
        t.process_batch(b"\x1b[?2026h");
    }
    t.window_title = format!("seed-{which}");
    t.icon_title = format!("icon-{which}");
    t.title_stack = match which {
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
        t.window_title = poison.clone();
        t.icon_title = poison.clone();
        if which == 3 {
            t.title_stack[15] = (Some(poison.clone()), Some(poison));
        } else {
            t.title_stack.push((Some(poison.clone()), Some(poison)));
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
    t.theme_default_colors = DefaultColors {
        fg: (10, 20, 30),
        bg: (40, 50, 60),
        cursor: (70, 80, 90),
    };
    t.theme_ansi_colors = Some(std::array::from_fn(|i| (i as u8, 2 * i as u8, 3 * i as u8)));
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

// Exact published original production is checked before every worker emission.
// This routine only emits actual states, never computes expected states.
fn emit_actual<W: std::io::Write>(writer: &mut W, corpus: &Value) {
    verify_original_sources();
    #[allow(unreachable_code)]
    {
        let mut count = 0;
        // Fresh dedicated process; entire raw ID domain is retained unchanged.
        // Do not reuse the process for other tests. No numeric/regex replacement.
        for which in 0..4 {
            let t = seed(which);
            if which == 0 {
                assert_eq!(
                    t.terminal.alt_grid.row_ids()[0],
                    RawRowId::new(6),
                    "fresh allocator required"
                );
            }
            serde_json::to_writer(
                &mut *writer,
                &json!([format!("seed_{which}"), retained_snapshot(&t)]),
            )
            .unwrap();
            writeln!(writer).unwrap();
            count += 1;
        }
        for case in corpus["cases"].as_array().unwrap() {
            let h = case["bytes_hex"].as_str().unwrap();
            let bytes = (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>();
            let policy = case["partition_policy"].as_str().unwrap();
            let schedules = 6 + if policy == "all_two_way" {
                bytes.len() - 1
            } else {
                0
            };
            for which in 0..4 {
                for schedule in 0..schedules {
                    let mut t = seed(which);
                    let mut start = 0;
                    for (feed, end) in ends(bytes.len(), schedule, policy == "bulk")
                        .into_iter()
                        .enumerate()
                    {
                        t.terminal.process_batch(&bytes[start..end]);
                        start = end;
                        serde_json::to_writer(
                            &mut *writer,
                            &json!([
                                format!(
                                    "{}_seed_{which}_schedule_{schedule}_feed_{feed}",
                                    case["id"].as_str().unwrap()
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
        assert_eq!(count, corpus["expected_record_count"].as_u64().unwrap());
    }
}

// Fresh-process wrapper: allocator identity is retained without numeric rewriting.
const WORKER_NAME: &str = "terminal::title_oracles::emit_worker";
const CHILD_MARK: &str = "FROST_TITLE_PRIVATE_WORKER";

fn run_worker(binary: &std::path::Path, corpus: &std::path::Path, dir: &std::path::Path) {
    use std::{
        fs::OpenOptions,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    assert!(
        std::env::var_os(CHILD_MARK).is_none(),
        "parent wrapper recursion forbidden"
    );
    assert!(dir.is_dir());
    let fresh = |name: &str| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(name))
            .unwrap()
    };
    // A bounded listing first verifies one exact ignored test. No output() call.
    let listing = fresh("worker-list.txt");
    let list_status = Command::new(binary)
        .args(["--list", "--ignored", "--exact", WORKER_NAME])
        .stdout(Stdio::from(listing))
        .stderr(Stdio::from(fresh("worker-list.stderr")))
        .status()
        .unwrap();
    assert!(list_status.success(), "worker listing failed");
    assert!(dir.join("worker-list.txt").metadata().unwrap().len() <= 4096);
    let list = std::fs::read_to_string(dir.join("worker-list.txt")).unwrap();
    assert_eq!(
        list.lines()
            .filter(|s| s.ends_with(": test"))
            .collect::<Vec<_>>(),
        vec![format!("{WORKER_NAME}: test").as_str()]
    );
    let output = dir.join("actual.jsonl.gz");
    assert!(!output.exists(), "never overwrite actual stream");
    let started = Instant::now();
    let mut child = Command::new(binary)
        .args([
            "--ignored",
            "--exact",
            WORKER_NAME,
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_MARK, "v1")
        .env("FROST_TITLE_ALLOCATOR_FIRST", "1")
        .env("FROST_TITLE_INPUT_CORPUS", corpus.canonicalize().unwrap())
        .env(
            "FROST_TITLE_ACTUAL_PATH",
            dir.canonicalize().unwrap().join("actual.jsonl.gz"),
        )
        .stdout(Stdio::from(fresh("worker.stdout")))
        .stderr(Stdio::from(fresh("worker.stderr")))
        .spawn()
        .unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(1800) {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            panic!(
                "worker deadline exceeded ({status}); stdout/stderr retained in {}",
                dir.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "worker failed ({status}); inspect {}/worker.stderr",
        dir.display()
    );
    assert!(output.is_file() && output.metadata().unwrap().len() > 0);
    // Downstream validation must stream records and enforce exact IDs/count/lane
    // keys and original source seal before considering this file a baseline.
}

#[test]
#[ignore = "only isolated parent wrapper may launch this actual-state worker"]
fn emit_worker() {
    use std::io::Write;
    assert_eq!(std::env::var(CHILD_MARK).as_deref(), Ok("v1"));
    assert_eq!(
        std::env::var("FROST_TITLE_ALLOCATOR_FIRST").as_deref(),
        Ok("1")
    );
    // Exact source guard covers the published production and pinned dependencies.
    // Keeping it before output creation prevents accidental partial baselines.
    verify_original_sources();
    #[allow(unreachable_code)]
    {
        let corpus: Value = serde_json::from_slice(
            &std::fs::read(std::env::var_os("FROST_TITLE_INPUT_CORPUS").unwrap()).unwrap(),
        )
        .unwrap();
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(std::env::var_os("FROST_TITLE_ACTUAL_PATH").unwrap())
            .unwrap();
        let mut gzip = std::process::Command::new("gzip")
            .args(["-n", "-c", "-6"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::from(out))
            .spawn()
            .unwrap();
        let mut stream = std::io::BufWriter::new(gzip.stdin.take().unwrap());
        emit_actual(&mut stream, &corpus);
        stream.flush().unwrap();
        drop(stream);
        assert!(gzip.wait().unwrap().success());
    }
}

fn verify_original_sources() {
    use std::io::Write;
    fn sha(text: &str) -> String {
        let mut p = std::process::Command::new("sha256sum")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        p.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        let o = p.wait_with_output().unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    }
    let terminal = include_str!("../terminal.rs").replace("#[cfg(test)]\nmod title_oracles;\n", "");
    let kitty = include_str!("../kitty_graphics.rs")
        .split("// TITLE_ORACLE_TEST_HELPER_BEGIN")
        .next()
        .unwrap();
    assert_eq!(
        sha(&terminal),
        "4c76aa0fce582d9b1398ebea0a6f7673b0da126e0188864662abba7faed6d013"
    );
    assert_eq!(
        sha(kitty),
        "4ec3977decb3782f6ad2fb83bb8a3a7c8566b6aafce2be149c1a4f913073f363"
    );
    assert_eq!(
        sha(include_str!("../../Cargo.toml")),
        "227f9f43dae9bf878a399849aae3363fc71159959f8a8559eeb8025aaf4b6290"
    );
    assert_eq!(
        sha(include_str!("../../Cargo.lock")),
        "be3abc7447dd55d1cca4e1caed3a1c24a0c9e288429baf22b38c93107af3f09d"
    );
}

#[test]
#[ignore]
fn seed_preflight() {
    std::env::set_var("FROST_TITLE_ALLOCATOR_FIRST", "1");
    verify_original_sources();
    for which in 0..4 {
        let t = seed(which);
        let value = retained_snapshot(&t);
        println!(
            "seed {} lanes={} bytes={}",
            which,
            value.len(),
            serde_json::to_vec(&value).unwrap().len()
        );
    }
}

#[test]
#[ignore = "explicit source-locked baseline capture driver only"]
fn capture_driver() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("FROST_TITLE_RUN_DIR")
            .expect("explicit create-new output directory required"),
    );
    let corpus = std::path::PathBuf::from(
        std::env::var_os("FROST_TITLE_INPUT_CORPUS").expect("input-only corpus path required"),
    );
    run_worker(&std::env::current_exe().unwrap(), &corpus, &dir);
}
