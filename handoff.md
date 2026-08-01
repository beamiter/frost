# Engineering handoff

Updated: 2026-08-01

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, configuration, terminal parsing, history, keybindings, and session
persistence.

## Remaining boundaries

### Make Agent restore and callbacks one-shot

Replace snapshot read/remove with an atomic claim and quarantine invalid evidence.
Bind approve/edit/reject and model/execution completions to
`(AgentSessionEpoch, ProposalId)`. Replace
`wrapping_add` generations with checked exhaustion and add stale-event, duplicate-ID,
two-opener, and counter-exhaustion tests.

### Decode session trees while enforcing budgets

The 1 MiB file cap is followed by ordinary Serde construction before session, tab,
tree-depth, cwd, field, and cumulative limits. Implement schema-aware bounded
visitors and adversarial wide-array, deep-tree, and cumulative-text tests.

### Own every helper process through pipe closure

`jsh_version_banner` can wait forever when a descendant inherits stdout, and its
output is unbounded. App-owned `fc-list`, `fc-match`, and notification helpers have
similar PATH/deadline gaps. Resolve trusted helpers, create a process group, drain
stdout/stderr concurrently under byte limits, enforce one deadline, and reap the
whole group.

### Apply one strict link-opening policy

Clickable links need a common HTTP(S)-only policy with authority and no userinfo,
controls, whitespace, bidi, or default-ignorable characters. Replace Windows
`cmd /C start` with an argv-safe opener and bound/reap opener processes. OSC8 is not
yet connected to clicking, but cap URI/id fields and apply the same policy before it
is enabled.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
```
