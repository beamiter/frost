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

Native input, modifiers, focus, IME and non-input events share the same order;
no keyboard bypass is added. The transport tests deliberately leave consumers
unpolled, exceed the old 100-event limit, exercise mixed events, cancellation,
aggregate budget saturation, and an oversized IME payload. Tests are run
explicitly by Frost's test CI job.

## Subscription admission budget (Frost host integration required)

The native subscription inbox now admits at most 8,192 event copies and 16 MiB
of accounted event storage across all listeners of a tracker. Keyboard, modifier,
IME, focus and mouse events remain FIFO; none are coalesced. A whole broadcast is
reserved before cloning. Payload accounting includes text and file paths.
Cancellation and dequeue release reservations. These are payload budgets, not
allocator/RSS limits, and do not bound the upstream winit or action proxy queues.

Exceeding either budget synchronously latches `Event::input_overloaded()` for the
process before cancelling native listeners. Timer recipes that discard their
native inbox remain active. Frost's always-active `PollShellVim` timer supplies an
independent wake for the persistent fault view; the fault is not a queued toast.
Subscription replacement cannot clear the latch. Frost rejects all application
messages except its explicit fault-exit control, and guards both Session queue
admission/draining and the final PTY write entry point. This includes a Return
already sitting in a downstream application message or a blocked PTY queue.
Unsent bytes are never replayed. Bytes already accepted by the kernel before the
fault cannot be recalled; commands already running may continue until exit.

Recovery is deliberately process restart, not an unlock button. The fault view
warns that its dedicated close control may terminate jobs and lose unsaved editor
changes. Ordinary window-close input does not bypass that warning. Upstream
window/proxy resource limits remain separate work; this patch must not be
represented as a whole-process memory guarantee.
