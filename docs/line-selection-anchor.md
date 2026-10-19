# Projected whole-line drag anchors (R02 component)

## Defect and minimal example

Base: `beamiter/frost` commit `ccbd19813328c814d135d407faef3f3bfc72d230`.
On a transformed history viewport, place these hard lines at width 8:

```
UP
ABCDEFGH
XY
```

Triple-click `ABCDEFGH`, drag down to `XY`, then reverse upward to `UP`.
The original code copied `UP\nAB`; the complete original line must remain
selected, giving `UP\nABCDEFGH`. A one-character lower line reduced it to `A`.
This requires history projection's trimmed source spans; a fixed live grid owns
its trailing blank cells and would mask the unequal-line-width case.

The production UI routes triple-click to `select_line_in_projection` and a
continued triple-click drag to `extend_line_selection_in_projection` in
[`src/main.rs`, lines 13484–13547](https://github.com/beamiter/frost/blob/ccbd19813328c814d135d407faef3f3bfc72d230/src/main.rs#L13484-L13547).
The latter recomputed the original line edge from the minimum/maximum of its
current anchor column and the *moving target's* column. After reversal those
columns no longer described the two edges of the original line.

## Production correction

Resolve the original anchor's current document row directly in the validated
projection plan. Its first and last raw slices provide the real left/right raw
cell identities, even when the anchor is outside the viewport or its display
row joins multiple source rows. A genuinely blank raw row uses a row-scoped
endpoint. Synthetic rows, untracked origins, zero-length slices and arithmetic
overflow fail closed. The existing snapshot/source-revision and independent
surface guards remain in force.

The line boundary means the current verified plan's display row containing the
stable original anchor. This patch does not freeze the raw-cell membership of
the first clicked display row. Same-width source regrouping after head trim or
live-to-history transitions retains the existing re-anchoring semantics; it is
not covered as a newly solved behavior by this patch. Width/hidden-set changes
continue to cancel selection through the unchanged guards.

The new lookup takes one `rows.get(document_row)`, first/last slice accesses,
and constant-size endpoint construction. It does not scan history, decode any
source row, allocate a vector or clone the complete plan. This is a bound on
the added drag-edge lookup, not an end-to-end constant-time claim for all
existing policy validation or selection rebuilding.

## Deterministic verification

Run `python3 scripts/test-line-selection-anchor.py` with an installed Rust
compiler, or set `RUSTC` to its path. Normal Cargo tests also include the same
`src/terminal/line_selection_tests.rs` module.

Seven targeted tests cover:

- Long original line, short target, reversed drag
- Repeated reversals and return to the original line
- Host spacer insertion/removal that changes document row numbers
- An original line outside the current viewport
- One displayed original line composed of two independently tagged raw rows
- Stale viewport-plan revision
- Changed history-source revision

Expected strings come from literal original text, not production mappings or
copy results. The joined-source test also asserts which original raw row and
column owns each fixed edge. The exact same tests fail five assertions on the
unmodified base; its two stale-guard tests already pass. The corrected source
passes all seven. Targeted negative controls reintroduce the wrong reverse
edge, use the first slice for the right edge, or omit either revision guard;
all four compile and then fail assertions.

## Scope and limits

The dependency-free runner extracts the actual selection/admission,
re-anchoring, materialization, mapping and copying methods. It substitutes
fixture allocation/storage for the grid, compressed rows and SmallVec; it uses
plain scalar text cells and directly declared plan geometry. Chat and semantic
block-spacing discovery are disabled in this fixture. The full planner,
compression implementation, parser, event loop, application mode routing,
clipboard providers and native pointer/GUI behavior are not certified by this
runner. A fixture-only Clippy allowance accounts for SmallVec becoming Vec.
It is not a full-crate build or full R02 acceptance.

This is one real production defect correction within R02. It does not convert
previous pure-copy coverage into native acceptance or change roadmap totals.
