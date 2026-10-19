//! Browser request ownership, filter semantics, and real-terminal folding.
use super::*;

fn hit(id: u64) -> block_mode::BlockSearchHit {
    block_mode::BlockSearchHit {
        zone_id: id,
        line_no: 0,
        is_output_line: false,
        line_text: "command".into(),
        command_preview: "command".into(),
        match_span: None,
    }
}

fn pending_match() -> (
    BlockSearchState,
    BlockSearchBuildIdentity,
    BlockSearchMatchBuild,
) {
    let identity = BlockSearchBuildIdentity {
        session_id: 7,
        epoch: 12,
    };
    let state = BlockSearchState {
        session_id: identity.session_id,
        epoch: identity.epoch,
        matching: true,
        query: "needle".into(),
        hits: vec![hit(99)],
        zone_version: BlockSearchZoneVersion {
            len: 1,
            oldest: Some(1),
            newest: Some(1),
        },
        ..Default::default()
    };
    let build = BlockSearchMatchBuild {
        cache: vec![block_mode::CachedBlockSearchZone::new(
            1,
            Some("needle"),
            Some("output".into()),
        )],
        intent: state.intent(),
        eligible: [1].into_iter().collect(),
        result: Some(Ok(block_mode::BlockSearchResults {
            hits: vec![hit(1)],
            capped: false,
        })),
    };
    (state, identity, build)
}

#[test]
fn match_worker_returns_the_owned_cache_and_enables_only_current_results() {
    let (mut state, identity, build) = pending_match();
    let cache_address = build.cache.as_ptr();
    let eligible = build.eligible.clone();
    assert!(!state.accepts_hit(&hit(99), identity.session_id, state.zone_version));
    assert!(state.install_match(identity, build, state.zone_version, &eligible));
    assert_eq!(
        state.cache.as_ptr(),
        cache_address,
        "matching must move, never clone, the index"
    );
    assert_eq!(state.hits, [hit(1)]);
    assert!(!state.busy());
    assert!(state.accepts_hit(&hit(1), identity.session_id, state.zone_version));
    assert_eq!(state.matched_intent, Some(state.intent()));
}

#[test]
fn superseded_query_and_changed_filter_membership_request_only_the_latest_pass() {
    for changed in ["query", "filter", "membership", "cancelled"] {
        let (mut state, identity, mut build) = pending_match();
        let mut eligible = build.eligible.clone();
        match changed {
            "query" => state.query = "latest".into(),
            "filter" => state.filter = BlockSearchFilter::Success,
            "membership" => {
                eligible.clear();
            }
            "cancelled" => build.result = None,
            _ => unreachable!(),
        }
        assert!(
            !state.install_match(identity, build, state.zone_version, &eligible),
            "{changed}"
        );
        assert_eq!(state.cache.len(), 1);
        assert_eq!(
            state.hits,
            [hit(99)],
            "stale results must not replace the visible list"
        );
        assert!(state.match_pending && state.busy());
        assert!(!state.matching);
    }
}

#[test]
fn metadata_changes_refresh_settled_filters_without_acknowledging_input_errors() {
    for (filter, old, new) in [
        (
            BlockSearchFilter::Slow,
            BlockSearchFilterContext::Slow(1_000),
            BlockSearchFilterContext::Slow(10_000),
        ),
        (
            BlockSearchFilter::Unread,
            BlockSearchFilterContext::Unread(Some(2)),
            BlockSearchFilterContext::Unread(Some(4)),
        ),
    ] {
        let mut state = BlockSearchState {
            filter,
            filter_context: Some(old),
            hits: vec![hit(3)],
            ..Default::default()
        };
        state.observe_filter_context(old);
        assert!(!state.busy(), "ordinary redraws must stay free");
        state.observe_filter_context(new);
        assert!(state.match_pending);
        assert!(!state.accepts_hit(&hit(3), state.session_id, state.zone_version));
        assert_eq!(
            state.match_anchor.as_ref().map(|anchor| &anchor.hit),
            Some(&hit(3))
        );
        let anchor = state.match_anchor.clone();
        state.observe_filter_context(new);
        assert_eq!(
            state.match_anchor, anchor,
            "repeat metadata updates coalesce"
        );

        state.query_error = Some("rejected input".into());
        state.match_pending = false;
        state.observe_filter_context(old);
        assert!(!state.match_pending);
        assert_eq!(state.query_error.as_deref(), Some("rejected input"));
        assert!(!state.accepts_hit(&hit(3), state.session_id, state.zone_version));
    }
    assert_eq!(
        BlockSearchFilter::All.context(Some(4), 10_000),
        BlockSearchFilter::All.context(None, 1_000),
        "irrelevant metadata must not invalidate an All result set"
    );
    assert_eq!(
        BlockSearchFilter::Slow.context(Some(4), 10_000),
        BlockSearchFilterContext::Slow(10_000)
    );
    assert_eq!(
        BlockSearchFilter::Unread.context(Some(4), 10_000),
        BlockSearchFilterContext::Unread(Some(4))
    );
}

#[test]
fn navigation_during_matching_updates_refresh_selection_but_not_a_new_query_anchor() {
    for preserving_refresh in [false, true] {
        for navigation in [
            "next",
            "prev",
            "first",
            "last",
            "page_forward",
            "page_backward",
        ] {
            let (mut state, identity, mut build) = pending_match();
            state.hits = (1..=30).map(hit).collect();
            state.selected = 10;
            state.match_anchor = preserving_refresh.then(|| state.selected_anchor().unwrap());
            build.eligible = (1..=30).collect();
            build.result = Some(Ok(block_mode::BlockSearchResults {
                hits: state.hits.clone(),
                capped: false,
            }));
            let eligible = build.eligible.clone();
            match navigation {
                "next" => state.select_next(),
                "prev" => state.select_prev(),
                "first" => state.select_first(),
                "last" => state.select_last(),
                "page_forward" => state.select_page(true),
                "page_backward" => state.select_page(false),
                _ => unreachable!(),
            }
            let expected = if preserving_refresh {
                state.selected
            } else {
                0
            };
            assert!(state.install_match(identity, build, state.zone_version, &eligible));
            assert_eq!(
                state.selected, expected,
                "{navigation}, refresh={preserving_refresh}"
            );
        }
    }
}

#[test]
fn stale_epoch_cannot_mutate_a_reopened_picker_and_stale_sources_force_rebuild() {
    let (mut state, identity, build) = pending_match();
    let eligible = build.eligible.clone();
    state.epoch += 1;
    assert!(!state.install_match(identity, build, state.zone_version, &eligible));
    assert!(
        state.matching,
        "an old completion cannot release a newer worker"
    );
    assert!(state.cache.is_empty());

    let (mut state, identity, build) = pending_match();
    let version = BlockSearchZoneVersion {
        len: 2,
        oldest: Some(1),
        newest: Some(2),
    };
    assert!(!state.install_match(identity, build, version, &eligible));
    assert!(state.rebuild_pending);
    assert_eq!(state.hits, [hit(99)]);
}

#[test]
fn invalid_input_during_matching_keeps_its_diagnostic_and_old_hits_inert() {
    let (mut state, identity, build) = pending_match();
    let eligible = build.eligible.clone();
    assert!(!state.apply_query_input("bad\u{202e}query".into()));
    let diagnostic = state.query_error.clone();
    assert!(!state.install_match(identity, build, state.zone_version, &eligible));
    assert_eq!(state.query_error, diagnostic);
    assert_eq!(state.query, "needle");
    assert!(!state.accepts_hit(&hit(99), identity.session_id, state.zone_version));
    assert!(!state.match_pending);
}

#[test]
fn index_refresh_cannot_acknowledge_a_rejected_input_or_reactivate_old_hits() {
    let mut state = BlockSearchState {
        loading: true,
        match_pending: true,
        query: "needle".into(),
        hits: vec![hit(99)],
        ..Default::default()
    };
    assert!(!state.apply_query_input("bad\u{202e}query".into()));
    let diagnostic = state.query_error.clone();
    // Another completed command can force a replacement index generation
    // while the error is visible; neither boundary acknowledges that input.
    state.release_index_for_rebuild();
    assert_eq!(state.query_error, diagnostic);
    let (_, _, build) = pending_match();
    assert!(!state.install_cache(block_mode::BlockSearchCacheBuild {
        zones: build.cache,
        older_not_indexed: false,
        resident_bytes: 0,
    }));
    assert_eq!(state.query_error, diagnostic);
    assert_eq!(state.cache.len(), 1);
    assert_eq!(state.hits, [hit(99)]);
    assert!(!state.busy());
    assert!(!state.accepts_hit(&hit(99), state.session_id, state.zone_version));
}

#[test]
fn background_job_slot_stays_owned_until_the_exact_worker_returns() {
    let identity = BlockSearchBuildIdentity {
        session_id: 7,
        epoch: 12,
    };
    let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut slot = Some(BlockSearchJob {
        identity,
        matching: true,
        cancelled: Arc::clone(&cancellation),
    });
    slot.as_ref().unwrap().cancel();
    assert!(cancellation.load(std::sync::atomic::Ordering::Relaxed));
    assert!(
        slot.is_some(),
        "close/reopen cannot free a worker that is still running"
    );
    assert!(!finish_block_search_job(&mut slot, identity, false));
    assert!(!finish_block_search_job(
        &mut slot,
        BlockSearchBuildIdentity {
            epoch: 11,
            ..identity
        },
        true
    ));
    assert!(!finish_block_search_job(
        &mut slot,
        BlockSearchBuildIdentity {
            session_id: 8,
            ..identity
        },
        true
    ));
    assert!(finish_block_search_job(&mut slot, identity, true));
    assert!(slot.is_none());
    assert!(!finish_block_search_job(&mut slot, identity, true));
}

#[test]
fn duplicate_query_messages_are_free_but_can_clear_a_rejected_paste() {
    let mut state = BlockSearchState {
        query: "needle".into(),
        ..Default::default()
    };
    assert!(!state.apply_query_input("needle".into()));
    state.query_error = Some("rejected input".into());
    assert!(state.apply_query_input("needle".into()));
    assert!(state.apply_query_input("needles".into()));
}

#[test]
fn filters_count_retained_metadata_and_never_infer_success() {
    let mut terminal = TerminalState::new(80, 24);
    terminal.process_input(
        b"\x1b]133;A\x07$ \x1b]133;B\x07command\r\n\x1b]133;C\x07output\r\n\x1b]133;D;0\x07",
    );
    let original = terminal.command_zones.front().unwrap().clone();
    let zones: Vec<_> = [
        (1, Some("success"), Some(0), Some(0)),
        (2, Some("failed"), Some(2), Some(10_000)),
        (3, Some("unknown"), None, Some(10_001)),
        (4, None, Some(2), None),
    ]
    .into_iter()
    .map(|(id, command, exit_code, duration_ms)| {
        let mut zone = original.clone();
        zone.id = id;
        zone.command = command.map(str::to_string);
        zone.exit_code = exit_code;
        zone.duration_ms = duration_ms;
        zone
    })
    .collect();
    let counts = BlockSearchFilter::ORDER.map(|filter| {
        zones
            .iter()
            .filter(|zone| filter.matches(zone, [2, 4].contains(&zone.id), Some(2), 10_000))
            .count()
    });
    assert_eq!(counts, [4, 1, 1, 2, 2, 2, 1]);
    assert!(BlockSearchFilter::Unread.matches(&zones[0], false, None, 10_000));
    assert!(!BlockSearchFilter::Unread.matches(&zones[3], false, Some(4), 10_000));
}

#[test]
fn bulk_fold_and_expand_preserve_live_command_cells_cursor_and_capture() {
    let mut terminal = TerminalState::new(80, 24);
    for command in ["one", "two"] {
        terminal.process_input(format!("\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07first line\r\nsecond line\r\n\x1b]133;D;0\x07").as_bytes());
    }
    terminal
        .process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07running\r\n\x1b]133;C\x07live progress\r\n");
    assert!(terminal.is_command_running());
    let ids: Vec<_> = terminal.command_zones.iter().map(|zone| zone.id).collect();
    assert_eq!(ids.len(), 2);
    let capture: Vec<_> = ids
        .iter()
        .map(|id| terminal.zone_output_text(*id))
        .collect();
    let cells = terminal.get_projected_viewport(false).cells().to_vec();
    let cursor = terminal.get_cursor_pos();
    let mut policy = ProjectionPolicy::new();
    assert_eq!(
        set_completed_output_collapsed(&terminal, &mut policy, true),
        2
    );
    assert_eq!(
        set_completed_output_collapsed(&terminal, &mut policy, true),
        0
    );
    assert_eq!(policy.collapsed_zone_ids().collect::<Vec<_>>().len(), 2);
    assert!(ids.iter().all(|id| policy.is_collapsed(*id)));
    assert_eq!(
        set_completed_output_collapsed(&terminal, &mut policy, false),
        2
    );
    assert_eq!(terminal.get_projected_viewport(false).cells(), &cells);
    assert_eq!(terminal.get_cursor_pos(), cursor);
    assert_eq!(
        ids.iter()
            .map(|id| terminal.zone_output_text(*id))
            .collect::<Vec<_>>(),
        capture
    );
    assert!(terminal.is_command_running());
    terminal.process_input(b"\x1b[?1049h");
    assert_eq!(
        set_completed_output_collapsed(&terminal, &mut policy, true),
        0
    );
}
