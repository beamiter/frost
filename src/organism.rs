//! Offline, volatile ASCII-organism chrome adapter.
//!
//! Deliberately does not access terminal bytes, widgets, PTYs, or files. The
//! host supplies one-shot completed commands and a content-free running flag.
//! Stable live Start identities are not public in Frost's terminal boundary,
//! so watching is display-only: no synthetic `command_started` is generated.
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use jterm_core::organism::{
    classify_command, sprite_frame_with_context, sticky_glyph_with_context, AmbientMind, Behavior,
    BodyLanguage, CircadianPhase, LifeState, NativeOrganism, Reaction, RenderContext, RepoVigil,
    Tone,
};
use jterm_core::organism_daily::{GentleInteraction, PreviewPose};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    Full,
    Calm,
    Static,
}

impl<'de> Deserialize<'de> for Motion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Ok(Self::Full),
            "calm" => Ok(Self::Calm),
            "static" => Ok(Self::Static),
            _ => Err(serde::de::Error::custom("expected full, calm, or static")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionChoice {
    Automatic,
    Full,
    Calm,
    Static,
}

impl MotionChoice {
    pub const ALL: [Self; 4] = [Self::Automatic, Self::Full, Self::Calm, Self::Static];

    pub fn configured(self) -> Option<Motion> {
        match self {
            Self::Automatic => None,
            Self::Full => Some(Motion::Full),
            Self::Calm => Some(Motion::Calm),
            Self::Static => Some(Motion::Static),
        }
    }

    pub fn from_config(value: Option<Motion>) -> Self {
        match value {
            None => Self::Automatic,
            Some(Motion::Full) => Self::Full,
            Some(Motion::Calm) => Self::Calm,
            Some(Motion::Static) => Self::Static,
        }
    }
}

impl fmt::Display for MotionChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Automatic => "Automatic",
            Self::Full => "Full",
            Self::Calm => "Calm",
            Self::Static => "Static",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pose(pub PreviewPose);

impl fmt::Display for Pose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.label())
    }
}

/// One volatile physiology clock per window, never one per pane or frame.
/// Timestamps are injected, monotonic durations; no wall-clock or I/O policy.
pub struct WindowLife {
    state: LifeState,
    anchor: Duration,
    last_input: Option<Duration>,
    last_activity: Duration,
    eligible: bool,
}

impl WindowLife {
    pub fn new_at(now: Duration) -> Self {
        Self {
            state: LifeState::default(),
            anchor: now,
            last_input: None,
            last_activity: now,
            eligible: false,
        }
    }

    pub fn state(&self) -> LifeState {
        self.state
    }

    pub fn replace_state(&mut self, state: LifeState) {
        self.state = NativeOrganism::from_persisted_state(state).state();
    }

    pub fn note_input(&mut self, now: Duration) {
        if now >= self.anchor && now >= self.last_activity {
            self.last_input = Some(now);
            self.last_activity = now;
        }
    }

    pub fn note_output(&mut self, now: Duration) {
        if now >= self.anchor {
            self.last_activity = self.last_activity.max(now);
        }
    }

    pub fn idle_for(&self, now: Duration) -> Duration {
        now.saturating_sub(self.last_activity)
    }

    pub fn advance(
        &mut self,
        now: Duration,
        eligible_live: bool,
        any_running: bool,
        phase: CircadianPhase,
    ) -> f32 {
        if !eligible_live {
            self.anchor = self.anchor.max(now);
            self.eligible = false;
            return 0.0;
        }
        if now < self.anchor {
            return 0.0;
        }
        if any_running {
            self.last_activity = self.last_activity.max(now);
        }
        if !self.eligible {
            self.anchor = now;
            self.eligible = eligible_live;
            return 0.0;
        }
        let elapsed = now.saturating_sub(self.anchor).min(Duration::from_secs(1));
        self.anchor = now;
        if elapsed.is_zero() {
            return 0.0;
        }
        let resting = !any_running && self.idle_for(now) >= Duration::from_secs(60);
        // Account for input over the consumed interval, not only its endpoint.
        // A 900ms dormant tick therefore cannot miss the 900ms input window.
        let start = now.saturating_sub(elapsed);
        let active = self.last_input.map_or(Duration::ZERO, |input| {
            let end = input.saturating_add(Duration::from_millis(900)).min(now);
            end.saturating_sub(input.max(start)).min(elapsed)
        });
        let inactive = elapsed.saturating_sub(active);
        if !active.is_zero() {
            self.state.tick(active.as_secs_f32(), true, resting, phase);
        }
        if !inactive.is_zero() {
            self.state
                .tick(inactive.as_secs_f32(), false, resting, phase);
        }
        elapsed.as_secs_f32()
    }
}

struct SessionLife {
    native: NativeOrganism,
    ambient: AmbientMind,
    context: RenderContext,
    /// A command already running when enabled is deliberately not replayed.
    accept_completion: bool,
    /// No event-source activation epoch exists: discard the initial PTY batch.
    quarantine_batch: bool,
    reaction_until: Instant,
    remote: bool,
    next_remote_probe: Instant,
}

impl SessionLife {
    fn new(id: usize, running: bool, now: Instant) -> Self {
        Self {
            native: NativeOrganism::from_persisted_state(LifeState::default()),
            ambient: AmbientMind::seeded(id as u64),
            context: PreviewPose::Calm.context(),
            accept_completion: !running,
            quarantine_batch: true,
            reaction_until: now,
            remote: true,
            next_remote_probe: now,
        }
    }
}

pub struct Organism {
    enabled: bool,
    sessions: HashMap<usize, SessionLife>,
    born: Instant,
    life: WindowLife,
    retreat_until: Instant,
    pub pose: Pose,
    hello: GentleInteraction,
}

impl Default for Organism {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            enabled: false,
            sessions: HashMap::new(),
            born: now,
            life: WindowLife::new_at(Duration::ZERO),
            retreat_until: now,
            pose: Pose(PreviewPose::Calm),
            hello: GentleInteraction::default(),
        }
    }
}

impl Organism {
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.sessions.clear();
            self.hello.cancel();
            self.enabled = enabled;
            self.life = WindowLife::new_at(self.born.elapsed());
        }
    }

    pub fn prime_session(&mut self, id: usize, running: bool) {
        if self.enabled {
            self.sessions
                .entry(id)
                .or_insert_with(|| SessionLife::new(id, running, Instant::now()));
        }
    }

    pub fn remote_probe_due(&self, id: usize) -> bool {
        self.sessions
            .get(&id)
            .is_some_and(|life| Instant::now() >= life.next_remote_probe)
    }

    pub fn set_remote(&mut self, id: usize, remote: bool) {
        if let Some(life) = self.sessions.get_mut(&id) {
            life.remote = remote;
            life.next_remote_probe = Instant::now() + Duration::from_millis(900);
        }
    }

    pub fn is_local(&self, id: usize) -> bool {
        self.sessions.get(&id).is_some_and(|life| !life.remote)
    }

    pub fn forget_session(&mut self, id: usize) {
        self.sessions.remove(&id);
    }

    pub fn retain_sessions(&mut self, ids: &[usize]) {
        self.sessions.retain(|id, _| ids.contains(id));
    }

    /// Called only from the existing completion drain, never by frame timers.
    /// No command text is retained, logged, sent, or persisted by this adapter.
    pub fn completed(
        &mut self,
        id: usize,
        command: &str,
        exit: Option<i32>,
        duration: Option<u64>,
    ) {
        if !self.enabled {
            return;
        }
        let Some(life) = self.sessions.get_mut(&id).filter(|life| !life.remote) else {
            return;
        };
        if life.quarantine_batch {
            return;
        }
        if !life.accept_completion {
            // Ignore precisely the in-flight completion observed on enable;
            // subsequent entries in the same one-shot batch may be new work.
            life.accept_completion = true;
            return;
        }
        let now = Instant::now();
        life.native.sync_state(self.life.state());
        let reaction = life
            .native
            .command_finished(classify_command(command), exit, duration);
        self.life.replace_state(life.native.state());
        life.ambient.interrupt();
        life.context = RenderContext::new(
            reaction.behavior,
            BodyLanguage::from_state(life.native.state()),
            false,
        );
        life.reaction_until = now + reaction_duration(&reaction);
    }

    pub fn batch_finished(&mut self, id: usize, running: bool) {
        if let Some(life) = self.sessions.get_mut(&id) {
            if life.quarantine_batch {
                life.quarantine_batch = false;
                life.accept_completion = !running;
            } else if !running {
                life.accept_completion = true;
            }
        }
    }

    pub fn note_output(&mut self, id: usize) {
        if self.enabled {
            self.life.note_output(self.born.elapsed());
            if let Some(life) = self.sessions.get_mut(&id) {
                life.ambient.interrupt();
            }
        }
    }

    pub fn accepted_input(&mut self) {
        if self.enabled {
            self.life.note_input(self.born.elapsed());
        }
        self.retreat();
    }

    /// Presentation-only retreat also serves blur and attempted gestures.
    pub fn retreat(&mut self) {
        self.retreat_until = Instant::now() + Duration::from_millis(900);
    }

    pub fn is_retreating(&self) -> bool {
        Instant::now() < self.retreat_until
    }

    pub fn close_preview(&mut self) {
        self.hello.cancel();
    }

    pub fn select_pose(&mut self, pose: Pose) {
        self.hello.cancel();
        self.pose = pose;
    }

    pub fn can_say_hello(&self) -> bool {
        let mut hello = self.hello.clone();
        hello.request(self.born.elapsed(), self.pose.0.context())
    }

    pub fn say_hello(&mut self) {
        let _ = self
            .hello
            .request(self.born.elapsed(), self.pose.0.context());
    }

    pub fn pause_clock(&mut self) {
        self.life
            .advance(self.born.elapsed(), false, false, CircadianPhase::Unlearned);
    }

    pub fn tick(&mut self, owner: Option<usize>, running: bool, any_running: bool) {
        let now = Instant::now();
        let elapsed = self.born.elapsed();
        let dt = self.life.advance(
            elapsed,
            self.enabled && owner.is_some(),
            any_running,
            CircadianPhase::Unlearned,
        );
        let retreating = self.is_retreating();
        if let Some(life) = owner.and_then(|id| self.sessions.get_mut(&id)) {
            life.native.sync_state(self.life.state());
            if running || retreating {
                life.ambient.interrupt();
            } else if now >= life.reaction_until {
                let behavior = life.ambient.step(
                    self.life.state(),
                    self.life.idle_for(elapsed).as_secs_f32(),
                    dt,
                    RepoVigil::None,
                );
                life.context = RenderContext::new(
                    behavior.display(),
                    BodyLanguage::from_state(self.life.state()),
                    false,
                );
            }
        }
    }

    fn frame(&self, motion: Option<Motion>) -> u64 {
        if motion == Some(Motion::Full) {
            self.born.elapsed().as_millis() as u64 / 100
        } else {
            0
        }
    }

    pub fn glyph(&self, id: usize, running: bool, settled: bool, motion: Option<Motion>) -> String {
        let context = if running {
            // Display only. No fabricated Start is fed into the life reducer.
            let behavior = if settled {
                Behavior::WatchSettled
            } else {
                Behavior::WatchCommand
            };
            RenderContext::new(behavior, BodyLanguage::from_state(self.life.state()), false)
        } else {
            self.sessions
                .get(&id)
                .map(|life| life.context)
                .unwrap_or_else(|| PreviewPose::Calm.context())
        };
        let context = RenderContext {
            body_language: BodyLanguage::from_state(self.life.state()),
            ..context
        };
        sticky_glyph_with_context(context, self.frame(motion)).into_owned()
    }

    pub fn preview(&self, motion: Option<Motion>) -> String {
        let mut hello = self.hello.clone();
        let context = hello.apply(self.born.elapsed(), self.pose.0.context());
        sprite_frame_with_context(context, self.frame(motion)).into_owned()
    }
}

fn reaction_duration(reaction: &Reaction) -> Duration {
    Duration::from_millis(match reaction.behavior {
        Behavior::Idle => 1500,
        Behavior::Celebrate if reaction.tone == Tone::Quiet => 1800,
        Behavior::Celebrate => 2500,
        Behavior::GlanceAside => 1400,
        Behavior::InspectError => 5000,
        Behavior::SitNearError => 10000,
        Behavior::CelebrateBig => 7000,
        Behavior::RestAfterPush => 5000,
        Behavior::UnknownOutcome => 4500,
        _ => 2500,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(state: LifeState) -> [f32; 8] {
        [
            state.energy,
            state.mood,
            state.curiosity,
            state.boredom,
            state.stress,
            state.social_need,
            state.attachment,
            state.confidence,
        ]
    }

    #[test]
    fn window_clock_is_single_bounded_and_resets_after_hidden() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        assert_eq!(
            life.advance(Duration::ZERO, true, false, CircadianPhase::Unlearned),
            0.0
        );
        assert_eq!(
            life.advance(
                Duration::from_secs(1),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            1.0
        );
        let state = values(life.state());
        assert_eq!(
            life.advance(
                Duration::from_secs(1),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            0.0
        );
        assert_eq!(
            life.advance(Duration::ZERO, true, false, CircadianPhase::Unlearned),
            0.0
        );
        assert_eq!(values(life.state()), state);
        assert_eq!(
            life.advance(
                Duration::from_secs(3600),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            1.0
        );
        life.advance(
            Duration::from_secs(3601),
            false,
            false,
            CircadianPhase::Unlearned,
        );
        let state = values(life.state());
        assert_eq!(
            life.advance(
                Duration::from_secs(7200),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            0.0
        );
        assert_eq!(values(life.state()), state);
        life.advance(
            Duration::from_secs(1),
            false,
            true,
            CircadianPhase::Unlearned,
        );
        assert_eq!(
            life.advance(
                Duration::from_secs(7201),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            0.0
        );
        assert_eq!(values(life.state()), state);
    }

    #[test]
    fn input_overlay_pauses_clock_without_resume_catchup() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        life.advance(Duration::ZERO, true, false, CircadianPhase::Unlearned);
        life.note_input(Duration::ZERO);
        life.advance(
            Duration::from_millis(100),
            true,
            false,
            CircadianPhase::Unlearned,
        );
        let before_overlay = values(life.state());
        for second in [1, 2, 60, 3600] {
            assert_eq!(
                life.advance(
                    Duration::from_secs(second),
                    false,
                    true,
                    CircadianPhase::Unlearned
                ),
                0.0
            );
            assert_eq!(values(life.state()), before_overlay);
        }
        assert_eq!(
            life.advance(
                Duration::from_secs(3601),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            0.0
        );
        assert_eq!(values(life.state()), before_overlay);
        assert_eq!(
            life.advance(
                Duration::from_millis(3_601_100),
                true,
                false,
                CircadianPhase::Unlearned
            ),
            0.1
        );
    }

    #[test]
    fn quiet_rest_and_recent_input_use_content_free_activity() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        life.advance(Duration::ZERO, true, false, CircadianPhase::Unlearned);
        let before = life.state();
        life.note_input(Duration::ZERO);
        life.advance(
            Duration::from_millis(900),
            true,
            false,
            CircadianPhase::Unlearned,
        );
        assert!(life.state().boredom < before.boredom);
        assert!(life.state().social_need < before.social_need);
        for second in 1..60 {
            life.advance(
                Duration::from_secs(second),
                true,
                false,
                CircadianPhase::Unlearned,
            );
        }
        let before_rest = life.state();
        life.advance(
            Duration::from_secs(60),
            true,
            false,
            CircadianPhase::Unlearned,
        );
        assert!(life.state().energy > before_rest.energy);
        assert!(life.state().stress < before.stress);
        life.note_output(Duration::from_secs(60));
        let before_work = life.state().energy;
        life.advance(
            Duration::from_secs(61),
            true,
            false,
            CircadianPhase::Unlearned,
        );
        assert!(life.state().energy < before_work);
        let before_running = life.state().energy;
        life.advance(
            Duration::from_secs(120),
            true,
            true,
            CircadianPhase::Unlearned,
        );
        assert!(life.state().energy < before_running);
        assert_eq!(life.idle_for(Duration::from_secs(120)), Duration::ZERO);
    }

    #[test]
    fn replacement_is_normalized_and_seeded_ambient_is_deterministic() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        let mut hostile = life.state();
        hostile.energy = f32::NAN;
        hostile.stress = f32::INFINITY;
        hostile.boredom = -1.0;
        life.replace_state(hostile);
        assert!(values(life.state())
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        let mut left = AmbientMind::seeded(17);
        let mut right = AmbientMind::seeded(17);
        for second in 0..120 {
            assert_eq!(
                left.step(life.state(), second as f32, 1.0, RepoVigil::None),
                right.step(life.state(), second as f32, 1.0, RepoVigil::None)
            );
        }
    }

    #[test]
    fn preview_and_disabled_window_do_not_advance_physiology() {
        let mut organism = Organism::default();
        let before = values(organism.life.state());
        organism.say_hello();
        organism.preview(Some(Motion::Full));
        organism.tick(None, false, false);
        assert_eq!(values(organism.life.state()), before);
        organism.set_enabled(true);
        organism.prime_session(3, false);
        organism.set_remote(3, false);
        organism.batch_finished(3, false);
        organism.completed(3, "cargo test", None, Some(1));
        organism.tick(Some(3), false, false);
        assert_eq!(
            organism.sessions[&3].context.behavior,
            Behavior::UnknownOutcome
        );
    }

    #[test]
    fn disabled_and_mid_command_enable_do_not_replay_work() {
        let mut organism = Organism::default();
        organism.prime_session(7, false);
        assert!(organism.sessions.is_empty());
        organism.set_enabled(true);
        organism.prime_session(7, true);
        organism.set_remote(7, false);
        organism.batch_finished(7, true);
        organism.completed(7, "cargo test", Some(0), Some(100));
        assert_eq!(organism.sessions[&7].context.behavior, Behavior::Idle);
        organism.completed(7, "cargo test", None, Some(100));
        assert_eq!(
            organism.sessions[&7].context.behavior,
            Behavior::UnknownOutcome
        );
        organism.set_enabled(false);
        organism.completed(7, "cargo test", Some(0), None);
        assert!(organism.sessions.is_empty());
    }

    #[test]
    fn session_teardown_does_not_depend_on_an_animation_timer() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(11, false);
        organism.set_remote(11, false);
        organism.batch_finished(11, false);
        organism.forget_session(11);
        organism.completed(11, "cargo test", Some(0), Some(1));
        assert!(organism.sessions.is_empty());
    }

    #[test]
    fn initial_batch_is_quarantined_even_when_cached_running_is_false() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(9, false);
        organism.set_remote(9, false);
        organism.completed(9, "cargo test", Some(0), Some(100));
        organism.completed(9, "cargo test", Some(1), Some(100));
        assert_eq!(organism.sessions[&9].context.behavior, Behavior::Idle);
        organism.batch_finished(9, false);
        organism.completed(9, "cargo test", None, Some(100));
        assert_eq!(
            organism.sessions[&9].context.behavior,
            Behavior::UnknownOutcome
        );
    }

    #[test]
    fn preview_does_not_create_live_state_and_automatic_is_still() {
        let mut organism = Organism::default();
        for pose in PreviewPose::ALL {
            organism.select_pose(Pose(pose));
            assert!(!organism.preview(None).is_empty());
        }
        organism.say_hello();
        assert!(organism.sessions.is_empty());
        assert_eq!(organism.frame(None), 0);
        assert_eq!(organism.frame(Some(Motion::Calm)), 0);
        assert_eq!(organism.frame(Some(Motion::Static)), 0);
    }
}
