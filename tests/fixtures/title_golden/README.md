# Frost immutable original title-state golden

Captured from published Frost ada5ae0cb2ff14b5b1177128aec3595c534f3a27,
using its original core598f15808086dbea14d56fc1b86e77b6cb2e3201, before title
owner extraction. No expected values come from the candidate engine.

sealed-evidence/ is the original seal directory unchanged: all138 inventory files
retain exact bytes and relative paths, including original Cargo manifests, raw
observer, source locks, three full original passes and eight negative controls.
The single binary fixture is sealed-evidence/frost-title-original.jsonl.zst.

Final replay uses an isolated fresh test-binary worker so host RawRowIds start at1.
The unchanged observation/corpus semantics compare53992 ordered records, each
with133 full-value lanes including retained frames. Only the candidate's three
title reads/seed assignments move to titles.*. The original source-only guard is
retained in historical provenance, not required of a migrated replay consumer.

The final Rust replay transport replaces external gzip with bounded zstd. Parent
and child validate the fixed corpusSHA; expected compressed bytes are hashed before
single-frame decode, windowmax23 and exact EOF reject oversized windows, truncation
and extra frames. Both streams have record/count/total/SHA limits and are compared
record-for-record byte-exactly, with schema/key/uniqueID checks. No3.7GB serde tree.
Listing and child deadlines, output-size monitors and RAII kill+wait are exercised
by fake-child tests. No replay method writes an expected fixture.

Current Cargo adds only direct dev edges zstd=0.14.3 and sha2=0.10.9 to already
locked packages. Real core/engine pin changes are separate from historical
capture manifests. See replay-overlay.json for source changes; do not edit the
sealed historical manifests to resemble a later build.
