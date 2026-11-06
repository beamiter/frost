# Maintained pin projection patch for wasmtimer

This directory starts from the official crates.io `wasmtimer` 0.4.3 archive:

- Source: https://crates.io/crates/wasmtimer/0.4.3
- Repository: https://github.com/whizsid/wasmtimer-rs
- Upstream revision: `fecc90e04b1884ac6298af2cdcd2ef3f8e0141de`
- Archive SHA-256: `1c598d6b99ea013e35844697fc4670d08339d5cda15588f193c6beedd12f644b`
- License: MIT; the original copyright notice is retained in `LICENSE`.

`Cargo.toml.orig`, `.cargo_vcs_info.json`, README, changelog, and all source
files other than the two listed below are preserved from that archive.
Upstream editor, CI and registry-cache metadata are not included. The upstream
lockfile is replaced by a standalone test lockfile; its wasm-bindgen family is
aligned with Frost's root lockfile and the CI test runner.

## Local changes

RUSTSEC-2026-0441 marks `pin-utils` as unmaintained. The dependency entered
Frost's all-target lockfile through `iced_futures -> wasmtimer`, even though
Frost's Linux target does not execute the WebAssembly backend. This patch
removes the dependency instead of ignoring the advisory or changing only its
version. `wasmtimer` remains version 0.4.3.

- `Cargo.toml`: replace optional `pin-utils` with `pin-project-lite` 0.2,
  preserving the existing `tokio` feature activation. Register the new pinning
  regression test. `Cargo.toml.orig` is historical upstream evidence, not the
  active manifest.
- `src/tokio/interval.rs`: directly use `Pin::new(&mut self.sleep)`. `Sleep`
  is already `Unpin`, and the upstream interval implementation uses this same
  safe operation for reset.
- `src/tokio/timeout.rs`: use `pin_project_lite::pin_project!` to project the
  future and delay without hand-written unsafe pinning. Preserve the public
  accessors, conditional `Unpin` behavior, and future-before-delay poll order.
- `tests/pinning.rs`: exercise an address-sensitive `!Unpin` future across
  polls and in-place cancellation, and preserve the ready-future priority when
  its deadline has elapsed. The original `tests/web.rs` is unchanged.

The root `[patch.crates-io]` entry selects this directory. The root lockfile
contains no `pin-utils` package. No RustSec suppression is added.

## Verification

Install `wasm32-unknown-unknown` with rustup and install `wasm-bindgen-cli`
0.2.127 (matching the test lockfile). Node.js must be available. Then run:

```sh
export CARGO_TARGET_DIR=target/wasmtimer
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo check --locked -p iced_futures --target wasm32-unknown-unknown
cargo check --manifest-path vendor/wasmtimer/Cargo.toml --locked \
  --target wasm32-unknown-unknown --no-default-features
cargo test --manifest-path vendor/wasmtimer/Cargo.toml --locked \
  --target wasm32-unknown-unknown --all-features
scripts/security-check.sh --audit
scripts/security-check.sh --policy
```

The Supply-chain security workflow runs the WebAssembly checks and regression
tests. The upstream suite covers sleep/reset, interval scheduling/missed ticks,
timeout success/expiry, delay queues, and serde. It retains the upstream
`cfg(browser)` and Node-only unused-import warnings; these do not disable or
skip any enabled tests.

Remove this local patch once an upstream release removes `pin-utils`, then
rerun the WebAssembly regression suite and both supply-chain checks.
