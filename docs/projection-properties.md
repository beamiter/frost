# Projection property tests (R02)

## Run and replay

- `python3 scripts/test-projection-properties.py` builds only SmallVec 1.15.2
  and the production projection kernels; no desktop service is started.
- `CARGO_NET_OFFLINE=true` uses an already populated Cargo registry.
- `FROST_PROJECTION_CASE=0 FROST_PROJECTION_STEPS=1 python3 scripts/test-projection-properties.py`
  replays the first failing sequence/prefix. Failures print seed, case and the
  complete operation prefix. Reduce the prefix to isolate the first failure;
  this is not a structural delta-debugger.
- Normal `cargo test` also runs `projection_properties` and the additional
  `projection_state_properties` module. The latter calls the actual terminal
  resize, policy and session-owned viewport entry points. The isolated runner
  does not run or emulate those entry points.

The fixed seed is `0x46524f5354523032`; widths are
`[1, 2, 3, 4, 5, 7, 8, 13, 16, 31, 64, 80]`. There are 10,000 independent
16-operation sequences, with a guaranteed initial resize/toggle/mode/scroll
cycle followed by randomly ordered operations. The full terminal test uses the
same sequences. No escape strings or clipboard providers are involved.

## Oracles and coverage

Original fixture cells form an independent `(raw row identity, raw column,
cell)` ledger. An eager reference packs complete glyphs directly from that
ledger, without production layout metadata, raw slices or mapping functions.
Collapse masking is computed directly against original row/column ranges.
Every surviving token and its order is checked, not just a mutually consistent
raw-to-view/view-to-raw round trip. Persistent selection anchors are checked
against the ledger after each operation.

Fixtures cover CJK, emoji wide-body/continuation pairs, precomposed accented
scalars, background/styled spaces and structural trailing blanks. Frost cells
store one scalar: accented scalars represent existing NFC cell values, not
unrestricted combining sequences, grapheme clusters or parser-input coverage.

Kernel modes are raw, collapsed/spaced and native chrome. Application mode
routing and cache admission belong to the separate full-terminal test. Chat
lifecycle/eligibility is outside this corpus. Randomized copy checks use a full
normal selection; deterministic cases additionally cover live-grid sources,
partial/reversed/block selections, continuation-only selection, hidden prefixes,
all-hidden output, adjacent collapse ranges, wide boundaries, summary ownership
and valid trailing-blank fallback. The pure text core is shared with production;
its original cache/revision guards remain in the caller.

The isolated runner extracts production types and algorithms verbatim. Its only
`TerminalState` replacement is fixture storage and allocation; it does not
simulate cache, parser, UI or provider behavior. The property module is private
and test-only. No production data visibility is widened.

## Honest work accounting

Counters increment at actual compressed-row decompression/layout and at each
materialized row/source-slice visit. With an already built plan and the same
7-row, 16-column visible shape, histories of 128, 4,096 and 65,536 rows all take
7 decompressions, 0 history layouts, 0 encoded layout-byte scans, 7 row visits
and 7 source-slice visits. Cold plan construction visits all history rows and is
explicitly asserted to be linear; resize/rebuild is not advertised as constant.

Work depends on visible source data, not just viewport height. A separate case
joins 160 one-cell source rows into two visible 80-column rows and observes
160 decompressions/160 slices, two visited rows and zero new history layouts.
These are warm-plan materialization measurements, not viewport-cache-hit or
end-to-end latency claims.

## Regressions fixed

1. Selection re-anchoring used a raw column as a projected column. Joining or
   splitting historical rows could select another cell. The exact containing
   raw slice now provides the projected column.
2. A hidden prefix's cell could fall through to the trailing-blank fallback
   and acquire a visible suffix coordinate. Cells owned by a collapsed summary
   are now rejected before that fallback; real trailing blanks retain it.
