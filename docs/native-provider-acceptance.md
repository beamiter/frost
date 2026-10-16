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


## Isolated Smithay native-test workspace

The first hosted run built the actual host and X11 test binary, but Cargo rejected `cargo test -p smithay-clipboard` because the patched dependency has dev-dependencies and is not a member of the fixture workspace. No native acceptance ran in that attempt. Metadata-only `cargo check --tests` did not detect that Cargo test-selection restriction.

`prepare-provider-tests.py` now creates a fresh workspace outside the repository with a source-identical copy of the reviewed Smithay crate as its sole member. It first runs the existing 70-file source/lock verifier, copies all crate files and notices, and verifies every copied non-manifest file byte-for-byte. The only manifest change removes the exact upstream `[dev-dependencies.sctk]` section, whose additional xkbcommon feature serves the upstream example. Both native lib-test bodies use std/native socket pairs; they are unchanged. Normal dependency declarations, default features and all other manifest semantics are asserted identical using parsed TOML. The patched production wayland-client path is retained. The reviewed `smithay-tests.lock` introduces no package version/source/checksum outside production Cargo.lock; all build/run commands use `--locked`.

This is the same explicit example-only omission used by the earlier local verification contract. It does not claim upstream examples or their extra feature coverage. Production workspace membership, root manifests/lock, real host graph, X11 test path, source pins and test counts remain unchanged. Preflight logs, commit identity, copied-source hashes, transformed manifest provenance, locked package identities, resolved test feature tree and linked test binary hashes are preserved even when a later build fails. The same two ignored Wayland tests run from this workspace; the same three ignored X11 tests run through the existing fixture manifest.

The preparation script also compares resolved provider-specific features with the actual host graph (Smithay clipboard, its sctk 0.20 dependency, patched wayland-client, and wayland-backend) and fails on drift. Unrelated host consumers can add shared dependency features such as libc extra_traits or rustix mm/system; these differences are recorded explicitly. The isolated tests do not claim full-application unified-feature equivalence. Normal provider dependency declarations/features and locked package identities are unchanged.
