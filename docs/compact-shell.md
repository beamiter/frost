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
Native window, PTY and physical input checks remain separate acceptance gates;
the headless geometry tests do not substitute for those checks.
