# Original Frost visible-output oracle

`frame-original-output.json` contains 30 snapshots captured before the immutable
frame adapter edits, from Frost `4c1196f57249ca6b3f82b7837e4db97c03b4361e` with
the separately recovered parser/grid architecture patch. Shared-core dependency
pins remained `a6e47726d02bed2ca4a6ba1a682511f970853527`.

The original production `get_projected_viewport` / `get_visible_cells` path was
compiled and executed first. The capture feeds `process_batch`, the production
cache-invalidation entry point. It covers live/history offsets, cursor-only
changes, styled text, wide/combining text, scrolling, alternate entry/exit,
cursor visibility, and physical resizing. Each snapshot includes cells,
wrapped-row flags, cursor position and visibility. The new test only reads the
frozen fixture; it has no regeneration switch.

SHA-256: `3344171177cfd0f3d302bcc440d70198bdbef7316a36d2b050a57cb48d25db52`.

The test module also retains the pre-frame visible-cell cache algorithm as an
independent ownership control. Its only adaptation is a separate test-only
cache field name, so old and new cache ownership cannot interfere. With held
9-column by 3-row snapshots, the control allocates cell rows totaling 0/27/27
cells for unchanged/cursor-only/one-row updates. The new raw-frame path is
checked against 0/0/9 using retained row pointer identity. These are copied-cell
ownership checks, not timing or transport backpressure measurements.
