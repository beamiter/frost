# Engineering handoff

Updated: 2026-08-10

This baseline exact-pins the hardened shared core and jagent revisions and now carries
the Block Mode hardening/discoverability pass on top of the prior Agent review,
configuration, terminal parsing, history, keybindings, and session-persistence work.
Block lifecycle identities and cell boundaries are checked, finalized rows own a real
gutter, stale UI targets fail closed, and the jsh/link policies from the prior baseline
remain intact.

## Completed since the previous handoff

- Block interaction now has a layout-reserved 8px gutter rather than a hit target that
  overlaps column zero. Only rows belonging to finalized selectable blocks own it;
  running/live-prompt rows remain ordinary terminal input. Left click keeps single,
  toggle, and range selection, while right click opens a stable pane/zone action panel
  for copy, recall/reinput, bookmark, top/bottom reveal, search, and session export.
  Mouse gesture ownership is frozen at press time, so Shift or application mouse-mode
  changes cannot produce an orphan press/release; running/full-screen and disabled
  Block Mode keybindings fall through to the PTY.
- Pane-local block bookmarks have bounded-history reconciliation, wrapped previous/next
  navigation, gutter and scrollbar markers, a default toggle chord (`Ctrl+Shift+B`),
  command-palette navigation, and search integration. Clear Blocks removes bookmarks;
  disabling/re-enabling Block Mode clears hidden selection/search/menu state without
  resurrecting it.
- Cross-block search adds All/Failed/Slow/Bookmarked/Background filters, blank-query
  browsing, responsive fixed-height results, palette autoscroll, and session-bound hit
  acceptance. Hits retain Unicode-safe match spans, long previews stay centered on the
  match, and retained output resolves through soft wraps to the actual physical row.
  Trimmed/reflowed mismatches degrade through logical-line start to the block header
  rather than targeting another pane's same numeric zone id.
- Search indexing now takes a lazy newest-first source snapshot capped at 8 MiB and
  performs lowercasing on a blocking worker under a 16 MiB resident-cache budget.
  Window-monotonic epochs reject late close/reopen results, finalized-zone versions
  refresh the picker for ordinary completion, missing-D recovery, Background output,
  and bounded-history eviction, and partial indexes are disclosed separately from the
  500-hit scan cap.
- Clear Blocks is no longer an immediate destructive shortcut. Keybindings, palette,
  and the block menu share one counted confirmation bound to the stable pane and newest
  zone id; history churn requires a fresh confirmation, and the modal states that block
  records, bookmarks, and captured output are permanently removed while a live prompt
  or running command is retained.
- OSC 133 parsing now requires an exact A/B/C/D marker field and correlates ordinary C/D
  ids before consuming state. Output capture preserves no-trailing-newline and same-row
  CR/BS/CUP writes, ignores alternate-screen paint, and clamps/rebases its row/column
  extent across trimming and resize. Completion truncation uses the extractor's exact
  flag, zone-id exhaustion seals history instead of reusing an identity, and Markdown
  cwd metadata is emitted as inert code with visual-spoof controls omitted.
- Command capture is bounded to a UTF-8-safe 16 KiB prefix across visible rows and OSC
  metadata. Oversized or unavailable commands remain explicit non-Background zones,
  are marked truncated, and cannot enter Recall/Reinput, Agent, completion-history, or
  other executable-looking consumers.
- Session export now writes a versioned `frost.block-session` v1 JSON envelope carrying
  pane identity, capture time/offset, block ordering, and retention/truncation counts;
  Markdown carries the same pane/time context. The previous unversioned bare JSON array
  is no longer the compatibility boundary.
- Tests now bridge real `TerminalState` OSC 133 lifecycles into session export and cover
  completed, Background, missing-D recovery, shell command truncation, exact markers,
  mismatched ids, same-row output, resize/alternate-screen boundaries, logical output
  row mapping, bookmark navigation, filtered search caps, and row-specific gutter hits.

- Agent restore now consumes `jterm_core::agent::SessionClaim`, backed by one
  atomic no-replace rename rather than the former local hard-link/unlink pair.
  Exactly one concurrent opener restores a valid snapshot; malformed, future,
  oversized, and semantically invalid evidence remains byte-identical at its
  private claim path. An empty or rejected local session still leaves the
  public path alone, so one process exiting cannot delete a newer checkpoint
  published by another.
- Session restore now uses a schema-aware bounded decoder rather than deriving
  an owned Serde tree before sanitizing it. The v1/v2 envelope and every nested
  child are borrowed as `RawValue`; parsers are dropped before recursion, and
  sessions, tabs, pane children, ratios, tree nodes/depth, and cumulative text
  are capped before ownership. Invalid tabs decode transactionally without
  losing valid neighbours, active-tab identity is remapped by original index,
  and invalid optional tabs/tree/split layouts warn and follow the existing
  tabs → tree → split fallback. Required known fields remain strict while
  unknown fields, including long future keys, stay forward-compatible.
- Execution generations use `checked_add`; exhaustion seals the session rather
  than reusing an identity a late completion could bind to.
- Model requests carry `{session epoch, request generation}` through both
  blocking and streaming callbacks. New Task, restore, reopen, and cancellation
  replace that identity before any late reply can mutate the current transcript.
- `jsh_version_banner` now starts the probe in its own process group, drains a
  byte-bounded banner on a concurrent reader, enforces one deadline, and
  signals and reaps the whole group. A shell that daemonises a descendant
  holding the probe's stdout can no longer make the probe wait forever, and the
  banner itself is bounded.
- One link-opening policy: `is_openable_url` accepts only absolute HTTP(S) URLs
  with an authority and no userinfo, controls, whitespace, backslash, or
  visually ambiguous characters. Links open through a non-user-writable
  absolute opener — the Windows `cmd /C start` path is gone — file operands
  follow `--`, and the opener process is reaped.
- `scripts/install.sh` and `scripts/uninstall.sh` now derive the default binary
  from the same `PREFIX/bin` contract (`~/.local/bin` by default). Explicit
  `--prefix`, `--bin-dir`, and `DESTDIR` keep their existing meanings, including
  runtime launcher paths inside staged packages. A temporary-HOME dry-run suite
  covers default reinstall/uninstall, explicit overrides, and staged paths, and
  CI checks the scripts with Bash and ShellCheck.
- Completed block outcomes now delegate to
  `jterm_core::block_contract::classify_completed` using the command text already
  resolved and stored on each completed zone. Frost keeps its renderer enum and
  serialized zone shape, while failed navigation, stepping, paint, and scrollbar
  markers share the same four-way result. A commandless zone with a raw non-zero
  status remains background, and a command without a reported status remains
  unknown. `jterm_core` is pinned to
  `48d25f155b960417609ffc85a98b7c9ba44c5772` (transitively jagent
  `a09fd1563b862f96bed7047834720aeb31c163e2`). Claim-acquisition errors are
  logged with the public path and leave that path untouched; there is no
  best-effort fallback read or delete.

## Remaining boundaries

### Own the remaining app-owned helpers

`jsh_version_banner` is now group-owned and bounded, but the app-owned
`fc-list`, `fc-match`, and notification helpers still resolve through a mutable
`PATH` and read unbounded output without a deadline. Give them the same
treatment: a trusted absolute program, a process group, concurrent bounded
drains of stdout and stderr, one deadline, and a reaped group.

### Connect OSC 8 to clicking, or keep it inert deliberately

OSC 8 targets are parsed but not clickable. When that changes, route them
through `link::is_openable_url` — it is now the single policy — and cap the URI
and id fields before interning them.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
```
