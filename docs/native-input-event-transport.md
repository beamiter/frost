# Native input event transport

A task-private synthetic input check proved that a 170-byte command arrived in
Bash as 164 bytes. Read-only PTY size sampling did not show a resize during a
separate successful run. A later bounded in-memory observer localized a new
170-to-169-byte failure: Winit received all 170 bytes, the Iced subscription
missed one space at offset 43, and encoded input, enqueue, actual PTY writes
and Bash history all contained the same 169 bytes. Native Enter ownership did
not consume a key. The append operation still succeeded because the missing
space did not invalidate the shell syntax, so an execution-effect assertion
alone is insufficient evidence of byte integrity.

The same run logged one `iced_futures::subscription::tracker` inbox-full
warning. The original six-byte failure also logged inbox-full warnings. Iced
0.14's subscription tracker used a 100-event channel and synchronous
`try_send`; full queues discarded events after logging a warning. A consumer
briefly delayed by renderer/application work could therefore lose keyboard
bytes before Frost's own input admission or PTY queue saw them.

Frost now vendors the exact already locked `iced_futures` 0.14.0 package and
changes this event inbox to an ordered non-discarding channel. It does not
upgrade dependencies, add a keyboard bypass, block the GUI thread, or alter
widget capture, IME ownership, key encoding or downstream sink backpressure.
A closed output consumer terminates its stream so that its inbox is released.
Cancellation releases the old stream before another opening receives events.
An additional deterministic test exposed an incomplete cancellation state in
the first fix: a live full output sink could suspend `send().await` without
polling cancellation. The send now selects cancellation too. This releases
the internal inbox even if the receiver stays alive and does not drain;
messages already admitted to the downstream sink cannot be recalled.

This channel is unbounded. An active stalled consumer can retain an increasing
event backlog, including non-input events; there is no hard memory cap. In a
controlled 65,536-event non-input test, process RSS grew from approximately
2.8 MiB to 10.8 MiB. Cancellation released the stream and prevented any old
event entering a new generation, but the allocator retained the RSS pages.
Neither this result nor a successful input test proves a fixed memory bound
or immediate return of freed pages to the operating system.

The input-preservation tests leave the consumer unpolled during each burst. The
unmodified upstream tracker delivers only 101 of 170 keyboard events and 101
of 4,096 mixed events. The patched tracker must retain their exact order and
content, and release cancelled or disconnected subscriptions. Test CI runs
these dependency tests explicitly before the application's test suite.

The temporary native memory observer is kept only in task evidence, not in
production source or logs. Original failed input/history artifacts remain
available. The final production binary passes the unchanged 2ms 170-byte
command with an exact post-outcome Bash history comparison. It also passes
the original non-DEBUG Chat native matrix: real Stop and same-shell recovery,
four-line heredoc, long wrapped input, foreground stdin/TUI, mode switch,
focus, reading anchor, resize and split checks. The same binary preserves the
unique companion's dense-output avoidance and safe clear/return behavior.

Strict application all-targets/all-features Clippy, dependency all-targets
Clippy and the 1,959-pass/0-fail/3-ignored debug bin suite pass. The five
transport tests and two dependency doctests pass separately. The existing
provider source/lock subset and font-stack checks also pass; this does not
replace native clipboard/provider acceptance.

The new four-case real Wayland IME protocol run remains unverified. Both the
initial attempt and the single retry after additional human approval were
refused by automatic review before any test process started. No further
attempt, route change or test on a replacement binary was made. The unit
transport test includes IME preedit and commit events, but does not stand in
for that desktop gate or physical keyboard/IME testing. No 1:1 visual or
unbounded-input guarantee is inferred from these bounded checks.
