# Shared Eva in Frost

## Enable / scope

Set `eva_companion_enabled = true` in Frost settings, Save, then open a new
window. `EVA_COMPANION_MODE=shared` is an explicit process override. Default is
false. `EVA_COMPANION_SOCKET` selects the local daemon socket; the default is
`$XDG_RUNTIME_DIR/jcompanion/eva.sock`. A supported `XDG_SESSION_ID` is required.
The daemon must be running in the same local graphical session. There are no
model credentials or audio devices in this integration.

A window owns exactly one shared adapter, cache and wake subscription, independent
of split/tab count. Shared mode suppresses every legacy body/preview/watch path;
daemon failure does not reactivate the local organism. Changing modes requires a
new window, so settings cannot race an unretired shared body against a legacy one.
Disable older unsupported terminal pets before claiming shared uniqueness.

## Native presentation boundary

The scoped `iced_winit::presentation_observer` resets prior draw markers before each native tree draw; its submission notification runs only after
`Compositor::present` returns `Ok`. A body-free root canvas marks the exact GUI
tree generation that was drawn. A revoke invalidates the token immediately; only
a newer cleared-tree generation observed by the successful submission hook can
produce `SurfaceRetired`. Receipt, a timer, layout, `window::frames()` (which is
broadcast *before* present), or a failed present cannot do so. Registrations are
window-scoped and dropped with their owner. No close-request callback invents
surface-destruction evidence; busy-job and unsaved-Vim guards run first. Healthy
close sends Withdraw, suppresses new interaction/body frames, and waits for a
native cleared-frame submission plus the daemon's distinct WithdrawalAccepted.
Nonowners also wait for this ack to cover queued-Grant races. A visible, explicit
“Close anyway” escape discloses that recovery may be blocked; it never manufactures
retirement proof and no elapsed timeout is treated as proof.

**NativePresented is native presentation submission, not GPU completion,
compositor acknowledgement or physical scanout.** Frozen owners and ambiguous
recovery remain blocked. `EVA_COMPANION_REQUIRE_COMPOSITOR=1` on the daemon rejects
this host for physical-proof mode. That stronger mode needs genuine compositor
proof/JWM single-node presentation, not a renamed renderer callback.

## Drawing and safety

The existing Agent inspector contains the single Eva dock/card; its original tabs
and assistant controls remain. The dedicated canvas reports its actual logical
bounds and renders a cached 40×22 colored ASCII atlas. It never occupies terminal
rows, alters `CanvasMotion`, PTY dimensions, document height, scroll anchors, or
input focus. Eight distinct source sketches are preserved as **static** settings
previews, alongside static selectors for all 13 poses and 9 optional artistic
moods. The rich atlas is explicitly implementation-derived from figures 4/5. No emoji/recolored generic face substitutes for that atlas.

Body → Compact → Hidden is determined by the safe inspector slot. Input, IME
preedit, selection, mouse-reporting/dragging, alternate screens, remote/read-only
sessions, approval/modal/agent UI, and insufficient geometry suppress painting.
Changing tabs/panes does not create a host. Follow, Stay, Hide, Recall are labeled
native buttons; the settings skin picker sends one shared appearance change.
Full/Calm/Static and system reduced motion govern finite animation deadlines;
hidden/static modes have no dedicated continuous animation subscription. Shared
IPC wake subscriptions are event-driven (the transport's bounded worker polls
locally off the UI thread).

Only focus/input booleans, safe rectangles and opaque task status references enter
IPC. Completed-command metadata comes from the existing validated one-shot drain;
only shell-reported exit zero is success, absent exit is unknown. No command text,
output, cwd, file, clipboard or audio is read/uploaded by the Eva module. Task
execution and approval paths are unchanged. The independent Eva text field sends only explicit user text, with a captured
opaque pane/task context and no attached terminal selection. Unconsumed field keys
cannot reach the PTY; reply/cancel/acknowledgement actions are turn-bound. A reply
from an earlier pane retains its source label. Frost marks a reply displayed only
on the explicit acknowledgement button, never on network receipt; it makes no
claim that text was spoken.

Settings include scoped memory list/pagination, save/edit/delete/clear proposals,
exact digest-bound approval/cancel, and pause. Task scope is the default; global
scope requires its labeled checkbox. A stale context/review/digest cannot approve
a replacement change. Only daemon result receipts change write status; submission
is not success. No cwd or files are attached. A separate explicit selection flow reads only the
current actual local terminal selection when “Preview current selection” is
clicked. The local bounded snapshot shows source pane and selection endpoints;
requesting review sends that snapshot to the local daemon, not the model. A
second exact approval permits attachment only to the next explicit dialogue
send. Cancel, pane/task/project changes and reconnect discard consent. With no
eligible actual selection the preview button is disabled.

Project management is explicit: assign a named identifier to the current pane
(no automatic directory/file access). Changing the project creates a fresh local
task scope, advances context revision and clears prior approvals; old task scopes
are not rebound or adopted. Project/global management selectors do not authorize
model memory retrieval.

Archived task registry and actual persisted memory-scope discovery are paginated
separately, including project-only archives. A selected or manually entered
scope requires a context- and digest-bound management grant before list/edit/
delete/clear. Existing mutation reviews remain separate and exact. Archived
management never takes over a task or sends `MemoryUse`. The current source
context remains unchanged; returning to the current scope clears old reviews.

## Evidence / limits

Final locked/offline validation against the exact public dependency pins passed:

- Production `cargo check --locked --offline --bin frost`.
- Shared Eva focused suite: **11/11**, including exact selection/source consent,
  archived scope review isolation, retirement/frame ordering and all skin/pose/
  motion cache inputs.
- Existing native companion regressions: **29/29**.
- Font/sidebar/native viewport harness: **43/43**; all **six negative mutations**
  correctly rejected. This uses extracted production geometry, not a live GUI.
- Scoped presentation-observer lifecycle test previously passed **1/1**.

The actual Iced tiny-skia software raster contact sheet covers all **8×13 static
skin/pose combinations**: [PNG](evidence/eva-native-atlas.png) and
[capture scope](evidence/eva-native-atlas.txt). It was visually inspected; the
raster test also runs in the focused suite.

### Separate baseline fixture corrections

Independent original-source builds reproduced the two stale geometry failures
before Eva changes. Tests alone now use the existing glass profile's 8px side
insets, 6px top gap and 42px toolbar: exact raw snapshots start at (278,86), and
reference heights are 836/564/500px. The four-row editor still measures 83.2px;
its remaining 976px-window terminal budget is 836−83.2 = 752.8px. Exact pixel,
footer alignment, row-count and all original scenario assertions are retained.

The unchanged native toolbar has Plus, optional Terminal, responsive Tools and
Send/Interrupt—not the old six-tool/model strip. Its source contract now checks
the exact font-scaled Terminal admission and Tools icon-only width expression;
the numerical harness checks both boundaries and control fit at three font
scales. Removed model/secondary token assertions were obsolete, not production
features removed by Eva. No production geometry was changed; the change in
`glass_native_viewport.rs` is confined to its `#[cfg(test)]` fixture.

Software raster and headless tests are not live GUI/compositor handoff, hardware
WGPU, HiDPI, physical CJK IME, selection/Vim, audio, or scanout proof. Those remain
unverified until the matching native desktop matrix is run. No live API or
credentials were used for these checks.

Commands:

```sh
# Use an installed Rust toolchain and the native development libraries documented
# by this repository; no private workspace environment scripts are required.
CARGO_BUILD_JOBS=2 cargo check --locked
mkdir -p target/eva-native-qa
CARGO_BUILD_JOBS=2 EVA_NATIVE_QA_DIR=target/eva-native-qa cargo test --locked eva_companion::tests -- --nocapture
```

## Reproducible dependency release

Shared coordinator/atlas source is pinned to public `beamiter/jcompanion`
commit `9aa569540091524d637a7115b64c587b9177a55b` (protocol 1).
Release manifests and Cargo.lock use immutable Git sources; Cargo acquires them
for source builds without an unversioned sibling companion checkout. Use
`cargo build --locked` and preserve existing user configuration during upgrades.
Coordinator installation and opt-in startup are documented in jcompanion's
`docs/deployment.md`; native application installation does not silently enable it.
Focused render/unit fixtures are not physical display, voice, or live API acceptance.

Shared terminal core is pinned to `beamiter/jterm_core` commit `974ccad203375032fe87706caecab39ad516c238`; normal and dev/fixture dependency identities are aligned.

Historical `recovery-evidence/` paths above describe local validation logs, not shipped files or required build inputs. Reproduction uses only the checkout, pinned Cargo sources and installed native libraries.
