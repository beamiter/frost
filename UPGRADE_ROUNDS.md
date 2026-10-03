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
   resource failures do not imply rollback. Superseded by round 83's complete
   staging plan and executable-last publication.
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

75. **Smaller direct dependency contract** — `unicode-width` and `lru` leave
    frost's manifest because no frost source or feature uses either crate
    directly. Both remain transitively locked where `jterm_core`/wgpu need them,
    but frost no longer claims their APIs as app-owned build inputs or requires
    future maintainers to review two misleading direct edges.

76. **Fixed and exhaustive CI baseline** — every validation job now names
    Ubuntu 24.04 instead of following the moving `ubuntu-latest` alias. The
    test job and documented local command also use `--no-fail-fast`, so one
    failing target cannot hide independent failures in the rest of the matrix.

77. **One fail-closed security entry point** — local `--all` and CI's
    `--policy`, `--audit`, and `--shell` modes now share a single script. It
    first proves the lockfile is usable, keeps cargo-deny and RustSec on that
    exact graph, exposes duplicate crates, and discovers every shell script so
    a new maintenance entry point cannot silently escape Bash and ShellCheck.
    The unified gate exposed cargo-audit's success exit for warnings and the
    newly reported unsound `lru 0.16.4` below iced/cryoglyph. A project audit
    policy now denies every new warning outside the two exact unmaintained
    exceptions, while a Rust-source-identical cryoglyph patch selects fixed
    `lru 0.18.2` until upstream publishes that dependency repair.

78. **Truthful release handoff and discoverable AI entry** — the remaining
    release boundary no longer calls the committed AI panel an uncommitted
    worktree or reports obsolete core and jagent revisions. It records the
    sole deliberate crates.io patch and its removal condition, while README's
    feature list and shortcut table now expose the shipped AI Chats chord and
    distinguish the review-only command-generation palette action.

79. **Installed workflow library** — source and prebuilt installs now copy all
    six accepted workflow fixtures into the selected data tree: the default
    follows `XDG_DATA_HOME`, while an explicit prefix owns its `share` tree.
    `--no-desktop` keeps these runtime resources, uninstall removes only the
    owned examples and leaves a non-empty user directory alone, and the real
    DESTDIR round trip compares every installed byte and proves
    install/uninstall symmetry.

80. **Whole-plan staged uninstall preflight** — every binary, current/legacy
    desktop asset, owned workflow, and cleanup directory is validated before
    the first removal under a non-root `DESTDIR`; per-target checks remain at
    use time. A regression places the unsafe symlink only under the later
    `share` branch and proves the earlier safe binary survives unchanged, so an
    escape refusal can no longer leave a partially uninstalled package tree.

81. **Truthful and inert configuration handoff** — the uninstaller now mirrors
    the application's `dirs::config_dir` rule: only an absolute
    `XDG_CONFIG_HOME` overrides `HOME/.config`. Its final preservation path is
    shell-quoted, so spaces remain unambiguous and an environment-provided
    newline or terminal control byte cannot forge another uninstall message.

82. **Whole-plan staged install destinations** — before the executable is
    replaced, the installer now walks the complete binary, workflow,
    applications, metainfo, and per-size icon directory chains under a
    non-root `DESTDIR`. A regression puts the symlink only in the final 256px
    icon branch and proves both the old binary and the outside directory remain
    byte-for-byte untouched when the entire upgrade is rejected.

83. **Stage-complete publication** — every workflow, transformed desktop entry,
    metadata file, icon, and executable is copied into a sibling temporary
    before the first destination rename. Any copy/transform failure cleans the
    entire queue and leaves the installed generation unchanged; resources are
    then atomically published one by one and the executable is the final commit
    marker. The contract explicitly stops short of claiming a cross-filesystem
    transaction during that short rename phase.

84. **Reversible publish phase** — before the first rename, every existing
    destination is snapshotted without following symlinks into a private sibling
    rollback backup. A failed rename or catchable termination restores every
    attempted destination in reverse order and removes new destinations that
    had no predecessor; a deterministic final-binary rename failure proves all
    pre-existing resources plus the executable regain their prior bytes and
    modes while an originally absent icon remains absent. If restoration itself
    fails, the recovery backup is retained and named.

85. **Truthful legacy-launcher cleanup** — removal of the pre-rename jterm3
    desktop entry is explicitly best-effort after the new generation commits.
    Its staged ancestor is revalidated at the removal point. A deterministic
    `rm` failure now emits a non-fatal warning, still refreshes caches and prints
    the success handoff, while the installed binary and new desktop entry remain
    complete and no transaction artifacts survive.

86. **Inode-faithful rollback snapshots** — a same-directory `ln -P` now keeps
    the exact old inode alive through publication, preserving ownership, mode,
    xattrs, hardlink identity, and dangling symlinks. The final-rename rollback
    regression checks device/inode/uid/gid for every predecessor, restores a
    dangling desktop symlink by link value, and checks a user xattr when the
    host supplies attr tools. Filesystems that reject hardlinks use a documented
    no-follow copy fallback with deliberately narrower ownership guarantees.

87. **Copy-safe PATH handoff** — the installer emits a direct, `%q`-escaped
    `export PATH=...:"$PATH"` line instead of nesting an arbitrary directory in
    single quotes inside an `echo` command. Empty, relative, and trailing-empty
    PATH cases stay exact; apostrophes and dollars cannot break the suggestion,
    while newline-bearing prebuilt or shadowing executable paths are reversibly
    quoted and cannot forge diagnostic lines.

88. **Side-effect-free special-target preflight** — final destinations are
    checked as a complete set before the first rollback link is created. A late
    directory, FIFO, or host-creatable Unix socket/device is rejected as neither
    a regular file nor symlink; an instrumented `ln` proves no earlier backup
    was attempted, the special object is unchanged, FIFO handling never blocks,
    and every staged temporary is removed.

89. **Symmetric uninstall target ownership** — the complete removal file set is
    now restricted to absent entries, regular files, and final symlinks before
    the first `rm`. A final symlink is unlinked without touching its referent;
    late directories, FIFOs, and host-creatable sockets/devices fail closed.
    Instrumented `rm` coverage proves both special-target and late ancestor
    rejection leave the earlier executable and the entire staged tree intact.

90. **Typed non-recursive cleanup** — the owned workflow cleanup path is now
    preflighted as either absent or a real directory before any file removal.
    Regular files, final symlinks, FIFOs, and host-creatable sockets/devices are
    rejected without invoking `rm`; the earlier binary and replacement object
    remain unchanged. Valid cleanup still uses only non-recursive `rmdir`, so a
    non-empty user workflow directory and its custom files are never traversed.

91. **Transactional uninstall quarantine** — after whole-plan preflight, every
    existing owned file is atomically renamed to a private sibling quarantine.
    A deterministic third-rename failure and a post-first-rename ancestor swap
    both restore earlier entries in reverse order with exact inode metadata; a
    post-rename `TERM` also proves in-flight bookkeeping is reconciled by inode.
    Only an all-renamed plan commits: later purge/rmdir failures are non-fatal,
    keep target names absent, retain a named quarantine when needed, and print
    a copy-safe `mv -fT` recovery command instead of reporting rollback.

92. **Inode-owned quarantine lifecycle** — mktemp reservations, rollback
    sources, and purge entries are now checked against their recorded inode at
    every use point. A later reservation replaced by a symlink aborts staging,
    restores the earlier binary, and leaves the substitute untouched; a
    post-rename quarantine substitution likewise invokes no `rm`, follows no
    referent, and is never advertised as the displaced original's recovery
    copy. Post-action command failures are reconciled by observed inode state.

93. **Directory-fd-bound uninstall operations** — every present target keeps a
    read-only parent fd, and reservation, quarantine rename, rollback, and purge
    execute through Linux `/proc/self/fd` names. Deterministic parent swaps from
    inside `mktemp`, `mv`, and `rm` prove placeholders never escape, interrupted
    staging restores the exact inode in the bound directory, and purge cannot
    unlink identically named files behind a replacement symlink. Logical parent
    identity is still checked before/after each phase; a failed purge names the
    bound physical directory on both sides of its recovery command; pooled fds
    close together after the optional post-commit phase.

94. **Bound post-commit cleanup and cache refresh** — workflow-directory
    cleanup records its target and parent inodes before the commit, while the
    optional desktop and icon refreshers receive only pre-opened directory-fd
    paths. Purge-time parent replacement skips cleanup/cache helpers entirely;
    replacements made inside either cache helper can touch only the displaced
    bound directory. Helper failures and identity changes remain explicitly
    non-fatal and never suppress the truthful uninstall success summary.

95. **Bound install helpers and capped fd pools** — legacy launcher removal,
    desktop validation, and desktop/icon cache refresh now reuse pre-publish
    applications/icon directory fds. Parent swaps inside `rm` or either cache
    helper cannot reach a replacement symlink referent; helper failures remain
    non-fatal and precede the truthful install summary. Install and uninstall
    pools deduplicate by device/inode under fixed ceilings, close once on every
    exit path, and preserve per-consumer use-point identity checks.

96. **Directory-fd-bound install publication** — every staged destination now
    records a pooled read-only parent fd plus exact temporary/original/backup
    identities. Temporary creation, hardlink/copy snapshots, publish rename,
    reverse rollback, and successful backup cleanup operate through Linux
    `/proc/self/fd` names and recheck the logical parent at each use point.
    Deterministic swaps from inside `mktemp`, `ln`, `mv`, and cleanup `rm` prove
    that work remains in the displaced inode: failed publication never removes
    an identically named replacement target, and successful cleanup leaves no
    rollback artifact in the old parent. Both directory pools now have a
    16-physical-inode ceiling and retain one close-on-exit owner per fd.

97. **Exact install-artifact transitions** — rollback reservations are reused
    only after their bound name is observably absent, and a hardlink snapshot
    is accepted only when it has the original target's exact device/inode.
    Substitutes injected after reservation `rm`, backup `ln`, or cleanup `rm`
    are retained as unowned names and never retried or removed. Conversely,
    non-zero wrappers after completed reservation/cleanup unlinks, hardlink
    creation, or final publish rename reconcile from exact post-action state;
    deterministic regressions prove the generation commits with no false
    rollback while every transaction-owned temporary and backup disappears.

98. **Descriptor-pinned staged and fallback copies** — every regular staging
    temporary stays open while byte copy and mode application address its fd,
    so cross-device sources cannot trigger destination inode replacement.
    Regular hardlink fallbacks likewise copy into an opened, exact reservation
    inode and revalidate source, descriptor, and bound name before ownership is
    accepted. Non-zero copy helpers reconcile only from complete content/mode
    plus exact identity; a name swapped after copy has ownership revoked and is
    retained untouched by cleanup or rollback. Deterministic regressions cover
    cross-device staging, forced hardlink fallback followed by reverse restore,
    and both staged-temporary and fallback-reservation ABA replacement.

99. **Pinned backup lifetime and post-publish name reconciliation** — every
    regular rollback snapshot now keeps a read-only fd until rollback/cleanup,
    preventing immediate inode-number reuse from making a replacement name look
    owned. Publish completion is defined by the staged inode reaching the exact
    destination and leaving its recorded source name; a different inode inserted
    at that source is retained as unowned even when `mv` returns non-zero.
    Backup identity is rechecked at the same boundary and revoked on mismatch.
    A deterministic two-publish regression replaces both names after the first
    completed rename, forces the second rename to fail, and proves rollback
    touches neither substitute while reporting the unrecoverable predecessor.

100. **Pinned symlink fallback snapshots** — a copied symlink backup is accepted
     only when its link text, uid/gid, and mode match the exact source, then a
     second same-directory hardlink pins that new inode through publish and
     rollback. The pin reservation itself stays descriptor-pinned while its
     placeholder name is removed, closing immediate inode-number reuse. Exact
     post-state reconciles `cp`/`ln` wrappers that finish then return non-zero;
     deterministic reservation and post-publish ABA regressions prove cleanup
     never unlinks substitutes, while failed rollback retains and diagnoses the
     original symlink inode under its recovery pin.

101. **Private-anchor symlink cleanup state machine** — each symlink snapshot
     now keeps a third hardlink inside a random `0700`, descriptor-bound private
     directory, plus the exact in-memory link text and uid/gid/mode (including
     preservation of trailing newlines). Cleanup treats main, pin, and anchor
     unlinks as independent use points, revalidating the logical parent,
     private directory, remaining
     peers, inode, and semantic fingerprint before advancing. Parent/private-dir
     renames, post-unlink substitutions, public-name hardlink merges, and even a
     forced three-name deletion with numeric inode reuse all fail closed without
     touching replacements. Any exact survivor yields one copy-safe `%q`
     recovery command based only on bound physical names; diagnostics never
     expose link text or its referent.

102. **Start identity captured at `C` and never minted at `D`** — the OSC 133
     field loop now recognises `session_id`, `seq` and `started_at_ms`, gated
     on the `C` mark, and stashes them with the execution id as one
     all-or-nothing `StartLifecycle`. jsh puts all four on `C` and none of the
     last three on `D`, so a completion that supplied them would be naming a
     Start generation this terminal never observed; `D` may only be compared
     against the captured id. The finished record carries that token to
     `main.rs`, where it becomes a `jterm_core::parser::CommandMeta` and then
     an `ExecutionLifecycle` — the journal's only durable-output capability,
     which core's writer re-checks against the authoritative on-disk Start
     under the journal lock. A boundary-inferred completion and a synthesized
     agent termination both carry `None`: neither corresponds to a journal
     Finish.

103. **Every interactive shell has a session identity** — `Session::spawn_*`
     passed `None` for `--session`, so every jsh under frost ran anonymously
     even though `pty.rs` had implemented the flag for both argv shapes.
     Without it jsh omits `session_id=` from `C`, the Start envelope is
     permanently incomplete, `ExecutionLifecycle::from_command_meta` always
     returns `None`, and the journal submit is a silent no-op — while jsh's own
     `start` events record a null session and its per-session snapshot never
     restores. The id is `agent_task_ui::terminal_session_id`, the same string
     the task reducer already binds terminals with, filtered through
     `is_valid_jsh_session_id`; the "is the shell actually jsh?" half of the
     gate stays in `pty.rs`, where the configured shell is finally resolved,
     and an explicit one-shot `command_argv` gets no session at all.

104. **The journal gate is asked before the copy, not after** — the submit loop
     cloned each finished block's whole captured output and only then let
     `submit` discover the journal was disabled, and its comment stated the
     gate backwards (the journal is on unless `JSH_EXECUTION_JOURNAL` turns it
     off). `output_capture_enabled()` is now checked once, ahead of the loop.
     `CompletionFacts::output` became `&'a str` upstream in the same round, so
     the correction trigger's per-command copy of up to
     `MAX_CAPTURED_OUTPUT_BYTES` is gone as well.

105. **A finished block names the directory the command RAN in** — `D`'s
     `cwd_url` is jsh's cwd *after* the command, so every block that changed
     directory was labelled with its destination: `cd /tmp` from
     `/home/u/proj` recorded `/tmp` as the working directory it ran in, and
     that value reaches the block menu, the Markdown export and the agent
     prompt. `D` may now only fill an empty cwd, never replace `C`'s. ember and
     forge both already guarded this.

106. **Untrusted archives are validated, staged privately, then published** —
     a remote directory download extracted the far side's tar straight into the
     destination's *parent* with nothing about its members checked, so an
     archive naming `../../.ssh/authorized_keys`, an absolute path, or a second
     top-level tree beside the requested one wrote wherever it named. Every
     member is now required to sit under the one expected top-level component
     (streamed and bounded, never retained), extraction happens in an
     owner-only `create_new` directory this process owns, the extracted shape
     is re-checked, and only that single verified tree is published with one
     `RENAME_NOREPLACE` namespace operation. The staging directory is removed
     on both paths.

107. **Notification text is sanitised before it leaves the terminal** — OSC
     9/777 titles and bodies reached `notify-send` with control characters and
     bidi overrides intact, so `printf '\e]777;notify;<RLO>Security
     Update;<RLO>approve\a'` put attacker-reordered text into a desktop toast
     wearing the desktop's chrome. Offending scalars become U+FFFD rather than
     disappearing, on the terminal-strict spoofing class, matching core's own
     `bounded_notification_field`; a field that trims to nothing falls back to
     the application identity.

108. **One exit, and it is the one that flushes** — closing the last session
     returned the exit task after writing only the session snapshot, so the
     Agent transcript, the AI-chat store, the jsh execution journal and the
     shared command-history index were abandoned mid-flight. Whether a
     session's last commands survived depended on which quit gesture the user
     used. Both gestures now return through `exit_flushing_durable_state`, each
     flush keeps its own short deadline so a stalled state filesystem cannot
     turn quitting into a hang, and a structural regression pins the exit task
     to that one construction site.

109. **Repeated OSC 133 slots and contradictory exit slots fail closed** —
     repeated keys were last-wins, so PTY output could retract a truncation
     disclosure with a second `cmd_truncated=0`, choose between two
     contradictory directories, or name an execution twice. A `D` carrying two
     outcome slots committed to the first, rendering `D;1;exit=0` as a
     successful block, and a malformed `exit=` deferred to the next field
     instead of occupying the slot it claimed. Both now follow core's parser
     exactly: a repeated slot is ambiguity, and ambiguity is Unknown.

110. **The OSC 133 alias set, decoding and text rules match core** —
     `command_url` and `cmdline` were unrecognised and a bare `command=` was
     taken without percent-decoding, so identical bytes decoded differently
     depending on which spelling a shell chose. Metadata was filtered for
     control characters only and then rendered raw in the block menu and the
     export, so a cwd or id spelled with a zero-width joiner, a bidi override
     or an interlinear annotation control drew as one path and named another;
     ids, commands and cwds now go through the now-public
     `review_input::is_terminal_visual_spoofing_character`, and cwds
     additionally through `execution_journal::is_valid_jsh_cwd`.

111. **The OSC 7 working directory is bounded** — the OSC 133 `cwd` param was
     capped while OSC 7 was not, even though OSC 7 is the wider liability: it
     is retained for the life of the pane, cloned into every command zone with
     no cwd of its own, written into the session snapshot, and inherited by
     every split. Both now use the same 4 KiB ceiling, which is also Linux's
     own pathname limit, and the cap is on the decoded path so percent-encoding
     buys no extra room.

112. **OSC 52 writes are gated in both directions** — frost was the one app of
     the four that let PTY output replace the host clipboard with no gate,
     while already gating reads. A program in the terminal — including one on
     the far side of an ssh connection — could silently choose what the user's
     next paste produced, into a shell or a password field.
     `allow_remote_clipboard_write` defaults off, matching anvil and forge's
     identically named option and ember's `osc52_clipboard_write`, and is
     independent of the read gate.

113. **The command-zone rebase is amortised** — past the scrollback cap every
     scrolled line trims a row, and rebasing the retained zones for it was a
     per-line walk of the whole 256-entry deque on the PTY ingest path. The
     shift is uniform, so it accumulates and is applied once per ingest batch;
     every path that can observe a zone's anchors settles it first, so no
     reader ever sees a stale row. A regression proves the batched result is
     byte-identical to the per-row one and that one batch rebases once however
     many rows it trimmed.

114. **The keyboard surfaces read the keyboard table** — the command palette
     and the Keyboard Shortcuts panel printed hardcoded default chords, so the
     two surfaces whose entire job is to teach the keyboard were the two most
     likely to be lying to a user who had configured one, and still showed a
     chord for a command they had unbound. `KeyBindings::chords_for` /
     `shortcut_label` render from the live table in core's frozen modifier
     order (which exposed two palette hints spelled `Ctrl+Alt+Shift+…`), an
     unbound command shows nothing rather than its former default, and the
     genuinely non-configurable app chrome keeps its literal hint.

115. **One ad-hoc block-context adapter, not three** — the three surfaces that
     attach a block to the AI as untrusted evidence each hand-rolled a
     `BlockContext`, with an unknown-status note none of the other three apps
     used and a second local bounding helper. `ad_hoc_block_context` — the
     family-shared adapter carried by anvil, ember and forge, and documented
     there as frost's own `AskAi` adapter — is re-adopted with its six
     regressions, so the sentinel, the explanatory note, the unavailable-output
     placeholder and the head/tail bound are the family's in every surface.

116. **The last-session quit flushes while its child can still answer** —
     unifying the two quit gestures (item 108) reordered the one they did not
     share: the last session's PTY was signalled *before*
     `exit_flushing_durable_state` ran, and `save_session_snapshot` is the third
     flush inside it. `terminate` returns as soon as SIGHUP/SIGTERM are away and
     its reaper waits before reaping, so the snapshot read the `/proc/<pid>/cwd`
     link of a zombie. For a shell that reports OSC 7 — jsh does — nothing was
     lost, but a plain bash session has no other source for its directory, so
     the last thing frost persisted before quitting was `cwd: None` and the next
     launch opened somewhere else. The flush now runs first and the signal
     second.

117. **The lifecycle-bound journal submit is pinned, not merely present** —
     rounds 102–104 and 112 landed with no test between them and the code:
     neutering the submit loop, inverting the capture gate, dropping a slot from
     the `CommandMeta` the submit path builds, or restoring the unconditional
     OSC 52 clipboard write all compiled and left the whole matrix green, which
     is the "compiles, passes, never journals" shape the round existed to
     remove. The conversion is now a named `journal_lifecycle`, driven end to
     end from a real OSC 133 A/B/C/D sequence and from the boundary-inferred
     completion that must yield nothing; the submit site, its two gates, and the
     `--session` identity handed to the one PTY spawn are pinned structurally,
     in the style of `the_process_has_exactly_one_exit_and_it_flushes_first`,
     because none of them is observable after the fact.

Agent-TUI fidelity and family parity adds rounds 118–129 (2026-09-19). Real
Claude Code, Codex and Kimi Code sessions recorded through a live emulator
were replayed into frost and into libvte 0.76; screen text matched libvte
everywhere except a resize, and an audit against ember found the rest.

118. **Wide over wide keeps its pair** — writing a double-width character
     whose right half lands on another wide lead orphaned that neighbour's
     continuation, so a later write blanked the new glyph's right half (CJK
     redraws in Claude Code shifted by one column). `put_char` now splits the
     pairs around the write, as ember does.
119. **IRM shifts respect wide pairs** — insert mode no longer leaves a lead
     without its continuation at the cut or the row end.
120. **Shrinking a pane drops blank rows first** — a height shrink evicted
     the rows above the cursor into scrollback and jumped the prompt to the
     top even with blank rows below; blank rows below the cursor now go first.
121. **Colour queries report the theme** — OSC 10/11/12 and OSC 4 (0–15)
     answered hard-coded white/black; frost now pushes the active theme into
     every session, and explicit OSC sets still win.
122. **Replies use the query's terminator** — BEL-terminated OSC queries get
     BEL-terminated replies, including OSC 52 and 5522 status replies.
123. **DSR 996 and mode 2031 notifications** — `CSI ? 996 n` reports
     dark/light, and a theme flip sends the same report to mode 2031
     subscribers unprompted.
124. **DECXCPR keeps its marker** — `CSI ? 6 n` answers `CSI ? r ; c R`.
125. **XTWINOPS 16t** — the cell size in pixels, which Claude Code requests.
126. **ANSI-mode DECRQM** — IRM (4) and LNM (20) are answered alongside the
     private-mode table.
127. **VT and FF feed lines** — they act as LF instead of being dropped.
128. **Three-byte escapes are consumed whole** — `ESC % G`, `ESC * B`,
     `ESC # 3` no longer leak their final byte, including across reads.
129. **Formatting gate** — rustfmt-only fixes to this evening's agent-task
     code, which left `cargo fmt --check` failing on master.

130. **Pending OSC 5522 reads precede EPERM** — while an asynchronous host read
     is still outstanding, later MIME-data batches in the same session are
     counted as bounded refusals rather than answered with an immediate EPERM,
     so a permission flip mid-flight cannot be mistaken for the older read's
     completion. A focused regression drives the outstanding-read path with
     consent revoked between batches.
131. **Process-global OSC clipboard reads** — iced's clipboard API is one
     in-flight read per process, but frost tracked `clipboard_read_in_flight`
     per session, so two panes could each spawn `iced::clipboard::read()` and
     race the host clipboard. A single owner `(session id, PTY fd)` now gates
     every OSC 52 and OSC 5522 host read; other sessions receive the
     interoperable busy/empty refusal without starting another read, closing a
     pane releases an outstanding owner, and late async completions clear the
     slot even when the originating session is already gone.

132. **OSC 52 GET permission pinned structurally** — `allow_clipboard_read`
     defaults closed, but only runtime config tests saw it. A structural
     regression now proves `iced::clipboard::read()` is enqueued only inside
     the read-permission block and never from the host-read-blocked refusal
     path, matching the write-permission pin added earlier.

133. **OSC 5522 read permission pinned structurally** — extended MIME reads
     share the same default-closed permission and process-global host-read
     owner as OSC 52 GET. A structural regression now proves
     `service_osc5522_read` stays behind `allow_clipboard_read`, host-read
     blocking cannot enqueue another `iced::clipboard::read()`, and read-disabled
     paths answer EPERM without touching the clipboard.

134. **OSC 5522 completion stays behind read permission** — async clipboard
     completions for extended MIME reads now pin `complete_osc5522_read` behind
     the same default-closed permission, answering EPERM without touching host
     data when reads are disabled.

135. **OSC 52 completion read permission pinned structurally** — async OSC 52
     clipboard completions stay behind the default-closed read gate; a structural
     regression pins `complete_osc52_read` clearing host data when reads are
     disabled.

136. **OSC 52 SET write rate limit** — once `allow_remote_clipboard_write` is
     granted, OSC 52 SETs are still capped at two host writes per rolling
     second so a remote PTY cannot spam-replace the user's paste buffer. A unit
     regression pins refusal of a third write inside the same window.

137. **Evolve round-25 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `f2d2ce0`
     + jagent `a7474e9` after `between()` 76, Find continue bookmark-empty,
     and STAGE 64 handoff sync. Manifests stay on published pins.

138. **Evolve round-27 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     STAGE 67 classify `--` peel, daemonize flag-only `-a`/`-v`, leftover
     `s6-envdir` pin, and anvil/forge STAGE 67 docs (rounds 88 / 122). Round
     26 already sat on the handoff tip. Manifests stay on published pins.

139. **Evolve round-28 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     STAGE 68 (`gnome-session-inhibit`), fail-closed nest/transparency deepen,
     Find/organism polish, and anvil/forge STAGE 68 docs (rounds 92 / 126).
     Manifests stay on published pins.

140. **Evolve round-29 path-patch smoke** — `cargo test --bin frost -- workflows command_correction` (12) against local
     `jterm_core` `ee39a12` + jagent `543415b` after STAGE 70 (`uclampset`/
     `gamemoderun`), Guard*→Celebrate* None survey, and anvil/forge sticky/
     find polish (rounds 95–97 / 129–130). Manifests stay on published pins.

141. **Evolve round-30 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     gnome-session-inhibit STAGE arity edges, timeout/nice classify/jagent
     nest, and membership/DISPATCHES/CLASSIFY_FORMS len-70 lockstep. Round
     29 already sat on the handoff tip. Manifests stay on published pins.

142. **Evolve round-31 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     wave-26 PATH non-launcher leftover pins, anvil sticky whitespace (100)
     + forge file-tree permission/missing (134), and STAGE 70 docs already
     present. Manifests stay on published pins.

143. **Evolve round-32 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` HEAD
     `68982a3` + jagent `9cd0211` (busybox/pipe nest + CLASSIFY/STAGE 70
     set-eq; anvil 101–104 / forge 135–138 pins). Manifests stay on
     published pins.

144. **Evolve round-33 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     busybox applet + pipe-to-bash nest deepenings for uclampset/gamemoderun/
     gnome-session-inhibit, CLASSIFY_FORMS set-equality with STAGE 70, and
     timeout/nice classify peels. Manifests stay on published pins.

145. **Evolve round-34 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     ambient→vigil/celebrate None, PATH wave-27 identity/agent leftovers
     out of STAGE, and anvil 105–107 / forge 139–141 organism/find/history
     edges. Manifests stay on published pins.

146. **Evolve round-35 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after PATH
     wave-28 systemd inspector leftovers out of STAGE, systemd-cat/inhibit
     busybox + timeout/nice nest deepenings, and prior waves. Manifests stay
     on published pins.

147. **Evolve round-36 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     ambient→hold/rest None, CelebrateBig fifteen/`between()` 91 UI lockstep,
     anvil 111–116 / forge 145–150 sticky/find/organism edges, and PATH
     wave-28. Manifests stay on published pins.

148. **Evolve round-37 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after openvt
     STAGE 71 + timeout/nice nest, Inspect/Sit→Unknown `between()` 93, anvil
     117–121 / forge 151–155 sticky/find/organism edges, and prior waves.
     Manifests stay on published pins.

149. **Evolve round-38 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after STAGE
     71 busybox/DISPATCHES set-eq + openvt pipe nests, anvil 122–125 / forge
     156–160 sticky/find/cancel/notice + STAGE 71 tip docs, and
     Inspect/Sit→Unknown `between()` 93. Manifests stay on published pins.

150. **Evolve round-39 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     openvt/daemonize busybox applet + pipe-to-bash deepenings, DISPATCHES
     STAGE 71 set-eq beside CLASSIFY_FORMS, and prior waves. Manifests stay
     on published pins.

151. **Evolve round-40 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     anvil 129–130 / forge 164–166 sticky punct/thin/hair + find empty-query
     + notice catch-up beside STAGE 71 / between() 93 / prior waves.
     Manifests stay on published pins.

152. **Evolve round-41 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after
     anvil 131–132 / forge 167–169 sticky quad-space/RLI/FSI + find All
     empty+stale + notice catch-up beside STAGE 71 / between() 93 / prior
     waves. Manifests stay on published pins.

153. **Evolve round-43 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after PATH
     wave-31 ctl/utility leftovers out of STAGE beside CLASSIFY/DISPATCHES
     lockstep at 71, anvil 133–136 / forge 170–174 Hangul/whitespace/Watch*
     pins, and prior waves. Manifests stay on published pins.

154. **Evolve round-42 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     137–140 / forge 175–179 Ogham sticky + All whitespace find +
     Guard/Celebrate Full-motion + notice catch-up beside STAGE 71 /
     between() 93 / prior waves (fills gap before round-43). Manifests stay
     on published pins.

155. **Evolve round-44 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     148–151 / forge 188–191 Idle/Rest Guard + hold→Watch + Failure→holds
     Full-motion beside anvil 141–147 / forge 180–187 hold/ambient/Watch/
     CrossBlock/Retry/FVS/NBSP pins, STAGE 71 / between() 93, and prior waves.
     Manifests stay on published pins.


156. **Evolve round-45 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     152–154 / forge 192–195 bidi sticky/find/notice + ambient disposition
     completeness beside anvil 148–151 / forge 188–191 Idle/Rest Guard pins,
     wave-32 PATH leftovers + aa-exec/socket-activate deepen, STAGE 71 /
     between() 93, and prior waves. Manifests stay on published pins.

157. **Evolve round-47 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     155–157 / forge 196–199 sticky VS/FVS4 + find marks + GlanceAside ambient
     beside anvil 152–154 / forge 192–195 bidi/ambient, STAGE 71 / between()
     93, and prior waves. Manifests stay on published pins.

158. **Evolve round-49 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after PATH
     wave-33 device/sysctl leftovers + setsid busybox/timeout/nice deepen
     beside anvil 158–160 / forge 200–202 CrossBlock cancel + Celebrate survey,
     STAGE 71 / between() 93, and prior waves. Manifests stay on published pins.

159. **Evolve round-48 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     161–164 / forge 203–207 sticky FE02/nirugu + Hangul find + Watch* Unknown
     + near-wrap finished beside anvil 158–160 / forge 200–202, STAGE 71 /
     between() 93, and prior waves (fills gap before round-49). Manifests stay
     on published pins.

160. **Evolve round-50 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     171–174 / forge 215–219 sticky FE04/syllable + FE03 find + MAX-3 cancel +
     Celebrate→Unknown beside anvil 165–170 / forge 208–214, STAGE 71 /
     between() 93, and prior waves. Manifests stay on published pins.

161. **Evolve round-51 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` after anvil
     175–178 / forge 220–224 sticky FE05/Manchu + FE04 find + MAX-4 cancel +
     Rest→Unknown beside anvil 171–174 / forge 215–219, STAGE 71 /
     between() 93, and prior waves. Manifests stay on published pins.

162. **Evolve round-52 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `a4d3b5d` +
     jagent `1c3d308` after anvil 179–183 / forge 225–230 sticky FE06/FE05/
     MAX-5/Celebrate-Rest/SitNear-Inspect beside prior waves (fills gap
     wave-34 skipped). STAGE 71 / between() 93 held. Manifests stay on
     published pins.

163. **Evolve round-53 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `f530cfb` +
     jagent `e8272a4` after anvil 184–187 / forge 231–235 sticky FE07/birga +
     FE06 find + MAX-6/GuardRecovery→Unknown beside prior waves. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

164. **Evolve round-54 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `f530cfb` +
     jagent `e8272a4` after wave-35 host/hw inventory leftovers + chrt/ionice
     STAGE deepen beside prior waves. STAGE 71 / between() 93 held. Manifests
     stay on published pins.

165. **Evolve round-55 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `f530cfb` +
     jagent `e8272a4` after anvil 188–191 / forge 236–240 sticky 1801 + FE07
     find + MAX-7/GuardRecovery UI sync beside prior waves. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

166. **Evolve round-56 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `5fa0f0b` +
     jagent `2204f36` after anvil 192–195 / forge 241–245 sticky 1802 + 1801
     find + MAX-8/Unknown→GuardRecovery beside prior waves. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

167. **Evolve round-58 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `8a8e607` +
     jagent `8825c4f` after anvil 196–199 / forge 246–250 sticky 1803 + 1802
     find + MAX-9/Unknown→GuardCautious beside prior waves. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

168. **Evolve round-59 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `bbb361e` +
     jagent `cbbe241` after anvil 200–203 / forge 251–255 sticky 1804 + 1803
     find + MAX-10/Unknown→GuardStuck beside prior waves. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

169. **Evolve round-60 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `d442073` +
     jagent `f85723f` after PATH wave-38 process-table monitor leftovers +
     softlimit/cgexec STAGE deepen beside sticky 1804/MAX-10 / round-59.
     STAGE 71 / between() 93 held. Manifests stay on published pins.

170. **Evolve round-61 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `bbb361e` +
     jagent `cbbe241` after anvil 200–203 / forge 251–255 sticky 1804 + 1803
     find + MAX-10/Unknown→GuardStuck beside round-60 wave-38. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

171. **Evolve round-62 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `00fde9b` +
     jagent `49655f5` after anvil 204–207 / forge 256–260 sticky 1805 + 1804
     find + MAX-11/Unknown→GuardFailure beside round-61. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

172. **Evolve round-63 path-patch smoke** — `cargo test --bin frost --
     workflows command_correction` (12) against local `jterm_core` `cb3c8e6` +
     jagent `d05c9cf` after anvil 208–211 / forge 261–265 sticky FF1A + 1805
     find + MAX-12/Unknown→Idle beside round-62. STAGE 71 /
     between() 93 held. Manifests stay on published pins.

173. **Fail-closed named ANSI color resolution** — `src/color.rs` no longer
     `unwrap()`s a partial named-color map after matching Default/Indexed/RGB.
     Named slots are an exhaustive `Color` match that returns `None` for the
     three non-named variants; a miss falls back to the theme default instead
     of panicking. Bold-brightening, dim attenuation, OSC palette overrides,
     and the 256-color cube/gray ramps are pinned by unit tests; the palette-less
     wrappers stay live through those tests rather than `allow(dead_code)`.

174. **Bounded find-preview windows** — `get_match_context` treated `contains("")`
     as a hit on every line and accepted an unbounded context radius, so an
     empty pattern or `usize::MAX` neighbors could dump the whole buffer. Empty
     patterns now return nothing, each window is capped at 8 neighbor lines,
     and only the first 64 matches are previewed with an explicit omitted count.

175. **Release-clean debug dump** — `debug::enabled` is compiled only under
     `debug_assertions`, matching `debug_log!`, so a release build no longer
     carries a dead OnceLock probe. `format_bytes` now names ESC/CR/LF/TAB,
     hex-escapes the rest, and truncates at 96 bytes with a remainder marker,
     pinned so a control byte cannot be echoed raw into a debug line.

176. **Empty regex replace is a no-op** — the public search-and-replace entry now
     refuses an empty pattern before choosing the literal or regex engine, so
     `replace_all` cannot insert the replacement at every empty match the regex
     crate would otherwise report.

177. **Out-of-range ANSI theme fallback** — `ThemeExt::ansi_color` is pinned to
     return the terminal foreground for every index outside 0–15, including
     `usize::MAX`, instead of relying on an untested `else` arm. RGBA iced
     conversion is also pinned so alpha is `u8 / 255`.

178. **Sanitized regex compile errors** — find and find-replace both quoted the
     regex crate's error, which echoes the draft pattern. ESC and bidi marks in
     an invalid pattern now go through `safe_inline_display` (160 bytes) before
     reaching the search bar or the replace-panel status line.

179. **Complete numeric config clamp matrix** — load-time normalization already
     bounded font/line-spacing/window size, but padding, opacity, scrollback,
     and scroll speed were untested. NaN/∞ now fall back, overflow clamps to
     the documented extrema (font 72, padding 20, opacity 1.0, speed 10,
     scrollback 100–100_000), and the live clamp helpers no longer need a
     stale `dead_code` allow.

180. **Fail-closed link columns and a per-row cap** — highlight mapping used
     `line[..byte_offset]`, which panics on a mid-codepoint or past-the-end
     offset after trimming a URL or path. Offsets now go through `str::get`
     and invalid spans are skipped. One row also stops at 64 actionable
     highlights so a paste of thousands of URLs cannot unbounded-allocate
     click targets.

181. **Bounded history-picker query** — the Ctrl+Shift+H overlay accepted iced
     `text_input` and raw key text into an unbounded `String`, so a paste could
     grow the fuzzy haystack without the workflow picker's 4 KiB one-line
     budget. `set_query` / `push_query_text` / `backspace` now drop controls,
     truncate on a UTF-8 boundary, and are the only write path from both the
     widget and the keyboard handler.

182. **Bounded command-palette query** — Ctrl+Shift+P assigned iced `text_input`
     and raw key text to an unbounded `String`, so a paste could grow the
     fuzzy haystack without the shared 4 KiB one-line budget. `set_query` /
     `push_query_text` / `backspace` now drop controls, truncate on a UTF-8
     boundary, and are the only write path from both the widget and the
     keyboard handler.

183. **Bounded tab-switcher query** — Ctrl+Shift+L assigned iced `text_input`
     and raw key text to an unbounded `String`, so a paste could grow the
     fuzzy haystack without the shared 4 KiB one-line budget. `set_query` /
     `push_query_text` / `backspace` now drop controls, truncate on a UTF-8
     boundary, and are the only write path from both the widget and the
     keyboard handler.

184. **Bounded terminal-find query** — the in-buffer search bar assigned iced
     `text_input` and raw key text to an unbounded `String`, so a paste could
     compile an unbounded regex against scrollback. `set_query` /
     `push_query_text` / `backspace` now drop controls, truncate on a UTF-8
     boundary at the same 4 KiB budget as block search, and history recall
     applies the same bound without dropping the navigation index.

185. **Bounded find-replace fields** — Ctrl+Alt+R assigned iced `text_input`
     into unbounded find and replace strings, so a paste could compile an
     unbounded regex or grow a prompt/clipboard payload without the review
     budget. Find now shares the 4 KiB search-query cap; replace shares the
     256 KiB prompt-insert cap; both drop control characters.

186. **Bounded workflow argument fields** — the parameter form assigned iced
     `text_input` into `ArgsForm` without a byte cap, so a paste could sit in
     memory until render refused it. `set_value` now drops controls and
     truncates on a UTF-8 boundary at `MAX_WORKFLOW_FIELD_BYTES` (4 KiB).

187. **Bounded Ask-AI request overlay** — the iced field rejected an oversized
     paste wholesale and the raw-key path dropped extra bytes without
     truncating, so a 4 KiB+1 paste left the previous request intact and a
     newline could sit in the overlay until submit. Both paths now drop
     controls and truncate on a UTF-8 boundary at
     `MAX_SUGGESTION_REQUEST_BYTES`.

188. **Bounded AI suggestion draft** — the review card rejected an oversized
     iced paste wholesale and stored a model reply verbatim, so a 256 KiB+1
     edit bounced and a newline in the generated command sat in the card until
     insert failed. Both the widget and `apply_reply` now drop controls and
     truncate on a UTF-8 boundary at `MAX_REVIEW_INPUT_BYTES`.

189. **Bounded Tasks follow-up composer** — the iced field rejected an oversized
     paste wholesale, so a 16 KiB+1 edit left the previous draft intact and
     ESC/BEL could sit in the composer until send. `set_follow_up` now keeps
     newlines and tabs, drops other controls, and truncates on a UTF-8
     boundary at `NATIVE_AGENT_FOLLOW_UP_MAX_BYTES`.

190. **Bounded AI-chats library filter** — the panel search kept the first
     1,024 Unicode scalars including ESC/newline, so a paste could restyle
     the filter and disagreed with the family's 4 KiB overlay budget.
     `set_search` now drops controls and truncates on a UTF-8 boundary at
     `MAX_PICKER_QUERY_BYTES`.

191. **Bounded AI-chats title draft** — the rename iced field stored the raw
     paste while only the ChatStore title was normalised, so ESC/newline and
     a 256-byte+1 paste could sit in the widget next to an 80-char/256-byte
     persisted title. `rename` now drops controls, replaces visual spoofing
     with U+FFFD, and truncates at the store envelope before the field updates.

192. **Bounded Agent-panel composer** — iced assigned the prompt into
     `AgentUi.input` unbounded, so a paste could sit past the 16 KiB
     `submit_user` envelope and carry ESC into the session. `set_input` now
     keeps newlines and tabs, drops other controls, and truncates on a UTF-8
     boundary at `NATIVE_AGENT_FOLLOW_UP_MAX_BYTES`.

193. **Bounded command-correction draft** — the iced card assigned the edited
     command into `CorrectionProposal` unbounded, so a paste could sit past
     the engine's 16 KiB single-line gate and carry ESC until accept refused.
     `set_draft` now drops controls and truncates on a UTF-8 boundary at
     `MAX_CORRECTION_COMMAND_BYTES`.

194. **Bounded tab-rename draft** — the iced rename field stored the raw paste
     while only submit applied a 64-char filter, so ESC/newline and a 256-byte+1
     title could sit in the strip editor next to the snapshot envelope.
     `TabRenameInput` and `apply_tab_rename` now share `bound_tab_title_draft`
     (drop controls, truncate at `MAX_RESTORED_TAB_TITLE_BYTES`).

195. **Bounded sidebar filter** — the files-panel filter assigned iced
     `text_input` into `sidebar_filter` unbounded, so a paste could restyle
     the tree match and grow without the family's 4 KiB overlay budget.
     `bound_sidebar_filter` now drops controls and truncates on a UTF-8
     boundary at `MAX_PICKER_QUERY_BYTES`.

196. **Bounded sidebar path bar** — the files-panel path editor assigned iced
     `text_input` unbounded, so ESC/bidi and a 4 KiB+1 paste could sit until
     submit refused. `bound_sidebar_path_input` now drops controls and
     directional marks and truncates on a UTF-8 boundary at
     `MAX_NAVIGATION_PATH_BYTES`.

197. **Bounded sidebar create/rename name** — the New File / New Folder /
     Rename dialog assigned iced `text_input` unbounded, so `/`, ESC, and a
     255-byte+1 paste could sit until submit refused. `bound_new_name` now
     drops controls and slashes and truncates on a UTF-8 boundary at 255 bytes.

198. **Bounded API-key draft** — the settings field stored the raw paste while
     only `write_api_key_file` refused, so ESC/newline and a 16 KiB paste could
     sit in RAM next to a credential that must be one line and one byte under
     the file cap. `SetAiKeyDraft` now drops controls and truncates on a UTF-8
     boundary at `MAX_API_KEY_DRAFT_BYTES`.

199. **Bounded AI model settings field** — iced assigned the model name into
     config unbounded, so ESC/bidi and a 256-byte+1 paste sat until
     `normalized()` replaced it with the default. `SetAiModel` now drops
     controls and visual spoofing and truncates at `MAX_CONFIG_NAME_BYTES`.

200. **Bounded AI base-URL settings field** — iced assigned the endpoint URL
     unbounded, so ESC/newline and a 4 KiB+1 paste sat until `normalized()`
     replaced it with the default. `SetAiBaseUrl` now uses `bound_config_text`
     at `MAX_CONFIG_VALUE_BYTES`.

201. **Bounded AI temperature draft** — the settings field kept the raw paste
     so a 32-byte+1 edit sat in the widget while only a parsed 0..=2 value
     reached config. `SetAiTemperature` now drops controls and visual spoofing
     and truncates at `MAX_AI_TEMPERATURE_DRAFT_BYTES`.

202. **Bounded AI provider settings field** — iced assigned the provider name
     unbounded, so ESC/bidi and a 256-byte+1 paste sat until `normalized()`
     replaced it with the default. `SetAiProvider` now uses `bound_config_text`
     at `MAX_CONFIG_NAME_BYTES`.

203. **Bounded API-key file path field** — iced assigned the credential path
     unbounded, so ESC/newline and a 4 KiB+1 paste sat until `normalized()`
     dropped it. `SetAiKeyFile` now uses `bound_config_text` at
     `MAX_CONFIG_VALUE_BYTES`.

204. **Bounded remote host name field** — iced assigned the display name
     unbounded, so ESC/bidi and a 256-byte+1 paste sat in config until
     `validate_remote_host` refused every consumer. `RemoteHostName` now uses
     `bound_config_text` at `MAX_CONFIG_NAME_BYTES`.

205. **Bounded remote host address field** — iced assigned the hostname
     unbounded, so ESC/newline and a 4 KiB+1 paste sat until
     `validate_remote_host` refused. `RemoteHostHost` now uses
     `bound_config_text` at `MAX_CONFIG_VALUE_BYTES`.

206. **Bounded remote host user field** — iced assigned the login name
     unbounded, so ESC/newline and a 4 KiB+1 paste sat until
     `validate_remote_host` refused. `RemoteHostUser` now uses
     `bound_config_text` at `MAX_CONFIG_VALUE_BYTES`.

207. **Bounded custom theme name draft** — the theme editor assigned iced
     `text_input` unbounded, so `/`, ESC, and a 160-byte+1 paste sat until
     save refused. `ThemeEditName` now drops controls, path separators, and
     visual spoofing, and truncates at the core filename envelope.

208. **Bounded theme hex draft** — each color slot stored the raw paste, so
     ESC and a megabyte hex string sat until save called `hex_to_rgb`.
     `ThemeEditColor` now keeps an optional `#` plus six hex digits.

209. **Bounded Agent proposal-edit field** — iced `validate_single_line` on
     every keystroke cleared the buffer on an oversized or control-bearing
     paste, wiping the reviewed command. `bound_agent_edit_command` now drops
     controls and visual spoofing and truncates at `MAX_AGENT_COMMAND_BYTES`.

210. **Bounded theme picker value** — `SetTheme` assigned the pick-list string
     into config unbounded, then `apply_config()` ran immediately, so ESC/bidi
     and a 256-byte+1 name could restyle chrome until `normalized()` reset it.
     The handler now uses `bound_config_text` at `MAX_CONFIG_NAME_BYTES`.

211. **Bounded font-family picker value** — `SetFontFamily` assigned the
     pick-list string unbounded, then `apply_config()` ran immediately.
     The handler now uses `bound_config_text` at `MAX_CONFIG_NAME_BYTES`.

212. **Bounded remote host deploy field** — iced assigned the deploy mode
     unbounded, so ESC/bidi and a 256-byte+1 paste sat until
     `validate_remote_host` refused. `RemoteHostDeploy` now uses
     `bound_config_text` at `MAX_CONFIG_NAME_BYTES`.

213. **Bounded toast chrome** — `push_toast` stored interpolated paths and
     errors verbatim, so ESC/bidi and a long `io::Error` could restyle the
     overlay. Every toast now goes through `safe_inline_display` at 256 bytes.

214. **Bounded AI-chats composer** — iced assigned the draft through
     `set_active_draft`, which truncates at 64 KiB but keeps ESC, so a paste
     could restyle the composer until send. `set_draft` now keeps newlines and
     tabs, drops other controls, and truncates at `MAX_LIVE_MESSAGE_BYTES`.

215. **Bounded sidebar transfer/busy notices** — transfer progress interpolated
     filenames and `DirectoryError::busy` stored backend text verbatim, so
     ESC/bidi and a long name could restyle the files panel. Both now go
     through `safe_inline_display` at 192 bytes.

216. **Bounded Agent status line** — protocol and IO errors were assigned into
     `AgentUi.status` verbatim, so ESC/bidi and a long transport error could
     restyle the panel. `set_status` now uses `safe_inline_display` at 256 bytes.

217. **Bounded AI-chats notice line** — restore/provider errors were assigned
     into the panel notice verbatim, so ESC/bidi and a long decode error could
     restyle the library chrome. `set_notice` now uses `safe_inline_display`
     at 256 bytes.

218. **Bounded AI provider chrome label** — `display_name()` was copied into
     Agent and AI Chats headers verbatim, so a hostile local-provider name
     could restyle the panel. Both now use `bound_provider_label` at 256 bytes.

219. **Bounded files-panel notice store** — worker errors, transfer ticks, and
     paste/delete failures were assigned into `sidebar_notice` raw, so ESC/bidi
     could restyle the Files chrome even when a later busy path was bounded.
     `set_sidebar_notice` and the notice view now run `bound_sidebar_notice`.

220. **Bounded AI-chats lifecycle status** — store Thinking/Info/Error text was
     drawn verbatim in the panel status line, so a persisted or transport error
     with ESC/bidi could restyle the library chrome. `status_line` now runs
     `bound_chat_notice` at 256 bytes.

221. **Agent edit start truncates instead of bouncing** — `AgentEditStart`
     refused the whole proposal when the command was over budget or contained
     a control, so Edit never opened. It now uses `prepared_agent_edit_command`
     so an oversized paste still opens a truncated, control-stripped buffer.

222. **Bounded theme-editor error line** — save/validation failures were drawn
     as danger chrome verbatim, so an IO error with ESC/bidi could restyle the
     overlay. Store and draw now run `bound_theme_editor_error` at 256 bytes.

223. **Bounded files-dialog path and error chrome** — New File / Rename overlays
     drew `path.display()` and validation errors as iced text, so a hostile
     filename or quoted problem could restyle the modal. Both now run
     `bound_sidebar_notice`.

224. **Bounded block-search query-error chrome** — regex compile failures quote
     the draft and were drawn as danger chrome verbatim. The matcher now
     sanitizes `InvalidRegex` payloads; the picker stores and draws
     `bound_query_error` at 160 bytes.

225. **Bounded delete-confirm path list** — the Files delete modal drew up to
     five `path.display()` values as iced text, so a hostile filename could
     restyle the confirmation. Those rows now use `bound_sidebar_path_label`.

226. **Bounded drop-import target path chrome** — hover/import notices and the
     transfer label interpolated `target_dir.display()` raw. Those now use
     `bound_sidebar_path_label` so a hostile drop destination cannot restyle
     the Files notice.

227. **Bounded files context-menu path header** — the right-click menu drew
     `path.display()` as the header, so a hostile filename could restyle the
     overlay. Single-item headers now use `bound_sidebar_path_label`.

228. **Bounded startup diagnostics overlay** — config, session, and keybinding
     load failures were drawn verbatim, so ESC/bidi in a path or parse reason
     could restyle the attention banner. Store and draw now run
     `bound_diagnostic_text` (keeps newlines, 1 KiB).

229. **Bounded drop-plan path problems** — import refusals interpolated
     `path.display()` into IO and plan errors, so a hostile dropped name could
     restyle later Files notices. Those messages now use
     `bound_sidebar_path_label` / `bound_sidebar_notice`.

230. **Bounded Find-bar error chrome** — scrollback search assigned engine
     errors into the overlay status verbatim. `set_error_message` and the Find
     bar now run `bound_query_error` so a quoted pattern cannot restyle it.

231. **Bounded workflow-form render feedback** — `render()` errors were copied
     into the argument overlay as danger chrome, quoting the template/values.
     `set_feedback` now uses `safe_inline_display` at 256 bytes.

232. **Bounded AI-suggestion card chrome** — insert refusals interpolated the
     prompt-boundary reason verbatim, and the provider badge copied
     `display_name()` raw. Feedback is now 256-byte `safe_inline_display`; the
     badge uses `bound_provider_label`.

233. **Bounded command-correction card chrome** — accept refusals and the
     destructive-risk line interpolated engine/PTY reasons as danger chrome.
     Store and draw now run `bound_correction_feedback` at 256 bytes.

234. **Files notices store through one setter** — leftover multiline
     `sidebar_notice = Some((…))` assignments still stored raw labels and
     navigation errors. They now call `set_sidebar_notice`, including follow
     destination labels.

235. **Bounded Agent transcript chrome** — user/thought/say/protocol lines,
     observation samples, streaming preview, and the destructive-risk label
     were drawn verbatim. They now run `bound_transcript_text` (keep newlines,
     1 KiB) including streamed proposed commands.

236. **Bounded Files-tree scan-error rows** — root and child Error/RefreshError
     rows interpolated `DirectoryError` as danger chrome. Those lines now run
     `bound_sidebar_notice`.

237. **Bounded tab labels including close-confirm chrome** — `bound_tab_title_draft`
     dropped Cc but not bidi, and close-confirm interpolated process names
     verbatim. Drafts and session/pane labels now strip bidi like OSC titles.

238. **Bounded workflow picker name chrome** — list names, tags, descriptions,
     and the argument-form header were drawn from disk files verbatim. Those
     strings now run `bound_workflow_feedback` at 256 bytes.

239. **Bounded history-picker cwd chrome** — recall rows interpolated the
     16 KiB-index cwd as iced text. `display_cwd` now escapes spoofing and
     truncates to 80 characters.

240. **Bounded pane-header cwd, git, and process chrome** — the split pane
     header interpolated `cwd_display()`, `format_strip`, and the foreground
     process verbatim. Those now use the tab-title envelope / toast bound.

241. **Bounded OSC desktop-notification chrome** — OSC 9/777 strings from the
     PTY were forwarded to `notify-send` verbatim. Title and body now share
     the toast envelope (controls/spoofing stripped, 256 bytes).

242. **Bounded status-bar segment chrome** — bottom-bar cwd/git/grid labels
     from `jterm_core::bottom_bar::compose` were drawn as iced text verbatim.
     Each segment now uses the toast envelope.

243. **Bounded Agent attached-context command chrome** — the sidebar card
     interpolated `last_manual_completed.cmd` (up to the history command
     budget) as iced text. Display now uses the diagnostic envelope.

244. **Bounded Files-tree entry names** — directory listings interpolated
     remote/local filenames as iced text. Names now share the sidebar
     envelope (without the empty-notice fallback).

245. **Bounded command-block menu preview** — the overlay truncated the
     command to 240 chars but left OSC/bidi and the zone cwd raw. Preview
     and cwd now reuse `display_command` / `display_cwd`.

246. **Bounded block-search hit previews** — picker rows clipped length but
     interpolated OSC/bidi from command/output lines. Overlay draw now runs
     `visible_bounded` (match spans stay on the stored original).

247. **Bounded long-block desktop-notification command** — OSC 133 completion
     toasts forwarded the raw command to `notify-send`. The title now uses
     `display_command`.

248. **Bounded workflow-arg form labels** — argument names/descriptions from
     disk workflows were interpolated as iced labels and placeholders.
     They now share the workflow-feedback envelope.

249. **Bounded AI chat turn chrome** — library transcript/partial replies were
     drawn as iced text with stored spoofing/control characters. Display now
     keeps newlines, replaces spoofing, and stays inside the live-message
     budget.

251. **Bounded Files-panel header leaf** — the Files title button used the
     current directory's raw `file_name()`. It now shares the sidebar
     filename envelope (breadcrumbs were already `safe_inline_display`).

252. **Bounded Agent-task provider chrome** — task list, action buttons, and
     toasts interpolated `provider.display_name()` verbatim. Those now share
     the provider-label envelope.

253. **Bounded shortcut-label chrome** — help overlay grouped chords and the
     palette reverse-lookup join were drawn as iced text. Labels now share a
     256-byte `safe_inline_display` envelope.

254. **Bounded block-search context and block-menu meta chrome** — the
     secondary hit line and the command-block menu status/cwd row concatenated
     extras after sanitizing the command. The whole line now runs
     `display_block_search_text`.

255. **Bounded native-session snapshot chrome** — phase Debug, command
     status, and approval-kind Debug were interpolated as iced text. Those now
     use the toast / visible_bounded envelopes.

256. **Bounded settings theme-picker names** — custom theme filenames from
     disk were listed in the iced pick_list verbatim. Names now run
     `bound_custom_theme_name` before display.

257. **Bounded command-correction card title chrome** — title, badge, and
     description were drawn from the engine as iced text. Those now share the
     correction-feedback envelope at draw.

258. **Ask-AI request strips visual spoofing** — overlay typing dropped
     controls but kept bidi/invisible characters in the NL request (and the
     review card interpolated `compact_one_line` verbatim). Request ingest
     now replaces spoofing; the card line uses the toast envelope.

259. **Ask-AI draft strips visual spoofing** — the reviewable command draft
     dropped controls but kept bidi in model replies and user edits. Draft
     ingest now replaces spoofing with U+FFFD.

260. **Bounded Agent-task git-diff header** — the diff card interpolated
     `requested_base()` as iced text. The header now uses the toast envelope
     (`display_requested_base`).

261. **Correction-card draft strips visual spoofing** — overlay typing
     dropped controls but kept bidi in the editable correction command. Draft
     ingest now replaces spoofing with U+FFFD.

262. **Find-bar query strips visual spoofing** — the Find overlay dropped
     controls but kept bidi in the regex/literal query (and history restore).
     Query ingest now replaces spoofing with U+FFFD.

263. **Find & Replace fields strip visual spoofing** — search and replace
     inputs dropped controls but kept bidi. Field ingest now replaces
     spoofing with U+FFFD.

264. **Bounded Find & Replace status chrome** — engine errors were stored
     and drawn as iced text. Status now uses the query-error envelope at
     ingest and draw.

265. **Command-palette query strips visual spoofing** — overlay typing
     dropped controls but kept bidi in the fuzzy query. Query ingest now
     replaces spoofing with U+FFFD.

266. **History-picker query strips visual spoofing** — recall overlay typing
     dropped controls but kept bidi in the filter query. Query ingest now
     replaces spoofing with U+FFFD.

267. **Workflow overlay typing strips visual spoofing** — picker queries and
     argument fields dropped controls but kept bidi. Ingest now replaces
     spoofing with U+FFFD before the core picker/form.

268. **Files-tree filter strips visual spoofing** — the inline name filter
     dropped controls but kept bidi. Filter ingest now replaces spoofing
     with U+FFFD.

269. **AI chat library search strips visual spoofing** — the library filter
     dropped controls but kept bidi. Search ingest now replaces spoofing
     with U+FFFD.

270. **Tab-switcher query strips visual spoofing** — the jump-to-tab overlay
     dropped controls but kept bidi in the filter. Query ingest now replaces
     spoofing with U+FFFD.

271. **API-key draft strips visual spoofing** — the settings key field dropped
     controls but kept bidi. Draft ingest now replaces spoofing with U+FFFD.

272. **AI chat composer strips visual spoofing** — the live chat draft kept
     newlines but also kept bidi. Composer ingest now replaces spoofing with
     U+FFFD.

273. **Agent composer strips visual spoofing** — the session composer kept
     newlines but also kept bidi. Composer ingest now replaces spoofing with
     U+FFFD.

274. **Task follow-up composer strips visual spoofing** — the Tasks follow-up
     field kept newlines but also kept bidi. Follow-up ingest now replaces
     spoofing with U+FFFD.

275. **New-file name drafts strip visual spoofing** — New File / Rename dialogs
     dropped slashes and controls but kept bidi. Name ingest now replaces
     spoofing with U+FFFD.

276. **Agent edit-command drafts replace visual spoofing** — the proposal
     editor dropped bidi silently. Edit ingest now replaces spoofing with
     U+FFFD so the reviewed command stays visible.

277. **Settings text fields replace visual spoofing** — live config ingest
     dropped bidi silently. Settings drafts now replace spoofing with U+FFFD.

278. **Custom theme names replace visual spoofing** — the theme-editor name
     dropped bidi silently. Name ingest now replaces spoofing with U+FFFD.

279. **Files path bar replaces remaining visual spoofing** — the path field
     dropped bidi marks but kept zero-width spoofing. Ingest now replaces all
     visual spoofing with U+FFFD, and navigation rejects replacement text.

280. **OSC window titles replace remaining visual spoofing** — PTY titles
     dropped bidi marks but kept zero-width spoofing. Title ingest now
     replaces all visual spoofing with U+FFFD.

281. **Tab-rename drafts replace remaining visual spoofing** — the rename
     field stripped bidi marks but kept zero-width spoofing. Draft ingest
     now replaces all visual spoofing with U+FFFD.

282. **Restored tab titles replace remaining visual spoofing** — snapshot
     loads stripped bidi marks but kept zero-width spoofing. Restore now
     replaces all visual spoofing with U+FFFD.

283. **Agent git-diff display replaces controls with U+FFFD** — review diffs
     mapped ESC/bidi to ASCII `?`, which could hide the neutralization. Diff
     display now uses U+FFFD and re-truncates after the wider replacement.

284. **OSC 8 hyperlinks reject visual spoofing** — interned link URIs and ids
     were size-checked but still admitted bidi and zero-width marks. Spoofed
     OSC 8 fields now close the current link instead of being interned.

285. **OSC 7 cwd reports reject visual spoofing** — decoded pane cwd rejected
     only NUL. Newlines, bidi, and zero-width marks now refuse the report and
     keep the last accepted directory.

286. **Foreground process names are bounded for pane chrome** — `/proc` comm
     strings reached the pane header raw. Names are now display-bounded and
     visual spoofing is replaced before the ▶ chip is drawn.

287. **Remote archive members reject visual spoofing** — download containment
     refused controls but still admitted bidi and zero-width names. Spoofed
     members now fail the archive instead of extracting.

288. **Remote home probe rejects remaining visual spoofing** — login-home
     ingest dropped bidi marks but kept zero-width spoofing. The probe now
     refuses any visually spoofed absolute path.

289. **API key files reject visual spoofing** — credential IO dropped controls
     but still stored bidi and zero-width marks. Read and write now refuse
     spoofed keys instead of persisting them.

290. **Kitty associated-text drops visual spoofing** — CSI-u reports stripped
     control bytes but forwarded bidi/zero-width as associated text. Spoofing
     is now dropped before those fields are encoded.

291. **xterm modifyOtherKeys skips visual spoofing in committed text** — the
     report used the first non-control scalar, including bidi. Spoofing is
     skipped so the real layout character remains the key codepoint.

292. **Files listings skip control and spoofed names** — remote/local directory
     rows kept newline and bidi filenames. Those names are now omitted before
     they can become clickable tree entries.

293. **File-path links refuse visual spoofing before open** — a click resolved
     any existing path, including bidi filenames. Spoofed file links now fail
     closed instead of launching the opener.

294. **File-path detection skips visual spoofing** — highlighted paths still
     became clickable when they contained bidi or zero-width marks. Spoofed
     paths are no longer detected as file links.

295. **Block-search queries strip visual spoofing** — the overlay query was
     length-bounded but kept bidi. Query ingest now drops controls and
     replaces spoofing with U+FFFD.

296. **Kitty graphics errors strip visual spoofing** — protocol error replies
     dropped controls but kept bidi. Spoofing is now replaced with U+FFFD
     before the APC response is written to the PTY.

297. **Restored session cwds reject visual spoofing** — snapshot working
     directories were only checked for size and NUL. Bidi and zero-width
     marks now discard the cwd before a pane is spawned there.

298. **New-file names refuse visual spoofing** — dialog ingest replaced bidi
     with U+FFFD, but create/rename still accepted the neutralized string.
     Validation now refuses spoofing and replacement characters.

299. **Custom theme names refuse U+FFFD** — editor ingest replaced bidi with
     replacement characters, but save still accepted them as filenames.
     Persist validation now refuses U+FFFD.
