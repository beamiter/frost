# Compact native workspace

Frost's native workspace uses a compact local glass profile. The terminal font,
user padding, native PTY, clipboard, selection and IME ownership remain under
the existing terminal controls. The shell returns pixels to that native surface
by reducing chrome rather than scaling the terminal.

At the default 14px UI font, the top bar is 40px, rail 44px, navigation 216px,
inspector 288px, center header 36px and tools row 42px. Outer insets are 4px;
panel gaps are 6px. Terminal side insets are 8px and its top/footer gaps are 6px.
The tools row has 32px controls, the header has 28px icon hit areas, and rail
buttons have 32px hit areas with 18px glyphs. Major surfaces retain a subtle
1px edge with 8px corners; controls use 6px corners. The canvas glow and nested
terminal/tool frames are removed. The header contains one title and the existing
session, search and palette actions. Its complete title remains in a tooltip.
Navigation uses its actual recent-tab height rather than reserving five rows.
The footer contains real New session, Terminal, Tools and native Send/Stop
actions; unavailable attachment and model placeholders no longer take space.

Both sidebars keep the existing requested widths, resizing grips, collapse
controls and narrow-window inspector drawer. The initial visibility preference
is unchanged. Explicit `sidebar_width` and `inspector_width` values retain their
existing bounds. Narrow windows no longer enlarge the rail beyond its compact
44px default.

| Logical window | Previous direct native pixels | Compact direct native pixels |
| --- | --- | --- |
| 1612 × 976 | 859 × 648 | 1028 × 836 |
| 1356 × 704 | 603 × 430 | 772 × 564 |
| 800 × 640 | 456 × 366 | 510 × 500 |

These are native viewport rectangles before the terminal's own scrollbar,
cell gutter and configured padding. The same rectangle feeds the PTY grid and
native pane. Chat reserves the actual bounded input height above the 42px tools
row, with one 6px history/input gap. Direct Block input and running/native
passthrough keep the existing terminal owner; a second editor is not created.
Before reserving input height, the shared Block/Chat dock checks that the
resulting PTY still has enough rows for the prompt projection. Oversized prompts
stay in the native terminal until they fit, so resize feedback cannot repeatedly
create and remove the input strip.
The dock measures at least the projection's two-row composer minimum. The grid
end can cut that padding short: a one-row prompt (bash over SSH) on the last PTY
row would otherwise measure one row, and the same prompt one row higher two rows.
Docking moves the prompt between those positions, so the input would keep
switching heights.
When a command takes the prompt, the input strip keeps its height, empty, for
up to 300ms (`INPUT_DOCK_HOLD`). A quick command's next prompt docks into the
same strip, so one Enter neither flashes the input away nor resizes the PTY
twice. A longer command gets the full pane when the hold expires; fullscreen,
mouse and image applications get it at once. The terminal keeps every key,
pointer and IME event during the hold.

Larger configured UI text grows the top bar, header and tools row through
`Layout::with_chrome_font_size`. The default compact geometry remains unchanged;
terminal content zoom never changes these chrome rows. The layout and PTY use
the same enlarged geometry. Inspector tabs use named icon tooltips when enlarged
UI text no longer fits their labels. Narrow top bars retain a mode toggle in
place of the two mode buttons.

Navigation section labels use their measured text height. Tab, destination and
Space labels stay on their allocated lines, with complete text in tooltips.
The duplicate informational session footer is omitted above a 24px UI font so
large status text cannot cover navigation or the terminal; session actions stay
available in their existing controls.

Iced may relayout a retained view at new window constraints before delivering the
window resize to the application. The native input Surface partitions its actual
allocated bounds with the same clips used for paint, pointer mapping and IME.
It lays out the retained terminal and dock from that partition, then the next
model view recomputes the shared PTY budget. The allocation assertion checks
that history, gap and input account for the full available height, including
short windows whose previous draft is larger than the new allocation.

The layout unit tests cover shared PTY/input budgets, panel collapse and width
restoration, short windows, bounded multiline input, and enlarged UI text.
After a release build, `python3 scripts/test-input-geometry.py` checks resize
convergence using the real terminal parser, projection and input budget,
including a one-row bash prompt pushed to the last PTY row by command output,
and a quick command held from Enter to its next prompt. It also verifies that
removing either guard brings back its oscillation and removing the hold brings
back the per-command collapse, without using SSH.
Native window, PTY and physical input checks remain separate acceptance gates;
the headless geometry tests do not substitute for those checks.
