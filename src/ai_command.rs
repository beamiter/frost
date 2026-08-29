//! Inline AI command generation: natural language → one reviewable shell
//! command, typed at the prompt only after an explicit user action.
//!
//! Ported from anvil `src/ai_palette_ops.rs` (with forge
//! `src/ui/command_palette.rs` as the convergent twin), adapted to frost's
//! overlay idiom: the palette opens a small prompt for the request, a
//! background worker drafts the command through
//! `jterm_core::ai::nl_to_command_with_context_blocking_cancellable`, and the
//! review card's primary action inserts the validated draft at the target
//! pane's prompt through the guarded replace path — the invariant from the
//! source stands verbatim: generated commands never run automatically.

use jterm_core::ai::AiCancellationToken;

/// The natural-language request is prompt-building input, not a command; the
/// core's prompt builder samples it further. This bound keeps overlay state
/// and display rows sane (the sources bound only at display time).
pub(crate) const MAX_SUGGESTION_REQUEST_BYTES: usize = 4 * 1024;

/// Status copy for the card while the worker runs; also the retry affordance's
/// pre-request state.
pub(crate) const DRAFTING_STATUS: &str = "Drafting a command for review…";
pub(crate) const REVIEW_STATUS: &str =
    "Review the proposal below. Nothing has been inserted or run.";

/// Allocate the next suggestion request id from the window's counter.
///
/// The id is the whole of the reply-routing identity: `AiSuggestionResolved`
/// carries nothing else, and an iced `Task` returned from `update` cannot be
/// aborted, so a superseded worker always delivers. Keeping the counter
/// window-wide and strictly increasing is what makes "this reply is for the
/// card in front of me" decidable; a per-card counter that restarted at 1
/// published request A's command onto request B's card. Exhaustion returns
/// `None` (refuse the request) rather than wrapping back onto a live id.
pub(crate) fn next_generation(counter: &mut u64) -> Option<u64> {
    let next = counter.checked_add(1)?;
    *counter = next;
    Some(next)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuggestionPhase {
    /// A worker holds the current generation; the draft row is hidden.
    Drafting,
    /// A generated command is displayed for editing and explicit insert.
    Review,
    /// The last request failed (or was stopped); Regenerate re-arms it.
    Failed,
}

/// One pane-bound suggestion session. frost keeps at most one, mirroring
/// anvil's `CommandSuggestionSession`: opening a new request (or a pane
/// closing) replaces it, and Drop cancels the in-flight worker so a dismissed
/// card's reply can never land.
pub(crate) struct CommandSuggestion {
    /// The app-wide request id this card is currently waiting on. It is
    /// allocated by the window, never by the card: a per-card counter that
    /// restarted at 1 made request A's reply indistinguishable from request
    /// B's, so a superseded worker could publish its command onto the card
    /// that replaced it (anvil keys the same guard on an app-level
    /// `command_suggestion_generation`).
    generation: u64,
    /// Stable terminal-session id whose prompt receives the insert.
    pub(crate) session_id: usize,
    /// The natural-language request, kept verbatim for Regenerate.
    pub(crate) request: String,
    /// Provider display label captured at request time.
    pub(crate) provider: String,
    /// Pane metadata captured at open; Regenerate reuses it verbatim.
    pub(crate) cwd: String,
    pub(crate) shell: String,
    /// Selected-block command/output, attached only under the
    /// `ai_share_command_context` consent (frost's gate; the sources attach
    /// unconditionally on the explicit `?` action).
    pub(crate) block_context: Option<jterm_core::ai::BlockContext>,
    phase: SuggestionPhase,
    /// The review card's editable command text (Review phase).
    pub(crate) draft: String,
    /// Inline card feedback: worker errors, validation refusals, or the reason
    /// an insert was rejected at the PTY boundary.
    pub(crate) feedback: Option<String>,
    cancellation: AiCancellationToken,
}

impl CommandSuggestion {
    /// `generation` comes from the window's monotonic counter; the caller
    /// launches the worker with the same number.
    pub(crate) fn begin(
        generation: u64,
        session_id: usize,
        request: String,
        provider: String,
        cwd: String,
        shell: String,
        block_context: Option<jterm_core::ai::BlockContext>,
    ) -> Option<Self> {
        let request = request.trim().to_string();
        if request.is_empty() || request.len() > MAX_SUGGESTION_REQUEST_BYTES {
            return None;
        }
        let session = Self {
            generation,
            session_id,
            request,
            provider,
            cwd,
            shell,
            block_context,
            phase: SuggestionPhase::Drafting,
            draft: String::new(),
            feedback: None,
            cancellation: AiCancellationToken::new(),
        };
        Some(session)
    }

    /// Whether this card belongs to the pane the user is looking at.
    ///
    /// The card renders, owns Escape, and accepts an insert only when it
    /// does. "Insert for review" resolves the target by `session_id` alone
    /// and sends Ctrl+U plus the generated command, so off-pane it would
    /// clear and retype a prompt the user cannot see — the review-first
    /// promise depends on the reviewed prompt being on screen. ember makes
    /// the same check the first line of its `show()`; frost's own correction
    /// card has always been scoped this way.
    pub(crate) fn is_bound_to_active_pane(&self, active_session_id: Option<usize>) -> bool {
        active_session_id == Some(self.session_id)
    }

    pub(crate) fn phase(&self) -> SuggestionPhase {
        self.phase
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn cancellation(&self) -> AiCancellationToken {
        self.cancellation.clone()
    }

    /// The card's one status line, derived from the phase so a stale callback
    /// can never leave "Drafting" showing beside a finished review.
    pub(crate) fn status(&self) -> &'static str {
        match self.phase {
            SuggestionPhase::Drafting => DRAFTING_STATUS,
            SuggestionPhase::Review => REVIEW_STATUS,
            SuggestionPhase::Failed => "Command suggestion failed; Retry drafts a new proposal.",
        }
    }

    /// A reply may publish only against the live generation of a still-running
    /// request — anvil's `suggestion_reply_is_current`: a stopped or
    /// superseded request's late reply is dropped, never presented.
    fn reply_is_current(&self, generation: u64) -> bool {
        self.phase == SuggestionPhase::Drafting && self.generation == generation
    }

    /// Apply a worker reply. Returns whether the card changed.
    pub(crate) fn apply_reply(&mut self, generation: u64, reply: Result<String, String>) -> bool {
        if !self.reply_is_current(generation) {
            return false;
        }
        match reply {
            Ok(command) => {
                self.draft = command;
                self.phase = SuggestionPhase::Review;
                self.feedback = None;
            }
            Err(error) => {
                self.phase = SuggestionPhase::Failed;
                self.feedback = Some(jterm_core::review_input::safe_inline_display(
                    &error,
                    2 * 1024,
                ));
            }
        }
        true
    }

    /// Re-arm the same request after a failure against a freshly allocated
    /// app-level `generation`. Returns false while a request is in flight, or
    /// if the caller handed back a generation this card has already seen —
    /// the counter only ever moves forward, and a repeat would re-open the
    /// window in which an older worker's reply looks current.
    pub(crate) fn regenerate(&mut self, generation: u64) -> bool {
        if self.phase == SuggestionPhase::Drafting || generation <= self.generation {
            return false;
        }
        self.generation = generation;
        self.phase = SuggestionPhase::Drafting;
        self.draft.clear();
        self.feedback = None;
        true
    }

    /// The draft validated for review-only insertion, with the sources' gate:
    /// `jterm_core::review_input::validate` at its 256 KiB budget, exactly like
    /// anvil's `CommandReviewCard::validated_command`.
    pub(crate) fn validated_insert_command(&self) -> Result<&str, String> {
        if self.phase != SuggestionPhase::Review {
            return Err("no generated command is ready for review".to_string());
        }
        jterm_core::review_input::validate(&self.draft).map_err(|error| error.to_string())
    }

    /// Record why an insert attempt was refused at the prompt boundary.
    pub(crate) fn reject_insert(&mut self, reason: impl Into<String>) {
        self.feedback = Some(format!("Cannot insert: {}", reason.into()));
    }

    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl Drop for CommandSuggestion {
    fn drop(&mut self) {
        // Replacing or dismissing the card always kills the in-flight curl
        // transport, like dropping anvil's `AiHandle`.
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One card, drafted at the id the window would have allocated for it.
    fn begin_at(counter: &mut u64) -> (CommandSuggestion, u64) {
        let generation = next_generation(counter).expect("counter has room");
        let session = CommandSuggestion::begin(
            generation,
            7,
            "  list files modified today  ".into(),
            "Ollama".into(),
            "/tmp".into(),
            "sh".into(),
            None,
        )
        .expect("valid request begins");
        (session, generation)
    }

    fn begin() -> (CommandSuggestion, u64) {
        begin_at(&mut 0)
    }

    #[test]
    fn begin_trims_and_rejects_empty_or_oversize_requests() {
        let (session, generation) = begin();
        assert_eq!(session.request, "list files modified today");
        assert_eq!(session.session_id, 7);
        assert_eq!(session.phase(), SuggestionPhase::Drafting);
        assert_eq!(generation, session.generation());
        let invalid = [
            "   ".to_string(),
            "x".repeat(MAX_SUGGESTION_REQUEST_BYTES + 1),
        ];
        for request in invalid {
            assert!(CommandSuggestion::begin(
                1,
                7,
                request,
                "p".into(),
                ".".into(),
                "sh".into(),
                None
            )
            .is_none());
        }
    }

    /// The card that replaced another must never accept its predecessor's
    /// reply. With a per-card counter both cards were generation 1, so a
    /// superseded worker's command — possibly destructive, always unrelated —
    /// published onto the live card one Enter from the prompt, and the live
    /// request's own reply was then dropped as "not current".
    #[test]
    fn a_superseded_requests_reply_never_publishes_onto_its_successor() {
        let mut counter = 0;
        let (first, first_generation) = begin_at(&mut counter);
        // The window replaces the card (Drop cancels the first worker, which
        // may already be past its cancellation point).
        drop(first);
        let (mut second, second_generation) = begin_at(&mut counter);
        assert_ne!(first_generation, second_generation);

        // Request A's late success and its late cancellation error both miss.
        assert!(!second.apply_reply(
            first_generation,
            Ok("find . -name '*.log' -mtime +7 -delete".into())
        ));
        assert!(!second.apply_reply(first_generation, Err("request cancelled".into())));
        assert_eq!(second.phase(), SuggestionPhase::Drafting);
        assert!(second.draft.is_empty());

        // Request B's own reply still publishes.
        assert!(second.apply_reply(second_generation, Ok("du -sh ./*".into())));
        assert_eq!(second.draft, "du -sh ./*");
    }

    /// The card is pane-bound: with no scope check it painted over an
    /// unrelated tab, stole that tab's Escape, and typed its command into the
    /// off-screen prompt it was bound to.
    #[test]
    fn a_card_belongs_only_to_the_pane_that_asked_for_it() {
        let (session, _) = begin();
        assert_eq!(session.session_id, 7);
        assert!(session.is_bound_to_active_pane(Some(7)));
        assert!(!session.is_bound_to_active_pane(Some(9)));
        // No pane at all (an empty window) is not the bound pane either.
        assert!(!session.is_bound_to_active_pane(None));
    }

    #[test]
    fn generations_are_window_wide_monotonic_and_refuse_to_wrap() {
        let mut counter = 0;
        assert_eq!(next_generation(&mut counter), Some(1));
        assert_eq!(next_generation(&mut counter), Some(2));
        assert_eq!(counter, 2);
        let mut exhausted = u64::MAX;
        assert_eq!(next_generation(&mut exhausted), None);
        assert_eq!(exhausted, u64::MAX);

        // A card only re-arms on an id the window has not used before.
        let (mut session, generation) = begin_at(&mut counter);
        assert!(session.apply_reply(generation, Err("offline".into())));
        assert!(!session.regenerate(generation));
        assert!(!session.regenerate(generation - 1));
        assert_eq!(session.phase(), SuggestionPhase::Failed);
        let next = next_generation(&mut counter).unwrap();
        assert!(session.regenerate(next));
        assert_eq!(session.generation(), next);
    }

    #[test]
    fn success_enters_review_with_the_generated_command() {
        let (mut session, generation) = begin();
        assert!(session.apply_reply(generation, Ok("find . -mtime -1".into())));
        assert_eq!(session.phase(), SuggestionPhase::Review);
        assert_eq!(session.draft, "find . -mtime -1");
        assert_eq!(
            session.validated_insert_command().as_deref(),
            Ok("find . -mtime -1")
        );
    }

    #[test]
    fn stale_or_replayed_replies_never_publish() {
        let (mut session, generation) = begin();
        // A reply for an older generation is dropped while drafting.
        assert!(!session.apply_reply(generation.wrapping_add(1), Ok("ls".into())));
        assert_eq!(session.phase(), SuggestionPhase::Drafting);
        // After publishing, a duplicate reply for the same generation is
        // dropped instead of clobbering the user's edits.
        assert!(session.apply_reply(generation, Ok("ls".into())));
        session.draft = "ls -la".into();
        assert!(!session.apply_reply(generation, Ok("pwd".into())));
        assert_eq!(session.draft, "ls -la");
    }

    #[test]
    fn failed_reply_offers_regenerate_and_regenerate_revalidates() {
        let (mut session, generation) = begin();
        assert!(session.apply_reply(generation, Err("provider offline".into())));
        assert_eq!(session.phase(), SuggestionPhase::Failed);
        assert_eq!(session.feedback.as_deref(), Some("provider offline"));
        assert!(session.validated_insert_command().is_err());
        let next = generation + 1;
        assert!(session.regenerate(next), "failed request can regenerate");
        assert_eq!(session.phase(), SuggestionPhase::Drafting);
        // The failed generation's late reply can no longer publish.
        assert!(!session.apply_reply(generation, Ok("ls".into())));
        assert!(!session.regenerate(next + 1));
    }

    #[test]
    fn insert_validation_is_the_sources_review_gate() {
        let mut counter = 0;
        let (mut session, mut generation) = begin_at(&mut counter);
        for unsafe_command in [
            "echo one\necho two",
            "printf \u{7}",
            "echo safe\u{202e}hidden",
            "   ",
        ] {
            assert!(session.apply_reply(generation, Ok("ls".into())));
            session.draft = unsafe_command.to_string();
            assert!(
                session.validated_insert_command().is_err(),
                "{unsafe_command:?}"
            );
            generation = next_generation(&mut counter).expect("counter has room");
            assert!(session.regenerate(generation), "review can regenerate");
        }
    }

    #[test]
    fn reject_insert_records_the_boundary_reason() {
        let (mut session, generation) = begin();
        assert!(session.apply_reply(generation, Ok("ls".into())));
        session.reject_insert("the prompt already contains input");
        assert_eq!(
            session.feedback.as_deref(),
            Some("Cannot insert: the prompt already contains input")
        );
    }
}
