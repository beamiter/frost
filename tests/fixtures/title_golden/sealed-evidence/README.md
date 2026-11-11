# Sealed original Frost title baseline

Actual source: ada5ae0cb2ff14b5b1177128aec3595c534f3a27, existing core598f158.
No candidate engine/consumer took part. Only recorded cfg(test) observation was
registered; Cargo/lock and all production bodies were unchanged for original runs.

An initial restored3 attempt was interrupted by an executor key change and its
partial output is not evidence of completion. A new unique restored3_retry1
provided the restored run. Interruption metadata is preserved.

Three fresh-process complete outputs are byte-identical: two originals before
mutations and a third after exact source/observer restoration. Each carries
53,992 ordered records and133 full-value lanes (all129 TerminalState fields plus
four retained-frame/alias lanes). Canonical JSONL is3,775,893,628bytes with maximum
record92,682bytes. It is losslessly represented by one3,501,727byte zstd frame,
independently decoded against original gzip and all record IDs/schema. Original
gzip fingerprints remain in reports; redundant multi-hundred-MB transport copies
are intentionally not bundled. No state is replaced by a fingerprint.

Eight real production mutations were caught at first full-state difference:
limit200, replacement-admission bypass, wrong outbound sanitizer, peek vs pop,
RIS clearing titles, RawRowId alias, single ID shift, and explicit duration7→8.
Each has source diff, exact SHA, full counterexample values/raw JSONL pair,
expected/actual record SHA and lane list. The duration control changes only the
seed admissibility assertion7→8 in addition to the source hash guard so the actual
wrong value reaches the unchanged observer. No expected record was changed.
Stopped worker groups were killed and reaped under a dedicated Linux subreaper.
Intentionally incomplete mutant gzip streams are not retained as baselines.

RawRowIds are NEVER rewritten: sole fresh ignored worker assertions establish
allocator1/6, and original output retains live/stale identity and aliases. Time
normalization only maps finished_at_ms presence and Instant presence; explicit
durations and lifecycle metadata are preserved. Two seed Arc views stay alive
through every feed and are fully observed along with current-cache alias booleans.
No snapshot invokes a mutating renderer getter. Seeds use an explicitly recorded
renderer cadence; primaryseed2 asserts an effective collapse while altseed3
asserts identity fallback. Endpoints use production projected-selection mapping.
The Kitty helper keeps complete state and asserts at most one Pending node.

Historical capture wrapper is source-locked evidence, not a publication-ready
replay wrapper. Final integration still needs bounded Rust zstd replay, strengthened
fresh subprocess lifecycle, original full test, candidate full test and review.
The original helper retains its drafting comment as part of immutable provenance.
This seal does NOT claim consumer migration or release completion.
