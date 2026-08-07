# Engineering handoff

Updated: 2026-08-08

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, configuration, terminal parsing, history, keybindings, and session
persistence. Agent snapshots are now claimed atomically, execution identities are
checked, the jsh identity probe owns its process group, and link opening has one
strict policy.

## Completed since the previous handoff

- `src/persistence.rs` gained `claim_exclusive`, a no-clobber hard-link/unlink
  claim, and `src/agent.rs` restores through it. Exactly one opener ever
  observes the snapshot, and evidence that cannot become a session is left at
  the claim path instead of being deleted.
- Execution generations use `checked_add`; exhaustion seals the session rather
  than reusing an identity a late completion could bind to.
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
  `9e79a5bf0d905575863def4d0e77f74a1f533638` with jagent unchanged transitively.

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

### Bind Agent callbacks to the session epoch

Approve/edit/reject resolve against the live session in the same frame, and the
execution generation is now checked, so a stale *execution* completion is
already rejected. A model completion still relies on the cancellation token;
carry `AgentSessionEpoch` into the in-flight request so a reply for a previous
task cannot be accepted after New Task or a restore.

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
