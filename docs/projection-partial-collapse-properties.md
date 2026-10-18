# Fixed-grid partial-collapse contract properties

This test-only extension adds an independent dense source-cell oracle to
`src/terminal/projection_properties.rs`. The production projection and copy
kernels are unchanged. Run it with the existing pure runner:

```
python3 scripts/test-projection-properties.py
```

The first corpus is intentionally hand-authored: 12 fixed live-grid fixtures at
widths 6 and 8. Each fixture has literal expected fragment coordinates, summary
ownership/counts, hard/soft boundaries and key copied strings, checked against
the model before any production call. Production actuals are then checked for
every materialized cell and raw-cell anchor. All ordered pairs of visible raw
cells are copied in normal and rectangular modes: 1,762 exact-text comparisons,
including equal endpoints, reverse directions and surviving wide continuations.
The all-hidden fixture separately checks empty core output.

Two additional `should_panic` tests protect the model's input preconditions.
They are not tests of production range admission. The model requires actual
cell ownership and tuple-order non-overlap, because the two representations of
a row seam are not interchangeable in admitted source intervals.

## Model construction

The expected document comes only from original cells, original wrap flags and
explicit input collapse ranges. Label a dense original-cell ledger with hidden
owners, emit each zone's summary at its first hidden token, and retain maximal
visible runs at their original columns. A visible fragment ending before its
physical row end is hard; a fragment reaching that end inherits the original
wrap flag. Count hidden physical rows by owner occupancy.

Copy expectations are a sequence of selected raw fragments and pairwise edges.
A rectangular selection retains every traversed raw fragment, including one
with an empty horizontal intersection. Normal copy joins a soft edge only when
no summary intervenes; a summary gap is Boolean, so adjacent summaries do not
multiply the separator. This does not reproduce the production pending-break
accumulator. It does not read production slices, row metadata, materialized
cells, selection mappings or copied text to derive expectations.

## Literal failure modes

- Retained suffix columns cannot shift to fill a hidden prefix.
- Same-row and multi-row partial ranges preserve the exact surviving cells.
- Summary gaps break otherwise soft continuations, but a summary before the
  selection start cannot create an extra leading break.
- Adjacent distinct zones preserve separate summaries and ownership.
- End-at-next-row-zero and start-at-previous-row-width exclude the untouched
  physical row from hidden counts.
- A clipped soft prefix becomes hard; an untouched suffix can remain soft.
- Whole wide pairs can be hidden; a surviving continuation stays selectable but
  emits no text.
- A rectangle traversing an empty intermediate intersection retains its blank
  line: `D\n\nP`.
- All cells hidden, including a blank live-grid row, produces empty core output.

## Scope

This is test-only coverage, not a production bug fix or full R02 acceptance.
No randomized or wide-width corpus is claimed. Inputs are fixed physical live
grids with positive dimensions, distinct tracked row IDs, printable scalars or
spaces, valid wide pairs and already-admitted glyph-aligned ranges. The width
must equal the physical grid width. Zero-cell seam ranges, overlapping or
rejected range admission, history/reflow, mixed history/grid sources, chrome,
clipped/scrolled viewports and summary/padding-only ordinary endpoints remain
out of scope. The all-hidden call is a separate pure-core regression. Cache and
selection admission guards, parser behavior, clipboard providers and native UI
are not exercised by the pure runner.
