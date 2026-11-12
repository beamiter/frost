# Native event-chain admission and validation scope

Source baseline: Frost `e9bf3bcdda6e20aab53e90acf4ba1859b7f03fa4`.
This document records the original hazards, adapter-owned admission changes and
validation boundaries. OS/compositor queues and total process RSS are NOT claimed
bounded. Existing Tracker admission and host/PTY rejection remain in force.

## Original ownership map and hazards

1. OS/winit delivers WindowEvent and UserEvent. The OS queue is outside this
   adapter's ownership. Payload allocation may already have happened there.
2. Runner moves events into an unbounded ingress channel. While compositor
   initialization/recovery awaits, polling the instance need not drain it.
3. run_instance converts native events into an `events` Vec; Output actions enter
   a `messages` Vec. Dequeue is therefore not a safe credit-release point.
4. AboutToWait clones matching events into per-window vectors before removing
   originals. This creates transient duplicate payload ownership.
5. Tracker broadcasts admitted copies into per-subscription streams, with its
   existing aggregate 8192-copy / 16 MiB budget.
6. Subscription/task output goes through Proxy. Its async channel has per-sender
   capacity effects and its synchronous send_action bypasses backpressure. Raw
   winit UserEvents retain these payloads. Existing free_slots occurs BEFORE
   batch processing, and cannot be reused as a correct ownership ledger.
7. update synchronously polls immediately-ready Task streams into an actions Vec
   until Pending/None. A perpetually Ready producer can starve the GUI and bypass
   Proxy entirely. Widget update can also generate application messages directly.
8. Host dispatch and final PTY writes check the permanent overload latch. Bytes
   already accepted by the kernel before fault cannot be recalled.

## Required safety invariants

- No keyboard, IME, focus, modifier, or command is silently sampled/coalesced.
  On failed synchronous admission set the permanent process input fault BEFORE
  dropping the rejected item. The fault UI explicitly reports rejected input.
- GUI callbacks never block waiting for capacity they themselves must release.
  Async producers suspend with cancellation-aware wakeup instead of spin/wait.
- Admission credits are shared by all clones, conserved across queue transfers,
  and held until the final owning batch is consumed or cancelled. No double
  accounting/freeing caused by direct vs async Proxy entry points.
- A stalled producer, dropped receiver, poisoned producer, or cancelled task
  cannot retain a credit forever. Waiters wake outside budget locks.
- Control needed to present the fault and explicitly close remains reachable.
  A permanent fault does NOT mean every event stream is permanently disabled.
  No automatic replay, unlock, or resume of rejected terminal input.
- Overload cannot accidentally execute already-delivered Return: existing host
  dispatch/Session/final Pty checks remain authoritative after fault.

## Implementation requirements

A. Establish a reusable nonblocking aggregate admission ledger with RAII leases,
checked count/known-byte arithmetic, cancellation-safe async waiters, and tests.
The ledger does not own/reorder application payloads.

B. Replace raw Proxy action delivery with an envelope holding a lease shared
across every clone. Async Sink admission must reserve capacity before accepting
an item. Synchronous send_action uses nonblocking admission and explicit fault.
Hold the lease across RawUserEvent and ingress until its batch completes; remove
old processed-count accounting. Use an independently bounded/coalesced wake
signal, not a queued user command, for waking the fault renderer. Exit/control
must not depend on obtaining a saturated data-plane lease.

C. Native ingress admits WindowEvent payloads before the adapter retains them.
Charge fixed envelope storage plus owned IME String capacity and path bytes;
account key text accurately for the actual winit representation. Transfer
leases through conversion and batch processing. Lifecycle/redraw/control state
needs an explicit bounded control lane; never treat keyboard events as redraw.
Replace per-window clone-then-retain with ownership partitioning.

D. Bound synchronous ready-Task draining. Reject an overlarge unapplied batch
explicitly instead of asynchronously reordering its tail after later input.
Truly Pending streams retain their original async semantics. Cap directly generated message/action
batches with an explicit fault before dropping surplus. Audit action recursion,
window creation/control ownership and shutdown delivery separately.

E. Native tests must deliberately suspend consumer progress and downstream
consumption; flood each real adapter entrance (including many Proxy clones);
verify retained counts/bytes, FIFO normal delivery, cancellation, wakeup and
fault-page pointer close. Run X11 and Wayland independently. Existing derived
Tracker fixture is a regression test, not evidence for these new entrances.

## Byte-budget contract limitation

`Action<T>` has opaque generic Output(T), Widget trait objects and image handles.
`size_of::<Action<T>>()` cannot measure their retained heap, and counting events
alone is not a process memory limit. Sink::poll_ready also does not receive the
next item: true byte admission needs a declared worst-case item bound or an
adapter that owns the pending item while awaiting its exact weight.

Do not invent a byte estimate or claim complete memory closure. First establish
shared slot limits and exact known-native-payload accounting. A subsequent host
contract must define per-message payload caps and shared-object accounting for
opaque Actions; verify every producer honors that contract. Tests and release
notes must state any remaining unbounded payload category explicitly.

## Acceptance gates

Independent design review, ledger conservation and adversarial scheduling tests,
full existing native/PTy overload regressions, complete library tests/Clippy,
exact-SHA CI, and real X11/Wayland fault display/input rejection/pointer close.
Neither a screenshot nor an offline OCR replay proves safe close. Do not weaken
frozen acceptance manifests, warning text assertions or source hash checks.

## Implemented limits / validation

The implementation has 256 shared Proxy slots, native admission
4096 slots / 8 MiB known payload accounting, and independent post-fault button
(32), pointer (128) and paint (64) reservations. Thus a continuing keyboard flood
cannot occupy the fault controls. This does not promise delivery of every click
under an unending mouse-button flood. Control window creation is limited to 32
queued windows / 1 MiB retained titles and 32 live native windows; opaque settings
and image/operation heap remain outside byte accounting. Lifecycle state coalesces;
Exit and Crash have dedicated control state.

An overlarge synchronous ready-Task batch is explicitly rejected before its
buffered actions run, rather than asynchronously reordering the tail after later
input. Exact-cap streams must be distinguished from cap+1. Widget operation chain
overflow also faults explicitly; already applied operations cannot be undone.
Direct UI callback message retention is checked after the callback; allocation
inside arbitrary trusted callback code is NOT bounded by that check.

The new isolated fixture copies iced_winit, stalls only instance polling AFTER
actual Runner admission, and keeps real native callbacks flowing. XTest/wtype
F24 events exceed unchanged production admission; the real rejection branch must
be recorded before the pause releases. The harness has a 20-second proof deadline (the fixture also checks elapsed time
on each callback), PID-bound
proof, preserved diff/source hashes and no production trigger. This controlled
consumer-stall test is not a naturally occurring compositor flood test.

Local final production-source validation passed 19 iced_winit library tests,
10 iced_futures tests and 2 doctests. The full host suite passed 2181 tests
with 5 ignored and 2 environment failures because the local root directory is
not system-owned for trusted Git resolution; all 4 font-stack integration tests
passed. An earlier real-PTY close-role assertion failed once, then passed both
an exact-binary diagnostic and the final full run without changing its assertion.
This is not evidence of its root cause. Real X11/Wayland controlled-stall,
postfault input isolation, and pointer-close acceptance must pass for the exact
published commit before claiming native acceptance. Earlier Tracker-only CI
results do not validate this new Runner/Proxy implementation.

## Final-write linearization

The host's final nonblocking PTY write and fault latching share a short mutex
fence. Faulting first stops admission of new writers, waits for the already
admitted syscall to finish, then publishes the fault. EINTR retries release the
fence and recheck admission. Bytes accepted before that linearization point cannot
be recalled. This is not wait-free: it can briefly contend with one nonblocking
syscall. It never waits for queue capacity or holds the fence across an await,
blocking IO, retry loop, or UI callback. The public closure contract prohibits
re-entering or latching the same gate while holding it.

Final local strict Clippy passed. The isolated Runner fixture compiled in dev
profile (followed only by PTY import-order formatting). This compile does not
exercise a compositor. CI builds the release fixture and must provide the actual
X11/Wayland evidence for the published source.

## First Runner desktop result and follow-up

At c26ec06, CI, Provider, Vim and conversation layout passed. Native Desktop
passed all ordinary production scenarios, but the new isolated Runner cases
failed: X11 proved real admission rejection after 585 keyboard callbacks and
992 ms, yet the screenshot remained on the old terminal UI; Wayland's key tool
rejected an explicit `-d 0` before admission. No postfault close/input-isolation
success is claimed for that commit.

The follow-up removes the invalid tool argument and makes first-fault view
rebuilding independent of `AboutToWait`, with a single reserved fault control
even when the rejected input itself cannot be queued. The isolated fixture now
records actual consumer, rebuilt-view, draw and successful-present milestones
bound to the visible process PID. This diagnoses the actual rendering stage
instead of accepting a timeout increase or weaker OCR. Exact new desktop CI is
still required to establish that this resolves the observed X11 symptom.

## Sustained X11 fault interaction follow-up

At 923c0b6, the native run 43160728015 passed the complete Wayland Runner
controlled-stall scenario, including real pointer close. X11 now displayed the
fault page and passed same-PID consumer/rebuild/draw/present milestones plus
blocked-shell-input observation, but timed out waiting for exit after the real
button click while native input remained busy. These are controlled consumer
stalls with real native ingress, not proof of natural-flood exhaustion.

The follow-up gives already-admitted fault-page pointer events one bounded local
update turn before receiving more native events. It reuses the normal UI update
path, without synthesizing a click, relying on OS idle, or allowing rejected
keyboard data to schedule turns. The normal native admission reservations and
permanent fail-closed input latch remain unchanged. Fresh native CI is required
to verify X11 close for this exact implementation; local tests are not desktop
acceptance.

The standalone projection property harness also imports the production failure
metadata enums introduced by projection admission. It does not stub those types
or remove the property/mutation gates. Its 14 tests (including 10,000 generated
sequences) and both unchanged negative-control mutants pass locally.

At 78870d3, CI, Provider and Layout passed. Wayland again completed the real
controlled-stall fault/input-block/close scenario. X11 still timed out at close,
so the local-update change alone is not a verified resolution. The isolated
fixture adds bounded real-pointer delivery/UI-status/exit-action diagnostics to
locate the remaining stage, without manufacturing input or changing production
behavior or acceptance conditions.

The independent Vim GUI run also exposed a presentation race: the fixture had
acknowledged restoration while the screenshot still showed the prior compatibility
frame. Buffer-first geometry now waits within the existing bounded deadline for
actual GUI RGB pixels, then applies the unchanged strict position assertions.
Visible incorrect geometry is not retried until it passes. Both observer negative
controls and the full 28 snapshot/typing tests pass locally; GUI acceptance still
requires the new exact-commit run.

At ae44934, Vim GUI, CI, Provider and Layout passed. Native Desktop again passed
ordinary scenarios and Wayland controlled-stall acceptance. X11 still failed:
PID 21826 recorded cursor motion through Runner, consumer and UI, but no native
MouseInput press/release callbacks. Wayland PID 21997 recorded both buttons,
Captured UI status, one message and the real exit action. Thus the evidence does
not attribute the remaining X11 failure to application close handling.

The next isolated fixture traces raw X11 button/filter stages and emulation
flags (bounded, no keyboard text). X11 injection now issues one genuine press,
requires its same-PID Runner observation, then releases (also on timeout). It
never retries a click, sets focus, manufactures an application event, or accepts
cleanup as exit. This distinguishes delivery/filtering failure from a short
press/release observation race; actual new CI evidence remains required.

### a8c9ca33 observer resource bound

Native run `43180105495` passed the ordinary desktop checks and Wayland
controlled-stall overload acceptance. X11 PID `23289` reached admission rejection
and all four consumer/view/draw/present milestones, but the very first full-frame
Tesseract invocation exceeded its existing 30-second timeout. No click was yet
injected, so the absence of raw XI/pointer traces in this run says nothing about
the earlier missing button delivery. CI, Vim, Provider and Layout passed.

The observer now bounds only the Tesseract child to `OMP_THREAD_LIMIT=1`.
It retains the exact screenshot transform, PSM 3, confidence requirements,
30-second subprocess deadline, live input flood and real press/close gates.
Replaying the actual failed frame locally took 0.953–1.057 seconds with default
workers and 0.770–0.772 seconds with one worker, with identical 2,923-byte TSV.
This bounds OCR resource use; it does not establish the CI timeout's CPU cause
or claim the pending X11 click acceptance is fixed.
