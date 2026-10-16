# OSC 52 asynchronous read ownership

Audited baseline: `8a62a46baa4c042a03358324c5fa9f133c4068a5`.
No clipboard disclosure defect was established. This change adds regression
coverage to the existing production gates; it does not weaken or replace them.

## Production route

- `TerminalState::handle_osc_52` retains at most eight query terminators.
- The PTY update captures stable session ID and descriptor. Host reads begin
  only when `allow_clipboard_read` is enabled and both session and global
  single-flight admission permit them. The Iced read callback carries that pair.
- Runtime session allocation uses monotonically increasing `next_id` (eight
  successful creation paths). Closing removes the session; it never resets the
  counter. Descriptor reuse alone therefore cannot recreate the pair. There is
  no independent generation: the stable session ID serves that purpose. Exact
  pair reuse is not a supported runtime lifecycle. Counter exhaustion is not
  exercised by these tests.
- Close releases only that session's global read owner. A late completion clears
  the owner only when the pair still matches; it cannot clear a replacement's
  read. `session_by_identity` rejects missing IDs and mismatched descriptors.
- Completion reads the current permission again. Denial, absent text and text
  larger than 1 MiB produce empty replies. The byte cap precedes base64 encoding.
- While a read is pending, at most eight further refusal terminators are queued.
  Completion emits the original answer followed by those empty refusals. Excess
  requests are dropped; the bound does not promise a response for every flood
  request. The original answer plus the eight deferred refusals is nine replies.
- Responses enter the owning terminal's output buffer and its session's
  `flush_responses`. The PTY response backlog has an 8 MiB byte cap plus a 4096
  entry cap; overflow logging contains session ID/counts, never clipboard text.
  The tested callback, ownership gates and completion helper contain no logs.

## Executable coverage

`python3 scripts/test-osc52-ownership.py` compiles verbatim production callback,
owner methods, completion/refusal helpers and parser query branch with Rust.
The only adapters are synthetic session storage, clipboard delivery, encoder
capture and PTY capture. It covers close plus recycled fd, changed descriptor,
reordered tabs, permission revocation, absent clipboard, exact/over byte cap,
query/refusal flooding and owner-lock isolation. Four intentional regressions
(fd-only matching, ignored revocation, missing size cap, foreign owner release)
must fail. This is execution of production gates, not a full application or OS
clipboard integration test.

`cargo test --bin frost osc52` registers the same callback suite and adds real
terminal parser/base64 response tests for the eight-query limit, denial and exact
size boundary. Existing ordering/refusal tests remain active. The repository CI
already runs all-target, all-feature cargo tests, so these tests are registered
without changing workflow policy. Python 3 and rustc must be available.

## Verification boundary

Only synthetic text is used; no real clipboard is read. A local standalone
harness pass does not prove the native cargo tests or compositor integration.
Native focused Cargo tests passed: 11 tests, zero failures. A fresh writable
Cargo cache allowed the pinned core and registry dependencies to be downloaded.
The full binary suite reported 1584 passed, 2 failed and 1 ignored. Both failures
are unchanged trusted-Git fixture checks because this container root directory
is not system-owned; the existing security policy was preserved. CI still needs
to confirm the complete suite in its supported environment.
Actual Iced/Wayland/X11 allocation and input responsiveness remain unmeasured:
Iced can materialize the host clipboard string before the response cap runs.
The cap bounds reflection/encoding, not the platform's initial allocation, and
this report does not claim a measured non-blocking UI latency guarantee.
