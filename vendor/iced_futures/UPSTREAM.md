# Iced subscription event transport

This is the existing locked `iced_futures` 0.14.0 package, not a version
upgrade. Its cached `.crate` SHA256 matches the original Cargo.lock checksum:
`8c0c85ccad42dfbec7293c36c018af0ea0dbcc52d137a4a9a0b0f6822a3fdf0a`.
All 22 archived package files were compared byte for byte with the cached
source before this local patch. The upstream revision in `.cargo_vcs_info.json`
is `3997291f318a8bc06fa522f5579836fb3feb94df`, directory `futures`.

The package declares the MIT license. The upstream Iced MIT notice is included
as LICENSE, using the already bundled `iced_graphics/LICENSE` notice. The
original archive does not itself include a standalone license file.

Only subscription transport is changed: the 100-event `try_send` inbox dropped
native keyboard events when a consumer briefly stalled. The inbox now retains
one FIFO without blocking the UI thread. The output sink still has its original
backpressure. A dropped output consumer terminates its stream and releases the
queued inbox; cancellation also releases it before another generation opens.
Cancellation is polled while a downstream send is backpressured as well as
while waiting for the next input event. Messages already accepted by the
downstream sink are distinct from the subscription's cancelled internal inbox.

This is an unbounded inbox, so backlog memory can grow while an active consumer
stalls. It has no hard memory cap. This preserves input identity instead of
silently discarding bytes or blocking the GUI. Native input, modifiers, focus,
IME and non-input events share the same order; no keyboard bypass is added.
The transport tests deliberately leave the consumer unpolled, exceed the old
limit, exercise mixed events and cancellation, and measure a 65,536-event
non-input backlog. Tests are run explicitly by Frost's test CI job.
