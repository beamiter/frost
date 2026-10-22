# PR #22 font-stack audit

Audited PR head `8bc0aa13264fec5b7c285c4575dc0be398a8a724` against
base `1e96427550688fff42e388f875216c54a11dcc9e` on 2026-10-23.

## Confirmed failure and coordinated upgrade

The original PR's [release job](https://github.com/beamiter/frost/actions/runs/39813505715/job/119945797398)
fails with E0061 at `terminal_view.rs` calls to `Buffer::set_size` and
`Buffer::set_text`. The [test job](https://github.com/beamiter/frost/actions/runs/39813505715/job/119945797558)
reports eight E0061 errors, including the layout tests. cosmic-text 0.19
removed the FontSystem argument from these setters and made setters lazy.
This is evidence of compilation failure, not evidence of a rendered regression.

The requested final solution is a coordinated upgrade to cosmic-text `=0.19.0`.
Iced stable remains 0.14.0; use iced_tiny_skia 0.14.1. Vendor the published
iced_graphics 0.14.0 and iced_tiny_skia 0.14.1 crates, adapt the graphics Buffer
setters and explicitly flush lazy shaping before reading/aligning layout.
Upgrade the existing cryoglyph 0.1 patch to the same shaper, preserving its lru
security fix. The lockfile contains no cosmic-text 0.15. Retain the other nine
Dependabot dependency upgrades. See `vendor/FONT_STACK.md` for patch provenance.

| Option | Cost and risk | Decision |
| --- | --- | --- |
| Retain 0.15.0 | Smallest fix; no rendering algorithm change, compiles against existing Iced | Superseded by request to use latest font stack |
| Upgrade together to 0.19.0 | Two local Iced crate patches; lazy-layout/API adaptation; newer harfrust/skrifa behavior requires regression coverage | Implemented |

A lock-graph check and a Rust Buffer type-identity test enforce the shared
version. Dependabot is prevented from independently moving cosmic-text beyond
this coordinated release. Future stack updates must change this policy too.

## Data flow

Frost's `measure_mono_ascii_advance` uses Basic shaping of printable ASCII,
checks uniform advances and passes the resulting scalar cell width into Metrics.
Unresolved/nonuniform fonts use the existing per-cell fallback. Frost uses a
separate FontSystem from Iced; matching versions does not by itself guarantee
identical font databases, font loading order or fallback selection.

Iced's text cache constructs cosmic-text Buffers. `iced_graphics` 0.14 and
`iced_tiny_skia` 0.14 require cosmic-text 0.15; the local cryoglyph 0.1 manifest
does too. cryoglyph reexports Buffer, LayoutGlyph and CacheKey and consumes this
model for WGPU rendering. All four lockfile consumers now resolve cosmic-text 0.19.0 through local patches.

Vim native rendering retains Vim's authoritative row/column/cell counts and
the shared projection for text, cursor, hit testing and IME anchors. Non-ASCII
text uses Advanced shaping; UTF-8 byte cluster boundaries are not cell counts.
No conversion between 0.15 and 0.19 glyph/cache types is introduced; all consumers compile against the 0.19 model.

## Other updates

The Linux implementation `dirs/src/lin.rs` is byte-identical between downloaded
6.0.0 and 7.0.0 sources; both use dirs-sys 0.5.0. Frost's home/config/data/local
data/state functions retain `Option<PathBuf>`. A compile-time API test covers
these calls. Keeping core's dirs 6 and Frost's dirs 7 is not the font-engine
problem: these APIs exchange standard-library paths, not versioned glyph types.

The original CI reached Frost after compiling the updated dependencies; the
fixed locked all-target check also validates libc, tokio, smallvec, serde_json,
toml, unicode-normalization, uuid and crossbeam-channel together. Runtime
semantics outside the font/path tests are not inferred from compilation alone.

## Verification

Only explicitly completed results below are passes.

- `cargo check --locked --all-targets`: passed on Rust 1.99.1 with the unified 0.19 stack.
- `cargo fmt --all -- --check`: passed.
- `python3 scripts/check-font-stack.py`: passed; the original PR lockfile fails
  this same check because it contains 0.15.0 and 0.19.0.
- `python3 scripts/test-font-sync.py`: three tests passed.
- New `tests/font_stack.rs`: shared Buffer identity, dirs API types, actual
  shaping of ASCII/CJK/fullwidth/combining text across sizes 12/20/32 and scales
  1/1.25/1.5/2. CI installs DejaVu and Noto CJK; missing glyphs fail explicitly.
- Added Iced cache/paragraph tests for initial layout, narrow wrapping and wide resize restoration, covering lazy-layout invalidation.
- `cargo test --locked --test font_stack`: 4 passed (new tests above).
- `cargo test --locked --bin frost terminal_view::tests`: 42 passed, including
  actual ASCII batch shaping and styled CJK glyph coverage with Noto CJK.
- `cargo test --locked --bin frost vim9_buffer::tests`: 35 passed, including
  fractional scrolling, text/caret/IME continuity, CJK source-cell anchors,
  mouse hit projections and split geometry at fractional scales.
- Font synchronization harness: 3 passed on the final sources.
- Projection harness: 14 passed; this standalone harness uses its own pinned
  dependencies and is supporting geometry evidence, not a whole-PR test.
- Native GUI smoke attempted with the compiled binary and Xvfb. Blocked before
  mapping a window: Xvfb cannot establish local/Unix listening sockets in this
  execution environment; Frost reports XOpenDisplayFailed. No pixel or real
  monitor HiDPI pass is claimed.
- The earlier broad 0.15 test run was stopped after the scope changed. No full
  suite pass or release-profile build pass is claimed for the final migration.

Runtime test commands used `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, and `CARGO_INCREMENTAL=0` to fit scratch disk limits;
these change build artifacts, not the committed dependency graph.

Existing blockers, deliberately not hidden by weakening checks:

- `scripts/test-font-groups.py` fails its source assertion at line 30 on both
  the exact base and this branch, before its Rust harness runs.
- Original dependency-policy CI reports unmaintained zbus_names 4.3.4,
  zvariant 5.15.0 and zvariant_derive 5.15.0 (RUSTSEC-2026-0371/0370/0372).
  These locked packages were not introduced or changed by PR #22.
- Vendor iced_winit emits existing deprecated `try_next` warnings.

Headless shaping/projection tests cannot certify actual GPU pixels, compositor
scale changes or subjective scroll smoothness. Before merge, run the native
Vim GUI smoke with Noto CJK and inspect CJK/combining/fullwidth text, caret/IME
alignment, fractional scrolling and monitor transitions at 100/125/150/200%.
Do not interpret this targeted repair as a claim that all CI is green.
