# Hosted native provider acceptance (R04)

This test-only addition depends on the independently reviewed R04 transport v3 and R11 genuine Wayland owner. It does not modify production, existing native-desktop workflow, or R11 fixtures. Source/CI acceptance is distinct from a hosted runtime PASS.

## Identity and isolation

`source-manifest.json` freezes every file, including licenses, in the five actual provider patches plus the reused native desktop harness, synthetic Wayland owner, and official data-control XML. `verify-provider-sources.py` fails closed on drift and verifies that the fixture lock adds no package identity beyond its own package to the production lock. winit 0.30.13 and softbuffer 0.4.8 are already production-locked dependencies. Softbuffer maps a real compositor-visible 320x200 window; it creates no additional concurrent display-reading thread. Softbuffer may synchronously dispatch/read while waiting for a reusable drawing buffer, on the same GUI callback thread; the legacy clipboard compatibility pump also reads only on that thread. The actual `Clipboard::connect_bounded` lives on the winit `run_app` GUI thread. Async completions send user events; synchronous reads run inside that callback, after winit's prepared-read guard ends. Provider is destroyed before surface/window/context.

The standalone workflow uses fresh headless Sway, disposable HOME/runtime directories, synthetic owner payloads, finite process deadlines and tracked children. The command parser accepts at most 256 lines of 128 bytes; fixture instructions contain only operation/ID/selection. No user clipboard is used. No clipboard bodies are logged; metadata contains length, deterministic FNV digest (integrity only), selection, identity, result and elapsed time. Screenshot records the actual host window. Binary SHA256, dependency tree, source identity and full bounded event timeline are retained.

## Assertions

- Two consecutive batches each accept eight mixed CLIPBOARD/PRIMARY reads; ninth is immediately WouldBlock and never completes. Each admitted request completes exactly once after cancellation. The second batch proves all permits reusable.
- At least one real compositor send request is observed before cancellation. A fresh healthy read on the same host succeeds afterward.
- Exact 1 MiB, cap+1, lossy UTF8 expanded beyond cap, small lossy UTF8, CRLF/CR normalization and PRIMARY are verified with expected length/digest or error. This is output-size acceptance, not an allocator accounting claim.
- Owned CLIPBOARD/PRIMARY are externally read during host idleness with no fixture user events. Latency must be below 750 ms (250 ms provider poll + 500 ms scheduler tolerance). Three seconds of quiet `/proc/PID/stat` CPU must consume less than 10% of one core. Four expected idle provider wakes/second are not called zero wakeups. Startup/configuration work is excluded by a 500 ms settling interval.
- Real callback synchronous CLIPBOARD and PRIMARY reads succeed; a stalled synchronous read fails within four seconds, then the host immediately closes within 1.5 seconds. A second mapped host closes with a real pending offered pipe in 1.5 seconds and the owner observes reader closure.
- All five existing ignored tests are explicitly run: three X11 synthetic/native tests on disposable Xvfb and two Wayland socket-pair initialization/teardown tests. Shell asserts exact pass counts; missing/unsupported cases fail rather than skip.

## Boundaries

Local checks are source/metadata only: `cargo check --offline --locked` for the real host and both provider test targets; Python syntax validation. Local native socket access is denied; no bypass, native execution, app linking or runtime PASS is claimed. Hosted workflow execution requires the reviewed candidate to be published following root GO. This fixture does not replace real Frost strict shell/UI, permission revoke/regrant, session lifecycle, actual settings-widget or backend build gates. R11 owns those UI fixtures. The synchronous direct-provider call establishes host-reader compatibility; it is not a claim that terminal Action paste exercises a legacy widget.
