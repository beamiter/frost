//! Pure description of Frost's existing organism wake requests.
//!
//! This does not run clocks or suppress terminal/agent events. Hosts supply
//! current visibility, owner admission and finite absolute deadlines.
use jterm_core::organism_motion::Motion;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnimationAudience {
    pub live: bool,
    pub preview: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeriodicReason {
    Animation(AnimationAudience),
    Maintenance,
}

impl PeriodicReason {
    pub const fn interval(self) -> Duration {
        match self {
            Self::Animation(_) => Duration::from_millis(100),
            Self::Maintenance => Duration::from_millis(900),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScheduleFacts {
    pub motion: Motion,
    pub focused: bool,
    /// Existing layout.presentable(): already includes enabled/layout gates.
    pub live_presentable: bool,
    /// Existing native_visible: painted body/status surface, not owner proof.
    pub native_visible: bool,
    /// Existing organism_owner(): already excludes remote/alt/non-input owner.
    pub live_owner: bool,
    /// Existing focused/settings/editor preview gate. Disabled live preview
    /// remains allowed, exactly as before; this is not an enabled flag.
    pub preview_visible: bool,
    pub preview_animation_enabled: bool,
}

/// At most one physical periodic subscription. Animation already coalesces
/// maintenance, so never add a second 900ms timer beside it.
pub fn periodic(facts: ScheduleFacts) -> Option<PeriodicReason> {
    let live_visible = facts.focused && facts.live_presentable && facts.native_visible;
    let full = facts.motion == Motion::Full;
    let audience = AnimationAudience {
        live: full && live_visible && facts.live_owner,
        preview: full && facts.focused && facts.preview_visible && facts.preview_animation_enabled,
    };
    if audience.live || audience.preview {
        Some(PeriodicReason::Animation(audience))
    } else if live_visible {
        Some(PeriodicReason::Maintenance)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeadlineReason {
    PreviewBoundary,
    PreviewGreeting,
    PreviewCooldown,
    RetreatExpiry,
    CompletionExpiry,
    LiveAttention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeadlineRequest<T = Duration> {
    pub at: T,
    pub reason: DeadlineReason,
    /// Host-owned transient identity, never terminal/session content.
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeadlineWake<T = Duration> {
    pub at: T,
    /// Equal timestamps share a wake, retaining each reason/identity.
    pub obligations: Vec<DeadlineRequest<T>>,
}

/// Only already-gated absolute deadlines enter this planner. No periodic
/// policy can cancel a semantic expiry. Each returned timestamp is unchanged.
/// Six optional slots bound allocation and make duplicate deadlines explicit.
/// This plans future timers only; it does not consume expired obligations.
/// The host retains its existing update/read-time expiry reconciliation.
/// Reason and generation values attribute delivery; they do not authorize
/// retiring state or change how the existing reducer handles a stale wake.
pub fn deadlines<T: Copy + Ord>(
    now: T,
    requests: [Option<DeadlineRequest<T>>; 6],
) -> Vec<DeadlineWake<T>> {
    let mut wakes: Vec<DeadlineWake<T>> = Vec::with_capacity(6);
    for request in requests
        .into_iter()
        .flatten()
        .filter(|request| request.at > now)
    {
        if let Some(wake) = wakes.iter_mut().find(|wake| wake.at == request.at) {
            if !wake.obligations.contains(&request) {
                wake.obligations.push(request);
            }
        } else {
            wakes.push(DeadlineWake {
                at: request.at,
                obligations: vec![request],
            });
        }
    }
    wakes.sort_by_key(|wake| wake.at);
    wakes
}

/// Pure contract helper only; production retains its existing expiry reducer.
/// A dispatched one-shot matches only the exact current obligation.
/// Earlier unrelated timers cannot retire a newer completion/greeting hold.
/// `current` is the retained registered obligation, NOT a future-only
/// next_deadline(now) query: a just-due obligation must still be accepted.
#[cfg(test)]
pub fn deadline_current<T: PartialEq>(
    dispatched: DeadlineRequest<T>,
    current: Option<DeadlineRequest<T>>,
) -> bool {
    current == Some(dispatched)
}

/// Attribution attached to an existing organism subscription delivery.
/// This value neither samples a clock nor performs expiry reconciliation.
#[derive(Debug, Clone)]
pub enum WakeCause {
    Periodic(PeriodicReason),
    Deadline(DeadlineWake<Instant>),
}

/// Content-free counts of organism wake reasons, not application-wide wakes.
/// A coalesced wake may increment several reasons, so these are not additive
/// physical-wake totals. Maintenance counts the explicit 900ms reason only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WakeCounterSnapshot {
    pub live_animation: u64,
    pub preview_animation: u64,
    pub maintenance: u64,
    pub preview_boundary: u64,
    pub preview_greeting: u64,
    pub preview_cooldown: u64,
    pub retreat_expiry: u64,
    pub completion_expiry: u64,
    pub live_attention: u64,
}

/// Fixed-size saturating diagnostics. No history, identities, timestamps,
/// terminal content, logging, timer creation, or expiry consumption is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WakeCounters {
    counts: WakeCounterSnapshot,
}

impl WakeCounters {
    /// Record one delivered organism wake. Each represented reason increments
    /// at most once, including when several generations share its deadline.
    pub fn record(&mut self, cause: &WakeCause) {
        match cause {
            WakeCause::Periodic(PeriodicReason::Animation(audience)) => {
                if audience.live {
                    self.counts.live_animation = self.counts.live_animation.saturating_add(1);
                }
                if audience.preview {
                    self.counts.preview_animation = self.counts.preview_animation.saturating_add(1);
                }
            }
            WakeCause::Periodic(PeriodicReason::Maintenance) => {
                self.counts.maintenance = self.counts.maintenance.saturating_add(1);
            }
            WakeCause::Deadline(wake) => {
                // A fixed list avoids allocation and counts each reason once
                // without discarding any obligation from the actual wake.
                for reason in [
                    DeadlineReason::PreviewBoundary,
                    DeadlineReason::PreviewGreeting,
                    DeadlineReason::PreviewCooldown,
                    DeadlineReason::RetreatExpiry,
                    DeadlineReason::CompletionExpiry,
                    DeadlineReason::LiveAttention,
                ] {
                    if wake
                        .obligations
                        .iter()
                        .any(|request| request.reason == reason)
                    {
                        let count = match reason {
                            DeadlineReason::PreviewBoundary => &mut self.counts.preview_boundary,
                            DeadlineReason::PreviewGreeting => &mut self.counts.preview_greeting,
                            DeadlineReason::PreviewCooldown => &mut self.counts.preview_cooldown,
                            DeadlineReason::RetreatExpiry => &mut self.counts.retreat_expiry,
                            DeadlineReason::CompletionExpiry => &mut self.counts.completion_expiry,
                            DeadlineReason::LiveAttention => &mut self.counts.live_attention,
                        };
                        *count = count.saturating_add(1);
                    }
                }
            }
        }
    }

    pub const fn snapshot(&self) -> WakeCounterSnapshot {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(motion: Motion) -> ScheduleFacts {
        ScheduleFacts {
            motion,
            focused: true,
            live_presentable: true,
            native_visible: true,
            live_owner: true,
            preview_visible: false,
            preview_animation_enabled: true,
        }
    }

    #[test]
    fn exhaustive_192_cases_preserve_existing_physical_cadence() {
        for motion in [Motion::Full, Motion::Calm, Motion::Static] {
            for bits in 0..64 {
                let facts = ScheduleFacts {
                    motion,
                    focused: bits & 1 != 0,
                    live_presentable: bits & 2 != 0,
                    native_visible: bits & 4 != 0,
                    live_owner: bits & 8 != 0,
                    preview_visible: bits & 16 != 0,
                    preview_animation_enabled: bits & 32 != 0,
                };
                // Literal existing formula after the preview visibility gate.
                let live_visible = facts.focused && facts.live_presentable && facts.native_visible;
                let preview_full = facts.focused
                    && facts.preview_visible
                    && facts.preview_animation_enabled
                    && motion == Motion::Full;
                let legacy = if preview_full || live_visible {
                    Some(Duration::from_millis(
                        if preview_full || (motion == Motion::Full && facts.live_owner) {
                            100
                        } else {
                            900
                        },
                    ))
                } else {
                    None
                };
                assert_eq!(
                    periodic(facts).map(PeriodicReason::interval),
                    legacy,
                    "{facts:?}"
                );
            }
        }
    }

    #[test]
    fn static_and_calm_keep_maintenance_not_animation() {
        for motion in [Motion::Static, Motion::Calm] {
            assert_eq!(periodic(live(motion)), Some(PeriodicReason::Maintenance));
        }
    }

    #[test]
    fn background_has_no_periodic_even_with_stale_owner_flag() {
        let mut facts = live(Motion::Full);
        facts.focused = false;
        facts.preview_visible = true;
        assert_eq!(periodic(facts), None);
    }

    #[test]
    fn disabled_live_and_closed_preview_have_no_periodic() {
        let mut facts = live(Motion::Full);
        facts.live_presentable = false;
        facts.live_owner = false;
        assert_eq!(periodic(facts), None);
    }

    #[test]
    fn remote_or_alternate_owner_cannot_get_live_animation() {
        let mut facts = live(Motion::Full);
        facts.live_owner = false;
        assert_eq!(periodic(facts), Some(PeriodicReason::Maintenance));
        facts.native_visible = false;
        assert_eq!(periodic(facts), None);
    }

    #[test]
    fn disabled_live_visible_preview_preserves_existing_animation_exception() {
        let mut facts = live(Motion::Full);
        facts.live_presentable = false;
        facts.live_owner = false;
        facts.preview_visible = true;
        assert_eq!(
            periodic(facts),
            Some(PeriodicReason::Animation(AnimationAudience {
                live: false,
                preview: true
            }))
        );
    }

    #[test]
    fn closing_preview_removes_only_preview_reason() {
        let mut facts = live(Motion::Full);
        facts.preview_visible = true;
        assert_eq!(
            periodic(facts),
            Some(PeriodicReason::Animation(AnimationAudience {
                live: true,
                preview: true
            }))
        );
        facts.preview_visible = false;
        assert_eq!(
            periodic(facts),
            Some(PeriodicReason::Animation(AnimationAudience {
                live: true,
                preview: false
            }))
        );
        facts.live_owner = false;
        assert_eq!(periodic(facts), Some(PeriodicReason::Maintenance));
    }

    #[test]
    fn pausing_preview_does_not_pause_valid_live_animation() {
        let mut facts = live(Motion::Full);
        facts.preview_visible = true;
        facts.preview_animation_enabled = false;
        assert_eq!(
            periodic(facts),
            Some(PeriodicReason::Animation(AnimationAudience {
                live: true,
                preview: false
            }))
        );
        facts.live_owner = false;
        assert_eq!(periodic(facts), Some(PeriodicReason::Maintenance));
    }

    fn deadline(at: u64, reason: DeadlineReason, generation: u64) -> DeadlineRequest {
        DeadlineRequest {
            at: Duration::from_secs(at),
            reason,
            generation,
        }
    }

    #[test]
    fn equal_deadlines_share_wake_without_losing_obligations() {
        let first = deadline(8, DeadlineReason::PreviewCooldown, 1);
        let second = deadline(8, DeadlineReason::CompletionExpiry, 2);
        let wakes = deadlines(
            Duration::ZERO,
            [Some(first), Some(second), Some(first), None, None, None],
        );
        assert_eq!(
            wakes,
            vec![DeadlineWake {
                at: first.at,
                obligations: vec![first, second]
            }]
        );
    }

    #[test]
    fn all_future_deadlines_stay_absolute_and_finite() {
        let early = deadline(1, DeadlineReason::RetreatExpiry, 1);
        let late = deadline(8, DeadlineReason::LiveAttention, 2);
        let input = [Some(late), Some(early), None, None, None, None];
        let first = deadlines(Duration::ZERO, input);
        assert_eq!(
            first.iter().map(|wake| wake.at).collect::<Vec<_>>(),
            vec![early.at, late.at]
        );
        assert_eq!(first, deadlines(Duration::from_millis(999), input));
        assert_eq!(deadlines(Duration::from_secs(1), input).len(), 1);
        assert!(deadlines(Duration::from_secs(60), input).is_empty());
    }

    #[test]
    fn motion_has_no_control_over_semantic_deadline_admission() {
        let request = deadline(8, DeadlineReason::CompletionExpiry, 7);
        for motion in [Motion::Full, Motion::Calm, Motion::Static] {
            let _periodic_request = periodic(live(motion));
            assert_eq!(
                deadlines(
                    Duration::ZERO,
                    [Some(request), None, None, None, None, None]
                )
                .len(),
                1
            );
        }
    }

    #[test]
    fn stale_expiry_cannot_retire_a_replaced_hold() {
        let old = deadline(2, DeadlineReason::CompletionExpiry, 1);
        let later = deadline(4, DeadlineReason::CompletionExpiry, 2);
        assert!(!deadline_current(old, Some(later)));
        assert!(!deadline_current(old, None));
        assert!(deadline_current(later, Some(later)));
    }

    #[test]
    fn due_obligation_remains_valid_when_future_schedule_is_empty() {
        let registered = deadline(2, DeadlineReason::CompletionExpiry, 1);
        let now = Duration::from_secs(2);
        assert!(deadlines(now, [Some(registered), None, None, None, None, None]).is_empty());
        assert!(deadline_current(registered, Some(registered)));
    }

    #[test]
    fn same_time_same_reason_keeps_distinct_generations() {
        let old = deadline(8, DeadlineReason::CompletionExpiry, 1);
        let current = deadline(8, DeadlineReason::CompletionExpiry, 2);
        let planned = deadlines(
            Duration::ZERO,
            [Some(old), Some(current), None, None, None, None],
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].obligations, vec![old, current]);
        assert!(!deadline_current(old, Some(current)));
        assert!(deadline_current(current, Some(current)));
    }

    #[test]
    fn same_time_changed_reason_is_not_the_same_obligation() {
        let completion = deadline(8, DeadlineReason::CompletionExpiry, 1);
        let attention = deadline(8, DeadlineReason::LiveAttention, 1);
        assert!(!deadline_current(completion, Some(attention)));
    }

    #[test]
    fn subscription_identities_support_hash_and_exact_equality() {
        use std::collections::HashSet;

        let live = AnimationAudience {
            live: true,
            preview: false,
        };
        let both = AnimationAudience {
            live: true,
            preview: true,
        };
        assert_eq!(HashSet::from([live, live, both]).len(), 2);
        assert_eq!(
            HashSet::from([
                PeriodicReason::Animation(live),
                PeriodicReason::Animation(both),
                PeriodicReason::Maintenance,
            ])
            .len(),
            3
        );
        let original = deadline(8, DeadlineReason::CompletionExpiry, 1);
        let replacement = deadline(8, DeadlineReason::CompletionExpiry, 2);
        let changed_reason = deadline(8, DeadlineReason::LiveAttention, 1);
        assert_eq!(
            HashSet::from([original, original, replacement, changed_reason]).len(),
            3
        );
        let wake = DeadlineWake {
            at: original.at,
            obligations: vec![original, changed_reason],
        };
        assert_eq!(HashSet::from([wake.clone(), wake]).len(), 1);
    }

    #[test]
    fn instant_deadlines_preserve_absolute_time_coalescing_and_due_identity() {
        let now = Instant::now();
        let at = now + Duration::from_secs(1);
        let completion = DeadlineRequest {
            at,
            reason: DeadlineReason::CompletionExpiry,
            generation: 1,
        };
        let attention = DeadlineRequest {
            at,
            reason: DeadlineReason::LiveAttention,
            generation: 2,
        };
        let input = [Some(completion), Some(attention), None, None, None, None];
        let planned = deadlines(now, input);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].at, at);
        assert_eq!(planned[0].obligations, vec![completion, attention]);
        assert!(deadlines(at, input).is_empty());
        assert!(deadline_current(completion, Some(completion)));
        assert!(!deadline_current(completion, Some(attention)));
        let cause = WakeCause::Deadline(planned[0].clone());
        assert!(matches!(cause.clone(), WakeCause::Deadline(_)));
        assert_eq!(
            std::collections::HashSet::from([planned[0].clone()]).len(),
            1
        );
    }

    #[test]
    fn periodic_counters_attribute_only_requested_audiences() {
        let mut counters = WakeCounters::default();
        for audience in [
            AnimationAudience {
                live: true,
                preview: false,
            },
            AnimationAudience {
                live: false,
                preview: true,
            },
            AnimationAudience {
                live: true,
                preview: true,
            },
            AnimationAudience {
                live: false,
                preview: false,
            },
        ] {
            counters.record(&WakeCause::Periodic(PeriodicReason::Animation(audience)));
        }
        assert_eq!(
            counters.snapshot(),
            WakeCounterSnapshot {
                live_animation: 2,
                preview_animation: 2,
                ..WakeCounterSnapshot::default()
            }
        );
        counters.record(&WakeCause::Periodic(PeriodicReason::Maintenance));
        assert_eq!(
            counters.snapshot(),
            WakeCounterSnapshot {
                live_animation: 2,
                preview_animation: 2,
                maintenance: 1,
                ..WakeCounterSnapshot::default()
            }
        );
    }

    #[test]
    fn one_deadline_wake_counts_all_six_reasons_without_losing_obligations() {
        let now = Instant::now();
        let at = now + Duration::from_secs(1);
        let input = [
            DeadlineReason::PreviewBoundary,
            DeadlineReason::PreviewGreeting,
            DeadlineReason::PreviewCooldown,
            DeadlineReason::RetreatExpiry,
            DeadlineReason::CompletionExpiry,
            DeadlineReason::LiveAttention,
        ]
        .map(|reason| {
            Some(DeadlineRequest {
                at,
                reason,
                generation: 1,
            })
        });
        let mut planned = deadlines(now, input);
        assert_eq!(planned.len(), 1);
        let cause = WakeCause::Deadline(planned.remove(0));
        let mut counters = WakeCounters::default();
        counters.record(&cause);
        assert_eq!(
            counters.snapshot(),
            WakeCounterSnapshot {
                preview_boundary: 1,
                preview_greeting: 1,
                preview_cooldown: 1,
                retreat_expiry: 1,
                completion_expiry: 1,
                live_attention: 1,
                ..WakeCounterSnapshot::default()
            }
        );
        let WakeCause::Deadline(wake) = cause else {
            panic!("deadline cause expected");
        };
        assert_eq!(wake.obligations.len(), 6);
        assert!(wake.obligations.into_iter().all(|request| request.at == at));
    }

    #[test]
    fn repeated_reason_generations_count_once_per_delivered_wake() {
        let now = Instant::now();
        let at = now + Duration::from_secs(1);
        let first = DeadlineRequest {
            at,
            reason: DeadlineReason::CompletionExpiry,
            generation: 1,
        };
        let replacement = DeadlineRequest {
            generation: 2,
            ..first
        };
        let planned = deadlines(
            now,
            [
                Some(first),
                Some(replacement),
                Some(first),
                None,
                None,
                None,
            ],
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].obligations, vec![first, replacement]);
        let cause = WakeCause::Deadline(planned[0].clone());
        let mut counters = WakeCounters::default();
        counters.record(&cause);
        counters.record(&cause);
        assert_eq!(
            counters.snapshot(),
            WakeCounterSnapshot {
                completion_expiry: 2,
                ..WakeCounterSnapshot::default()
            }
        );
    }

    #[test]
    fn all_nine_counters_saturate_and_keep_fixed_content_free_storage() {
        let saturated = WakeCounterSnapshot {
            live_animation: u64::MAX,
            preview_animation: u64::MAX,
            maintenance: u64::MAX,
            preview_boundary: u64::MAX,
            preview_greeting: u64::MAX,
            preview_cooldown: u64::MAX,
            retreat_expiry: u64::MAX,
            completion_expiry: u64::MAX,
            live_attention: u64::MAX,
        };
        let mut counters = WakeCounters { counts: saturated };
        counters.record(&WakeCause::Periodic(PeriodicReason::Animation(
            AnimationAudience {
                live: true,
                preview: true,
            },
        )));
        counters.record(&WakeCause::Periodic(PeriodicReason::Maintenance));
        let at = Instant::now();
        counters.record(&WakeCause::Deadline(DeadlineWake {
            at,
            obligations: [
                DeadlineReason::PreviewBoundary,
                DeadlineReason::PreviewGreeting,
                DeadlineReason::PreviewCooldown,
                DeadlineReason::RetreatExpiry,
                DeadlineReason::CompletionExpiry,
                DeadlineReason::LiveAttention,
            ]
            .map(|reason| DeadlineRequest {
                at,
                reason,
                generation: 1,
            })
            .to_vec(),
        }));
        assert_eq!(counters.snapshot(), saturated);
        assert_eq!(
            std::mem::size_of::<WakeCounters>(),
            9 * std::mem::size_of::<u64>()
        );
        assert_eq!(
            std::mem::size_of::<WakeCounterSnapshot>(),
            9 * std::mem::size_of::<u64>()
        );
    }
}
