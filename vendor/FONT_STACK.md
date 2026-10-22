# Coordinated cosmic-text 0.19 migration

Sources are the published crates.io archives:

- iced_graphics 0.14.0 (Iced stable 0.14.0)
- iced_tiny_skia 0.14.1 (latest stable software backend at audit time)
- existing cryoglyph 0.1.0, retaining Frost's lru 0.18.2 security patch

On 2026-10-23, crates.io reported cosmic-text 0.19.0, iced/iced_graphics/
iced_wgpu 0.14.0, iced_tiny_skia 0.14.1 and cryoglyph 0.1.0 as the latest
non-yanked stable releases. The Iced crates still require cosmic-text 0.15;
these local patches coordinate the upgrade without importing Iced git master.

Changes from the published graphics/software-backend sources:

1. Require cosmic-text =0.19.0 in both manifests and cryoglyph.
2. Adapt graphics Buffer setters to the lazy 0.19 API.
3. Shape before text measurement/alignment and after alignment invalidation;
   this covers the text cache, plain/rich paragraphs and paragraph resizing.
4. Shape the initial Iced editor buffer explicitly. Subsequent editor updates
   already finish with `shape_as_needed`.

The software renderer and cryoglyph keep their rendering code: compilation
checks their LayoutGlyph, CacheKey and rasterization API against the same
cosmic-text version. Keep the package identities coordinated when updating.
Do not restore a second cosmic-text version to satisfy an older consumer.

Verification lives in `tests/font_stack.rs`, the production terminal/Vim
tests and `scripts/check-font-stack.py`. The latter checks all four consumers,
the exact direct pin and lockfile edge integrity without needing a compiler.
See `docs/dependency-pr22-audit.md` for evidence and remaining limitations.
