# Frost bounded native clipboard patches

Opt-in R04 transport extensions preserve exact locked upstream versions and packaged license notices. Existing default constructors remain legacy-compatible. No core dependency revision changes.

## Provider provenance and changed source files

- window_clipboard 0.5.1: MIT
  - Modified src/lib.rs
  - Modified src/platform/linux.rs
- clipboard_wayland 0.2.2: Apache-2.0
  - Modified src/lib.rs
- clipboard_x11 0.4.3: MIT
  - Modified Cargo.toml
  - Modified Cargo.toml.orig
  - Modified src/lib.rs
  - Added BOUNDED_READS.md
  - Added src/bounded.rs
- smithay-clipboard 0.7.3: MIT
  - Modified src/lib.rs
  - Modified src/state.rs
  - Modified src/worker.rs
  - Added src/bounded.rs
- wayland-client 0.31.15: MIT
  - Modified src/conn.rs
  - Modified src/globals.rs

## Contract

- The Linux iced Clipboard::Read Action submits to a provider-owned request endpoint. Synchronous widget clipboard reads remain synchronous, including the bounded GUI-thread Wayland compatibility pump.
- Eight queued plus active requests, 1 MiB wire and decoded payload caps, and three-second admission-based absolute deadlines are enforced by providers. Cancellation and completion callbacks must be nonblocking; their panics are contained.
- Wayland data and primary-selection devices live as long as their seat. Keyboard capability loss releases only the keyboard: destroying the devices raced with offers already in flight, which libwayland discards without reserving their ids, so the next offer failed with "not a valid new object id" and closed the display connection shared with winit.
- Wayland external-reader mode is restricted to the proved winit run_app host-reader lifecycle. The joined worker never prepares or reads the shared display. It dispatches pending queues with 10 ms active/init/flush and 250 ms idle wakeups. This deliberately permits four idle wakeups per second and up to 250 ms ordinary owned-source service latency. No unsafe Send or raw-handle thread transfer is added; the GUI compatibility pump uses a documented libc::poll call.
- X11 startup connection/authentication remains synchronous, with no new read-path connection or reconnect. Each request has a unique requestor, bounded property/INCR reads, and start/final selection-owner checks. The final check does not promise atomic ownership after completion. Ordinary owner cancellation permits endpoint reuse; an interrupted partial protocol operation fails closed. Retirement can add at most 100 ms before callback delivery and is skipped during shutdown.
- App completions retain session id, descriptor and generation. Permission changes through settings, reload, or reset abort the old request and emit ordered bounded refusals. Session/window teardown cancels outstanding work.
- Neither body text nor body-derived diagnostics are logged. Existing write and primary APIs are retained. Provider source checks and pure tests do not replace native hosted acceptance.

## Dependency additions

clipboard_x11 directly uses already locked rustix 1.1.4 (finite fd polling) and x11rb-protocol 0.13.2 (preserve DISPLAY/xauth setup). No new upstream version is introduced. wayland-client exposes a narrow opt-in external-reader registry initializer while retaining private GlobalList construction and the existing default roundtrip.

## Required hosted acceptance

Run the unchanged strict delayed-owner R11 fixture, the three ignored synthetic X11 tests, both ignored Wayland startup/shutdown tests, the ignored Wayland keyboard-unplug in-flight offer test, actual Wayland owner transfer cap/cancel/stale-offer fixtures, focus/session permission revoke/regrant, idle CPU and external paste latency, and app lifecycle tests. Synthetic isolated providers only; never a user clipboard.
