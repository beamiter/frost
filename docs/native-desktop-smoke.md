# Native desktop functional gate

`scripts/native-desktop-smoke.py` launches the installed production binary inside an isolated real X11 or Wayland compositor. It sends compositor keyboard events into the actual window; it never calls application message handlers or relaxes shell/foreground guards.

The gate checks each Normal, Block and Chat mode, typed and platform-pasted commands, creation of another shell and focus return. Sentinels include shell PID and tty: a shortcut which fails to switch sessions cannot pass just by accepting another command in the original terminal. Isolated HOME/XDG and compositor clipboard contain only synthetic data. Mode fixtures disable session restore.

Artifacts include actual window screenshots, compositor/application logs, the installed executable SHA256, environment package versions and shell-written sentinels. Sentinels are proof of shell execution through input, not a full raw PTY transcript. Failures preserve artifacts and fail CI. The build is a production-code debug build, not an optimized release certification.

## Still required before R11 is complete

Live mode changes, restart/recovery, fractional and multi-output scaling, actual IME preedit/commit non-duplication, and a release-optimized candidate remain separate acceptance work. Unicode keyboard text or paste must not be presented as IME testing. Software headless rendering is not a physical GPU test.

## R04 remains open

The vendored Iced shell calls `clipboard.read(target)` synchronously inside `Action::Clipboard` dispatch. Locked `window_clipboard 0.5.1` delegates to `clipboard_x11 0.4.3` (three-second read timeout and full allocation before returning String) or synchronous `clipboard_wayland 0.2.2` load. Frost's response cap and ownership checks occur after this platform read. Therefore this gate's fast small synthetic paste does not prove bounded initial allocation or UI latency. A delayed provider plus measured event latency is required, followed by a safe bounded asynchronous adapter design rather than moving GUI-owned handles blindly to a worker.

## Local execution limitation

The preparation container compiled and staged the app successfully. Both Xvfb and Sway were blocked before window startup because ordinary AF_UNIX socket creation returned EPERM. Sway initialized headless/pixman/shared-memory rendering first. No transport or security-setting workaround was attempted. The hosted CI gate is the intended execution route; local compilation and script syntax checks are not represented as a desktop pass.
