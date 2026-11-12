# Bounded history normalization scratch

The width-normalization path now consumes retained compressed rows in order,
with one decompressed source row and at most one target-width cell buffer.
It retains only the newest `max_scrollback` reflowed rows instead of constructing
all expanded rows before trimming them. Empty groups, soft-wrap boundaries,
wide-cell splitting and one-column wide-glyph fallback preserve the previous
algorithm. Even rows evicted from the retained tail still receive a fresh row
identity, preserving the allocation cadence.

A regression uses real terminal printing to fill 64 rows at 64 columns. The old
algorithm expands these to 4,096 temporary one-column rows before retaining 64;
the new path retains 64 while its instrumented cell-scratch peaks are 64 source
cells and 1 target cell. These are structural counters, **not measured RSS**.
A test-only copy of the former algorithm compares retained cell contents,
wrapping and cached row-layout metadata across small adversarial cases.

This change is limited to `normalize_scrollback_width`. It is not a hard cap on
total process memory: the consumed source `VecDeque` retains its original
metadata capacity until its iterator is dropped, compression briefly creates
one additional output row before eviction, collection capacity may exceed
length, and existing grids, history, images, frames and allocator overhead
remain live. Work remains linear in source cells and expanded output rows;
processing a huge wrapped line can still take substantial CPU time.

The separate projection-plan path can still expand an entire retained history
document at a narrow viewport width before viewport slicing. That limitation
requires a mapping-preserving sparse or windowed plan; this patch deliberately
does not silently discard projection rows or change selection/anchor semantics.
Native GUI behavior has not been verified by these headless tests.
