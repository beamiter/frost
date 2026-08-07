# Engineering handoff

Updated: 2026-08-08

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, configuration, terminal parsing, history, keybindings, and session
persistence. Agent snapshots are now claimed before restore, execution identities are
checked, the jsh identity probe owns its process group, and link opening has one
strict policy.

## Completed since the previous handoff

- Agent restore now consumes `jterm_core::agent::SessionClaim`, backed by one
  atomic no-replace rename rather than the former local hard-link/unlink pair.
  Exactly one concurrent opener restores a valid snapshot; malformed, future,
  oversized, and semantically invalid evidence remains byte-identical at its
  private claim path. An empty or rejected local session still leaves the
  public path alone, so one process exiting cannot delete a newer checkpoint
  published by another.
- Execution generations use `checked_add`; exhaustion seals the session rather
  than reusing an identity a late completion could bind to.
- Model requests carry `{session epoch, request generation}` through both
  blocking and streaming callbacks. New Task, restore, reopen, and cancellation
  replace that identity before any late reply can mutate the current transcript.
- `jsh_version_banner` now starts the probe in its own process group, drains a
  byte-bounded banner on a concurrent reader, enforces one deadline, and
  signals and reaps the whole group. A shell that daemonises a descendant
  holding the probe's stdout can no longer make the probe wait forever, and the
  banner itself is bounded.
- One link-opening policy: `is_openable_url` accepts only absolute HTTP(S) URLs
  with an authority and no userinfo, controls, whitespace, backslash, or
  visually ambiguous characters. Links open through a non-user-writable
  absolute opener — the Windows `cmd /C start` path is gone — file operands
  follow `--`, and the opener process is reaped.
- `scripts/install.sh` and `scripts/uninstall.sh` now derive the default binary
  from the same `PREFIX/bin` contract (`~/.local/bin` by default). Explicit
  `--prefix`, `--bin-dir`, and `DESTDIR` keep their existing meanings, including
  runtime launcher paths inside staged packages. A temporary-HOME dry-run suite
  covers default reinstall/uninstall, explicit overrides, and staged paths, and
  CI checks the scripts with Bash and ShellCheck.
- Completed block outcomes now delegate to
  `jterm_core::block_contract::classify_completed` using the command text already
  resolved and stored on each completed zone. Frost keeps its renderer enum and
  serialized zone shape, while failed navigation, stepping, paint, and scrollbar
  markers share the same four-way result. A commandless zone with a raw non-zero
  status remains background, and a command without a reported status remains
  unknown. `jterm_core` is pinned to
  `fd25f905aadab9d8ca111a67b9b6422a22ef2d6c` (transitively jagent
  `3aece307766ca8f3ca33ed0376d2a271cc2322b3`).

## Remaining boundaries

### Decode session trees while enforcing budgets

The 1 MiB file cap is still followed by ordinary Serde construction before
session, tab, tree-depth, cwd, field, and cumulative limits apply. Implement
schema-aware bounded visitors — anvil's `src/session.rs` now has a worked
example for the same shape — and add adversarial wide-array, deep-tree, and
cumulative-text tests.

### Own the remaining app-owned helpers

`jsh_version_banner` is now group-owned and bounded, but the app-owned
`fc-list`, `fc-match`, and notification helpers still resolve through a mutable
`PATH` and read unbounded output without a deadline. Give them the same
treatment: a trusted absolute program, a process group, concurrent bounded
drains of stdout and stderr, one deadline, and a reaped group.

### Connect OSC 8 to clicking, or keep it inert deliberately

OSC 8 targets are parsed but not clickable. When that changes, route them
through `link::is_openable_url` — it is now the single policy — and cap the URI
and id fields before interning them.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
```
