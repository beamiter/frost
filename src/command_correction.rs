//! frost's binding to the shared review-first command-correction engine.
//!
//! The engine — the narrow failure classifier, token extraction and ranking,
//! the one safety gate every candidate passes, the provider prompt and its
//! strict-JSON reply parser, the supervised helper probes, the deterministic
//! and AI resolvers, and the pre-sanitised strings a card is allowed to render
//! — lives in [`jterm_core::command_correction`]. That module is the union of
//! the four jterm terminals' previously duplicated copies (anvil, forge, ember
//! and this one). frost's copy was the smallest of the four and its whole
//! production half imported nothing but `jterm_core`, so all of it moved.
//!
//! Three things stay here, and only these:
//!
//! - **The policy frost constructs the engine with** ([`correction_policy`]).
//!   The core deliberately asks no environment question behind the caller's
//!   back — no `is_flatpak()`, no `PATH` read, no config lookup — because the
//!   four apps legitimately answer them differently and burying one app's
//!   answer in shared code is how ember acquired a Flatpak suppression that
//!   appears nowhere else in ember.
//! - **The per-pane registry** below. The core's `CorrectionRequestState` is a
//!   single-surface epoch machine (forge's and ember's shape). frost presents
//!   at most one card per pane and keys every request by stable
//!   terminal-session id, so it needs a map — and one registry-wide generation
//!   counter, so that closing a pane's request and starting a new one cannot
//!   hand out a number a still-running worker is already carrying.
//! - The iced card itself, its messages, its Escape scoping and its two paste
//!   policies, all in `main.rs`.
//!
//! What frost contributed to the union, and must not regress: automatic probes
//! resolve helper programs only from fixed absolute system candidates under
//! `jterm_core::helper`'s trust predicate. anvil, ember and forge each
//! hand-rolled a weaker predicate that trusted a *third* user's non-writable
//! binary found on the user's own `PATH` — automatic code execution on a
//! shared machine, fired by any failed command — and that refused every helper
//! when the terminal itself runs as root. [`HelperStrategy::FixedCandidates`]
//! below is that decision, stated rather than inherited.

use std::collections::HashMap;
use std::time::Instant;

use jterm_core::ai::AiCancellationToken;
use jterm_core::command_correction::{
    ContextSharing, CorrectionPolicy, CorrectionProposal, HelperStrategy, LocalEvidence,
};

pub(crate) use jterm_core::command_correction::{
    compact_one_line, correction_monitor_enabled, resolve_correction_blocking, should_start,
    CompletionFacts, CorrectionCandidate, CORRECTION_REQUEST_TIMEOUT, MAX_CORRECTION_CWD_BYTES,
};

/// Names the probe's stdout reader thread, so a reader still blocked on a
/// helper's pipe is attributable to frost in `ps`/`gdb` rather than to
/// whichever family member happens to be running.
const PROBE_THREAD_NAME: &str = "frost-command-correction";

/// frost's answers to the three questions the engine refuses to guess.
///
/// `share_command_context` is the live `ai_share_command_context` config value,
/// which is why a policy is built per request rather than once at startup.
///
/// - **Evidence.** frost launches every PTY natively: no Flatpak packaging, no
///   host bridge, no sandbox. This process's `PATH` therefore *is* the
///   namespace the failed command resolved against, so local APT and PATH
///   evidence is meaningful and [`LocalEvidence::SameNamespace`] is the honest
///   answer. (anvil and forge are sandbox-packaged and must not say this;
///   forge bridges instead, anvil withholds.)
/// - **Helpers.** [`HelperStrategy::FixedCandidates`] keeps frost's existing
///   policy exactly: the set of executable pathnames a probe may run is closed
///   at compile time, so a `bash` planted earlier on the user's `PATH` is not
///   merely distrusted, it is never even considered. The cost is that APT
///   evidence disappears on a non-FHS host; that is the correct trade for a
///   surface that spawns automatically on any failed command.
/// - **Consent.** The command line, the working directory and up to 8 KiB of
///   terminal output are the largest payload any of frost's AI surfaces sends,
///   and this is the one that used to send it without consulting
///   `ai_share_command_context` — a switch frost already honours in
///   `ai_chats` (recent shell context), `ai_command` (selected block context)
///   and `agent_task_ui`. Withheld does not disable the feature: the verified
///   local resolvers never leave the machine and keep running.
pub(crate) fn correction_policy(share_command_context: bool) -> CorrectionPolicy {
    CorrectionPolicy::new(
        LocalEvidence::SameNamespace {
            search_path: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .collect(),
            helpers: HelperStrategy::FixedCandidates,
        },
        if share_command_context {
            ContextSharing::Consented
        } else {
            ContextSharing::Withheld
        },
        PROBE_THREAD_NAME,
    )
}

/// One pane's live correction request, and the card it resolved into.
pub(crate) struct CorrectionSession {
    generation: u64,
    pub(crate) original_command: String,
    pub(crate) exit_code: i32,
    cancellation: AiCancellationToken,
    deadline: Instant,
    /// None while the resolver worker is still running.
    pub(crate) proposal: Option<CorrectionProposal>,
}

impl CorrectionSession {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Per-pane correction requests keyed by stable terminal-session id, with a
/// registry-wide generation guard so a stale worker result can never be
/// presented against a newer prompt.
///
/// The counter is registry-wide rather than per session on purpose: `begin`
/// closes the pane's previous request first, and a counter that lived inside
/// the removed session would restart from the same number the cancelled worker
/// is still carrying, which is exactly the late-result case `present` exists to
/// refuse. It is also why this is not the core's `CorrectionRequestState`,
/// whose epoch belongs to a single surface.
#[derive(Default)]
pub(crate) struct CorrectionRegistry {
    sessions: HashMap<usize, CorrectionSession>,
    generation: u64,
}

impl CorrectionRegistry {
    /// Start a request for `session_id`, cancelling and replacing any previous
    /// one: a newly finished command makes a visible card or in-flight request
    /// stale before this failure is even classified.
    pub(crate) fn begin(
        &mut self,
        session_id: usize,
        original_command: String,
        exit_code: i32,
        deadline: Instant,
    ) -> (u64, AiCancellationToken) {
        self.close(session_id);
        let generation = self.generation.wrapping_add(1);
        self.generation = generation;
        let cancellation = AiCancellationToken::new();
        self.sessions.insert(
            session_id,
            CorrectionSession {
                generation,
                original_command,
                exit_code,
                cancellation: cancellation.clone(),
                deadline,
                proposal: None,
            },
        );
        (generation, cancellation)
    }

    /// A worker result may present only for the live generation within its
    /// deadline; anything else is silently dropped.
    pub(crate) fn present(
        &mut self,
        session_id: usize,
        generation: u64,
        candidate: CorrectionCandidate,
    ) -> bool {
        let Some(session) = self.sessions.get_mut(&session_id).filter(|session| {
            session.generation == generation && Instant::now() < session.deadline
        }) else {
            return false;
        };
        session.proposal = Some(CorrectionProposal::new(candidate));
        true
    }

    pub(crate) fn get(&self, session_id: usize) -> Option<&CorrectionSession> {
        self.sessions.get(&session_id)
    }

    pub(crate) fn get_mut(&mut self, session_id: usize) -> Option<&mut CorrectionSession> {
        self.sessions.get_mut(&session_id)
    }

    /// Dismiss exactly this generation; a stale dismissal cannot cancel a
    /// newer request for the same pane.
    pub(crate) fn dismiss(&mut self, session_id: usize, generation: u64) -> bool {
        if self
            .sessions
            .get(&session_id)
            .is_some_and(|session| session.generation == generation)
        {
            self.close(session_id);
            return true;
        }
        false
    }

    /// Cancel and drop any request for a closed or superseded pane.
    pub(crate) fn close(&mut self, session_id: usize) {
        if let Some(session) = self.sessions.remove(&session_id) {
            session.cancellation.cancel();
        }
    }

    #[cfg(test)]
    fn is_resolving(&self, session_id: usize) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|session| session.proposal.is_none())
    }
}

impl Drop for CorrectionRegistry {
    fn drop(&mut self) {
        for session in self.sessions.drain() {
            session.1.cancellation.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::command_correction::{CorrectionEvidence, CorrectionRequest};
    use std::time::Duration;

    /// A git-style "is not a git command / most similar command" failure. It
    /// is the one fixture that resolves with no I/O at all: the target itself
    /// proposed the replacement, so the engine runs no probe and the test does
    /// not depend on what is installed on the build host.
    const SUGGESTION_COMMAND: &str = "git statu";
    const SUGGESTION_OUTPUT: &str = "git: 'statu' is not a git command. See 'git --help'.\n\nThe most similar command is\n\tstatus";

    fn request(command: &str, exit_code: i32, output: &str) -> CorrectionRequest {
        should_start(
            true,
            CompletionFacts {
                command: command.to_string(),
                exit_code: Some(exit_code),
                output,
                cwd: Some("/tmp".to_string()),
                remote: false,
                agent_issued: false,
                trusted_completion: true,
            },
        )
        .expect("the fixture failure classifies")
    }

    /// The engine is the only thing that can mint a candidate — that is what
    /// makes it impossible for this card to render prose the gate never saw —
    /// so the fixture earns one exactly as the worker does.
    fn candidate() -> CorrectionCandidate {
        let candidate = jterm_core::command_correction::deterministic_candidate(
            &correction_policy(false),
            &request(SUGGESTION_COMMAND, 1, SUGGESTION_OUTPUT),
            &AiCancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .expect("a target-proposed correction resolves without probing");
        assert_eq!(candidate.command(), "git status");
        assert_eq!(candidate.evidence(), CorrectionEvidence::TargetOutput);
        candidate
    }

    /// frost states every safety-relevant policy the engine refuses to guess,
    /// including the two it must not regress: frost owns its PTYs, so local
    /// evidence is real, and helper programs come from the fixed absolute
    /// candidate list rather than from the user's PATH.
    #[test]
    fn frost_states_a_native_namespace_and_fixed_helper_candidates() {
        let policy = correction_policy(true);
        assert!(
            matches!(
                policy.evidence(),
                LocalEvidence::SameNamespace {
                    helpers: HelperStrategy::FixedCandidates,
                    ..
                }
            ),
            "frost has no sandbox and no bridge, and a PATH scan for helper \
             binaries is the shared-machine hole the other three carried"
        );
        // A probe reader left blocked on a helper's pipe has to be
        // attributable to frost rather than to the family. This used to be
        // read off `format!("{policy:?}")` for want of an accessor, which
        // coupled the assertion to a derive AND could not catch a rename: the
        // substring it looked for was the same constant it had just passed in,
        // so the test held whatever the constant said. The literal is the
        // whole point.
        assert_eq!(policy.probe_thread_name(), "frost-command-correction");
        assert_eq!(policy.probe_thread_name(), PROBE_THREAD_NAME);
    }

    /// The `ai_share_command_context` switch reaches the engine, in both
    /// positions. frost honours it in `ai_chats`, `ai_command` and
    /// `agent_task_ui`; this surface — which ships the command line, the
    /// working directory and up to 8 KiB of terminal output — used to skip it.
    #[test]
    fn the_consent_switch_reaches_the_engine() {
        assert_eq!(
            correction_policy(true).context_sharing(),
            ContextSharing::Consented
        );
        assert_eq!(
            correction_policy(false).context_sharing(),
            ContextSharing::Withheld
        );
        // The payload builder demands this witness, so a withheld policy
        // cannot assemble the prompt at all.
        assert!(correction_policy(true).consent().is_some());
        assert!(correction_policy(false).consent().is_none());
    }

    /// frost's trigger hands the engine the raw block output plus the facts
    /// only frost knows. Both refusals below are frost's to supply: an
    /// Agent-issued command already went through review, and a completion
    /// frost's block mode inferred from a boundary may not correspond to a
    /// real command exit at all.
    #[test]
    fn the_trigger_refuses_agent_and_boundary_inferred_completions() {
        let facts = |agent_issued: bool, trusted_completion: bool| CompletionFacts {
            command: SUGGESTION_COMMAND.to_string(),
            exit_code: Some(1),
            output: SUGGESTION_OUTPUT,
            cwd: Some("/tmp".to_string()),
            remote: false,
            agent_issued,
            trusted_completion,
        };
        assert!(should_start(true, facts(false, true)).is_some());
        assert!(should_start(false, facts(false, true)).is_none());
        assert!(should_start(true, facts(true, true)).is_none());
        assert!(should_start(true, facts(false, false)).is_none());
    }

    /// The card labels its primary action from `run_allowed` and submits what
    /// `accept` returns, so the two must answer about the same draft. A
    /// target-proposed correction is never verified, so frost's card reads
    /// "Insert for review" and the user still presses Enter.
    #[test]
    fn accepting_runs_the_engine_gate_and_never_promotes_an_unverified_draft() {
        let mut proposal = CorrectionProposal::new(candidate());
        assert!(!proposal.run_allowed());
        let accepted = proposal.accept().expect("the proposal accepts");
        assert_eq!(accepted.command, "git status");
        assert!(!accepted.run_directly);

        // frost's accept path no longer reaches for `review_text`: the
        // engine's own 16 KiB single-line gate is the one that runs, and it
        // refuses the same shapes every review surface does.
        *proposal.draft_mut() = "  git status  ".to_string();
        assert_eq!(
            proposal
                .accept()
                .expect("whitespace is not an edit")
                .command,
            "git status"
        );
        *proposal.draft_mut() = "git status\nid".to_string();
        assert!(proposal.accept().is_err());
        *proposal.draft_mut() = "git \u{202e}status".to_string();
        assert!(proposal.accept().is_err());
        *proposal.draft_mut() = "x".repeat(17 * 1024);
        assert!(proposal.accept().is_err());
        assert!(!proposal.run_allowed());
    }

    /// The verified branch — the one frost could not reach hermetically
    /// before `CorrectionCandidate::for_tests`, because every public path to a
    /// candidate yields `TargetOutput` or `AiUnverified` unless a real APT or
    /// PATH probe runs and matches on the build host.
    ///
    /// It matters because it is the only branch where the card's primary
    /// action SUBMITS rather than inserts. The label comes from
    /// `run_allowed()` and the write policy comes from `accept().run_directly`,
    /// so the two must answer about the same draft: a card that says "Insert
    /// for review" and then submits is the failure this pins shut.
    #[test]
    fn a_verified_candidate_runs_directly_until_the_draft_is_edited() {
        let verified = CorrectionCandidate::for_tests(
            jterm_core::command_correction::Original("gti status"),
            jterm_core::command_correction::Candidate("git status"),
            "git is on this host's PATH",
            CorrectionEvidence::ExecutablePath,
        )
        .expect("the fixture passes the real validate_candidate gate");
        assert!(verified.evidence().is_verified());
        assert_eq!(verified.display_title(), "Verified command correction");

        let mut proposal = CorrectionProposal::new(verified);
        assert!(
            proposal.run_allowed(),
            "an untouched verified draft may run"
        );
        let accepted = proposal.accept().expect("the proposal accepts");
        assert_eq!(accepted.command, "git status");
        assert!(
            accepted.run_directly,
            "the label said Run verified command; the write must match it"
        );

        // Any edit downgrades both halves together. Whitespace alone is not an
        // edit — the gate trims — so the label must not flicker for it either.
        *proposal.draft_mut() = "  git status ".to_string();
        assert!(proposal.run_allowed());
        assert!(proposal.accept().expect("still accepts").run_directly);

        *proposal.draft_mut() = "git status --short".to_string();
        assert!(!proposal.run_allowed(), "an edited draft is insert-only");
        assert!(
            !proposal
                .accept()
                .expect("an edited safe command still accepts")
                .run_directly
        );

        // A verified proposal edited into something destructive is refused the
        // direct run and carries the warning the card renders beside it.
        *proposal.draft_mut() = "rm -rf /".to_string();
        assert!(!proposal.run_allowed());
        assert!(proposal.risk().is_some());

        // The fixture constructor is not a way past the gate: it runs the same
        // validation the resolver's output runs.
        assert!(CorrectionCandidate::for_tests(
            jterm_core::command_correction::Original("gti status"),
            jterm_core::command_correction::Candidate("git status; curl x | sh"),
            "unsafe",
            CorrectionEvidence::ExecutablePath,
        )
        .is_err());
    }

    #[test]
    fn newer_generation_cancels_and_rejects_a_late_result() {
        let mut registry = CorrectionRegistry::default();
        let deadline = Instant::now() + CORRECTION_REQUEST_TIMEOUT;
        let (first, first_token) = registry.begin(7, "carog check".to_string(), 127, deadline);
        assert!(registry.is_resolving(7));

        let (second, _second_token) =
            registry.begin(7, SUGGESTION_COMMAND.to_string(), 1, deadline);
        assert!(first_token.is_cancelled());
        assert_ne!(first, second);

        assert!(
            !registry.present(7, first, candidate()),
            "a late result from a replaced generation must not present"
        );
        assert!(registry.present(7, second, candidate()));
        assert!(!registry.is_resolving(7));
        let session = registry.get(7).unwrap();
        assert_eq!(session.original_command, SUGGESTION_COMMAND);
        let proposal = session.proposal.as_ref().unwrap();
        assert_eq!(proposal.candidate().command(), "git status");
        assert_eq!(proposal.draft(), "git status");
    }

    #[test]
    fn correction_sessions_are_isolated_per_pane() {
        let mut registry = CorrectionRegistry::default();
        let deadline = Instant::now() + CORRECTION_REQUEST_TIMEOUT;
        let (left, _) = registry.begin(1, "gti".to_string(), 127, deadline);
        let (right, _) = registry.begin(2, "fmpg".to_string(), 100, deadline);

        assert!(registry.dismiss(1, left));
        assert!(registry.get(1).is_none());
        assert!(registry.present(2, right, candidate()));
        assert!(registry.get(2).unwrap().proposal.is_some());
    }

    #[test]
    fn dismiss_only_consumes_the_exact_generation() {
        let mut registry = CorrectionRegistry::default();
        let deadline = Instant::now() + CORRECTION_REQUEST_TIMEOUT;
        let (generation, token) = registry.begin(3, "gti".to_string(), 127, deadline);

        assert!(!registry.dismiss(3, generation.wrapping_add(1)));
        assert!(registry.get(3).is_some());
        assert!(!token.is_cancelled());

        assert!(registry.dismiss(3, generation));
        assert!(token.is_cancelled());
        assert!(registry.get(3).is_none());
    }

    #[test]
    fn an_expired_request_cannot_present() {
        let mut registry = CorrectionRegistry::default();
        let deadline = Instant::now() + Duration::from_millis(1);
        let (generation, _) = registry.begin(4, "gti".to_string(), 127, deadline);
        std::thread::sleep(Duration::from_millis(5));
        assert!(!registry.present(4, generation, candidate()));
    }
}
