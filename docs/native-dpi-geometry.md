# Native DPI geometry gate

`native-desktop-scale.py` is an independent, bounded Wayland gate. It exercises
100%, 125%, 150%, 200%, then 100% with real compositor input, shell PID/TTY and
`stty size`. It does not run or certify clipboard, recovery Ready OCR, hardware
GPU, multi-output or IME checks. No tool installation is part of this script.

The expected grid is calculated **before reading stty** by a small Rust observer:

- Include the actual shared `reference_shell::Layout` and `native_viewport`.
- Extract `Frost::reference_layout`, `term_width`, `term_height`, banner/bar
  budget and `Metrics::with_font/grid_size` verbatim from the current source.
- Measure the explicitly configured DejaVu Sans Mono with the locked production
  cosmic-text shaper. Record the installed font file/hash independently.
- Reuse the exact Iced/cosmic-text rlibs identified by Frost's Cargo fingerprint.
  No alternate Cargo target, dependency download or copied geometry algorithm.
- Require the observer's complete Rust source digest to match the frozen app
  manifest. The app's real SHA is checked before launch.

Actual client logical bounds, output scale and physical extent come from Sway
IPC, with owned PID/focus checks and stable before/after capture geometry. Raw
native PNG panel edges independently establish rail/navigation/inspector
visibility. A responsive collapse is accepted only when native pixels agree;
the Layout formula alone cannot certify a collapse. Actual grid, same shell,
same font metrics and exact reset are required. Incorrect columns, rows,
PID/TTY, reset, DPR or panel visibility fail.

Use an existing locked release build and its sidecar manifest:

```sh
python3 scripts/native-desktop-scale.py \
  --binary /task/private/frost \
  --binary-manifest /task/private/frost.source-manifest.json \
  --deps /task/build/frost/release/deps \
  --rustc /existing/toolchain/bin/rustc \
  --output /task/evidence/frost-dpi
```

For a task-local SDK, retain its existing `RUSTC_WRAPPER` and linker environment;
`--runtime-wrapper` controls only the app launch, while
`FROST_SCALE_PROBE_RUNTIME_WRAPPER` controls the small observer's runtime. The
outer dbus session must also use a private HOME/XDG directory. These options do
not alter system programs or security settings.

Display-free controls use an already built observer:

```sh
FROST_SCALE_PROBE=/task/evidence/frost-dpi/production-geometry/production_probe \
  python3 scripts/test-native-scale-geometry.py
python3 scripts/native-recovery-report.py --self-test
```

The old restart/recovery runners now invoke the same exact geometry contract
instead of requiring globally decreasing columns. They keep their other native
assertions, so passing the independent DPI entry point never implies those full
suites passed. Original failure records remain valid evidence of the former
oracle and are not rewritten.
