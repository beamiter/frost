# Engineering handoff

Updated: 2026-08-15

This baseline exact-pins the hardened shared core and jagent revisions and now carries
native bounded OSC 8 interaction, hardened app-owned helper processes, and a tested
prebuilt-artifact install path on top of the Block Mode, Agent review, configuration,
terminal parsing, history, keybindings, and session-persistence work. Block and link
lifecycle identities and cell boundaries are checked, finalized rows own a real gutter,
stale UI targets fail closed, and automatic helper resolution no longer trusts `PATH`.

## Completed since the previous handoff

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
