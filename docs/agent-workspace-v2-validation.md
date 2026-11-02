# Agent Workspace v2 validation

Verified on 2026-11-02 on Linux with Rust 1.99.1. Checks used synthetic
workspace state, empty application/provider homes and an ordinary test UID.
No real provider transcripts, credentials, models or remote SSH server were
used. Development builds used the adjacent shared-core source. The final locked
Git-source type check is recorded separately below; it does not establish that
the proposed commit is available from remote GitHub master.

## Compiled and executed checks

| Gate | Result |
| --- | --- |
| `cargo check --tests` | Passed |
| Debug app build, including the final portrait layout and Context button | Passed |
| Search request/evidence reducers | 4 passed |
| Revisioned Context/handoff reducers | 4 passed |
| Current-task metadata/result reducers | 6 passed |
| Read-only remote response/connection reducers | 2 passed |
| API chat receipt, draft, dispatch and source-switch guards | 14 passed |
| Sidebar asynchronous hydration regression | 1 passed |
| Review-text rendering and input sanitation tests | 11 passed |
| Existing widget identity, press and input isolation tests | 14 passed |

These are **56 different passing tests**, not a claim that the complete app
suite was executed. The named groups can be exercised from the binary test
target:

```sh
cargo test --locked --bin frost -- \
  workspace_tools::tests:: \
  workspace_context::tests:: \
  workspace_review::tests:: \
  workspace_remote::tests:: \
  workspace_chat_bridge::tests:: \
  agent_sessions::workspace_navigation_tree_tests:: \
  review_text::tests:: \
  widget_identity_scope::tests:: \
  --test-threads=1
```

## Actual embedded host smoke

`verify_workspace_host.py` passed against the built Ember and Frost programs.
It exercises the real embedded socket/SQLite host, revisioned drafts, exclusive
submission admission, a newer unsent draft surviving completion, old receipt
rejection, pinned organization and reading-position persistence. Stored history
and reading state survive replacing the Ember host with the Frost host. The
protocol fixture does not dispatch an HTTP/model request.

```sh
python3 scripts/verify_workspace_host.py /path/to/ember /path/to/frost
```

## Native UI observations

Actual X11 windows were inspected in Xvfb using the TinySkia software rendering
path, separate temporary HOME/XDG directories and a synthetic shared host.
Actual X geometry was measured at 1280×900, 540×960 and 360×640.

- The sidebar hydrates asynchronously with three API conversations and stays
  usable through selection, filtering, collapse and restoration.
- The original three-line API draft is shared with Ember and survives switching
  to a read-only evidence view and back. Its visual breathing row does not add
  text to the persisted draft.
- At 360 pixels wide, long conversation headings stay within their own region.
  New and Context remain visible; the API card places Chats/New/Close below
  its heading, and its action footer stays inside the window.
- The explicit header Context button opens the Context tab directly. The
  drawer displays saved revision 1, goal/completed/remaining/constraint fields
  and selected material provenance, including at 360×640.
- Typing `Reading` into the workspace search finds the matching sentence in a
  synthetic Chinese/English body excerpt. Its title and source metadata do not
  contain the query. The evidence view identifies imported history as read-only
  and displays its exact source event and revision `qa-v1`.
- Evidence retains its actual paragraph breaks. Opening imported evidence does
  not start or resume a provider conversation, and its unsupported activation
  control stays disabled.
- Collapsing and restoring Agent Workspace retains the same selected API
  conversation and unsent draft.
- A read-only audit after these flows found zero `api_jobs` and zero
  `native_jobs`, the exact original draft with two line breaks, Context revision
  1 and one selected material.

## Failures reproduced and corrected

The initial native launch exposed a widget-state downcast panic when API rows
arrived asynchronously. A regression test reproduced the panic. Every keyed
navigation item now has the same outer one-child column, allowing the inner
tree to check widget tags before a different widget kind receives old state.
The test passes for insertion, reordering, deletion and reinsertion. Existing
identity scopes and their press/input isolation guards remain in place; their
14 tests also pass.

Portrait inspection exposed overflowing conversation and API card headers.
The final layout reserves the action widths, clips the heading and stacks the
card controls and footer when space is narrow. Inspection also found that the
header's Context button used the generic inspector toggle and could open Run;
it now selects Context explicitly, verified in the running final build.

A final screenshot review found underlying API text showing through the narrow
Context drawer: its panel inherited the window's glass alpha. The drawer now
uses the same inspector color with an opaque background; the docked wide
inspector retains its existing theme opacity. This small style correction is
covered by the final locked-source type check below. The earlier portrait
screenshots were not recaptured after this correction.

The single-line metadata renderer displayed document line breaks as literal
backslash-n. Evidence now uses a separate byte-bounded multiline renderer.
The regression verifies real paragraph breaks, escaped directional/control
characters and the existing byte limit without changing stored text.

An apparent search-keyboard failure was traced to delayed native frames and
stale synthetic XTest modifier state. Separating input actions and clearing the
test driver's modifiers produced the exact search text and result. No product
keyboard, IME or Enter-ownership protections were changed for this observation.

## Final locked Git-source check

Production and development manifests and `Cargo.lock` now pin
`jterm_core` to `d7387fce27bf2b1e60ff2652ab04239a0b809dd3`.
`cargo metadata --offline --format-version 1` regenerated the lockfile. Relative
to the preceding validation lockfile, only the `jterm_core` package source
record changed; no other dependency package or version changed.

The following final check passed with exit code 0 in 265.88 seconds, including
the opaque narrow-inspector background correction described above:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo check --offline --locked --tests \
  --config 'profile.dev.package."*".opt-level=0'
```

The dependency optimization setting matches the isolated QA build profile;
repository build profiles were not changed. The command used the manifest's
Git source and lockfile with no local-path override for `jterm_core`. Its Git
objects were primed from the exact local commit, not downloaded from remote
GitHub. The checkout HEAD and tracked core sources were verified against that
commit. Remote commit availability and pushing master remain separate from
this local release check.

This gate type-checks the app and test target. It does not rerun the 56 tests
or native UI flows recorded above, and it adds no new passing-test count.

## Coverage limits

Xvfb and direct XTest input do not establish Wayland, real IME composition or
candidate handling, screen-reader behavior or GPU-driver coverage. Direct CJK
query entry was observed in Ember; Frost's native search check used the ASCII
sentence contained in the bilingual evidence. This run did not test real
model success, billing, a real SSH host, DLC task execution or GUI restart
during an in-flight model request. The SSH viewer remains read-only; remote
task execution is not supplied by this UI.
