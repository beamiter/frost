# Projection resource budgets

Projection admission is a host resource policy, not a change to VT parsing or a
promise of unlimited history rendering. Retained terminal history is not deleted
when a projection is rejected.

## Limits and accounting

The production policy bounds one projection build to 101,024 physical source
rows, 131,072 planned display rows, 262,144 raw slices and 1,048,576 wide-cell
metadata entries. Geometry is checked before full document expansion. Collapsed
ranges, spacing, native chrome and Chat additions are included in admission;
incremental updates must pass admission before mutating the previous plan.

History materialization additionally bounds actual source decoding and output
cell storage to 1,048,576 cells each. A short visible prefix does not make a wide
source row cheap: accounting uses its actual stored width, including trimmed
styled blanks. Transformed viewport and canvas/Chat-dock materialization also
charge the actual referenced source widths before decoding. Repeated slices of
the same source may be charged repeatedly, which is deliberately conservative.

Raw history paths that clone a compressed suffix additionally admit at most
32 MiB of source payload plus row/wide metadata lengths. This does not measure
allocator capacity. The wide-aware geometry estimate can use a conservative
`cols - 1` upper bound and may reject earlier than the exact rendered row count,
especially at width two. These thresholds are safety admission limits, not a
guarantee that every document just below one threshold will render.

Checked arithmetic rejects overflow rather than saturating into an accepted
estimate. Intermediate bounded collections can coexist, so these limits are
**not a total process RSS ceiling**. Existing grids, image caches, retained
history, old immutable frames, allocator overhead and constant-factor working
copies remain live. This is not a general allocator-OOM recovery mechanism.

## Unavailable output and recovery

An over-budget projection produces an explicit unavailable output surface with
no rendered terminal content or origin mapping. It does not silently truncate
rows, reuse stale coordinates, or fall back to raw/live cells. In particular,
inline scope and Chat visibility rules cannot be bypassed by resource failure.

The surface explains that output is temporarily not displayed while the child
process continues running. Keyboard/IME input, window resizing and closing the
session remain available. Mapping-dependent selection, copying, pointer/scroll
reports, links, search navigation, block actions and dragging are refused while
unavailable. A width/history/policy change can trigger fresh admission and
restore rendering; this is not a permanent session fault latch.

Current terminal authority and captured projection keys protect live actions.
Queued widget/menu callbacks from a rejected or superseded view cannot become
valid merely because a later view recovered. An old immutable viewport can still
be inspected as a pure geometry snapshot; that does not authorize an action on
the current terminal.

Successful and rejected admission results are bounded caches keyed by relevant
source versions, geometry, policy and offsets. They avoid repeated idle-frame
scans, but source changes can still require linear bounded metadata work. This
patch does not provide a hard CPU-time guarantee or implement a fully sparse,
lazily indexed history document.

## Verification boundary

Small injected-budget tests exercise rejection, arithmetic, actual-width/byte
accounting, empty unavailable frames, stale live actions and recovery. Pure
geometry oracle expectations are retained; synthetic fixtures requiring live
interaction must explicitly register current frame authority. No golden output
is regenerated to hide a changed mapping.

Headless tests do not establish native GUI accessibility, compositor behavior or
end-to-end desktop acceptance. Exact-commit CI and native acceptance remain
separate gates, recorded with the implementation's validation evidence.
