# Shared dynamic color override ownership

Frost consumes `jterm_engine` from exact core Git revision
`57ed89a62fdd61db060a3c976ab9e8cdac9ae823`.
This stage moves only active and saved-primary dynamic foreground, background,
cursor, and 256 palette overrides into two `DynamicColorState` owners.
There are no mirrored active override fields. Renderer palette reads borrow the
owner's fixed array; scalar reads return optional RGB values.

Alternate-screen entry explicitly clones active overrides into the primary
stash without clearing active overrides. Exit clones the saved primary state
back and clears the stash. These are fixed-size inline copies, not heap-backed
palette clones. Existing mode guards and screen-transition order remain intact.
RIS clears active overrides at its original position in the reset sequence;
DECSTR continues to retain them.

Frost still owns color parsing, admission, theme defaults, optional ANSI theme
colors, query formatting and current BEL/ST terminator, subscription/2031
notification timing, and reset decisions. In particular its first-component RGB
width scaling and unequal-width inputs are not replaced by Ember parsing.
`DynamicColorState` exposes no Ember parsing/theme/query methods. Ember retains
its concrete `ColorState` API and existing behavior. This does not claim that
Forge or Anvil have migrated their color ownership.

## Verification

The original Frost production baseline was captured from
`3ff47cbea7992108aa271fd0553ff088270cb617`, before migration. The sealed v2
fixture contains 44,720 records with 133 full-value lanes. Three fresh original
runs matched exactly, and twelve production-path negative controls were caught
before a restored original replay passed. Only the unchanged golden stream and input corpus are included under
`tests/fixtures/color_golden`. Historical sources, observers, source locks and
capture logs are preserved separately; they are not required by CI.

The migrated consumer passed its complete serial `terminal::` suite:
**452 passed, 0 failed, 3 ignored**. This includes the new full color baseline,
existing parser oracles and existing full title baseline. Expected snapshots
were not regenerated. The shared-core candidate separately passed Rust 1.86
and 1.89 (136 tests each) and the unchanged Ember terminal suite
(493 passed, 0 failed, 11 ignored).

These are local verification results, not a claim that publication CI has
completed. Policy, packaging and other release checks are tracked separately.
