# Fixed-grid raw-endpoint selection with host chrome

This test-only R02 component adds a deterministic partial-selection corpus for
the live grid. Run the existing `scripts/test-projection-properties.py` pure
runner, or normal Cargo tests. Replay one case with
`FROST_LIVE_CHROME_CASE=<0..1023>`; other tests still run normally.

Seed `0x46524f53544c4956` generates 1,024 grids at widths
`[1, 2, 3, 4, 7, 13, 31, 80, 255, 256, 1024]`. Each has two to six physical
rows. Cells include valid wide-body/continuation pairs, precomposed accented
scalars, ordinary default blanks and styled/hyperlinked spaces. Width changes
select different fixture shapes; this is not resize/reflow coverage.

Sixteen raw endpoint pairs per grid, both normal/block modes and both drag
directions produce 65,536 exact-text checks. Expected text comes directly from
original grid cells and hard/soft row flags. Expected document coordinates
come from a separate original-row list with explicit host-row placeholders;
no production plan, mapping or materialized cells supplies expected answers.
Every physical raw cell's value and provenance is checked after insertion.
Host rows must own no raw cells and must not enter copied text.

All raw rows request one to three organism chrome rows. A preceding soft-wrapped
raw row makes the boundary ineligible. The full seed corpus exercises 2,563
accepted and 1,551 rejected boundary requests. There is no spare-tail trimming.

## Limits

Endpoints always refer to original raw grid cells, including wide continuation
cells. Host-only endpoints, history/grid transitions, collapse summaries,
history reflow, duplicate boundaries, other chrome kinds and tail trimming are
outside this corpus. Physical trailing grid blanks are owned cells; this is
different from padding introduced by history projection.

Only the pure copy core is called. Test anchors are UNTRACKED placeholders,
so this does not validate real selection/cache admission, stale revisions,
selection-owner lifetime, parser input, or application mode routing. The oracle
does not assert materialized host-cell contents or every host-row metadata
field. No rendering, accessibility, PTY, clipboard/provider, DBus or socket
test runs here. No new production defect or behavior fix is claimed, and whole
R02 acceptance remains open.
