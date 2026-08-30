# Frost upgrade rounds

Rounds 1–10 record the preceding pass; this pass's additional twenty rounds
are numbered 11–30.

1. **Prefix validation** — unsafe empty, relative, control-bearing, and lexical
   parent-traversing paths are rejected without excluding Unicode or spaces.
2. **Binary-directory validation** — install/uninstall enforce the same rules
   and reject an explicit empty override.
3. **DESTDIR confinement** — staging concatenation cannot escape through a
   lexical `..` component.
4. **Explicit root staging** — `DESTDIR=/` keeps staged cache and diagnostic
   behavior after the stored prefix is normalized.
5. **Dependency preflight** — install, temp, rename, cleanup, and desktop tools
   are checked before the build or first mutation.
6. **Atomic binary replacement** — a same-directory mode-`0755` temporary is
   renamed over the target without following a destination symlink.
7. **Pre-commit cleanup** — EXIT cleanup tracks the sole live temporary and
   preserves the old executable only until rename commits the binary; later
   resource failures do not imply rollback.
8. **Atomic desktop replacement** — an unpredictable same-directory temporary
   removes the predictable `.new` staging race.
9. **Remote-host semantic gate** — one application gate combines spoofing and
   byte-budget checks with shared argv/session/deploy/path validation, and is
   re-run by the picker, connection launcher, and remote filesystem; app text
   checks precede any shared diagnostic that could quote a draft value.
10. **Non-destructive resource bound** — incomplete/invalid drafts and entries
    after the first 128 keep round-tripping for repair; runtime surfaces mark
    them unavailable and Settings refuses Add at the active limit.
11. **Install-source preflight** — every desktop/metadata/icon source is checked
    as a readable non-link regular file before build or mutation.
12. **Non-empty prebuilt contract** — zero-byte descriptor input is rejected
    while the prior executable remains intact.
13. **Scoped staging ancestry** — normalized non-root packaging roots are
    checked from `/` through every existing component, rejecting disguised
    symlink roots before install/uninstall while retaining host-prefix
    compatibility; this is not a concurrent-mutation guarantee.
14. **Atomic metadata/SVG install** — explicit mode is applied to sibling temps
    before atomic rename.
15. **Atomic raster icons** — both PNG resolutions use the same commit boundary.
16. **Desktop structure validation** — exact TryExec and canonical Exec counts,
    plus rejection of alternate commands, precede publication.
17. **Unset-PATH resilience** — nounset no longer breaks successful install
    diagnostics.
18. **Hostile-path regression suite** — contract tests exercise empty artifacts,
    staging ancestor links, and public destination links.
19. **Index-neutral private errors** — internal remote helpers no longer label
    every invalid reference as profile #1.
20. **Bounded picker rendering** — 256 rows cap UI work while keeping entry 129
    visible and all omitted drafts stored.
21. **Runnable-only keyboard navigation** — initial selection and arrows skip
    invalid and inactive profiles; Enter still revalidates.
22. **Bounded settings editor** — rendering stops at 256 with an explicit
    retained-off-view count and no vector truncation.
23. **Active selector boundary** — remote file-tree and tab-menu choices stop at
    the first 128 executable profiles.
24. **Safe picker metadata** — deploy/name text passes bounded inline display
    before entering iced widgets.
25. **Actionable save summary** — explicit Save distinguishes invalid active
    drafts from over-limit retained drafts.
26. **Shared problem accounting** — one helper drives those counts and their
    regression assertions.
27. **Actual atomic disk round trip** — save/reload preserves an invalid draft
    and entry 129 exactly.
28. **Neutral fallback regression** — empty runtime identity is displayed as
    “remote host”, never a fabricated index.
29. **Pre-spawn consumer evidence** — remote-fs tests prove unknown, invalid,
    and high-index profiles fail before process creation, while gate tests
    prove oversized/RLO deploy drafts are rejected without raw-value echo.
30. **Documented bounded-draft contract** — README records render limits,
    navigation behavior, save diagnostics, and atomic public assets.

Block Mode convergence continues with rounds 31–33:

31. **Frost range-safe newest edge** — Frost's newer step at the end of a
    multi-selection contracts to the active newest block before a later step
    exits selection.
32. **Explicit selection exit** — the eventual key-owned clear produces the
    same feedback as Ember instead of making the highlight disappear silently.
33. **Current shared security pin** — the exact `jterm_core` revision advances
    to `0f47569`, adopting AI origin/credential/no-proxy validation without changing the
    four-way completed-block or lifecycle-health contracts.

AI chat-library convergence adds rounds 34–46:

34. **Guarded quit-path write** — `persist()` ran unconditionally on quit, so a
    session that never opened the AI panel replaced the saved chat library with
    the store's empty default; `can_persist()` now requires that this run
    actually read the file it is about to replace.
35. **Failed restore blocks writing** — an unreadable or undecodable library
    (truncated, over the read bound, a future sibling's schema version) is left
    byte-identical for the rest of the run instead of being overwritten by a
    fresh empty one; the panel stays usable and says why.
36. **Single-instance write ownership** — only the window holding the instance
    lock republishes the shared file; a second window restores it, uses it, and
    states in its notice that chats started there are not saved.
37. **Shared chat state machine** — frost's private multi-chat store collapses
    to a 35-line shim over `jterm_core::ai::chat_store`, the union of the four
    terminals' drifted copies (1,888 lines, 47 tests).
38. **Explicit busy policy** — `BusyChatPolicy::Refuse` is pinned at every
    construction site, because frost's panel has no cancel-then-mutate step;
    archive/delete on a chat with a request in flight refuse with a stated
    reason rather than inheriting a silent default.
39. **Compaction before serialisation** — `snapshot_for_persistence` compacts
    live history first, so a grown library can no longer reach a size the shared
    schema refuses outright and leave nothing saveable at all.
40. **Truncation markers sync back** — both compaction passes run on a clone,
    and what they dropped is carried into the live library so its rows admit it.
41. **Detaching retry materialisation** — retry payloads merge into the
    throwaway persistence clone, so saving cannot disturb the live composer.
42. **Pane-scoped suggestion card** — the AI command card renders, owns Escape,
    and inserts only for the pane that asked for it; an off-pane insert is
    refused instead of clearing and retyping an off-screen prompt.
43. **Window-wide suggestion generations** — the request id is app-level and
    strictly increasing rather than restarting at 1 per card, so a superseded
    worker's command can no longer publish onto its successor; exhaustion
    refuses the request instead of wrapping onto a live id.
44. **Card teardown follows the pane** — closing a pane drops its card, whose
    `Drop` cancels the drafting worker.
45. **Panel keyboard ownership** — while the chats panel is open, keys its
    focused inputs did not capture are swallowed rather than reaching the shell
    behind it; its own chord and Escape close it.
46. **Canonical id, chord, and history budgets** — `ai_chat:toggle` (singular,
    matching `agent:toggle`) on `ctrl+shift+alt+a`, with the palette hint
    asserted equal to the core's rendering of the default binding; the shared
    command-history index's command bound is read from
    `review_input::MAX_REVIEW_INPUT_BYTES` instead of re-declared, and its cwd
    bound rises from 4 KiB to the core writer's 16 KiB.

Verification: `bash scripts/test-install-paths.sh`, Frost config tests, and the
full formatting/check/Clippy/test gates.

Verification for rounds 34–46: `cargo fmt --all -- --check`, `cargo clippy
--locked --all-targets --all-features -- -D warnings`, and `cargo test`
(1,017 passing, zero failures). The temporary local `[patch]` those rounds were
developed under is gone: the gate was rerun with `--locked` against the
published `jterm_core` `1a04f1e` and `jagent` `f9383ec`. The working tree still
carries the uncommitted AI panel itself.

Shared review-first command correction adds rounds 47–58. Rounds 34–46 used this
numbered list for the chat-store migration, so it continues here rather than
restarting:

47. **Shared correction engine** — `src/command_correction.rs` collapses from
    1,552 lines to a 424-line shim over `jterm_core::command_correction`
    (229 insertions against 1,357 deletions), the union of the four terminals'
    drifted copies (3,937 lines with tests). The pinned core revision advances
    to `badcce222fb5471a6afbfc5d5e898e2bc3faf632`. Three things stay in frost:
    the policy, the per-pane request registry, and the iced card.
48. **Consent gate on the correction payload** — the failed command, the cwd and
    up to 8 KiB of captured output are the largest payload any frost AI surface
    sends, and this was the one surface that never consulted
    `ai_share_command_context`. It now reaches the engine as `ContextSharing`,
    built per request because the value is live config. With the switch off —
    the default — the AI fallback goes silent and only locally verified
    corrections (target output, APT index, executable PATH) are offered.
49. **Pipe-to-interpreter refusal** — the gate's `syntax_markers` superset test
    only asks whether a marker is *present*, so against an original that already
    contains a pipe, appending `| sh` added no new marker and passed. frost had
    no check at all; the shared rule splits the pipeline quote-aware and
    compares the set of interpreters its stages run, pinned against
    `jagent::safety::is_interpreter`.
50. **Stated evidence and helper policy** — `LocalEvidence::SameNamespace`
    (frost owns its PTYs: no sandbox, no bridge, so this process's `PATH` is the
    namespace the failed command resolved against) and
    `HelperStrategy::FixedCandidates` (frost's existing closed candidate list,
    the copy that got helper trust right, preserved and now stated) are
    positional arguments with no `Default`, following round 38's
    `BusyChatPolicy` precedent.
51. **Sanitised card strings** — the card renders only `display_title`,
    `display_badge`, `display_description` and `feedback`. The provider's
    `message` used to be interpolated raw into a label directly above an
    editable, pre-filled, auto-focused command field, so a reply carrying U+202E
    could reverse the rendered order of the text beside it.
52. **Destructive-risk label** — the `⚠ destructive: {reason}` line the Agent
    approval card already carried, recomputed against the live draft.
    `is_dangerous` never gated whether a candidate is *offered*, so `rm -rf
    ~/work` reached this card in exactly the chrome `git status` got.
53. **16 KiB classification bound** — a failed command line longer than this
    surface's own declared budget is no longer classified, ranked, probed or
    prompted about.
54. **One validated draft behind label and action** — `run_allowed` and `accept`
    now answer about the same validated string. They used to disagree, one
    comparing the raw field text and the other the trimmed one, so a verified
    proposal differing only by surrounding whitespace was downgraded to
    "Insert for review".
55. **Bounded inline feedback** — one line, 200 characters, sanitised on the way
    in. This is the card's only remaining channel for text the engine did not
    author, one line above the command field.
56. **Absolute-only PATH walk** — the candidate-name fallback ignores relative
    and empty `PATH` entries, so opening a project that sits on a relative
    `PATH` element cannot contribute its filenames as correction candidates.
57. **One trigger decision** — `should_start(enabled, CompletionFacts { .. })`
    owns the toggles, the missing exit status, the output sample and the narrow
    classifier; frost supplies only `agent_issued` and `trusted_completion` as
    named fields. frost's own `enabled` gate is still answered first, before the
    facts are built, because `CompletionFacts` takes the block output by value.
58. **Documented user-visible cost** — README's AI configuration block states
    that the correction fallback is now gated on `ai_share_command_context`, and
    a new section lists what stops being offered and what keeps working.

Verification for rounds 47–58: `cargo fmt --all -- --check`, `cargo clippy
--locked --all-targets --all-features -- -D warnings`, and `cargo test --locked
--all-targets --all-features --no-fail-fast` (1,006 passing, zero failures),
all against the published `jterm_core` `badcce2` with no local `[patch]`.
Fifteen of the twenty-three tests in `src/command_correction.rs` went with the
engine; the eight that remain are the registry's four, the card's accept-path
wiring, and three new ones pinning frost's evidence/helper policy, the consent
switch in both positions, and the trigger's two frost-supplied refusals. Both
policy tests were mutation-checked here: `FixedCandidates` → `TrustedPathScan`
and an inverted consent mapping each turn the suite red. The probe-thread-name
assertion does not — it compares the policy's `Debug` output against the same
constant it renders, so it proves the constant is plumbed through but cannot
detect a rename; `CorrectionPolicy::probe_thread_name` has no accessor upstream.

The shared TOML/YAML workflow library adds rounds 59–69. Rounds 47–58 used this
numbered list for the command-correction migration, so it continues here rather
than restarting:

59. **Shared workflow subsystem** — `src/workflows.rs` collapses from 827 lines
    to 210 (153 insertions against 770 deletions) and `src/workflow_picker.rs`
    gives up its own fuzzy list and its own value bookkeeping (136/116), over
    `jterm_core::workflows` (five files, 3,186 lines, 73 tests). The pinned core
    revision advances to `790d06ab19b9f3dec7c188728fc468f008df5414`. Four things
    stay in frost: the search-path policy, the load order, the iced overlay, and
    the keyboard routing in `main.rs`.
60. **The missing-value guard, reachable at last** — `render()` refuses a
    declared argument with no default and no value, and frost implemented and
    unit-tested that. The form seeded every declared argument with `""` before
    the user saw it, so it never fired: `kill -9 {pid}` with an untouched Pid
    field rendered `kill -9 ` and was typed at the prompt. The contract is now
    stated once — an empty value is meaningful only if the file says so — and
    `render` claims the undefaulted, blank-or-absent arguments into its missing
    set *before* building the binding list, so a caller that pre-seeds cannot
    seed past it.
61. **Unset and supplied kept apart in the type system** — `WorkflowArgsState`
    wraps `ArgsForm` instead of a `Vec<String>` that cannot represent the
    difference. Emptying a *defaulted* field stays a deliberate empty value
    (`deploy  --env=staging` still renders); emptying an *undefaulted* one is a
    missing value. Whitespace-only counts as unfilled.
62. **The refusal is visible before it happens** — outstanding rows carry
    `(required)` in their label, so Insert's `missing values:` error is never
    the first the user hears of it.
63. **Nested brace pairing** — an unterminated `{{` no longer claims a later
    placeholder's `}}`. `awk '{{print $1}' {{log}} | sort -u` used to render
    `awk '{print $1}' access.log | sort -u`, a different and executable awk
    program, while the same leading bytes with nothing after them round-tripped
    unchanged.
64. **Declared names held to the placeholder spelling** — placeholder names are
    trimmed, so a padded declared name (`name = "pid "`) could load clean,
    validate clean and match nothing while the user's typed value was dropped
    on the way to the prompt. Both sides of that lookup are now trimmed, and a
    padded declared name is rejected at load.
65. **Both halves of a skip log sanitised** — frost logged
    `workflows: skipping {path}: {err}` with the path raw and the parser error
    raw, and `toml::from_str` quotes the offending source line back verbatim, so
    a file whose unterminated string carried an OSC sequence wrote it onto a
    warn line for whatever tty was tailing the log. Both cross
    `review_input::safe_inline_display`, bounded.
66. **One query boundary for the picker** — the three `selected = 0` resets and
    the ad-hoc printable-character filter in `main.rs` are replaced by
    `set_query` / `push_query_text` / `backspace`, so programmatic and
    accessibility input cross the same one-line and `MAX_PICKER_QUERY_BYTES`
    bounds as typing. frost's policy is stated as `PickerPolicy::new(15, false)`
    — fuzzy, fifteen results, command template not searchable.
67. **Every divergence-prone choice injected, none with a `Default`** — the XDG
    backend (`XdgEnvDirs` here, glib in anvil/forge), the app identity
    (`SearchPathSpec::for_app` derives both the `frost/workflows` segment and
    `FROST_WORKFLOW_DIR` from one name), the load order (`LoadOrder::Precedence`
    here, `ByName` in ember/forge — and `LoadOrder` has no `Default`, so a
    silent shim does not compile), and the dev-tree root (`env!` expands against
    the compiling crate, so evaluating it in the core would point all four apps
    at a directory that does not exist while their bundled-library tests kept
    passing).
68. **An empty search-path entry contributes no tier** — a trailing or doubled
    `:` in `$FROST_WORKFLOW_DIR` is dropped, and a non-absolute `XDG_DATA_DIRS`
    entry no longer contributes a tier resolved against the process CWD. The
    `dirs` backend never produced a relative user tier here, so this is
    hardening for frost and a fix for a sibling.
69. **The bundled example the guard would have missed, and the documented
    contract** — `scripts/workflows/docker-tail-logs.yaml` declared
    `default: ""` for its required `container` argument, which under the new
    contract is an explicit empty value: Insert would have produced
    `docker logs -f --tail 100 `. The empty default is removed. README's
    capability bullet, its shortcut table and a new "Workflow 模板" section state
    the required-argument rule, how to opt a parameter back into being
    optional, and the two template tightenings.

Verification for rounds 59–69: `cargo fmt --all -- --check` (no diff),
`cargo clippy --locked --all-targets --all-features -- -D warnings` (silent),
and `cargo test --locked --all-targets --all-features --no-fail-fast` (992
passing, zero failures), all against the published `jterm_core` `790d06a` with
no local `[patch]`. The drop from 1,006 is this migration exactly: workflows had
twenty-three tests and has nine — three in `src/workflows.rs` pinning the search
path, the precedence load order and the bundled-library contract, six in
`src/workflow_picker.rs`. The rewritten form test is the load-bearing one: it
previously asserted that `deploy api --env=` renders fine and called that
emptiness intentional, which was the fossil record of round 60's defect rather
than behaviour to preserve. The install-path and packaging checks were not
rerun; this round touched no script, desktop file or metadata.

Round 70 closes the remaining argument-form affordance gap:

70. **Reversible workflow arguments** — every parameter row exposes **Reset**,
    which calls the shared `ArgsForm::clear` contract: a defaulted argument
    returns to its declared value, while an undefaulted argument returns to the
    genuinely-unset state. Editing or resetting also clears stale render
    feedback, and the form regression proves reset is not equivalent to typing
    an empty string.

71. **Current-toolchain clean placement boundary** — the completed-graphics
    filter now states its real predicate directly: with no live lifecycle every
    placement is stashed, otherwise only rows before the live start are. This
    removes the Rust 1.96 `nonminimal_bool` failure without changing the
    finished/live graphics contract covered by the clear/undo regression.

72. **Fail-closed dependency policy** — CI now runs a pinned `cargo-deny`
    against the committed lockfile. Wildcard requirements, unapproved
    licenses, unknown registries and unknown git sources fail the build; the
    two family git dependencies are admitted only at their exact reviewed
    revisions. Duplicate versions stay visible as warnings while the iced/wgpu
    graph still requires them. The core git dependency now also states its
    crate version, so a revision pin no longer counts as a wildcard API
    requirement. Two unavoidable *unmaintained* (not vulnerability or
    unsoundness) notices are exact-ID exceptions; every other advisory remains
    fail-closed.

73. **One missing-argument truth** — the iced form no longer restates
    `default.is_none() && value.trim().is_empty()`. It snapshots the shared
    `ArgsForm::missing()` result once per render and uses those names for every
    `(required)` label, so a future renderer-rule change cannot leave the UI
    claiming the opposite. The existing default/empty/whitespace/reset matrix
    now tests the delegated result.

74. **Strict documentation gate** — CI now builds all frost documentation with
    warnings denied, matching forge's release boundary. The local verification
    list and handoff say the same thing, and the former redundant intra-doc-link
    exception is retired instead of remaining stale institutional knowledge.
