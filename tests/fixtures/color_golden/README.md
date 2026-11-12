# Frost original color full-state golden

This directory contains only the two runtime fixture files from the independently
reviewed original-production seal captured at Frost
`3ff47cbea7992108aa271fd0553ff088270cb617`: `frost-color-original.jsonl.zst`
and `corpus.json`. Their bytes and fixed hashes are unchanged. The remaining
211 source snapshots, capture logs and provenance artifacts are preserved
separately and are not part of the repository runtime fixture.

The normal `terminal::color_oracles::frozen_original_color_full_state` test runs
this candidate in a fresh exact-name ignored worker, validates the fixed input
corpus hash, and compares every canonical byte and all 133 full-value lanes in
44,720 records. The private worker writes only actual output in a newly-created
scratch directory. There is no baseline regeneration or capture test in CI.

The stream is bounded to an 8 MiB zstd window, 85,791 bytes per canonical record,
3,342,082,180 total canonical bytes and exactly 44,720 records. Expected compressed
bytes and SHA-256 are fixed in the loader. Extra frames, trailing bytes, truncation,
wrong hashes, records and keys fail closed. The fresh worker is bounded by time,
diagnostics and actual compressed size; on failure its scratch evidence remains.
The original 173-case coverage attempt is historical and superseded by this
175-case seal, which includes subscribed and unsubscribed real theme transitions.
