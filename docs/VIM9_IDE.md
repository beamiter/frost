# Frost native Vim9 IDE

Frost provides a native Iced workbench and buffer text surface around **a real
Vim 9 process**. Vim owns text, modes, undo, registers,
macros, buffers, splits, completion and plugins. The file/buffer/build/problem
controls are native widgets; they send typed operations over the shared
`jterm_core::vim9` authenticated channel. The buffer uses the same channel to
publish visible text and GUI styles. Frost's `NativeBuffer` widget draws those
runs directly with Iced text shaping and RGB decorations. No second editable
document model is introduced.

## Buffer-first defaults

Running `vim` in a Frost local interactive shell, or entering `frost --ide`,
shows the GUI buffer with Vim's own status/command rows.
The extra workbench toolbar, buffer strip, status bar, project sidebar and
build/problem dock are all off by default. They do not reserve empty space or
construct their hidden widget trees. Directory scanning starts when the project
sidebar is explicitly opened; Quick Open starts its index on demand.

These keys live in `~/.config/frost/config.toml` and use the shared four-terminal
configuration contract:

```toml
[vim9]
auto_gui = true
native_buffer = true
smooth_scroll = true
toolbar = false
buffer_tabs = false
status_bar = false
project_panel = false
tool_panel = false
```

Set individual controls to `true` to show them at editor startup. The native
buffer renderer is independent: hiding every control still keeps GUI text and
RGB styling enabled. Setting only `native_buffer = false` selects compatible
terminal painting without enabling the controls. Existing configuration files
without `[vim9]` receive these defaults.

`Ctrl+Shift+E` toggles the project sidebar and `Ctrl+Shift+J` toggles the tool
dock. Both have a close button when visible. Explicit Search/Build actions may
reveal their result dock. Quick Open, Go to and other dialogs remain available
on demand and Escape returns to Vim. Save, Vim mappings and editor font zoom
continue to work without the toolbar. Errors appear in a dismissible transient
overlay if the added status bar is hidden.

## GUI buffer rendering

Wheel and trackpad events target the Vim split under the pointer without
changing the active window. Discrete wheel notches request three lines;
trackpad pixels move the displayed text immediately using the measured line
height. The shared `Vim9PixelScroll` keeps one source-bound request in flight
per split, rebases the remaining fraction from Vim's explicit acknowledgement,
and clears accumulated movement at file edges. No second momentum animation is
added to the operating system's event stream. Requests carry buffer generation
and view identity, so a queued wheel cannot scroll a replacement buffer. The
bridge frame and request completion are observed together to avoid retiring a
valid gesture between its acknowledgement and response.

`Vim9Viewport` projects visible text, up to two supplied margin rows, the caret,
source hits and the IME candidate rectangle through the same pixel transform.
Missing margin coverage bounds the visible movement rather than inventing
offscreen text. Separators, status rows and peer splits stay fixed. The mouse
uses an immutable snapshot of the frame and offsets actually drawn, including
when a newer acknowledgement has arrived but has not yet been painted.
Keyboard input, preedit, source/layout changes, zoom, focus loss and compatible
rendering retire pending gestures. `smooth_scroll = false` uses the existing
Vim wheel-reporting path.

A plain primary click during a presented fractional scroll captures source
UTF-8 bytes; its character drag and release stay in the same buffer generation.
Motion is coalesced while a pointer request is pending. Stationary clicks,
modifiers, custom mappings, multi-clicks and unsupported modes retain the
compatibility path while the image is stationary. An unsupported press on a
fractionally moved image settles the view and consumes that press and its
release; the next press on the stationary image uses the original Vim path.
This also covers native rendering falling back before its next image is drawn.
A cancelled native press consumes its remaining drag and release instead of
emitting an unpaired terminal gesture. Native fractional
drag currently stops at the last valid position when leaving its split; edge
autoscroll and cross-split selection are not implemented. A multi-click that
begins during a fractional native gesture does not promise Vim's complete
word/line multi-click behavior; stationary multi-clicks remain Vim-owned.

`guifg`, `guibg` and `guisp` are preserved as exact RGB values independently of
`ctermfg`, ANSI palettes and `termguicolors`. Bold and italic use native font
attributes; underline, undercurl, strikethrough, the cursor and Visual selection
are GUI decoration layers. The established input adapter continues to handle
Vim mouse reports and native IME; its terminal painter is clipped outside the
GUI buffer rectangles so it cannot constrain buffer colors or glyphs. The
compatibility bands are disjoint even for mixed horizontal and vertical splits;
separators and status rows remain visible and keep their Vim input behavior.

The optional GUI status bar shows **GUI buffer · RGB** while a supported frame is present.
Vim's command/status rows remain in the compatible surface. Horizontal,
vertical and mixed internal Vim windows use native text in every supported
window, with a cursor and IME anchor only in the active one. Folds, diff,
popup completion, conceal and unsupported shaping temporarily use
**Compatible view**, with the runtime's reason shown. Resize,
disconnection and a terminal-local Shift selection also use the compatible
path until a current supported frame is available. Old native frames are never
kept over a known unsupported state.

Vim's logical display columns are retained for correct input positioning.
Proportional text layout, arbitrary per-line heights and native completion
popups are future work. Ctrl+wheel changes the IDE's font size without changing
other terminal processes, native form text sizes or saved configuration. If
the configured font family is absent, the IDE resolves an installed monospace
family before measuring and painting it. This keeps regular, bold and italic
text on the same Vim columns instead of squeezing a proportional substitute
into a guessed cell width.

Every native view, glyph background and cursor uses the shared
`Vim9Viewport` logical-pixel projection built from `Vim9RenderMetrics`.
Fractional padding, line heights and font zoom stay intact. Projected runs are
prepared once for each window and painted in background/text order. Replace
mode has a horizontal cursor; Insert mode retains the vertical cursor.

The shared core exposes semantic anchors with frame revision, Vim window and
buffer IDs, changedtick, source line/UTF-8 byte range and screen rectangle. The
native renderer uses the same transform as these anchors. This is the layout
foundation for later floating inspectors, Markdown previews, images and color
pickers; those complete features are not introduced by this change. Vim still
owns text and undo, and stale buffer anchors must be resolved again after edits
or viewport changes. Native IME continues to use Iced's existing over-the-spot
overlay at the active GUI cursor.

## Launch

```sh
vim +42 -- "file with spaces.rs" another.rs
frost --ide ~/projects/jwm
frost --ide ~/projects/jwm/src/main.rs
frost --ide
```

Frost prepends a private `vim` shim only to local interactive shell child
environments. The shell itself performs argument expansion; the shim forwards
the exact resulting argv and working directory to a dedicated native editor
process and waits for its exit. `:cq 7` returns status 7 to that shell, and
commands after `vim` resume only after Vim exits. It does not modify shell
startup files or the parent process's PATH. Batch/Ex mode, piped input/output
and remote shells remain ordinary terminal commands. Explicit absolute Vim
paths and aliases/functions that bypass PATH also retain their shell meaning.
Set `[vim9] auto_gui = false` before creating a shell to disable this behavior,
or use `JTERM_VIM9_AUTO_GUI=0 vim file` for one invocation.

Auto-entry opens the existing dedicated Frost GUI editor window. It does not
replace the editor in the terminal's existing pane. The original shell remains
available after the editor closes.

The target is optional and defaults to the current directory. A file target
uses the shared core's project-marker discovery. A new filename is supported
when its parent directory exists; it is not written until you save it. Paths
must be UTF-8. Use `frost --ide -- -filename` for a name beginning with `-`.
`--ide` must be the first option, and accepts one target, not a shell command
or terminal-mode override.

The normal terminal toolbar also has a **Vim9 IDE** button. It opens the active
local pane's directory in a separate Frost workbench process. Managed or
observed SSH panes are not treated as local project roots. Closing the terminal
that launched an IDE does not close that independent editor. Launching plain
`frost` keeps the existing terminal behavior.

Install Vim 9 with `+vim9script`, `+eval`, `+channel`, `+job`, `+timers` and
`+quickfix`. The normal Vim configuration and installed plugins load before the
bundled bridge runtime. A different executable can be selected explicitly:

```sh
JTERM_VIM9_BIN=/usr/local/bin/vim frost --ide ~/project
```

The workbench does not install plugins or language servers, overwrite `.vimrc`,
run project commands at startup, or restore terminal sessions into the editor.
The runtime is embedded in the pinned shared core and materialized in its
managed cache. A missing executable or failed bridge is shown in the window.

## Workbench

**Quick open…** (`Ctrl+Shift+P`) searches the whole project by filename. A
background ripgrep index respects ignore files, includes useful hidden files,
and skips Git internals without following directory symlinks. Results use
case-insensitive subsequence matching with a filename preference: `dpndl` can
find `src/deep needle.rs`. Type to filter, use Up/Down or Page Up/Page Down to
select, and Enter to open. Escape cancels. Matching runs only when the query or
index changes; the window does not rescan the filesystem while typing. The
index retains at most 20,000 files and the picker shows the best 80 matches,
with the current selection kept visible. Refresh rebuilds the index; indexing,
errors and truncation are shown in the picker. Use Open… for a new filename or
an explicit external path.

**Go to…** (`Ctrl+Shift+G`) accepts a positive `line` or `line:column`. It moves
within the current buffer, including unnamed buffers, and uses Vim's byte
column convention. Invalid coordinates keep the form open with an explanation.
The typed request does not interpolate user text into an Ex command.

**Back / Forward** (the header arrows, `Alt+Left` / `Alt+Right`) follow Vim's
actual jump list. Native file opens and line jumps participate in that list,
so a definition, Quick Open result or diagnostic can be followed and then
returned from. The arrows reflect whether Vim has a previous/next location.
Ordinary `Ctrl+O` and `Ctrl+I` still reach Vim; no separate GUI cursor history
can diverge from Vim's own navigation. Modified-buffer checks remain active.

The project sidebar lists one directory at a time, directories first, with Up
and Refresh controls. Reads run on a worker with a single in-flight scan and
one latest queued request. The list is capped at 1,024 entries. Directory
navigation, including resolved symlinks, stays inside the project root. Open
accepts an explicit local path outside that sidebar as well. This release uses
a native path-entry form in addition to the indexed Quick Open picker.

The buffer strip reflects real Vim buffer IDs, the selected buffer and modified
markers. Hover a tab to see its full path. Each tab has a close control that
requests a non-forced buffer deletion; modified buffers keep Vim's refusal and
remain recoverable. Ctrl+Shift+Page Up/Page Down switches buffers, and
Ctrl+Shift+W closes the current one, resolved inside Vim after any preceding
navigation. Save and Save All retain Vim's normal write hooks and error handling.
The status line reports when a save is waiting for Vim to finish input.
Navigation does not silently discard unsaved changes; with `hidden` disabled,
Vim can refuse to leave a dirty buffer. Modes, filenames, cursor line/byte
column and line count, filetype, encoding, split count, pending requests and
connection state appear in the status line. Vertical split and Horizontal split
open real Vim splits; regular Vim window commands continue to navigate them.

Search uses ripgrep with native controls for **Smart / Match / Ignore case**,
**Regular expression**, **Whole word**, and an optional **single file glob**.
The defaults preserve literal smart-case search: a lowercase query ignores
case, while uppercase text makes matching case-sensitive. `*.rs` narrows the
search to Rust files; an empty glob searches all eligible files. Spaces belong
to the glob and are never split into shell arguments. Options persist for the
current editor process, and are sent only when Apply or Enter starts a search.
Ignore rules still apply. Install `rg` for this action. Invalid expressions and
search failures remain visible. **Stop search** is available in the form and
the Problems view while a search runs; cancellation is shown explicitly.

Search results and build diagnostics use Vim quickfix and the native Problems
view. Clicking a diagnostic opens its real filename and location. The panel
separately labels the current quickfix title, source and active entry, and the
last project search count and executed options, so a build that replaces
quickfix cannot appear to be the previous search. Changing search controls
does not relabel the options used for existing results. Other Vim diagnostic
providers can contribute their own entries. Search retains up to 256 matching
lines and reports omissions.
Buffer and diagnostic snapshots are also bounded to 256 items; truncation is
indicated rather than presented as a complete list.

Build opens a command-entry form for explicit review. An empty value uses Vim's
`makeprg`; Cargo projects initially suggest `cargo check --message-format=short`.
Apply starts an asynchronous job in the project root. **Build does not save
files implicitly.** A separate **Save + build…** action explicitly saves all
buffers before running its reviewed command; a failed write prevents that build
from starting. Stop Build is available while a build is running, and cancelled
builds have their own status. Vim's `errorformat` parses compiler output; use
`:compiler` or your vimrc for a different toolchain.
The native output view shows the shared runtime's bounded 80-line tail and
reported exit status and the number of omitted output lines. Search and build
share the current quickfix result list, so a later operation can replace the
previous list.

Files and Problems/output can be toggled on demand. Once enabled, large windows
show the project sidebar and bottom dock. The sidebar is hidden below 820
logical pixels and the dock below 580 pixels; the Open form and Vim commands
remain available. Optional toolbar and buffer strips scroll horizontally
instead of forcing the editor wider than its window. Ctrl+wheel changes the
editor font size for this process without rewriting configuration.

## Keyboard and mouse

| Shortcut | Native action |
| --- | --- |
| Ctrl+Shift+P | Quick Open project filename picker |
| Ctrl+Shift+G | Go to line or line:column |
| Ctrl+Shift+E | Toggle project sidebar |
| Ctrl+Shift+J | Toggle problems/build-output dock |
| Alt+Left / Alt+Right | Back / forward through Vim's jump list |
| Ctrl+Shift+O | Open a file/new-file path form |
| Ctrl+S | Save current buffer |
| Ctrl+Shift+F | Search project form |
| Ctrl+Shift+B | Review build/test command |
| Ctrl+Shift+S | Save all buffers |
| Ctrl+Shift+Page Up / Page Down | Previous / next buffer |
| Ctrl+Shift+W | Close current buffer, retaining unsaved-change checks |
| F8 / Shift+F8 | Next / previous diagnostic |
| Ctrl+Shift+C | Copy a terminal selection explicitly |
| Ctrl+Shift+V | Paste explicitly into Vim |

Save-current is also available in the toolbar or through Vim's `:write`.
Ordinary Vim chords, including Ctrl+P, Ctrl+O and Ctrl+W, remain available through
the editor. A focused native form owns its typing and Enter; held
Enter ownership is tracked physically so submission does not become input in
the editor below it. IME preedit/commit uses Frost's native ownership adapter.

Vim application mouse reporting is retained. Shift bypasses application mouse
reporting for terminal selection; double/triple clicks use Frost's word/line
selection. Clipboard reads and writes requested by PTY OSC output are denied
inside this IDE process; native copy/paste and primary-selection paste are
explicit actions. Normal terminal clipboard settings are unchanged. When Vim
has bracketed paste disabled, multiline prompt insertion is restricted to its
first line with a visible notice.

## Optional language services

Definition, Hover, References, Rename and Format are enabled only when the
shared runtime advertises a supported provider. The current adapter targets
Yegappan's Vim9 LSP command vocabulary (`LspGotoDefinition`, `LspHover`,
`LspShowReferences`, `LspRename`, `LspFormat`); native tags/tagfunc also support
definition. Language servers are configured in the user's Vim environment.
Registered commands do not guarantee every server supports every action, and
normal Vim errors remain visible. Other installed plugins continue to work in
Vim; this release does not add a native debugger, Git client or automatic AI
editing workflow.

## Closing and isolation

A native window close sends **Vim's non-forced Quit** and waits for an actual
PTY child-exit event. Unsaved buffers can keep the window open. Save and close
again, or use `:qa!` in Vim only when explicitly intending to discard changes.
There is no hidden forced-quit fallback. A disconnected bridge is not evidence
that Vim has exited: the editor remains accessible for `:write` and `:qa`.
A nonzero Vim exit from an explicit `--ide`/toolbar launch retains its last
screen and error until you close the window. A shell-origin launch closes on
every real child exit and returns that exact status to the waiting shell.

The IDE early launch path does not enter `Frost::new`, acquire the terminal
instance lock, load terminal workspace snapshots or call their persistence
writer. Editor argv and bridge credentials are not saved as restorable shell
sessions. Bridge environment entries are passed only to the dedicated child;
the runtime removes its bootstrap token before it starts build jobs.

## Reproduce the checks

The focused GUI-buffer test compares actual native-window pixels against an
exact GUI RGB color while `termguicolors` is disabled and `ctermfg` deliberately
uses a different color. It checks real editing/Ctrl+S/undo, RGB pixels in
every vertical/mixed split, active-window changes and editing, window resize,
and font zoom. It verifies the explicit fallback reason when a real Vim
horizontal scroll clips a CJK glyph, recovery after Vim returns to column zero,
and removal/restoration when an unsupported window-local display option is
toggled:

```sh
xvfb-run -a -s '-screen 0 1600x1000x24' python3 scripts/native-vim9-buffer-smoke.py \
  --binary target/debug/frost --app frost --output /tmp/frost-native-buffer
```

Requires xdotool, ImageMagick and Pillow. The output retains PNG evidence and
JSON results. X11/software-GPU acceptance does not certify Wayland, physical
GPU drivers or an interactive IME.

The actual shell round-trip test launches a normal Frost window and types
`vim +42 -- "first file.txt" "second file.txt"` at its Bash prompt. It verifies
exact argv/cwd, native RGB, scrolling the inactive split without moving the
active one, shell blocking, normal exit, a second GUI entry returning `:cq 7`,
and batch Vim staying in the terminal:

```sh
xvfb-run -a -s '-screen 0 1600x1000x24' python3 scripts/test-shell-vim-gui.py \
  --binary target/debug/frost --output /tmp/frost-shell-vim
```

```sh
cargo fmt --all -- --check
cargo check --tests --locked
cargo test --bin frost --locked vim9_buffer::tests -- --test-threads=1
cargo test --bin frost --locked vim9_ide::tests -- --test-threads=1
cargo test --bin frost --locked vim9_ide::native_tests::real_vim9_native_pty -- --ignored --exact --test-threads=1
cargo build --locked --bin frost
WGPU_BACKEND=gl xvfb-run -a -s '-screen 0 1600x1000x24' python3 scripts/test-vim9-window.py target/debug/frost
```

The buffer tests include real Iced widget `update`/`draw` calls with a null
renderer: raw pixel-wheel dispatch, painted-frame capture, source-byte pointer
routing, terminal multi-click counting and the projected IME candidate
rectangle. State tests cover acknowledgement rebasing, one request per split,
coalesced pointer release, stale source/geometry cancellation, compatibility
fallback and a new acknowledgement arriving before repaint. These tests verify
event routing and geometry; they do not measure physical touchpad latency or
certify an OS input method.

The explicit PTY test requires Vim 9, ripgrep and loopback sockets. It drives
the actual Frost PTY/parser and shared Rust/Vim bridge: editing, dirty quit
refusal, Save/Save All, special filenames, compiler diagnostics, project search
and a real child exit. It leaves reaping to the production PTY owner.

The window test requires X11/Xvfb, xdotool, libX11 and libxkbcommon-x11. It
requires the WGPU renderer (without a tiny-skia fallback) and uses actual XTest
input through the Iced window and native forms/shortcuts. It verifies recursive
fuzzy Quick Open with ignored paths excluded, arrow selection, line:column
navigation and jump-list back/forward, immediate Escape-then-save, Ctrl+S, real
splits, buffer switching,
dirty buffer-close refusal and rapid navigation followed by current-buffer
close. Search checks click the native case/regex/whole-word/file-glob controls
and inspect actual ripgrep results in Vim's quickfix list. A deliberately slow
search fixture verifies the Stop search button terminates its job while keeping
the previous quickfix list; other searches use the installed ripgrep binary.
The test also verifies that Alt+Left in a native form does not move Vim's cursor.
It sends a real
`WM_DELETE_WINDOW` request, checks dirty-close refusal and graceful saved exit,
and verifies isolated terminal state/data files remain byte-for-byte unchanged.
These checks do not certify physical GPU performance, every Wayland compositor,
CJK IME behavior on a user's desktop, or all plugin configurations.


### Recovery validation — 2026-10-20

Pinned shared core: `c387ab33caff52a0bd4f759fb20a35aae2586cd0`.
Passed 11 focused tests and the real X11/WGPU window smoke: RGB in all three panes, Unicode, edit/save/undo, resize, font zoom, compatibility fallback/recovery and normal quit.
These runs use Xvfb/software graphics; physical GPU, Wayland and OS IME candidate windows are not certified.


## Native plugin highlights

Single-line `matchadd()` / `:match` patterns and byte-based `matchaddpos()`
positions now use native GUI RGB paint, including the built-in `matchparen`
plugin. Match priorities, equal-priority insertion order, search precedence,
Visual selection and independent split-window ownership follow Vim. Plugin
matches update and disappear without editing the buffer or emitting a special
invalidation hook. Syntax, properties, text, undo and input remain Vim-owned.

Multiline/context-dependent/zero-width regexes and conceal matches retain an
explicit compatibility reason. Native paint is bounded to 64 matches and 2048
positions per window and 512 match/search ranges per source line; exceeding a
budget shows the real Vim compatibility view instead of partial decoration.

CJK fallback now checks actual font outlines rather than accepting a Latin
family returned by a generic Fontconfig language query. If a CJK family has no
slanted face with glyph coverage, its upright face keeps the text readable;
GUI colors, bold weight, decorations and Vim's source/display advances remain.

### Plugin validation — 2026-10-21

Pinned shared core: `feae27f7cd965332253a81723095f75850241e81`.
The 19 native-buffer/config tests, the real CJK glyph-coverage regression
(normal/bold/italic combinations), and all-target/all-feature Clippy with
`-D warnings` pass. The Xvfb/software-WGPU buffer smoke passes, including
90 exact plugin-regex RGB pixels and 77 positional CJK RGB pixels with
`notermguicolors`, removal without a buffer edit, split edit/save/undo, resize,
zoom and compatibility recovery. Font coverage and screenshots were inspected.
Artifacts: `/tmp/frost-vim9-plugin-feae27f-v2` on the validation host.


## Continuous wheel scrolling — 2026-10-21

Pinned shared core: `2b00c1d3d92f4f5555969d6b62a410866d4acb34`.
Supported Normal-mode buffer views now turn each three-line wheel notch into
a 100 ms continuous input transition. New notches retain their own completion
deadlines; reversing cancels the old direction's unplayed tail. The GUI frame
clock advances motion even without new Vim render messages. Trackpad pixels
continue following OS input directly. Source-checked acknowledgements rebase
paint, hit testing and caret/IME together; a captured press freezes future
wheel animation. Editing, preedit, focus changes, zoom and changed layout
retain their cancellation rules. Turning smooth scrolling off keeps the
existing compatibility behavior.

Render protocol 4 prefetches eight real source rows on each edge (protocol 3
hosts retain two). This gives ordinary notches and short bursts text to expose
while Vim handles the request. File boundaries, unsupported views and missing
margin coverage remain bounded by the actual Vim source.

The shared runtime passes 93 real-Vim regressions. The core Rust Vim9 suite
passes 77 regular checks plus its explicit real-PTY round trip; deterministic
wheel tests cover 60/120/240 Hz timelines, exact distance, acknowledgement
continuity, reversal, file edges and pointer capture.

The vendored Winit 0.30.13 patch ignores legacy X11 wheel-button releases,
which otherwise turn one press/release pair into two notches. See the
[upstream package provenance and patch](../vendor/winit/JTERM_PATCH.md).
The locked build, all-target/all-feature Clippy with `-D warnings`, and full
Xvfb/software-GPU buffer smoke pass. The smoke injects an actual complete X11
wheel notch, observes Vim's topline change from 50 to 53, and captures
intermediate native GUI pixels before checking save/undo, splits, resize,
font zoom and compatibility recovery. PNG encoding is deferred until after
XCB pixel sampling to keep screenshot I/O out of the gesture.

Observed highlight Y positions: 362 → 322 → 295 → 290 pixels. These sparse samples verify actual intermediate positions, not a frame-rate guarantee. Artifacts: `/tmp/frost-vim9-wheel-2b00c1d-v4`.


## Vim-owned native cursor — 2026-10-21

Shared core: `2fa91124b7890acb97b2d628c57b84e92d9b6d38`. The native buffer
now follows Vim's `guicursor` shape, percentage, GUI RGB highlight and blink
timings. Normal/Visual, exclusive Visual, operator-pending, Insert and Replace
have independent settings. Ordered `a:` overlays and linked highlight groups
are preserved; the second highlight applies when language mappings are active
in Insert/Replace. An unspecified group reverses the actual syntax/selection
colors; an explicitly cleared group retains its configured appearance.

A solid block repaints the source glyph in the cursor foreground so Latin,
CJK and combining text remain readable. Thin bars change only cursor paint;
source hit testing and the IME anchor retain their full character rectangle.
Cursor-only updates retain the existing text-shaping cache. Unfocused cursors
use steady outlines. Blink timing follows `blinkwait`, then `blinkoff`/`blinkon`;
any zero value disables blinking. Input, source and mode changes restart the
wait, while unrelated render revisions do not.

For a steady block in Normal/Visual, a 25% insertion bar and a 20% replacement
underline, put this in the usual vimrc:

```vim
highlight Cursor guifg=#20242c guibg=#d8dee9
highlight InsertCursor guifg=#20242c guibg=#73a2e1
set guicursor=n-v:block-Cursor,i:ver25-InsertCursor,r:hor20-Cursor,a:blinkon0
```

Existing frames without `appearance` retain their compatibility cursor.
Command-line, popup and other unsupported views still use Vim's live terminal
surface. Native showmatch timing is not inferred from terminal output.

Validation: locked build, all-target/all-feature Clippy (`-D warnings`), focused
native-buffer tests and the full Xvfb/software-GPU smoke pass. The window test
measured block 12×24, CJK 24×24, insertion 3×24
and replacement 12×8 pixel rectangles, with 28 exact cursor-foreground pixels
under the solid block. XCB samples alternate visible/hidden cursor pixels.
Existing plugin RGB, wheel, edit/save/undo, split, resize, font-zoom and fallback
checks remain passing. Artifacts: `/tmp/frost-vim9-cursor-2fa9112-v2`.
The shared core passes 80 Rust Vim9 checks plus the explicit real-PTY round
trip and 99 real-Vim runtime regressions. These are X11/software-renderer
checks, not a physical-display or Wayland performance certification.
