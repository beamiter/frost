# Engineering handoff

Updated: 2026-08-01

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

## Remaining boundaries

### Decode session trees while enforcing budgets

The 1 MiB file cap is still followed by ordinary Serde construction before
session, tab, tree-depth, cwd, field, and cumulative limits apply. Implement
schema-aware bounded visitors — jterm1's `src/session.rs` now has a worked
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
```
