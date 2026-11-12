# Frost native-key observer and bounded clipboard patches

Upstream: iced_winit 0.14.0, official crates.io archive. UPSTREAM_INTEGRITY.json records its Cargo.lock-verified archive SHA256 and all packaged file hashes. The archive omits the workspace license; LICENSE is copied from the exact upstream commit 3997291f318a8bc06fa522f5579836fb3feb94df.

The original native-key patch modifies src/lib.rs and adds src/native_events.rs. An optional process-local callback observes window-scoped native events before conversion. Without registration the event path is unchanged. The callback can consume only real key presses; releases, focus events and synthetic snapshots always continue through the existing conversion path. No synthetic action is generated.

Frost uses positive synthetic Enter presses as held-key snapshots and ignores synthetic releases as release evidence. Actual physical releases end native held ownership. Frost separately acknowledges queued Iced Enter deliveries so a release processed before a widget message cannot erase delivery ownership. A captured prefix count owns only deliveries already pending at the claim; subsequently appended fresh presses do not inherit that count. Passive surface refresh does not claim queued confirmations. This prevents a key first pressed outside the window from submitting a command recalled by mouse immediately after focus returns.

On every Iced/winit upgrade, verify the recorded upstream delta, event placement before conversion, physical identities, focus snapshots, both X11/Wayland feature configurations, and the non-executing native recorder matrix. Remove the patch if an equivalent supported upstream hook becomes available. The native-key portion remains limited to this event boundary.

Native recorder proof covers X11. X11-only and Wayland-only builds are checked separately. Pinned Winit clears its Wayland repeat state and timer on keyboard Enter/Leave and starts repeats from delivered Key(Pressed) events; the missing focus snapshot alone therefore does not establish the same failure there. This is a source-based inference, not real Wayland runtime validation.

## Bounded native clipboard Action reads

The R04 extension also modifies src/clipboard.rs and the Clipboard::Read Action branch in src/lib.rs. On Linux, it opts into window_clipboard's bounded owned provider and submits reads without waiting on the GUI thread. Cancellation follows the oneshot receiver; unavailable, oversized, expired, or cancelled reads resolve without clipboard text. Admission is capped at eight, and provider-enforced payload and decoded output limits are 1 MiB with an absolute three-second deadline. Clipboard contents are never logged.

Synchronous core::Clipboard widget reads remain synchronous compatibility calls. Wayland compatibility reads pump only on the known winit run_app GUI reader outside a prepared-read guard; the provider worker never reads the shared socket. Existing window ownership and joined worker teardown are retained. The five provider patches retain locked upstream versions and licenses. Their default constructors retain legacy behavior. X11 connection initialization remains synchronous and no asynchronous request reconnects. Native hosted tests must cover both backends, cancellation, teardown, and unchanged strict delayed-owner UI latency fixtures before acceptance.

## Presentation recovery after suspend

`SurfaceError::Other` can represent a lost WGPU device. The shell replaces the
compositor, all window renderers and their surfaces, while retaining native
windows, widget state and application/PTY state. Old swapchains are dropped before
replacement surfaces are configured. Pending native frame evidence is invalidated
through the paired Winit patch; recovery itself never certifies a presentation.
Lost/outdated surfaces retain their existing surface recovery path. Every failed
presentation uses a 100 ms to 2 s capped retry delay, including redraws caused by
input. Focus regain and de-occlusion request a fresh frame.

`scripts/native-selection-recovery.py` uses a private Xvfb server and a test-only
Vulkan loader proxy to inject device loss into the real acquire path. It checks
newly painted output, drag-copy and unchanged shell/PTY identities across three
recoveries. This does not simulate the laptop's power transition or certify the
physical lock-screen compositor. The native-feedback harness covers retry pacing.

## Retained native ingress and action admission

The local shell adds aggregate RAII admission across Proxy clones and holds
credits through deferred event/message batches. Synchronous admission never
blocks the GUI; an over-limit input/action batch permanently quarantines input
through iced_futures' host-checked latch. Async Proxy producers backpressure with
bounded cancellable waiter registrations. Opaque generic messages, widget
operations and image handles are count-limited, not heap-byte measured.

Runner's retained WindowEvents use 4096 slots / 8 MiB known-payload accounting.
A fault routes pointer buttons, movement and paint feedback into separate small
reservations so continuing keyboard traffic cannot consume the close controls.
No guarantee is made for every click under infinite button traffic. Only
idempotent lifecycle state coalesces. Control creation has bounded queue/title
storage and a 32-window limit; Exit/Crash remain independently deliverable.

A synchronous ready Task exceeding 256 buffered actions is explicitly rejected,
not silently moved after later input. Widget chains are limited to 64 applied
steps before a continuation faults. Already applied effects cannot be recalled.
UI Vec message retention is bounded after callbacks, not allocations inside
arbitrary trusted callback code. These are adapter-owned retention guarantees,
not a total RSS or OS queue bound.

The isolated Runner acceptance fixture stalls consumer polling while real
compositor events pass the unchanged admission code. It is a controlled-stall
regression, not a natural-load flood claim. Production code has no fixture file
trigger or overload reset. See docs/NATIVE_EVENT_BUDGET_DESIGN.md for acceptance
scope and outstanding validation.
