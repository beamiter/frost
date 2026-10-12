//! Read-only terminal facts mapped into the shared live attention vocabulary.
//! No submitted-input history, clock, or greeting policy is stored here.
use jterm_core::organism_live::LiveActivityKnowledge;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ActivityFacts {
    pub running: bool,
    pub prompt_ready: bool,
    pub input_tainted: bool,
    pub input_dirty: bool,
    pub submission_pending: bool,
    pub cancel_pending: bool,
    pub typeahead_pending: bool,
    pub agent_armed: bool,
    pub agent_active: bool,
}

impl ActivityFacts {
    fn can_check_prompt(self) -> bool {
        !self.running
            && !self.input_tainted
            && !self.input_dirty
            && !self.submission_pending
            && !self.cancel_pending
            && !self.typeahead_pending
            && !self.agent_armed
            && !self.agent_active
    }

    /// The existing prompt capture may scan cells. Do not perform it while a
    /// cheap, authoritative field already prevents positive idle evidence.
    pub fn with_prompt_ready(mut self, check: impl FnOnce() -> bool) -> Self {
        self.prompt_ready = self.can_check_prompt() && check();
        self
    }

    pub fn knowledge(self) -> LiveActivityKnowledge {
        if self.running {
            LiveActivityKnowledge::Busy
        } else if self.can_check_prompt() && self.prompt_ready {
            LiveActivityKnowledge::KnownIdle
        } else {
            LiveActivityKnowledge::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::organism_daily::PreviewPose;
    use jterm_core::organism_live::{
        LiveAttention, LiveAttentionEvent, LiveAttentionInput, LiveAttentionOutcome,
        LiveAttentionSource,
    };
    use std::time::Duration;

    fn facts(bits: u8) -> ActivityFacts {
        ActivityFacts {
            prompt_ready: true,
            input_tainted: bits & 1 != 0,
            input_dirty: bits & 2 != 0,
            submission_pending: bits & 4 != 0,
            cancel_pending: bits & 8 != 0,
            typeahead_pending: bits & 16 != 0,
            agent_armed: bits & 32 != 0,
            agent_active: bits & 64 != 0,
            ..ActivityFacts::default()
        }
    }

    #[test]
    fn positive_running_is_busy_and_every_unclean_combination_is_unknown() {
        assert_eq!(
            ActivityFacts::default().knowledge(),
            LiveActivityKnowledge::Unknown
        );
        for bits in 0..128 {
            let facts = facts(bits);
            assert_eq!(
                facts.knowledge(),
                if bits == 0 {
                    LiveActivityKnowledge::KnownIdle
                } else {
                    LiveActivityKnowledge::Unknown
                }
            );
            assert_eq!(
                ActivityFacts {
                    running: true,
                    ..facts
                }
                .knowledge(),
                LiveActivityKnowledge::Busy
            );
        }
    }

    #[test]
    fn cheap_blockers_short_circuit_the_real_prompt_capture() {
        for bits in 1..128 {
            let observed = facts(bits).with_prompt_ready(|| panic!("blocked capture must not run"));
            assert_eq!(observed.knowledge(), LiveActivityKnowledge::Unknown);
        }
        let running = ActivityFacts {
            running: true,
            ..ActivityFacts::default()
        }
        .with_prompt_ready(|| panic!("running capture must not run"));
        assert_eq!(running.knowledge(), LiveActivityKnowledge::Busy);
        assert_eq!(
            facts(0).with_prompt_ready(|| false).knowledge(),
            LiveActivityKnowledge::Unknown
        );
        assert_eq!(
            facts(0).with_prompt_ready(|| true).knowledge(),
            LiveActivityKnowledge::KnownIdle
        );
    }

    #[test]
    fn mapped_facts_use_actual_shared_pointer_and_explicit_admission() {
        for facts in [
            ActivityFacts::default(),
            facts(0),
            ActivityFacts {
                running: true,
                ..facts(0)
            },
        ] {
            let mut live = LiveAttention::default();
            live.set_owner(Some(1));
            let input = LiveAttentionInput {
                base_context: PreviewPose::Calm.context(),
                hard_eligible: true,
                hover_available: true,
                pointer_inside: true,
                activity: facts.knowledge(),
            };
            let ticket = live.entry_offer().unwrap();
            live.advance(
                Duration::ZERO,
                &1,
                input,
                LiveAttentionEvent::PointerEntered { ticket },
            )
            .unwrap();
            let due = live
                .advance(
                    Duration::from_millis(600),
                    &1,
                    input,
                    LiveAttentionEvent::Reconcile,
                )
                .unwrap();
            assert_eq!(
                due.active.is_some(),
                facts.knowledge() == LiveActivityKnowledge::KnownIdle
            );
            let mut explicit = LiveAttention::default();
            explicit.set_owner(Some(1));
            let result = explicit
                .advance(Duration::ZERO, &1, input, LiveAttentionEvent::Explicit)
                .unwrap();
            assert_eq!(
                result.outcome == LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit),
                !facts.running
            );
        }
    }
    #[test]
    fn observed_unknown_output_retires_candidate_without_stationary_replay() {
        let mut live = LiveAttention::default();
        live.set_owner(Some(1));
        let mut input = LiveAttentionInput {
            base_context: PreviewPose::Calm.context(),
            hard_eligible: true,
            hover_available: true,
            pointer_inside: true,
            activity: facts(0).knowledge(),
        };
        let ticket = live.entry_offer().unwrap();
        live.advance(
            Duration::ZERO,
            &1,
            input,
            LiveAttentionEvent::PointerEntered { ticket },
        )
        .unwrap();
        input.activity = ActivityFacts::default().knowledge();
        let lost = live
            .advance(
                Duration::from_millis(599),
                &1,
                input,
                LiveAttentionEvent::Reconcile,
            )
            .unwrap();
        assert!(lost.candidate.is_none());
        input.activity = facts(0).knowledge();
        let restored = live
            .advance(
                Duration::from_millis(601),
                &1,
                input,
                LiveAttentionEvent::Reconcile,
            )
            .unwrap();
        assert!(restored.active.is_none());
        assert!(restored.candidate.is_none());
        assert_eq!(restored.entry, Some(ticket));
        let explicit = live
            .advance(
                Duration::from_millis(601),
                &1,
                input,
                LiveAttentionEvent::Explicit,
            )
            .unwrap();
        assert_eq!(
            explicit.outcome,
            LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit)
        );
    }
}
