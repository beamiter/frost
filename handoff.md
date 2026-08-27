# Engineering handoff

Updated: 2026-08-27 (Files remote-target safety)

This baseline exact-pins the hardened shared core and jagent revisions and now carries
native bounded OSC 8 interaction, hardened app-owned helper processes, and a tested
prebuilt-artifact install path on top of the Block Mode, Agent review, configuration,
terminal parsing, history, keybindings, and session-persistence work. Block and link
lifecycle identities and cell boundaries are checked, finalized rows own a real gutter,
stale UI targets fail closed, and automatic helper resolution no longer trusts `PATH`.

## Completed since the previous handoff

- **Files remote-target safety (2026-08-27)**: the Files header now exposes a
  keyboard-reachable terminal action. Local opens a normal new session at the
  visible tree root; Remote explicitly says **default dir** and reuses the
  selected profile's connection path. Remote-host config replacement no longer
  lets a numeric index silently redirect the tree or an old file clipboard:
  only one exact, complete old-profile identity may rebind across the active
  list. Missing, edited, out-of-range, or duplicate identities fail closed to
  Local, cancel/retire remote transfer state, and clear old selection, menus,
  dialogs, delete confirmation, clipboard, hover/filter state, and drop work.
  Menu-derived create/rename/delete intents additionally carry the tree
  generation and revalidate immediately before dispatch; off-thread drop plans
  and file-op reports carry equivalent stale-result guards. File-op reports
  validate both context epoch and location before clearing even transfer UI,
  so a late cancelled job cannot erase a newer transfer. A unique exact-profile
  index move invalidates old tree intents/drop work and immediately reloads the
  same root with the new index; the old directory result is generation-stale,
  so the panel cannot remain stuck in Loading. Already-dispatched file work
  carries the complete destination profile and can finish against that one
  unique new slot. Copy/Cut replacements have checked monotonic identities, so
  exhaustion fails closed and an old completion can never alias or retire a
  newer Copy/Cut. Backend-confirmed clipboard settlement runs before the
  stale-UI gate: partial/cancelled cuts retire only sources actually moved and
  deleted, while successful Rename/Delete retires matching dangling Copy/Cut
  paths and their descendants. Dispatch also binds the source filesystem, so
  equal path text on Local and a remote profile cannot cross-retire entries. An
  open Paste menu freezes that identity and visibly requires reopening instead
  of substituting a later clipboard. A failed
  remote home probe now returns to a loaded Local root with bounded inline
  feedback,
  so the user can select the profile again instead of being trapped refreshing
  an old path. Pure remap/copy tests cover reorder, full-identity change,
  duplicate ambiguity (including inactive retained duplicates), and
  Local/Remote entry wording; sidebar tests cover generation expiry and
  reindex-load replacement. Validation passed all 861 tests, `cargo check`, and
  warning-denied Clippy across every target.

- **Block Search 4.4 (2026-08-26)**: result-local bookmark controls now carry
  visible action labels (`☆ Bookmark` / `★ Remove`) and honest row-local
  tooltips; the footer alone documents that `Ctrl+Shift+B` acts on the
  highlighted result. Bookmarked zero-result states distinguish an empty
  bookmark set, missing indexed text in the selected scope, and a real query
  miss, including non-empty queries when the selected scope has no bookmarked
  text. The empty-set guidance now gives a reachable path — choose the **All**
  filter and search for a block before adding a bookmark — instead of
  suggesting a shortcut with no selected result. Pure copy/reason tests lock
  these pointer-versus-selection semantics;
  validation passed all 851 tests, all 25 Block Search tests, `cargo check`, and
  warning-denied Clippy across every target.

- **Block Search 4.3 (2026-08-26)**: picker rows now expose in-place `☆`/`★`
  bookmark actions, with exact physical `Ctrl+Shift+B` latched for the full B
  key lifetime so repeats and modifier-release ordering cannot double-toggle or
  leak text into the query. Loading and stale rows remain non-actionable and
  fail closed with picker-local feedback; toggles preserve the closest stable
  selection anchor, synchronize duplicate hits for the same zone, and
  immediately recompute the active Bookmarked view. Empty-query metadata
  browsing now emits only real, meaningful command/output text in the selected
  scope, never synthetic or blank rows. Validation passed the full 849-test
  suite plus 23 targeted Block Search checks.

- **Block Search 3.9 (2026-08-26)**: the picker now exposes a fully labelled
  **Refresh** button with an explanatory tooltip; clicking it and pressing bare
  `F5` share the same bounded rebuild path. The currently configured
  `block:search` chord wins if remapped onto F5; other modified F5 chords remain
  inert under the input-owning overlay. Iced key-repeat events are rejected, so
  one physical F5 press starts at most one refresh; button refresh remains
  repeatable and returns focus to the query. Invalid-query refreshes fail fast
  without hiding their diagnostic or starting a worker, and repeated requests
  while a worker is busy coalesce into at most one follow-up build. If the
  current intent becomes invalid or an unfiltered empty query before the first
  worker lands, its now-useless follow-up is cancelled; real finalized-zone
  version churn still takes priority and rebuilds the latest snapshot. A
  repeated edge from the physical toggle chord that opened the picker is now
  consumed without closing it; a fresh non-repeat chord still closes normally.

- **Block Search 3.3 (2026-08-26)**: Enter and Shift+Enter now close or
  advance only after the selected zone is revalidated and actually revealed.
  A result evicted between paint and activation keeps the picker open, refreshes
  the hit list, and reports the stale target instead of silently stepping.

- **Block Search 3.2 (2026-08-26)**: Home/End and ten-row PageUp/PageDown
  navigation now complement the existing wrapping arrows and position label.
  Every move keeps the selected virtual window visible, and stale out-of-range
  selection state is clamped before navigation rather than producing a bad row.

- **Block Search 3.1 (2026-08-26)**: the picker now exposes `All / Cmd / Out`
  surface scopes with a `Ctrl+O` cycle. Scope is enforced inside the bounded
  matcher before the 500-hit cap, including empty-query metadata browsing, so
  excluded command/output text cannot consume the requested surface's budget.

- **Block Search 3.0 (2026-08-26)**: the bounded per-open index now supports
  Unicode whole-word matching alongside `Aa` and regex, with `Ctrl+W` parity
  for keyboard users. Whole-word literal and regex scans validate boundaries
  without allocating per line; case-insensitive whole-word literals use the
  linear regex engine so rejected prefixes cannot turn a long log line into a
  quadratic rescan. Query errors and stale-result activation gates are unchanged.

- **Single-interpretation native JSON boundaries (2026-08-25)**: after their
  existing raw byte ceilings, the private `auth.json` reader and every Codex
  app-server JSONL record now run through
  `jterm_core::bounded_json::validate_no_duplicate_members` before typed or
  `Value` decoding. Duplicate object members are rejected recursively,
  including escaped-equivalent names and duplicates inside ignored/future
  extension objects. The private serde_json RawValue sentinel is also reserved,
  so feature-unified `Value` decoding cannot reparse unchecked embedded JSON.
  An app-server frame therefore cannot select one `id`,
  `method`, or nested result for request correlation while another decoder or
  audit surface sees a different value; credential parsing likewise has one
  structural interpretation. The shared preflight retains no decoded value
  tree and never reflects the untrusted member name in its error.

- Frost range navigation now protects the newest edge: a newer step on a
  multi-block selection first contracts it to the active newest block, and only
  the following step clears selection with explicit feedback.
  This keeps one accidental keypress from destroying a reviewed range while
  preserving the established single-block exit hatch. The shared core exact
  pin advances to `21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`, adding
  core-owned Agent claim durability and jagent's recursive duplicate-member
  rejection without changing the completed-block outcome/lifecycle contract.

- Block Search 2.0 adds `Aa` case-sensitive and bounded Rust-regex matching to
  the existing All/Failed/Slow/Bookmarked/Background picker. Invalid or
  oversized expressions are query errors rather than false zero-hit results:
  the last usable background-built index/hits remain intact but hidden and
  non-activatable until correction. Query state is rebuilt into a compact
  allocation retaining at most 4 KiB plus one complete UTF-8 overflow scalar,
  and regex compilation has a 2 MiB heap limit,
  including for whitespace-heavy pastes. Source accounting now includes its
  Vec allocation and String capacities inside the 8 MiB UI-thread retained
  ceiling; the 16 MiB worker-built retained cache counts its Vec plus original
  and lowercase capacities. Rebuild drops old cache/hit allocations before
  extracting source; the first rejected source (up to the roughly 1 MiB zone
  cap) and one rejected lowercase candidate remain short-lived allocations
  outside those retained ceilings. Only one worker generation runs at once;
  zone churn is coalesced and a stale completed build is dropped before the
  replacement snapshot is extracted. Literal case-fold expansions (`İ` → `i`
  + combining dot) and regex byte ranges both map back to original Unicode
  scalar spans, so
  previews and soft-wrap row reveal never consume normalized-string offsets.
  Activation additionally requires `!loading`, the current session/zone
  version, and exact membership in the current hit list; queued stale clicks
  remain inert.

- Held-open task terminals (Agent CLI fallback and validation runs whose child
  exited but whose tab stays for transcript review) now behave as read-only
  transcripts end to end. PTY-bound user bytes are refused before reaching the
  writer: `Session::write_pty_with_origin` gates on `transcript_read_only()`
  (exactly `hold_after_exit`), so keyboard, IME commit, every paste flavor, the
  control-key keybinding sender, mouse reports, and Agent payloads can no longer
  hit EIO on the dead master fd; the keyboard/IME/paste/keybinding dispatchers
  pre-check and surface one throttled toast ("Task terminal exited; its
  transcript is read-only", 1.6 s interval so key repeat cannot stack toasts)
  instead of a dead echo. The chrome carries the persistent affordance: tab
  strip, dock list, and window title gain an " (exited)" suffix through
  `Session::label`, and the pane header shows a dim `■ exited` chip beside the
  running-process indicator. Task terminals also leave the session snapshot
  entirely — the filter won over restore-with-suffix because task metadata is
  runtime-only and a restored plain shell parked in a task worktree is a trap.
  `session_persistence::prune_sessions` drops excluded sessions and rewrites
  every index-bearing reference against the post-filter numbering (pane-tree
  leaves, tab focus falling back to the first surviving leaf, `active_index`,
  `active_tab`); single-survivor splits collapse so the restore-side shape
  validation never sees one-child splits, tabs left without panes drop, and the
  legacy compat `tree` keeps deriving from the pruned tabs inside
  `SessionsSnapshot::new`. Exclusion covers both task-bound sessions
  (`task_for_terminal_session`, Agent and validation roles) and any
  `hold_after_exit` transcript whose binding a retry replaced. Headless tests
  cover the input-block/label predicates and pruning identity, leaf/focus/active
  remapping, split collapse, tab drop with focus repair, all-excluded, and
  fail-closed short keep lists.

- The jterm_core pin advances to `b8b1b89` (`b8b1b89148726204e2d46518ba9327d05a968f8e`,
  routing notifications through the bounded helper boundary). With the old-pin
  compatibility shim no longer needed, `src/review_text.rs` shrinks to the genuinely
  frost-specific extras — per-surface byte limits, the parameterized
  `validate_single_line`, the multiline `sanitize_prompt_payload` /
  `sanitize_history_replay` / `sanitize_untrusted_single_line`, and `visible_bounded` —
  while the duplicated visual-spoof primitives are deleted here and every former call
  site (`block_mode`, `history_picker`, `agent`, `agent_task::{native, event, task,
  diff, drivers::codex_app_server}`) now calls `jterm_core::review_input`
  (`is_visual_spoofing_character`, `contains_visual_spoofing`) directly. The three
  failed-block palette actions gain default chords in the free Ctrl+Alt letter family:
  `block:fix_with_agent` on Ctrl+Alt+X, `block:explain_with_agent` on Ctrl+Alt+E, and
  `block:retry_failed` on Ctrl+Alt+T — no collision with Ctrl+Alt+F/G/R, the pane-focus
  arrows, opacity `=`/`-`, or the Alt-copy fallback (their alt-less bases ctrl+x/e/t are
  unbound). They dispatch through the same block-context PTY-passthrough preflight, and
  the command palette, the help overlay, and the README shortcut table all list them.

- The jterm_core pin advances to `86661a7` and the app-owned boundaries sink
  into it. `src/app_helpers.rs` is deleted: font and notification helpers now
  call `jterm_core::helper::{fc_list, fc_match, notify_send}` directly, which
  carry the same fixed candidates, canonical-chain trust, clamped child PATH,
  per-stream byte caps, and single group-killing deadline on top of core's
  `SupervisedChild`. `link::is_openable_url` delegates to
  `jterm_core::link::is_openable_url`, the family-shared HTTP(S)-only opener
  policy. `persistence::prepare_command_history_path` is now a one-line
  delegate to `jterm_core::command_history::prepare_path` — the preflight the
  old pin lacked — and its three regression tests moved to core with the
  implementation. The session decoder's text budget and borrowed deferred raw
  fields now come from `jterm_core::bounded_json` (`TextBudget`,
  `DeferredRawField`); only frost's schema, repair counters, and seeds remain
  app-side.

- The experimental native Codex task runtime is ported from ember as `src/agent_task/`
  (context, diff, driver, event, launcher, native, runtime, task, validation, worktree,
  the app-server and fake drivers, plus `pinned_dir` descriptor capabilities), with the
  iced-side state in `src/agent_task_ui.rs`. The feature is gated by
  `experimental_task_sidebar = false` and adds a **Tasks** dock panel plus a failed-block
  **Create task** menu action. **Start Codex** enters a bounded, cancellable background
  Preparing phase (registered-worktree verification, descriptor pinning, trusted
  codex/node launcher resolution, prompt construction, private 0700 `CODEX_HOME`) driven
  by a 50 ms/500 ms iced tick rather than egui repaints; completion is gated by task
  generation plus the still-current `ai_enabled` + `ai_share_command_context` policy, and
  cancelled/stale/revoked results drop their directory capabilities, credential buffers,
  and temp home without spawning. The provider worker re-proves Git identity and the
  trusted launch chain immediately before spawn. The native protocol stays version-gated
  to the audited codex-cli 0.147.0 app-server identity with access-token-only login,
  pre-thread effective-config attestation (rejecting inherited MCP/hooks/plugins/apps/
  project trust/managed authority), hosted search and tool network disabled, approval
  policy `never`, and display-and-deny managed approvals (Deny only). Sequential
  follow-up turns reuse the same loaded thread with identical cwd/sandbox/env/approval
  authority, duplicate/overlapping turns are rejected, live sessions cap at 32 turns
  with completed-turn tombstones, a later turn invalidates earlier validation evidence,
  and **Finish Codex** (cgroup empty + leader reaped before the terminal event) gates
  validation; sessions remain single-use with no resume. Containment keeps the
  descriptor-pinned worktree plus transient user-systemd cgroup (cgroup v2,
  `cgroup.kill` guardian), `/tmp` excluded from writable roots, and a no-login,
  proxy-free tool environment with a vetted absolute PATH. Validation replays the exact
  single-line source command in a separate read-only-after-exit terminal: canonical
  source-subdir → worktree mapping, missing-dir/control/bidi/symlink-escape rejection,
  non-login no-rc shell argv plus `BASH_ENV`/`ENV`/`ZDOTDIR=/dev/null` (new
  `Pty::new_with_cwd_env` extra-env seam), Git registration + branch rechecked, and the
  cwd carried through the pinned descriptor's `/proc/self/fd` path. Results land as
  running/passed/failed/needs-review/cancelled via the PTY-exit hook (real child status
  from `Pty::exited_code`); pass-gated **Mark complete** stays explicit. **Review diff**
  runs the bounded `git status --short` + tracked `git diff <base>` worker. The opaque
  terminal fallback (direct, retry, or post-native-failure) spawns a new tab in the
  worktree through the trusted launcher argv and atomically rebinds the task's terminal
  session, preserving sticky Terminal/TerminalFallback provenance; task-bound PTY exits
  and closes reduce through `handle_terminal_session_exit`/`_closed` keyed by frost-local
  stable session strings. OSC 133 command zones now record `command_exact` (metadata vs
  prompt-row reconstruction) so validation keeps failing closed on inexact commands.
  Task metadata is runtime-only; **Hide task** archives metadata and leaves the worktree.

- Failed completed blocks now expose ember's Fix / Explain / Retry action chain through the
  block context menu and the command palette, adapted to the per-command-approval Shell Agent
  rather than ember's task dashboard. Fix/Explain start a fresh Agent task bound to the source
  pane by stable session id: the path never claims the persisted Agent snapshot, never replaces
  a running approved command, a pending model round, or an open transcript, and attaches the
  exact command, bounded captured output, and verified cwd as framed untrusted context. Retry is
  a guarded semantic replay into the source pane: exact, non-truncated (16 KiB capture),
  single-line commands only, at an idle empty bracketed-paste prompt on the main screen, and only
  while the recorded cwd matches an independently observed local shell-process cwd; SSH/tmux-style
  wrappers fail closed because their local process cwd is not the reported workspace. Background
  blocks and unknown-status records are never eligible; eligibility, cwd provenance, and the
  Agent replace-guards are pure functions/unit-tested state-machine paths.

- OSC 8 metadata now reaches the ordinary link interaction pipeline. URI and id fields
  are rejected before allocation above 2 KiB / 256 B, the terminal-local interner is
  capped at 4096 entries, and compact cell keys survive live-grid edits, in-memory
  scrollback compression, scrolling, resize/reflow, and safe alternate-screen swaps.
  Explicit spans override overlapping heuristic links and are revalidated through the
  one HTTP(S)-only `is_openable_url` policy at parse, projection, hit-test, app merge,
  and opener boundaries. Ctrl+single-left activation carries the projection revision,
  so output or viewport movement cannot retarget a stale click.
- App-owned `fc-list`, `fc-match`, and `notify-send` calls now share a bounded helper
  process boundary. Programs resolve only from fixed absolute system candidates whose
  canonical file and directory chain is trusted. Each helper leads a process group,
  stdout and stderr drain concurrently under independent byte caps, and one deadline
  terminates and reaps the entire group. Exit observation uses `waitid(WNOWAIT)` so the
  group leader retains its PID until cleanup and a recycled PGID can never be signalled.
- The source installer accepts `--binary PATH`, allowing release archives, CI artifacts,
  and distro staging to reuse the same path contract without Cargo. A real `DESTDIR`
  install/uninstall test checks all six artifacts, modes, launcher paths, escaping, and
  failure diagnostics. Desktop and AppStream validation now run in CI; custom desktop
  executable paths with undefined/unportable `%`, forbidden `=`, or control characters
  fail explicitly. The application manifest is `publish = false` because its exact-pinned
  git core cannot form a usable crates.io package.
- The fork-based PTY startup-timeout fixture now closes unrelated inherited descriptors
  before deliberately pausing. This mirrors the production child's immediate exec and
  prevents a parallel test's CLOEXEC process lock from remaining held by the fixture.
- The durable snapshot concurrency test now treats only the production lock's bounded
  `TimedOut` result as permissible under saturated filesystem contention. It still
  requires a complete single-generation publish with no staging residue, then performs
  another production write after every contender returns to prove the lock was released.

- Block interaction has a layout-reserved 8px gutter rather than paint over column zero.
  Finalized rows are a local static-card surface: a single prompt/header click selects,
  Shift selects a range from any card row, Ctrl+Shift toggles, and right click anywhere
  opens a pointer-anchored stable pane/zone action panel. Plain/double/triple output
  clicks retain native text selection, and Ctrl-click retains link activation. The menu
  covers selected copy/Markdown, exact-block Agent context, recall/reinput, bookmark,
  top/bottom reveal, search, and exact-block Markdown/JSON export. Mouse ownership is frozen at press time;
  right/middle releases follow their press across pane bounds, completed-row wheel input
  stays local even with primary application mouse reporting, and live/alternate-screen
  interactions still route to the PTY. Running/full-screen and disabled Block Mode
  keybindings fall through to the PTY.
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
  Core syncs retirement of the public name before exposing a live session, so
  a crash cannot replay an already consumed approval.
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
  unknown. Completion provenance, lifecycle health, and their assessment
  function are now direct public re-exports of the shared contract rather than
  local semantic mirrors; Frost's serializer adapters delegate to the shared
  stable snake-case vocabulary. `jterm_core` is pinned to
  `21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d` (transitively jagent
  `a462ec81f3a4c6ad85a455780ced232172f127ea`). Claim-acquisition errors are
  logged with the public path and leave that path untouched; there is no
  best-effort fallback read or delete.

## Remaining release boundary

The repository still has no formal release tag, so AppStream deliberately has no
fabricated `<releases>` entry. Add the first release node with the real version and date
when the first tag is cut; `appstreamcli validate --pedantic --no-net` currently reports
that omission as its one expected pedantic note.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
```
