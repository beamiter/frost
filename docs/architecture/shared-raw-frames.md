# Shared immutable raw paint frames

Frost's `VisibleCellsCache` and identity `ProjectedViewport` retain `VisibleRows`.
Its raw storage owns the engine `CellFrame`; native painting, link detection,
selection geometry, and native chrome borrow the frame's row slices through
`RowView`. Historical reflow, synthetic projection, canvas-margin policy and
provenance stay Frost-owned and retain materialized storage.

On a raw cache miss, unchanged cell rows share their immutable allocations with
the prior frame, even when an older viewport is held. Changed rows get fresh
allocations. Contents are compared rather than relying solely on dirty-row
stamps. This reduces copied cells, not the number of cells inspected. Wrapped
flags and cursor metadata are captured with each frame. Both the raw cache and
projected viewport cache account for cursor metadata changes independently of
PTY/grid revisions. Normal input continues to use `process_batch`.

A transient host cursor outside the grid is represented by hidden, in-bounds
paint metadata; capture does not mutate the terminal's protocol cursor. Normal
right-margin pending-wrap coordinates are preserved. Frame revision/layout
counters are local to the terminal. Epoch 1 is only an in-process paint-stream
marker: this adapter does not implement transport identity, byte admission,
subscriber admission, ACK scheduling, or a Tauri renderer.

The existing nested-Vec `ProjectedViewport::cells` interface remains an explicit
compatibility materialization. The raw storage itself stays immutable, and
native consumers use `row_view` without initializing that compatibility copy.

## Independent validation

`terminal/frame_adapter_tests.rs` compares 30 snapshots with a fixture captured
from the old production adapter before this change, and retains the old cache
algorithm as an independent allocation-ownership control. For retained 9x3
snapshots, newly copied cells for unchanged/cursor-only/one-row updates are
0/27/27 in the old control and 0/0/9 in the frame path. This is not a timing or
transport-backpressure claim. See `tests/fixtures/frame-original-output.README.md`
for provenance and the fixture checksum.

The five focused tests also check held-frame immutability, shared untouched
rows, alternate and resize layout boundaries, saved cursor shrink/restore,
right-margin pending wrap, exceptional host cursor bounds, direct cursor
shape/visibility changes, and native row access without compatibility copying.
Run `cargo test --locked --bin frost frame_adapter_`.
