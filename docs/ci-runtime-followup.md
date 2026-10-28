# Runtime follow-up to the published native shell

This is a local follow-up to the published checkpoint, not a visual 1:1 sign-off
or an update to master. The normal reference layout and composer geometry are
unchanged.

## Runtime corrections

- Files follow now tests the actual right inspector visibility and callback
  ownership. Closing the left navigation must not disable a still-visible Files
  inspector; hiding its inspector or replacing its callback owner must do so.
- The Tasks inspector restores the input-focus observer around its actual
  fields, preventing unconsumed field keys from reaching the terminal.
- Running Chat output reserves the same two host-only role-header rows as
  completed output. OSC completion no longer moves unchanged native output.
  Those synthetic rows remain unmappable to native cells.
- Recovery is a full-window notification below the global toolbar. The prior
  fixed-height notification was placed in the narrow center and clipped the
  preservation/repaired-session wording. Its existing viewport charge is used
  once, and Dismiss retains the recovery report and write protections.
- The native input surface removes an unnecessary unwrap without changing its
  single input owner. Organism geometry tests use actual row coordinates and
  preserve the conservative body envelope and composer exclusion.

The native-provider frozen-source change is limited to the previously modified
desktop smoke runner's private short runtime path and cleanup. Its script was
reviewed and exercised before synchronizing its manifest hash; provider
implementations and ignored native test bodies are unchanged.

The native QA runner can use the existing task runtime wrapper while hashing
the actual application binary. It now records its private DISPLAY/Xvfb PID,
requires one initial Frost window, and checks that Frost owns keyboard focus
before injecting each typed string or key.

## Verification scope

All Cargo commands used the existing Rust 1.97.1 toolchain, the existing task
SDK/target directory, locked dependencies, offline mode, and two jobs. No user
application or system runtime was installed or changed.

| Check | Result and scope |
| --- | --- |
| Full debug bin tests, all features | **1959 passed, 0 failed, 3 ignored**; debug assertions enabled, debug info and incremental output disabled |
| Files follow harness | **22 passed**; actual inspector/layout functions and callback ownership |
| Parser/projection tests | **621 passed**; the four implicated original assertions also pass against the exact original baseline/core, fail before this fix, and pass afterward |
| Production strict Clippy | **Pass**, `cargo clippy --release --bin frost --all-features --locked --offline --jobs 2 -- -D warnings` |
| Formatting | **Pass**, `cargo fmt --all -- --check` |
| Release build | **Pass**, `cargo build --release --bin frost --all-features --locked --offline --jobs 2`; source digest unchanged across the build |
| Serial native Chat/Block QA | **Both passed** with recorded private display/PID and keyboard-focus assertions: actual PTY input, Files focus, mode switching, foreground read/cat, TUI resize, cancellation, multiline/long input, tools reading anchor, and split focus; Chat also verifies structured diff Copy |
| Frozen provider inputs | **70 entries** plus production lock subset verified; isolated provider preparation retained identical implementation sources |
| Ignored provider native tests | **3 X11 + 3 Wayland passed**, in task-private sessions |
| Recovery component/action probe | **Passed on final source**: full notice text, real Dismiss, same PID/TTY, font zoom, tab roundtrip, snapshot/backup permissions; the exact binary/source bridge is in the evidence manifest |

The full unit command was:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
cargo test --bin frost --all-features --locked --offline --jobs 2 --no-run
```

The resulting test binary ran directly in a private Xvfb with a task-only
runtime-patched copy. Its `.text` and `.rodata` hashes match the original test
binary. The process uses umask 0022 and explicitly pinned compiler paths.

The earlier local 269-error record came from a profile without the
debug-assertion-only null renderer needed by test mocks; its original full
command/features were not retained. It is not evidence that the current debug
all-features suite is blocked. Failed and intermediate runs remain in evidence,
including the isolated test runner's accidental task-private Rustup download;
the successful run uses only the existing pinned compiler and creates no Rustup
directory in its private home.

## Still open

- Strict debug **all-targets/all-features** Clippy is now closed by the native
  contract migration described in `native-presenter-test-migration.md`.
  Eighteen unreachable test-only presenters and their dead message construction
  chains were removed. Twenty-two main contracts and six archived widget tests
  became twenty-eight tests of actual native component/ownership semantics:
  no test-count reduction, warning suppression or dummy references. The final
  debug all-features bin run is **1959 passed, 0 failed, 3 ignored**; native
  component/viewport scripts are **28 + 4 passed**, and the lightweight CI font
  scripts are **17 + 11 passed**. These component results do not certify native
  shell font zoom or GPU pixels; those remain separate gates below.
- Existing baseline Installer ShellCheck SC2317 and the later projection
  harness missing `materialize_projection_plan_with_margin` remain separate
  baseline issues. They do not explain the newly fixed Files follow failure.
- Full provider-host acceptance still requires `wl-paste`, unavailable in this
  environment. Passing the six ignored native provider tests is a narrower
  result.
- The original whole-frame Ready OCR recovery gate still fails locally; the
  component/action probe is a separate, explicitly bounded native result.
  Scale assertions must account for measured sidebar-collapse thresholds,
  rather than assuming PTY columns strictly decrease across every breakpoint.
- The initial concurrent Block/Chat QA runs failed at a Files sentinel and a
  heredoc respectively. Their DISPLAY/PID values were not recorded, so a shared
  display/focus collision cannot be established or excluded after exit. Those
  failed results remain preserved. Serial checks retain the original behavior
  assertions; a passing serial run does not retroactively diagnose the earlier
  failures as mere input timing.
- The Find OCR observer has its own commit and exact-value positive/negative
  controls; it does not relax clipboard byte or PRIMARY ownership assertions.
- Physical-keyboard IME, hardware-GPU behavior, all display-scale combinations,
  and complete reference-image fidelity are not certified by these checks.
- Proportional shell-role fonts need a separate native font-zoom check. A changed
  UI config value or terminal-cell size alone does not prove shell labels scale.

Local evidence is under `evidence/frost/ci-followup`,
`evidence/frost/ci-terminal-contracts`, and `evidence/frost-desktop-followup` in
the task workspace. Build/source manifests and screenshot results identify the
actual binaries; diagnostic images from older builds are not relabeled as final
source evidence. The report and raw images remain local; no new upload or push
is part of this batch.
