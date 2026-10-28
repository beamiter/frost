# Native shell contract migration

This cleanup follows runtime checkpoint `ffba633f`. It removes the unreachable
`#[cfg(test)]` tab/status/sidebar presenters rather than constructing them solely
to satisfy strict all-targets linting. No archived GUI source strings, warning
suppressions or dummy references stand in for the current native shell.

The current presentation is `reference_workspace` with `native_viewport::raw_rect`
and one retained `TermWidget` per pane. The application supplies one native
organism body for the current eligible owner. The widget's painted geometry and
physical pointer observations carry the exact projection, block and geometry
epoch. Configuration chrome is separate from terminal input and live attention.

The migrated contracts keep these invariants:

- One body/owner; no fallback clone or extra animation clock.
- Native body and speech remain in proven blank terminal space and the effective
  parent clip. They never obscure the caret, occupied cells or composer.
- Physical enter/exit retains its offer ticket; layout/scroll/owner changes only
  reconcile geometry and cannot manufacture a greeting.
- The real speech-close control emits one configuration action and does not
  redispatch that gesture into terminal selection or application mouse input.
- Selection, IME, graphics, running work, remote/read-only ownership, overlays,
  pointer buttons and drag state retain their existing conservative admission.
- The native PTY viewport charges the shared 80px composer exactly once. Tiny
  windows saturate at zero; reading and recovery chrome use their actual budget.

Retired product surfaces are explicit: the narrow ASCII-footer reopen arrow,
84px glyph/toggle slot, separate 88px companion card, 40px legacy tab strip and
bottom status bar are not recreated in the new shell. Their safety intent moves
to the real Settings/native body and allocator. In Block/Chat, legacy companion
expansion and `bottom_bar` settings cannot deduct phantom terminal rows.

`src/native_companion_tests.rs` replaces the twenty main source-string contracts
and the five archived widget-helper tests. The additional paired-offer event
test also uses the real `TermWidget::organism_body` update path. Shared pure
admission predicates are called by production; they are not test-only substitute
models. The former `OrganismHoverEnter/Exit` message adapters are replaced by
native `OrganismBodyObserved` observations.

The CI font script entry points preserve their backend-free build budget.
`test-font-sync.py` uses rustc to compile the actual `ui_typography`,
`native_viewport`, path-dependency `reference_shell`/`chat_mode`, and sidebar
virtualization helpers. `test-font-groups.py` builds actual configuration
fields/setters/reload tests and the Iced scope modules with only iced_core,
iced_widget (no renderer backends), serde and toml. Neither requires the native
Frost SDK or tests the retired status bar. The temporary Cargo harness generates
its lock and runs locked with two jobs. Local gates set CARGO_NET_OFFLINE=true;
CI retains its existing ability to populate the ordinary crate cache.
`test-native-layout-contract.py` separately executes the real debug/all-features
Frost native body and allocator contracts, and can save exact source hashes.
It does not select an arbitrary newest core rlib.

The Settings copy now names the real Companion toggle and Show companion status
bubble setting. The retired status-arrow and legacy width thresholds are not
presented as recovery guidance. Visibility remains conditional on safe blank
space; no new input owner, wake source or feature was introduced.

Native shell font zoom is a separate known product gap. Passing font-group
configuration/scope tests or old helper tests does not demonstrate that fixed
native shell labels scale. This cleanup does not change native-shell typography
or claim pixel acceptance. Original native frames and the supervising task's
visual checks remain separate evidence.

The exact replacement count is 22 main tests plus 6 archived-widget tests,
replaced by 28 current native component contracts. The 25 primary entries in
the read-only plan remain mapped individually; paired-offer handling and two
additional stale-snapshot/attention contracts close its dependency graph. Three
main Settings guidance tests and one organism guidance test remain, with current
control names and visibility constraints. Test counts do not decrease.

Final local verification uses the pinned existing Rust 1.97.1 toolchain,
RUSTUP_AUTO_INSTALL=0, locked/offline dependencies and two jobs. Debug strict
all-targets/all-features Clippy passes. The complete debug bin suite passes
1959 tests, with 3 unchanged ignored tests. The real native component/viewport
runner passes 28 + 4 tests; the backend-free CI font scripts pass 17 + 11.
Formatting and diff whitespace checks pass. Existing vendor deprecation warnings
are unchanged. Intermediate failures and the initial sandbox Xvfb failure stay
in the task evidence alongside the final test-binary and source hashes.
