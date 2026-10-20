# Frost native Vim9 IDE

Frost provides a native Iced workbench around **a real Vim 9 process**, rendered
by Frost's existing GPU terminal grid. Vim owns text, modes, undo, registers,
macros, buffers, splits, completion and plugins. The file/buffer/build/problem
controls are native widgets; they send typed operations over the shared
`jterm_core::vim9` authenticated channel. This is not Neovim, Vim key emulation,
or a second editor document model. The center editor is Vim's terminal UI, not
a new implementation of Vim's own GUI drawing protocol.

## Launch

```sh
frost --ide ~/projects/jwm
frost --ide ~/projects/jwm/src/main.rs
frost --ide
```

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

Search uses the shared runtime's **literal ripgrep search** and honors its
ignore rules. Install `rg` for this action. Search results and build diagnostics
use Vim quickfix and the native Problems view. Clicking a diagnostic opens its
real filename and location. Search retains up to 256 matching lines and reports
omitted matches. Buffer and diagnostic snapshots are also bounded to 256 items;
truncation is indicated rather than presented as a complete list.

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

Files and Problems/output can be toggled. Large windows show the project
sidebar and bottom dock. The sidebar is hidden below 820 logical pixels and
the dock below 580 pixels in this initial layout; the Open form, Vim commands
and status line remain available. Toolbar and buffer strips scroll horizontally
instead of forcing the terminal wider than its window. Ctrl+wheel changes the
editor font size for this process without rewriting configuration.

## Keyboard and mouse

| Shortcut | Native action |
| --- | --- |
| Ctrl+Shift+P | Quick Open project filename picker |
| Ctrl+Shift+G | Go to line or line:column |
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
A nonzero Vim exit retains its last screen and error until you close the window.

The IDE early launch path does not enter `Frost::new`, acquire the terminal
instance lock, load terminal workspace snapshots or call their persistence
writer. Editor argv and bridge credentials are not saved as restorable shell
sessions. Bridge environment entries are passed only to the dedicated child;
the runtime removes its bootstrap token before it starts build jobs.

## Reproduce the checks

```sh
cargo fmt --all -- --check
cargo check --tests --locked
cargo test --bin frost --locked vim9_ide::tests -- --test-threads=1
cargo test --bin frost --locked vim9_ide::native_tests::real_vim9_native_pty -- --ignored --exact --test-threads=1
cargo build --locked --bin frost
WGPU_BACKEND=gl xvfb-run -a -s '-screen 0 1600x1000x24' python3 scripts/test-vim9-window.py target/debug/frost
```

The explicit PTY test requires Vim 9, ripgrep and loopback sockets. It drives
the actual Frost PTY/parser and shared Rust/Vim bridge: editing, dirty quit
refusal, Save/Save All, special filenames, compiler diagnostics, project search
and a real child exit. It leaves reaping to the production PTY owner.

The window test requires X11/Xvfb, xdotool, libX11 and libxkbcommon-x11. It
requires the WGPU renderer (without a tiny-skia fallback) and uses actual XTest
input through the Iced window and native forms/shortcuts. It verifies recursive
fuzzy Quick Open with ignored paths excluded, arrow selection, line:column
navigation, immediate Escape-then-save, Ctrl+S, real splits, buffer switching,
dirty buffer-close refusal and rapid navigation followed by current-buffer
close. It sends a real
`WM_DELETE_WINDOW` request, checks dirty-close refusal and graceful saved exit,
and verifies isolated terminal state/data files remain byte-for-byte unchanged.
These checks do not certify physical GPU performance, every Wayland compositor,
CJK IME behavior on a user's desktop, or all plugin configurations.
