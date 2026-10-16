# Native desktop functional gate

`scripts/native-desktop-smoke.py` launches the installed production binary inside an isolated real X11 or Wayland compositor. It sends compositor keyboard events into the actual window; it never calls application message handlers or relaxes shell/foreground guards.

The gate checks each Normal, Block and Chat mode, typed and platform-pasted commands, creation of another shell and focus return. Sentinels include shell PID and tty: a shortcut which fails to switch sessions cannot pass just by accepting another command in the original terminal. Isolated HOME/XDG and compositor clipboard contain only synthetic data. Mode fixtures disable session restore.

Artifacts include actual window screenshots, compositor/application logs, the installed executable SHA256, environment package versions and shell-written sentinels. Sentinels are proof of shell execution through input, not a full raw PTY transcript. Failures preserve artifacts and fail CI. The workflow builds with `cargo build --release --locked --bin frost`, installs that optimized binary, and compares installed bytes with the build output before exercising it. Artifact hashes identify the binary actually tested.

## Still required before R11 is complete

Live mode changes and multi-output scaling remain separate acceptance work. Restart/recovery, single-output fractional scaling, and genuine normal-mode Wayland IME are exercised by the extended fixtures below. Release-profile acceptance requires a successful run of the optimized installed candidate. Unicode keyboard text or paste must not be presented as IME testing. Software headless rendering is not a physical GPU test.

## R04 remains open

The pre-transport baseline vendored Iced shell called `clipboard.read(target)` synchronously inside `Action::Clipboard` dispatch. Its locked `window_clipboard 0.5.1` delegates to `clipboard_x11 0.4.3` (three-second read timeout and full allocation before returning String) or synchronous `clipboard_wayland 0.2.2` load. Frost's response cap and ownership checks occurred after that platform read. The new transport must pass delayed-owner and bounded-transfer regressions before acceptance; fast small synthetic paste alone does not prove bounded initial allocation or UI latency.

## Local execution limitation

The preparation container compiled and staged the app successfully. Both Xvfb and Sway were blocked before window startup because ordinary AF_UNIX socket creation returned EPERM. Sway initialized headless/pixman/shared-memory rendering first. No transport or security-setting workaround was attempted. The hosted CI gate is the intended execution route; local compilation and script syntax checks are not represented as a desktop pass.


## Extended fixture scope

The recovery fixture closes the real window through its window manager, reads the actual saved two-tab snapshot, and restarts the installed binary. It verifies both working directories through real shells, the restored active tab, two distinct new shell PIDs, and distinct live PTYs. PTY device names may be reused across restart. The Wayland variant additionally requests compositor output scales 1, 1.5, 2, 1 and verifies both rows and columns shrink then return exactly to the starting grid without changing the active shell.

The IME fixture is a minimal genuine input-method-v2 client connected to Sway. It sends a synthetic preedit and commit through the compositor into winit; no application event handler is invoked directly. Normal-mode shell output must contain exactly one committed Chinese character and no preedit text. Screenshots before preedit, during composition, and after commit support a separate visual check. CI installs official Noto CJK fonts so committed Chinese glyphs can be inspected. This does not certify X11 XIM, full fcitx/IBus behavior, or every mode's composition UI. The protocol XML is copied unmodified from the locked wayland-protocols-misc 0.3.12 package with its upstream copyright notice retained.

The delayed X11 clipboard fixture owns only synthetic selection contents. It compares zero-delay and two-second text-transfer owners and records queued-input-to-shell latency, matching real text requests/replies by request identity. A successful characterization means measurements were collected; it does not mean the UI is nonblocking. R04 initial-allocation and nonblocking acceptance remain open until the platform adapter is corrected and measured.

## Genuine Wayland clipboard regression fixtures

The separate data-control owner uses its own Wayland connection and compositor-provided nonblocking transfer pipes. It offers only synthetic fixture bytes through the locked upstream protocol (MIT notice retained). It bounds concurrent transfers, payload storage, work per iteration, stream output and lifetime. Logs contain request ids, byte counts and timing, never real clipboard bodies.

The actual installed Frost application is exercised with small, delayed, stalled, oversized, continuous and revoked-owner transfers, then with pending-read window teardown. Short shell commands must complete before delayed replies; rejected data must not contaminate later shell input. Byte counts prove observed transfer behavior, not allocator accounting. The existing X11 delayed-owner regression remains unchanged.

A second UI fixture pastes into the actual Find text input, which delegates to iced TextInput's synchronous core clipboard API. It checks visible text and copies the selection for an independent external read after an idle interval. It also selects a real terminal word, checks PRIMARY and CLIPBOARD through independent clients, middle-pastes a synthetic PRIMARY command, and closes after a bounded stalled legacy read. This synchronous compatibility path is not claimed nonblocking. Idle-service acceptance uses the 250 ms worker interval plus an explicit 500 ms hosted scheduling allowance (750 ms total); actual measured times are recorded. Idle-service latency includes scheduling tolerance and does not establish zero wakeups or an idle CPU budget.

These fixtures do not establish eight simultaneous provider admissions, allocator bounds, every exact-cap/invalid-UTF8 variant, or all OSC permission/session-generation transitions. Those remain explicit provider/application acceptance cases. All native claims require hosted results; syntax checks alone are not acceptance.
