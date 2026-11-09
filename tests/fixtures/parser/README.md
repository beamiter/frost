# Frost original-scanner oracle

`frost-original-scanner-oracle.json` was captured from Frost's production scanner
**before** replacing its `pending_escape`, OSC/DCS/APC carry, and UTF-8 decoder
with the shared parser. It is independent expected output, not the extracted
parser running on both sides. Do not regenerate it from a migrated scanner.

## Provenance and integrity

- Original full `src/terminal.rs` SHA-256, before the test-module registration:
  `feab8952b2dda2876e7288673c9e30cb7b8f8f8a5ac8065c795e4b1f703fb990`.
- The original source was immediately frozen without overwriting an existing
  file at `/tmp/frost-scanner-original-baseline.rs` during the migration.
- Fixture SHA-256:
  `144c34868283c3589cffd8ea16f700ed52e3e70fed5c5e66b57733406f598c30`.
- Fixture size: 12,799,690 bytes.
- 72 cases, 2,139 feed partitions, 6,958 feed-boundary observations,
  3,287 deduplicated snapshots, and 91 separately named semantic/state lanes.

Capture is opt-in with `FROST_CAPTURE_PARSER_ORACLE=1`. It hashes the actual
compiled terminal source, stripping only this test's module registration, and
requires the immutable original source hash above. It additionally refuses an
existing fixture and opens with `create_new(true)`. Both original capture and
normal comparison passed before extraction. A repeated capture was verified to
fail without changing the fixture. Normal test runs only read it.

## Format and observation contract

Each input has a name, byte count and specified FNV-1a-64 fingerprint. `records`
contains the exact cumulative feed `ends` and corresponding snapshot indexes.
Each feed uses `process_batch`, including empty feeds. A record compares against
its own original partition, never against a one-shot result assumed equivalent.
Every intermediate and final feed is observed, including one-byte feeds.

The `snapshots` catalog deduplicates identical complete observations. Its lanes
are separately named, rather than one opaque whole-state hash. Short values use
exact Debug text. Larger values use the byte count and specified FNV-1a-64 of
that text; carry buffers use their original bytes directly, avoiding megabytes
of byte-list JSON. FNV-1a uses offset 14695981039346656037 and prime 1099511628211,
with wrapping unsigned 64-bit arithmetic. It is a stable regression fingerprint,
not a security hash.

Observed lanes include both screen grids and styles; text, cursor, wrap and
saved state; modes, keyboard state, colors and title; replies and pending
clipboard/notification requests; active parser carry bytes, scan offsets, APC
discard state and active UTF-8 state; watch batch and generation; command
lifecycle, zones, completed commands and export text; background raw bytes,
chunk metadata and geometry; retained output provenance; decoded Kitty image
pixels and placements; and identity/collapsed projection cells, origins and
layout. Batch/row/history versions are compared because feed partitions match.

Process-global raw-row IDs are normalized to retained absolute row coordinates.
Locally measured command duration/finish times, image generations, and cache
handles are excluded. Only active UTF-8 bytes and expected length are recorded,
not inactive buffer padding or stale expected length. Terminal dimensions are
24 columns by 6 rows, with 32 retained scrollback rows to bound overflow cases.

## Corpus

The 52 short cases cover every two-way cut and selected multipart partitions
(one byte, two bytes, and seven bytes per feed):

- C0 controls, valid multibyte/wide/combining text, invalid UTF-8 including
  `E0 80 80`, interrupted `E2 82 A`, surrogates and out-of-range scalars.
- CSI parameter collection at 127/128/129 bytes, empty parameters, saturated
  numbers, colon SGR, embedded C0, CAN/SUB/ESC cancellation and invalid bytes.
- Charset `ESC (` followed by another ESC, multiple designators/intermediates,
  unknown ESC followed by C0 or high bytes, save/restore and alternate screens.
- OSC titles, colors/queries, hyperlinks, clipboard; modes and ordered replies;
  Kitty direct/multipart transfers and decoded image state.
- OSC, DCS, SOS, PM and APC, each with ST, CAN, SUB, RIS, and ESC restart, with
  cuts including split ST and outstanding prefixes.
- OSC 133 accepted/rejected start, end, prompt and watcher fragmentation;
  shell lifecycle identity and completion; idle background output, private
  control-string exclusion, raw capture, edits, UTF-8 and zone/projection finish.

The 20 giant cases are all five control-string families at carried packet
lengths 1 MiB minus one, exactly 1 MiB, plus one and plus 33. Only selected
partitions are used: direct, opener cuts, length boundaries, split ST, and
multipart boundaries, followed by valid colored recovery text. This preserves
original differences such as interrupted UTF-8 glyph/cursor behavior across
partitions and oversized APC replies in direct versus fragmented delivery.

## Focused validation

Use the existing validation mirror, whose `src` and `tests` point at this real
checkout. The local `jterm_core` override is only a validation transport:

```sh
export CARGO_HOME=/workspace/scratch/9226dcee068a/.cargo-task
export RUSTUP_HOME=/workspace/scratch/9226dcee068a/.rustup-task
export PATH="$CARGO_HOME/bin:$PATH"
cargo test \
  --config 'patch."https://github.com/beamiter/jterm_core.git".jterm_core.path="/workspace/shared/four-terminals-validation/jterm_core"' \
  --manifest-path /workspace/shared/four-terminals-validation/frost/Cargo.toml \
  --locked --bin frost parser_oracle -- --test-threads=1
```

The original capture passed in 11.36 seconds and normal comparison in 12.34
seconds. This focused test is not a claim that unrelated Frost tests ran.
