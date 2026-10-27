# Native workspace shell

Frost binds `jterm_core::reference_shell` to Iced widgets. The contract was
measured from the supplied 1448 × 1086 JPEG by the supervising task; the
implementation task did not obtain original image pixels. Pixel-level visual
acceptance remains with that task.

At 1356 × 704 logical pixels the title bar is 42 px high, the rail is 55 px,
workspace navigation is 237 px, the center header is 49 px high, and the right
inspector is 306 px. The center receives surplus width. Below 1078 px the
inspector collapses and its toolbar/rail controls open a right-hand drawer.
Below 772 px workspace navigation collapses as well. Window controls use the
existing native window messages.

`src/native_shell.rs` owns the toolkit binding, rail, workspace/recent navigation,
center header, composer tools, and Agent/Context/Files inspector. Existing
`Frost::pane_view` and the pane tree remain the terminal/editor surface. The
same terminal dimensions are used for Iced layout and kernel PTY resizing.
Files and Tasks visibility gates follow their actual inspector/drawer rather
than the independent left navigation toggle. Private tab titles also suppress
new path and repository labels/tooltips in the shell.

Only Block and Chat are public presentations. Legacy Normal configuration is
readable and migrates to Block presentation without restarting its shell.
Shell, Vim, agent sessions, search, splits and Files are capabilities of the
same shell. Existing projection, cell provenance, reading anchors, fractional
scrolling, selection, copy, IME and press-time submit/interrupt tokens remain
the input authority.

Block keeps a dark native terminal inside the pale central surface. Its 80 px
white composer surface contains a keyboard hint and the shared tool row; it
does not create a second shell editor. Chat uses one retained TermWidget and
state tree, with its history and input drawn through separate clips. Only the
presentation layout of the dock is translated into the white composer; its
PTY grid, cursor, IME request and pointer mapping remain native. The former
dock paint region is excluded from history so the input cannot appear twice.
The raw terminal viewport reserves the complete 80 px composer from the first
layout, independently of Prompt, Running, passthrough or alternate-screen state.
At the reference size the Chat native body is 506 px high; the outer retained
Surface includes a 44 px input/status strip and the separate tools row is 36 px.
The model, pane layout and kernel PTY therefore receive the same native size.
The native child stays at one canonical origin in one retained Tree. A proven
prompt alone uses the existing clipped dock paint/IME transform. Multiline
input grows to a bounded area and follows its actual caret without resizing
the PTY. Prompt does not subtract the input strip a second time.

Running programs, primary/alternate-screen TUIs and split panes keep all native
output and stdin in that upper native viewport. The white strip is a non-editable
host hint, with a guarded Stop action when the existing foreground authority
permits it. It neither paints raw terminal cells nor creates an input owner.
Keyboard Enter, Ctrl-C, EOF, IME and mouse protocols still reach the real native
terminal. The scrollbar uses its actual native bounds, with no paint-only footer
subtraction. A missing lifecycle boundary never guesses whether raw data is
program output or echoed input.

The default Chat palette uses dark red/green ANSI colors for legibility on a
pale canvas. Explicit custom terminal palettes retain their configured colors.
The existing window-owned organism stays in the active native surface; the
new shell does not create a companion per pane or inspector.

The inspector reads actual local session, agent and saved-task state. Its
terminal preview is bounded to 12 displayed lines and 38 columns; it is a
read-only summary. It does not report fabricated progress or test success.
Attach, Image, Code and model selection are visible but disabled until an
authoritative implementation exists. Other terminal products in Spaces are
marked unavailable rather than launching a different installed application.
Structured diff Apply remains disabled without an authorized edit target.
`reference_conversation.rs` renders native proportional-font messages, avatars,
metadata pills, steps, diffs with Copy/disabled Apply and inline terminal cards
from the shared typed model. `JTERM_REFERENCE_FIXTURE=1` displays the common
explicitly labelled sample data and never submits its sample commands. The
production Chat projection uses actual command capsules and host-only Shell
role/avatar rows; output stays on the original TermWidget/cell path including
selection, search, images, reading anchors and fractional scroll. It does not
relabell stdout as Assistant prose or infer agent progress/steps/diffs from text.
Production structured assistant steps and diffs still require authoritative
runtime messages; their component rendering is fixture-validated only. Inspector
state maps real task/session data through the same typed component renderer.
The shared fixture chronology includes the date separator, icon metadata pills,
25 px task rows and a nine-line diff. Headline uses weight 700 at 20 px;
page/role titles use the shared 12 px hierarchy, while body/user text stays
regular and left aligned. Inspector labels and values form five 19 px rows, followed by the current step, compact queue and bottom-anchored
140 px terminal preview. Current Step is 45 px high at the reviewed y298;
model and tools use the shared soft palette, a 120 px model slot and 28 px
controls at y657. Five Recent rows are 32 px, Spaces rows are 30 px and the
operational footer is a fixed 49 px area outside the scroller. The calibrated navigation anchors the selector at
y77, first destination at y117, Recent at y273 and Spaces at y477; rows use
26 px height with 3 px gaps. Production values map by semantic field name.
All core rail/navigation/composer icons use the shared native vector paths.
The common five-item rail order is Workspace, Files & Code, Shell, Agent Tasks,
Search, with Settings and the local user initial anchored below. The toolbar
shows the implemented Ctrl+Shift+P palette shortcut; Ctrl+K stays available to
the shell readline binding.

## Verification

`scripts/qa-reference-shell.py` launches the actual binary in a private Xvfb
desktop, uses public OSC 133 shell integration, runs the common PTY fixture,
and captures only the native window. Its report records mode, physical window
bounds, scale, DPR, binary SHA256, source tree SHA256, dirty diff SHA256 and
the optional binary build manifest. It tests exactly-once shell input, search
focus restoration, Files focus with hidden navigation, mode switching on the
same shell PID, four-line and over-cap wrapped native input, long output, wheel
events and 620/800/1600 px windows. It also captures the real Tools palette and
the existing unique organism in its safe idle Block surface. Native cell
selection/clipboard copy, structured diff Copy, foreground cancellation and
new split → original PTY keyboard-focus return are verified with exact text/PID
assertions. The private Openbox fixture has no global shortcut bindings because
its defaults otherwise own Ctrl+Alt+Left for desktop switching; the earlier
failing reports are retained alongside the corrected desktop run. Captures
are evidence for visual review; a screenshot alone is not an anchor assertion.

`scripts/native-ime-smoke.py --mode block|chat` retains the existing real
Sway input-method-v2 → text-input-v3 preedit/commit path and exact Unicode
assertion. Its headless runner uses Pixman; Sway's documented
`--unsupported-gpu` flag permits the software fixture on hosts whose installed
Nvidia driver otherwise causes an early compositor exit. It does not exercise
the host GPU or replace physical-keyboard IME acceptance. `--multiline` composes
after four real input lines, records the text-input cursor rectangle and checks
the exact bytes reaching the shell. `--foreground` runs real cat and verifies
the same preedit/commit path reaches foreground stdin, with its cursor remaining
in the native canvas.

`scripts/qa-display-scale.py` uses an actual private Sway output at 125% or
150%, keeps app `ui_scale = 1`, and crops only the 1356 × 704 logical native
window. Captures measure 1695 × 880 and 2034 × 1056 physical pixels. This host
Sway exposes integer Wayland surface buffer scale 2 rather than the newer
fractional-scale-v1 protocol; its compositor applies the requested fractional
output transform. Reports separate those facts from the older X11 application
zoom captures, which keep display DPR 1.

Release builds and native acceptance run from an isolated target directory.
The existing complete `cargo test --release --no-run` suite currently fails
to compile because its renderer fixtures use `()` with Iced 0.14 renderer
traits. The unchanged `4857993c` baseline reproduces the same 269 errors;
those assertions were not disabled or weakened. The standalone existing
`src/transcript_canvas.rs` adapter tests can run against the built core and
cover 20 reading/follow, fractional motion, reflow and geometry cases.


## Acceptance boundaries

The native shell and component geometry have actual window screenshots, and the
supervising task owns original-image comparison. Fonts are shared DejaVu Sans
and DejaVu Sans Mono because the JPEG font cannot be uniquely recovered. This
change does not assert pixel-perfect visual acceptance. Fixtures intentionally
fill the structured conversation and inspector with labelled sample state.
Production command capsules, Shell role headers, output, current session, task
queue and inspector preview use observed native state. Assistant prose, semantic
steps, metadata pills and structured diffs still need authoritative agent-message
binding before they appear as such in normal conversations. Diff Copy works;
Apply remains unavailable without that edit target. The fixture's Iced
scrollable is component evidence, while production keeps the retained transcript
canvas, native cell renderer, viewport culling, search and selection.

Native keyboard, Unicode paste, mode switching without replacing the shell,
Files focus after hiding navigation, long/wrapped input, preedit/commit, narrow
and wide layouts have been exercised. Hardware keyboard IME engine integration,
GPU frame pacing, exhaustive split/Vim flows and frame-by-frame flash analysis
remain outside these headless screenshots. No fallback tests were weakened.

The viewport probe also compares `stty size` before and after real `read` and
`cat`, records ioctl dimensions while an alternate-screen child is resized,
and asserts its actual Up-key bytes. `scripts/native-surface-retention.rs`
compiles against the production Surface and verifies that Prompt → Running →
TUI → Prompt creates exactly one native state and retains its identity. Timed
output-frame crops check that native canvas pixels change while the read-only
composer strip stays identical.

`src/native_viewport.rs` is the production allocator used by both `term_height`
and the retained Surface layout. Its executable tests verify the raw rectangle
ends at the composer, the complete reservation is 80 px, Prompt does not deduct
the 44 px strip twice, and real reading/recovery chrome is charged once. The
legacy companion layout test now executes that allocator instead of expecting
the removed statusbar-height strings; its toggle/apply-config and single-owner
assertions remain. `scripts/test-native-layout-contract.py` executes the migrated
production test body unchanged outside the unrelated blocked renderer fixtures,
resolving only its `include_str` file location. Full-suite execution is still
blocked and is not represented by these independent tests.

### Bounded P1 native details

The Tools button now opens the shared six-row dark menu. The first three rows dispatch the existing terminal-focus, command-palette and canvas-search actions; Attach/Image/Code remain visibly unavailable. Its persistent overlay stack retains the native terminal subtree at index zero. Opening the menu never calls `apply_config`, resizes the PTY or changes canvas motion. Keyboard navigation owns its presses/releases until dismissal, including the existing physical Enter latch; outside clicks are captured. Opt-in debug traces record the actual retained surface node and reading/follow facts for native acceptance checks.

The workspace selector preserves one-line identity and a bounded, tooltip-backed branch slot. It uses the actual cached repository branch only through the existing private-title gate, otherwise the neutral `Local` label. Product/Spaces PNG handles are cached with explicit 24/22 logical-pixel allocation. Diff headers show their actual language/counts and native outlined Copy/Apply surfaces; Copy copies the shared diff contents and Apply remains disabled without an authorized target.

The one existing companion owner paints the supervising task's reconstructed shared cubic contour. The already locked tiny-skia dependency rasterizes those native paths once at3× into a stable native image handle; it adds no event loop, owner, timer or input widget. Its motion, life/status/explanation and admission still come from the existing owner. The minimum native blank envelope is64px high; smaller real cards reject placement rather than extending or covering cells. An expanded speech bubble occupies only the same verified blank suffix and has a real dismiss action. Rich fixture messages do not receive another companion. This is a reviewable reconstructed contour, not the JPEG's recovered original SVG.

The labelled reference fixture explicitly supplies its shared `main · sample` branch metadata and the common branch vector. Production never inherits that fixture value. PNGs are decoded to RGBA once before entering Iced's native image cache, including quiet startup. The inspector's28px tabs are vertically centered in their49px strip. Companion dismissal is consumed before native canvas pause so it cannot change reading/follow intent.

While Tools is visible, the existing embedded Vim instance loses its keyboard/IME subscription through `focus_embedded`; the workspace handles menu navigation. Queued editor keyboard/IME messages are rejected at that same ownership boundary. Closing the menu restores the original instance. Native acceptance samples the real Vim process, buffer, cursor, changedtick, unsaved text and undo/redo through a read-only Vim timer; it does not inject application messages or synthesize a draft.

`qa-reference-shell.py --companion-output` keeps the frozen binary and exercises the actual avoidance boundary: quiet expanded body, continuous stdout through its former area and a full native viewport, foreground Ctrl+C acknowledgement, the same shell PID accepting input again, and safe reappearance only after a real shell `clear` creates default-cell space. It records72 native software frames across the transition plus actual PTY dimensions. This does not reserve a new companion lane, extend a command card or alter the application layout.
