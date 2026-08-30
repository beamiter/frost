# frost security patch

This directory is the source of crates.io `cryoglyph` 0.1.0, whose published
checksum is `08bc795bdbccdbd461736fb163930a009da6597b226d6f6fce33e7a8eb6ec519`.
The Rust source and upstream license files are unchanged.

The only intentional upstream delta is the `Cargo.toml` requirement on `lru`:
frost selects 0.18.2 instead of the 0.16 release line affected by
RUSTSEC-2026-0253. Remove this patch when iced/cryoglyph publishes a compatible
release that already uses a fixed `lru`.
