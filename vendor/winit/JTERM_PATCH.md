# Winit 0.30.13 local patches

This copy is extracted from the crates.io package verified against Cargo.lock:

`a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`

The X11 wheel change ignores XI_ButtonRelease for legacy wheel buttons 4–7
in xinput2_button_input. The original emits MouseWheel for both halves of a
single press/release pair.

Upstream source: https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/linux/x11/event_processor.rs

The native Vim buffer window smoke injects a complete X11 notch, checks exactly
three source rows and traces intermediate GUI pixel positions. This patch can
be removed after upgrading to an upstream release with equivalent behavior.

The XKB key builder snapshots the cached effective Num Lock state with each
translated event on X11 and Wayland. `KeyEvent::num_lock_active` exposes that
snapshot. Other backends report no state.
