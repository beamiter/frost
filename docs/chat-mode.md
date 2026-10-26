# Chat presentation

Select **Settings → Terminal → Chat**, or add:

```toml
terminal_mode = "chat"
```

The explicit `terminal_mode` accepts `normal`, `block`, and `chat`, and takes
precedence over the legacy `block_mode` switch. With no explicit mode, the
existing boolean and its default are unchanged. Saving Chat preserves `chat`.
The setting applies to all sessions in the window, as the existing presentation
setting does; it does not create simultaneous mixed Chat/Block panes.

Chat is a new presentation of the same running shell and PTY:

- Submitted short prompt/command rows become right-aligned capsules, including
  the command currently running.
- Results use left-aligned full-width cards. Wide output and long commands keep
  their native column budget; they are never wrapped to a chat-message width.
- At a proven shell prompt, the original terminal cells are docked at the bottom.
  History scrolls independently while the draft and cursor remain in the dock.
- The optional ASCII companion remains local and uses the existing state/motion
  controls. One window-owned body follows the active composer in safe blank space;
  no per-pane companion status rows are inserted and inactive splits draw no copy.
- The up-arrow sends the existing native Enter encoding once, after checking the
  current parser state, pane, projection, and actual foreground shell. Keyboard Enter,
  Tab, multiline editing, IME, paste, Ctrl-C and EOF still use their normal path.
- While a command runs, the existing title bar shows elapsed time and **Stop**.
  Stop sends one native Ctrl-C to the same still-running execution. It does not
  claim success or completion; the program can handle or ignore the interrupt.
  The controls hide in alternate-screen apps and very narrow windows; keyboard
  Ctrl-C remains available. Their fixed title-bar placement never resizes the PTY.
- Optional buttons make a single immediate, one-byte native write. They do not
  enter the delayed input FIFO, drain older input, or automatically retry. Busy
  input, an ownership change, or a refused write leaves the draft/control unaccepted.
  If the active keyboard protocol encodes Enter or Ctrl-C as multiple bytes, its
  optional button is disabled and shows a keyboard hint. Use the physical key;
  its native enhanced encoding and normal input FIFO are unchanged. No enhanced
  sequence is truncated or replaced by a legacy byte.
- Stop targets the displayed execution and actual foreground group at admission.
  A press fires at most once; releasing it over a replacement command does nothing.
  Shell-owned builtins such as `read` remain interruptible.

There is no second text editor, command-execution pipeline, or input transcript.
Password input is never copied into a command capsule. Existing command-history
settings retain their existing meaning; Chat adds no separate history or logs.

## Interactive programs and conservative fallback

A running command owns the real terminal and all its stdin. `sudo`, `read`, `cat`
and REPLs do not expose the Chat send button. Alternate-screen programs such as
`vim` or `top` receive the full native pane, and Chat returns on the next usable
shell prompt. Missing, partial or ambiguous lifecycle boundaries retain the
native terminal. Primary-screen programs enabling mouse reports or displaying
graphics also receive native geometry. A running program moving its cursor above
its output boundary falls back as well. Such a takeover stays native until the
next valid shell prompt, even if the program turns those modes off again.
Extremely tall drafts and shared-row prompt boundaries retain the native surface rather than hiding content.

Chat changes neither PTY row/column counts nor the bytes sent by normal input.
It applies a reversible display projection. Real cells retain stable raw origins
for selection, copy, search, links, images and application mouse reports; host
spacing and the submit footer have no terminal origin. The companion is paint-only. Source
indices for the independently scrolled transcript and fixed dock are stored
separately. Selection stays within the transcript or composer, so dragging
across their separator cannot silently copy intervening off-screen history,
including word/line drags. Initial word selection is row-local. Link wrapping
is broken at host rows and discontinuous history/dock seams. Synthetic horizontal
capsule padding cannot paste, select or become a terminal mouse gesture.
Wide, colored and styled cells are preserved.

## Verification

Native parser/projection tests cover configuration precedence and persistence,
right/left alignment, fixed-dock scroll and copy, synthetic-cell isolation,
multiline/completion extents, full-width output, pending submission, foreground
input, alternate-screen return, primary-screen mouse/graphics/cursor takeover,
same-batch command rewrites, missing integration and oversized-prompt fallback.
An opt-in real Bash PTY test exercises one persistent shell, native dimensions,
cwd/environment, Tab completion, multiline input, hidden `read -s` input and
Ctrl-C exit status. Run it explicitly with `cargo test chat_real_bash -- --ignored`.
That fixture tests shell/parser/projection behavior, not Session admission.
Separate production-helper tests cover zero/failed writes, FIFO/backpressure,
stale action/session/foreground, enhanced-key rejection, press/release ownership
and acceptance only after an actual one-byte write.
Actual desktop appearance, IME and pointer end-to-end testing require a GUI
session; passing these tests alone is not a claim of GUI verification.

## Completed card actions

Right-click a retained completed command or output cell to open its read-only
card menu: **Block details**, **Copy command** (when available), **Copy output**,
**Copy block**, and **Copy as Markdown**. Copies target that one card, regardless
of an older Block selection. Details show retained output and lifecycle metadata;
missing or evicted text is reported instead of borrowing another card's output.

Use Up/Down, Home/End, Enter, and Escape within the menu. Closing it returns to
the terminal without changing the live draft. Ordinary and Shift-drag selection
remain text selection. Composer cells, running output, and decorative spacing
do not open card menus. Chat does not expose collapse, recall, command review,
rerun, deletion, or agent dispatch here. The configurable **block:menu** shortcut
(default Shift+F10) remains Block-only.

## IME composition ownership

An in-progress terminal composition belongs to that terminal session. Switching
panes or tabs, closing its session, losing window focus, or opening an input-owning
panel cancels it. A delayed commit from the cancelled composition is discarded;
it is never replayed into a hidden pane or inserted into the newly focused prompt.
The already committed shell draft is unchanged. Confirming a candidate with Enter
while composition is active or awaiting cancellation consumes that press and its repeats through release;
a separate normal Enter remains terminal input. After cancellation, only the first
candidate-confirm press is claimed. If the platform emits no further IME events,
a fresh Enter after physical release can submit normally while stale-commit
rejection remains armed.

The window input-method API does not provide a pane ID or composition generation.
Frost therefore treats Opened, or Closed followed by a new nonempty Preedit, as a
new lifecycle. Otherwise it discards the cancelled cycle through its next Commit;
subsequent input, including commit-only input methods, works normally. If a platform
omits these boundaries after a focus change, the first attempted composition may
need to be entered again. Commit followed by an unrelated Enter without any active
preedit cannot be identified as candidate confirmation by this API. Native IME
behavior across Linux display backends still requires graphical acceptance tests.
