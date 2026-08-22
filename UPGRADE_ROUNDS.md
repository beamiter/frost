# Frost upgrade rounds

Rounds 1–10 record the preceding pass; this pass's additional twenty rounds
are numbered 11–30.

1. **Prefix validation** — unsafe empty, relative, control-bearing, and lexical
   parent-traversing paths are rejected without excluding Unicode or spaces.
2. **Binary-directory validation** — install/uninstall enforce the same rules
   and reject an explicit empty override.
3. **DESTDIR confinement** — staging concatenation cannot escape through a
   lexical `..` component.
4. **Explicit root staging** — `DESTDIR=/` keeps staged cache and diagnostic
   behavior after the stored prefix is normalized.
5. **Dependency preflight** — install, temp, rename, cleanup, and desktop tools
   are checked before the build or first mutation.
6. **Atomic binary replacement** — a same-directory mode-`0755` temporary is
   renamed over the target without following a destination symlink.
7. **Pre-commit cleanup** — EXIT cleanup tracks the sole live temporary and
   preserves the old executable only until rename commits the binary; later
   resource failures do not imply rollback.
8. **Atomic desktop replacement** — an unpredictable same-directory temporary
   removes the predictable `.new` staging race.
9. **Remote-host semantic gate** — one application gate combines spoofing and
   byte-budget checks with shared argv/session/deploy/path validation, and is
   re-run by the picker, connection launcher, and remote filesystem; app text
   checks precede any shared diagnostic that could quote a draft value.
10. **Non-destructive resource bound** — incomplete/invalid drafts and entries
    after the first 128 keep round-tripping for repair; runtime surfaces mark
    them unavailable and Settings refuses Add at the active limit.
11. **Install-source preflight** — every desktop/metadata/icon source is checked
    as a readable non-link regular file before build or mutation.
12. **Non-empty prebuilt contract** — zero-byte descriptor input is rejected
    while the prior executable remains intact.
13. **Scoped staging ancestry** — normalized non-root packaging roots are
    checked from `/` through every existing component, rejecting disguised
    symlink roots before install/uninstall while retaining host-prefix
    compatibility; this is not a concurrent-mutation guarantee.
14. **Atomic metadata/SVG install** — explicit mode is applied to sibling temps
    before atomic rename.
15. **Atomic raster icons** — both PNG resolutions use the same commit boundary.
16. **Desktop structure validation** — exact TryExec and canonical Exec counts,
    plus rejection of alternate commands, precede publication.
17. **Unset-PATH resilience** — nounset no longer breaks successful install
    diagnostics.
18. **Hostile-path regression suite** — contract tests exercise empty artifacts,
    staging ancestor links, and public destination links.
19. **Index-neutral private errors** — internal remote helpers no longer label
    every invalid reference as profile #1.
20. **Bounded picker rendering** — 256 rows cap UI work while keeping entry 129
    visible and all omitted drafts stored.
21. **Runnable-only keyboard navigation** — initial selection and arrows skip
    invalid and inactive profiles; Enter still revalidates.
22. **Bounded settings editor** — rendering stops at 256 with an explicit
    retained-off-view count and no vector truncation.
23. **Active selector boundary** — remote file-tree and tab-menu choices stop at
    the first 128 executable profiles.
24. **Safe picker metadata** — deploy/name text passes bounded inline display
    before entering iced widgets.
25. **Actionable save summary** — explicit Save distinguishes invalid active
    drafts from over-limit retained drafts.
26. **Shared problem accounting** — one helper drives those counts and their
    regression assertions.
27. **Actual atomic disk round trip** — save/reload preserves an invalid draft
    and entry 129 exactly.
28. **Neutral fallback regression** — empty runtime identity is displayed as
    “remote host”, never a fabricated index.
29. **Pre-spawn consumer evidence** — remote-fs tests prove unknown, invalid,
    and high-index profiles fail before process creation, while gate tests
    prove oversized/RLO deploy drafts are rejected without raw-value echo.
30. **Documented bounded-draft contract** — README records render limits,
    navigation behavior, save diagnostics, and atomic public assets.

Verification: `bash scripts/test-install-paths.sh`, Frost config tests, and the
full formatting/check/Clippy/test gates.
