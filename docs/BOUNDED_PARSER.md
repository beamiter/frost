# Bounded parser policy

Frost opts into the shared engine's bounded scanner while retaining the Frost
semantic profile for ordinary valid input. This is a scanner resource policy,
not a claim that all host queues, retained events or CPU work are bounded.

## User-visible differences

- OSC, DCS, APC, SOS and PM payloads are limited to 1 MiB, for complete input as
  well as strings split across PTY reads. A complete single-batch Kitty APC no
  longer bypasses the limit. Introducers and terminators are not payload.
- One extra ESC can be retained while awaiting a split ST; snapshots also
  include introducer bookkeeping. Allocator capacity is not an exact byte cap.
- On overflow, input stays opaque through ST, CAN or SUB. BEL terminates OSC,
  not other string kinds. Embedded escapes in a rejected tail cannot execute
  as screen commands, clipboard requests, titles or protocol replies.
- Oversized and cancelled Kitty APCs no longer produce the previous
  payload-derived error responses. A payload-free parser signal silently aborts
  unfinished image assembly and pending placement/transfer identity; completed
  images and placements remain. Ordinary non-APC boundaries do not abort a transfer.
  Normal small valid Kitty requests retain
  their existing handling. Producers should use protocol-appropriate small
  chunks instead of relying on oversized complete APCs.
- Any scanner rejection clears the current hyperlink without inspecting the
  rejected payload, preventing a rejected OSC 8 from extending an old safe
  link. Already-written cells keep their links; other state is not reset.
- ESC intermediates have an eight-byte limit. CSI keeps its existing fixed
  limits and truncation semantics, not a newly claimed complete semantic guard.

Rejected private control strings must not enter idle background capture. After
termination, subsequent normal output can be captured again. Complete and
fragmented malicious input and recovery are covered at the real consumer,
not only by scanner traces.

## Compatibility and validation scope

Only the explicitly historical parser-migration oracle uses the legacy scanner
for its recorded former overflow/cancellation behavior. Its original fixture
is not regenerated. Production bounded tests independently assert rejection,
recovery, ordinary-stream compatibility and background capture; ordinary
semantic, reset and color golden tests retain the production scanner.

The shared scanner borrows complete fresh payloads only after checking the
first framing-sensitive byte within the exact limit. All incomplete, cancelled,
restarted and oversized paths retain the bounded state machine. Parser-only
microbenchmarks do not establish application latency or GUI acceptance.

Native desktop/Vim acceptance must come from the corresponding real workflow
for the exact published commit. Compilation, headless models and tests that
return early for missing display or D-Bus do not count as native acceptance.
