//! Offline, volatile ASCII-organism chrome adapter.
//!
//! Deliberately does not access terminal bytes, widgets, PTYs, or files. The
//! host supplies one-shot completed commands and a content-free running flag.
//! Stable live Start identities are not public in Frost's terminal boundary,
//! so watching is display-only: no synthetic `command_started` is generated.
use crate::organism_hover::LiveGreeting;
use crate::organism_preview::{PreviewAction, PreviewRequest, PreviewSequence};
use crate::organism_watch::WatchObservation;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use jterm_core::organism::{
    classify_command, sprite_frame_with_context, sticky_glyph_with_context, AmbientMind, Behavior,
    BodyLanguage, CircadianPhase, LifeState, NativeOrganism, Reaction, RenderContext, RepoVigil,
    Tone, WatchRhythm,
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

    pub fn description(self) -> &'static str {
        match self {
            Self::Automatic | Self::Calm => "Still poses that respond to work.",
            Self::Full => "Animated poses that respond to work.",
            Self::Static => "Still poses that respond to work; no output rhythm.",
        }
    }

    pub fn interaction_hint(self) -> &'static str {
        match self {
            Self::Static => "Hover greetings are unavailable in Static.",
            _ => "While idle, hover over the status-bar glyph for 600 ms to say hello. Typing or busy work cancels it. One greeting per visit, with an 8-second cooldown.",
        }
    }

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

pub fn expanded_companion_hint(bottom_bar: bool) -> &'static str {
    if bottom_bar {
        "Reserves a fixed-height strip above the bottom bar. When space allows, narrow windows show only the body. It stays blank while hidden; hover the status-bar glyph in a wider window to greet."
    } else {
        "Turn on Bottom status bar (cwd, git, last command) to show the live companion."
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelloAvailability {
    Demonstrating,
    Busy,
    CoolingDown,
    Available,
}

/// Absolute preview deadlines stay stable across unrelated redraws.
#[derive(Default)]
struct PreviewDeadlines {
    greeting_until: Duration,
    ready_at: Duration,
}

impl PreviewDeadlines {
    fn accepted(&mut self, now: Duration) {
        self.greeting_until = now.saturating_add(GentleInteraction::HOLD);
        self.ready_at = now.saturating_add(GentleInteraction::COOLDOWN);
    }

    fn cancel(&mut self) {
        self.greeting_until = Duration::ZERO;
    }

    fn next(&self, now: Duration, eligible: bool) -> Option<Duration> {
        [Some(self.greeting_until), eligible.then_some(self.ready_at)]
            .into_iter()
            .flatten()
            .filter(|deadline| *deadline > now)
            .min()
    }
}

/// One render snapshot shared by the fixed glyph and its passive detail view.
#[derive(Default)]
pub struct LivePresentation {
    pub glyph: String,
    pub sprite: String,
    pub explanation: Cow<'static, str>,
}

/// Describe only the output rhythm already selected for this render snapshot.
/// These observations do not infer command progress, intent, or stalled work.
fn live_explanation(context: RenderContext) -> Cow<'static, str> {
    let base = jterm_core::organism_daily::behavior_explanation(context.behavior);
    if !matches!(
        context.behavior,
        Behavior::WatchCommand | Behavior::WatchSettled
    ) {
        return Cow::Borrowed(base);
    }
    let observation = match context.watch_rhythm {
        WatchRhythm::Steady => return Cow::Borrowed(base),
        WatchRhythm::Busy => "Recent output activity.",
        WatchRhythm::Waiting => "No new output observed recently.",
        WatchRhythm::Resumed => "Output activity resumed.",
    };
    Cow::Owned(format!("{base} {observation}"))
}

pub struct PreviewPresentation {
    pub sprite: String,
    pub pose: PreviewPose,
    pub demo_step: Option<usize>,
    pub demo_paused: bool,
    pub hello: HelloAvailability,
}

pub struct Organism {
    enabled: bool,
    sessions: HashMap<usize, SessionLife>,
    born: Instant,
    life: WindowLife,
    retreat_until: Instant,
    pub pose: Pose,
    hello: GentleInteraction,
    preview_deadlines: PreviewDeadlines,
    preview_sequence: PreviewSequence,
    preview_visible: bool,
    live_greeting: LiveGreeting,
    watch: WatchObservation,
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
            preview_deadlines: PreviewDeadlines::default(),
            preview_sequence: PreviewSequence::default(),
            preview_visible: false,
            live_greeting: LiveGreeting::default(),
            watch: WatchObservation::default(),
        }
    }
}

impl Organism {
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.sessions.clear();
            self.watch.reset();
            self.close_preview();
            self.cancel_live_greeting();
            self.enabled = enabled;
            self.life = WindowLife::new_at(self.born.elapsed());
        }
    }

    /// Config reloads and the settings control share the same transient boundary.
    /// An unchanged preference must not interrupt an existing visit.
    pub fn motion_changed(&mut self, previous: Option<Motion>, next: Option<Motion>) {
        if previous != next {
            self.reset_watch();
            self.cancel_live_greeting();
        }
    }

    /// Session indices can shift while the same foreground session stays active.
    pub fn owner_changed(&mut self, previous: Option<usize>, next: Option<usize>) {
        if previous != next {
            self.reset_watch();
            self.cancel_live_greeting();
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
        if remote {
            self.watch.reset_owner(id);
            self.live_greeting.cancel_owner(id);
        }
        if let Some(life) = self.sessions.get_mut(&id) {
            life.remote = remote;
            life.next_remote_probe = Instant::now() + Duration::from_millis(900);
        }
    }

    pub fn is_local(&self, id: usize) -> bool {
        self.sessions.get(&id).is_some_and(|life| !life.remote)
    }

    pub fn forget_session(&mut self, id: usize) {
        self.watch.reset_owner(id);
        self.live_greeting.cancel_owner(id);
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
        self.watch.reset_owner(id);
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
        self.live_greeting.cancel_owner(id);
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
        if running {
            self.live_greeting.cancel_owner(id);
        }
        if let Some(life) = self.sessions.get_mut(&id) {
            if life.quarantine_batch {
                life.quarantine_batch = false;
                life.accept_completion = !running;
            } else if !running {
                life.accept_completion = true;
            }
        }
    }

    pub fn watch_batch_admitted(&self, id: usize) -> bool {
        self.enabled
            && self
                .sessions
                .get(&id)
                .is_some_and(|life| !life.remote && !life.quarantine_batch)
    }

    pub fn reset_watch(&mut self) {
        self.watch.reset();
    }

    pub fn observe_watch(&mut self, owner: Option<usize>, running: bool) {
        self.watch.observe(
            owner.filter(|id| self.enabled && self.is_local(*id)),
            running,
            self.born.elapsed(),
        );
    }

    pub fn watch_output(
        &mut self,
        id: usize,
        owner: Option<usize>,
        running: bool,
        nonempty: bool,
        admitted: bool,
    ) {
        let now = self.born.elapsed();
        self.watch.observe(
            owner.filter(|id| self.enabled && self.is_local(*id)),
            running,
            now,
        );
        if nonempty {
            self.watch.output(id, now, admitted);
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
        self.watch.reset();
        self.cancel_live_greeting();
        self.retreat_until = Instant::now() + Duration::from_millis(900);
    }

    /// Only the actual palette-opening branch may undo its own key's extension.
    /// Already canceled visits/watch are deliberately not restored.
    pub fn restore_retreat_after_palette_open(&mut self, previous: Option<Instant>) {
        self.retreat_until = previous.unwrap_or_else(Instant::now);
    }

    pub fn is_retreating(&self) -> bool {
        Instant::now() < self.retreat_until
    }

    pub fn sync_preview_visibility(&mut self, visible: bool) {
        if self.preview_visible && !visible {
            self.close_preview();
        }
        self.preview_visible = visible;
    }

    pub fn close_preview(&mut self) {
        self.preview_visible = false;
        self.preview_sequence.stop();
        self.hello.cancel();
        self.preview_deadlines.cancel();
    }

    pub fn select_pose(&mut self, pose: Pose) {
        self.close_preview();
        self.pose = pose;
    }

    pub fn preview_control_epoch(&self) -> std::sync::Arc<()> {
        self.preview_sequence.control_epoch()
    }

    pub fn preview_action_identity(&self, action: PreviewAction) -> std::sync::Arc<()> {
        self.preview_sequence.action_identity(action)
    }

    pub fn preview_request(&self, action: PreviewAction) -> PreviewRequest {
        self.preview_sequence.request(action)
    }

    pub fn apply_preview_request(&mut self, request: &PreviewRequest) -> bool {
        self.apply_preview_request_at(request, self.born.elapsed())
    }

    fn apply_preview_request_at(&mut self, request: &PreviewRequest, now: Duration) -> bool {
        if !self.preview_sequence.accepts(request, now) {
            return false;
        }
        match request.action {
            PreviewAction::Play => self.start_demo_at(now),
            PreviewAction::Pause => self.preview_sequence.pause(now),
            PreviewAction::Resume => self.preview_sequence.resume(now),
            PreviewAction::Stop => {
                self.stop_demo();
                true
            }
            PreviewAction::Hello => self.say_hello_at(now),
        }
    }

    fn start_demo_at(&mut self, now: Duration) -> bool {
        if self.preview_sequence.start(now) {
            self.hello.cancel();
            self.preview_deadlines.cancel();
            true
        } else {
            false
        }
    }

    pub fn stop_demo(&mut self) {
        self.preview_sequence.stop();
    }

    pub fn preview_animation_enabled(&self) -> bool {
        !self.preview_sequence.is_paused()
    }

    fn hello_availability_at(&self, now: Duration) -> HelloAvailability {
        if self.preview_sequence.sample(now).is_some() {
            return HelloAvailability::Demonstrating;
        }
        let context = self.pose.0.context();
        if !GentleInteraction::default().request(now, context) {
            HelloAvailability::Busy
        } else if self.hello.clone().request(now, context) {
            HelloAvailability::Available
        } else {
            HelloAvailability::CoolingDown
        }
    }

    fn say_hello_at(&mut self, now: Duration) -> bool {
        if self.preview_sequence.sample(now).is_some() {
            return false;
        }
        if self.hello.request(now, self.pose.0.context()) {
            self.preview_deadlines.accepted(now);
            self.preview_sequence.retire_controls();
            true
        } else {
            false
        }
    }

    pub fn preview_deadline(&self) -> Option<Instant> {
        self.preview_deadline_at(self.born.elapsed())
            .map(|deadline| self.born + deadline)
    }

    fn preview_deadline_at(&self, now: Duration) -> Option<Duration> {
        if self.preview_sequence.sample(now).is_some() {
            return self.preview_sequence.next_wake(now);
        }
        let eligible = GentleInteraction::default().request(now, self.pose.0.context());
        self.preview_deadlines.next(now, eligible)
    }

    pub fn retreat_deadline(&self) -> Option<Instant> {
        self.retreat_deadline_at(Instant::now())
    }

    fn retreat_deadline_at(&self, now: Instant) -> Option<Instant> {
        (now < self.retreat_until).then_some(self.retreat_until)
    }

    pub fn pause_clock(&mut self) {
        self.watch.reset();
        self.cancel_live_greeting();
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

    pub fn sync_greeting_geometry(&mut self, glyph: bool, presentable: bool) {
        if !glyph {
            self.live_greeting.cancel_hover();
        }
        if !presentable {
            self.pause_clock();
        }
    }

    pub fn cancel_live_greeting(&mut self) {
        self.live_greeting.cancel();
    }

    pub fn leave_live_greeting(&mut self) {
        self.live_greeting.leave();
    }

    pub fn live_greeting_exit_reference(&self, id: usize) -> (usize, u64) {
        self.live_greeting
            .exit_reference()
            .unwrap_or((id, self.live_greeting.epoch()))
    }

    pub fn exit_live_greeting(&mut self, id: usize, epoch: u64) {
        self.live_greeting.exit(id, epoch);
    }

    pub fn live_greeting_epoch(&self) -> u64 {
        self.live_greeting.epoch()
    }

    pub fn enter_live_greeting(&mut self, id: usize, epoch: u64, eligible: bool) {
        if let Some(life) = self.sessions.get(&id) {
            self.live_greeting.enter(
                id,
                epoch,
                self.born.elapsed(),
                life.context,
                eligible && self.enabled && !life.remote,
            );
        }
    }

    pub fn poll_live_greeting(&mut self, owner: Option<usize>, eligible: bool) {
        let context = owner
            .and_then(|id| self.sessions.get(&id))
            .map(|life| life.context)
            .unwrap_or_else(|| PreviewPose::Calm.context());
        self.live_greeting.advance(
            owner,
            self.born.elapsed(),
            context,
            self.enabled && eligible,
        );
    }

    pub fn greet_live(&mut self, id: usize) -> crate::organism_hover::ExplicitGreeting {
        let Some(life) = self
            .sessions
            .get(&id)
            .filter(|life| self.enabled && !life.remote)
        else {
            return crate::organism_hover::ExplicitGreeting::Busy;
        };
        self.live_greeting
            .request_explicit(id, self.born.elapsed(), life.context)
    }

    pub fn live_greeting_candidate_visible(&self, id: usize) -> bool {
        self.enabled
            && self.sessions.get(&id).is_some_and(|life| {
                !life.remote
                    && self
                        .live_greeting
                        .candidate_visible(id, self.born.elapsed(), life.context)
            })
    }

    pub fn explicit_greeting_active(&self) -> bool {
        self.live_greeting.explicit_active(self.born.elapsed())
    }

    pub fn live_greeting_deadline(&self) -> Option<Instant> {
        self.live_greeting
            .deadline(self.born.elapsed())
            .map(|deadline| self.born + deadline)
    }

    fn frame_at(motion: Option<Motion>, now: Duration) -> u64 {
        if motion == Some(Motion::Full) {
            now.as_millis() as u64 / 100
        } else {
            0
        }
    }

    pub fn presentation(
        &self,
        id: usize,
        running: bool,
        settled: bool,
        motion: Option<Motion>,
    ) -> LivePresentation {
        self.presentation_at(id, running, settled, motion, self.born.elapsed())
    }

    fn presentation_at(
        &self,
        id: usize,
        running: bool,
        settled: bool,
        motion: Option<Motion>,
        now: Duration,
    ) -> LivePresentation {
        let context = if running {
            // Display only. No fabricated Start is fed into the life reducer.
            let behavior = if settled {
                Behavior::WatchSettled
            } else {
                Behavior::WatchCommand
            };
            let context =
                RenderContext::new(behavior, BodyLanguage::from_state(self.life.state()), false);
            if motion == Some(Motion::Static) {
                context
            } else {
                context.with_watch_rhythm(self.watch.rhythm(id, now))
            }
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
        let context = if running || motion == Some(Motion::Static) {
            context
        } else {
            self.live_greeting.apply(id, now, context)
        };
        let frame = Self::frame_at(motion, now);
        LivePresentation {
            glyph: sticky_glyph_with_context(context, frame).into_owned(),
            sprite: sprite_frame_with_context(context, frame).into_owned(),
            explanation: live_explanation(context),
        }
    }

    pub fn preview_presentation(&self, motion: Option<Motion>) -> PreviewPresentation {
        self.preview_presentation_at(motion, self.born.elapsed())
    }

    fn preview_presentation_at(
        &self,
        motion: Option<Motion>,
        now: Duration,
    ) -> PreviewPresentation {
        let demo = self.preview_sequence.sample(now);
        let pose = demo.map_or(self.pose.0, |step| step.pose);
        let context = if demo.is_some() {
            pose.context()
        } else {
            self.hello.clone().apply(now, pose.context())
        };
        let display_pose = if context.behavior == Behavior::Approach {
            PreviewPose::Greeting
        } else {
            pose
        };
        let frame_time = if demo.is_some() {
            self.preview_sequence.presentation_time(now)
        } else {
            now
        };
        PreviewPresentation {
            sprite: sprite_frame_with_context(context, Self::frame_at(motion, frame_time))
                .into_owned(),
            pose: display_pose,
            demo_step: demo.map(|step| step.number),
            demo_paused: demo.is_some() && self.preview_sequence.is_paused(),
            hello: self.hello_availability_at(now),
        }
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
    fn paused_preview_freezes_full_frame_and_motion_preferences_still_win() {
        let mut organism = Organism::default();
        organism.select_pose(Pose(PreviewPose::Curious));
        organism.start_demo_at(Duration::ZERO);
        let pause = organism.preview_request(PreviewAction::Pause);
        assert!(organism.apply_preview_request_at(&pause, Duration::from_millis(2500)));
        let frozen =
            organism.preview_presentation_at(Some(Motion::Full), Duration::from_millis(2500));
        for now in [3000, 90000] {
            let sample =
                organism.preview_presentation_at(Some(Motion::Full), Duration::from_millis(now));
            assert_eq!(sample.sprite, frozen.sprite);
            assert_eq!(sample.pose, frozen.pose);
            assert_eq!(sample.demo_step, Some(2));
            assert!(sample.demo_paused);
            assert_eq!(sample.hello, HelloAvailability::Demonstrating);
            assert_eq!(
                organism.preview_deadline_at(Duration::from_millis(now)),
                None
            );
            assert!(!organism.preview_animation_enabled());
        }
        for motion in [None, Some(Motion::Calm), Some(Motion::Static)] {
            let sample = organism.preview_presentation_at(motion, Duration::from_secs(90));
            assert_eq!(
                sample.sprite,
                sprite_frame_with_context(PreviewPose::Working.context(), 0)
            );
            assert!(sample.demo_paused);
        }
        assert_eq!(
            organism
                .preview_presentation_at(Some(Motion::Full), Duration::from_secs(90))
                .sprite,
            frozen.sprite
        );
        let resume = organism.preview_request(PreviewAction::Resume);
        assert!(organism.apply_preview_request_at(&resume, Duration::from_secs(90)));
        assert_eq!(
            organism
                .preview_presentation_at(Some(Motion::Full), Duration::from_secs(90))
                .sprite,
            frozen.sprite
        );
        assert_eq!(
            organism.preview_deadline_at(Duration::from_secs(90)),
            Some(Duration::from_millis(91500))
        );
        assert_eq!(organism.pose, Pose(PreviewPose::Curious));
    }

    #[test]
    fn paused_demo_stop_pose_close_and_disable_retire_old_resume_without_live_cooldown_change() {
        for cancel in 0..4 {
            let mut organism = Organism::default();
            organism.set_enabled(true);
            organism.select_pose(Pose(PreviewPose::Curious));
            organism
                .live_greeting
                .request_explicit(1, Duration::ZERO, PreviewPose::Calm.context());
            organism.start_demo_at(Duration::ZERO);
            let pause = organism.preview_request(PreviewAction::Pause);
            organism.apply_preview_request_at(&pause, Duration::from_millis(500));
            let resume = organism.preview_request(PreviewAction::Resume);
            match cancel {
                0 => {
                    let stop = organism.preview_request(PreviewAction::Stop);
                    organism.apply_preview_request_at(&stop, Duration::from_secs(1));
                }
                1 => organism.select_pose(Pose(PreviewPose::Calm)),
                2 => organism.close_preview(),
                _ => organism.set_enabled(false),
            }
            assert!(!organism.apply_preview_request_at(&resume, Duration::from_secs(20)));
            assert_eq!(
                organism
                    .preview_presentation_at(None, Duration::from_secs(20))
                    .demo_step,
                None
            );
            assert_eq!(
                organism.live_greeting.request_explicit(
                    1,
                    Duration::from_secs(1),
                    PreviewPose::Calm.context()
                ),
                crate::organism_hover::ExplicitGreeting::Cooldown
            );
        }
    }

    #[test]
    fn hello_preview_explanation_tracks_display_pose_then_restores_manual_pose() {
        for pose in [
            PreviewPose::Calm,
            PreviewPose::Curious,
            PreviewPose::Sleeping,
        ] {
            let mut organism = Organism::default();
            organism.select_pose(Pose(pose));
            assert!(organism.say_hello_at(Duration::ZERO));
            let sample =
                organism.preview_presentation_at(Some(Motion::Static), Duration::from_secs(1));
            assert_eq!(sample.pose, PreviewPose::Greeting);
            assert_eq!(
                sample.pose.explanation(),
                PreviewPose::Greeting.explanation()
            );
            assert_eq!(
                sample.sprite,
                sprite_frame_with_context(PreviewPose::Greeting.context(), 0)
            );
            assert_eq!(organism.pose, Pose(pose));
            assert_eq!(
                organism
                    .preview_presentation_at(None, Duration::from_secs(2))
                    .pose,
                pose
            );
            assert_eq!(
                organism.hello_availability_at(Duration::from_secs(2)),
                HelloAvailability::CoolingDown
            );
        }
    }

    #[test]
    fn preview_visibility_retires_once_and_paused_cancel_cannot_resume_or_spend_life() {
        let mut organism = Organism::default();
        organism.sync_preview_visibility(true);
        let hello = organism.preview_request(PreviewAction::Hello);
        organism.say_hello_at(Duration::ZERO);
        let before = values(organism.life.state());
        let ready = organism.preview_deadlines.ready_at;
        organism.start_demo_at(Duration::from_secs(1));
        let pause = organism.preview_request(PreviewAction::Pause);
        organism.apply_preview_request_at(&pause, Duration::from_millis(1500));
        let resume = organism.preview_request(PreviewAction::Resume);
        organism.sync_preview_visibility(false);
        let closed_epoch = organism.preview_control_epoch();
        organism.sync_preview_visibility(false);
        assert!(std::sync::Arc::ptr_eq(
            &closed_epoch,
            &organism.preview_control_epoch()
        ));
        organism.sync_preview_visibility(true);
        assert!(!organism.apply_preview_request_at(&resume, Duration::from_secs(2)));
        assert!(!organism.apply_preview_request_at(&hello, Duration::from_secs(9)));
        assert_eq!(
            organism
                .preview_presentation_at(None, Duration::from_secs(9))
                .demo_step,
            None
        );
        assert_eq!(organism.preview_deadlines.ready_at, ready);
        assert_eq!(values(organism.life.state()), before);
    }

    #[test]
    fn palette_open_restores_only_previous_retreat_deadline_not_canceled_state() {
        let mut organism = Organism::default();
        let previous = Instant::now() + Duration::from_millis(300);
        organism.retreat_until = previous;
        organism.live_greeting.enter(
            1,
            organism.live_greeting.epoch(),
            Duration::ZERO,
            PreviewPose::Calm.context(),
            true,
        );
        let snapshot = organism.retreat_deadline();
        organism.retreat();
        organism.restore_retreat_after_palette_open(snapshot);
        assert_eq!(organism.retreat_until, previous);
        assert_eq!(organism.live_greeting.deadline(Duration::ZERO), None);
        organism.retreat();
        organism.restore_retreat_after_palette_open(None);
        assert!(!organism.is_retreating());
        assert_eq!(organism.live_greeting.deadline(Duration::ZERO), None);
    }

    #[test]
    fn greeting_geometry_preserves_explicit_body_hold_but_retires_hidden_or_hover() {
        let mut organism = Organism::default();
        let context = PreviewPose::Calm.context();
        organism.live_greeting.enter(
            1,
            organism.live_greeting.epoch(),
            Duration::ZERO,
            context,
            true,
        );
        organism.sync_greeting_geometry(false, true);
        assert_eq!(organism.live_greeting.deadline(Duration::ZERO), None);
        organism.sync_greeting_geometry(true, true);
        organism.live_greeting.enter(
            1,
            organism.live_greeting.epoch(),
            Duration::ZERO,
            context,
            true,
        );
        assert_eq!(organism.live_greeting.deadline(Duration::ZERO), None);
        assert_eq!(
            organism
                .live_greeting
                .request_explicit(1, Duration::ZERO, context),
            crate::organism_hover::ExplicitGreeting::Accepted
        );
        for (glyph, presentable) in [(true, true), (false, true), (false, true), (true, true)] {
            organism.sync_greeting_geometry(glyph, presentable);
            assert_eq!(
                organism
                    .live_greeting
                    .apply(1, Duration::from_millis(500), context)
                    .behavior,
                Behavior::Approach
            );
        }
        organism.sync_greeting_geometry(false, false);
        organism.sync_greeting_geometry(true, true);
        assert_eq!(
            organism
                .live_greeting
                .apply(1, Duration::from_millis(500), context),
            context
        );
        assert_eq!(
            organism.live_greeting.deadline(Duration::from_millis(500)),
            None
        );
        assert_eq!(
            organism
                .live_greeting
                .request_explicit(1, Duration::from_secs(1), context),
            crate::organism_hover::ExplicitGreeting::Cooldown
        );
    }

    #[test]
    fn explicit_live_greeting_does_not_change_life_or_preview_cooldown() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.say_hello_at(Duration::ZERO);
        let before = values(organism.life.state());
        let preview_ready = organism.preview_deadlines.ready_at;
        assert_eq!(
            organism.greet_live(1),
            crate::organism_hover::ExplicitGreeting::Accepted
        );
        assert_eq!(values(organism.life.state()), before);
        assert_eq!(organism.preview_deadlines.ready_at, preview_ready);
        assert_eq!(
            organism.greet_live(1),
            crate::organism_hover::ExplicitGreeting::Cooldown
        );
        assert_eq!(
            organism.greet_live(99),
            crate::organism_hover::ExplicitGreeting::Busy
        );
    }

    #[test]
    fn motion_changes_cancel_transients_but_keep_cooldown_and_unchanged_visits() {
        use jterm_core::organism::WatchRhythm;
        for previous in [
            None,
            Some(Motion::Full),
            Some(Motion::Calm),
            Some(Motion::Static),
        ] {
            for next in [
                None,
                Some(Motion::Full),
                Some(Motion::Calm),
                Some(Motion::Static),
            ] {
                let mut organism = Organism::default();
                let context = PreviewPose::Calm.context();
                organism.watch.observe(Some(1), true, Duration::ZERO);
                for millis in [0, 100, 200, 300] {
                    organism
                        .watch
                        .output(1, Duration::from_millis(millis), true);
                }
                organism.live_greeting.enter(
                    1,
                    organism.live_greeting.epoch(),
                    Duration::ZERO,
                    context,
                    true,
                );
                organism
                    .live_greeting
                    .advance(Some(1), Duration::from_millis(600), context, true);
                organism.say_hello_at(Duration::ZERO);
                let before = values(organism.life.state());
                let now = Duration::from_millis(700);
                assert_eq!(organism.watch.rhythm(1, now), WatchRhythm::Busy);
                assert_eq!(
                    organism.live_greeting.apply(1, now, context).behavior,
                    PreviewPose::Greeting.context().behavior
                );
                organism.motion_changed(previous, next);
                if previous == next {
                    assert_eq!(organism.watch.rhythm(1, now), WatchRhythm::Busy);
                    assert_eq!(
                        organism.live_greeting.apply(1, now, context).behavior,
                        PreviewPose::Greeting.context().behavior
                    );
                } else {
                    assert_eq!(organism.watch.rhythm(1, now), WatchRhythm::Steady);
                    assert_eq!(organism.live_greeting.apply(1, now, context), context);
                    assert_eq!(organism.live_greeting.deadline(now), None);
                    // A new visit still cannot spend the accepted greeting's cooldown.
                    organism.live_greeting.leave();
                    organism.live_greeting.enter(
                        1,
                        organism.live_greeting.epoch(),
                        now,
                        context,
                        true,
                    );
                    organism.live_greeting.advance(
                        Some(1),
                        Duration::from_millis(1300),
                        context,
                        true,
                    );
                    assert_eq!(
                        organism
                            .live_greeting
                            .apply(1, Duration::from_millis(1300), context),
                        context
                    );
                }
                assert_eq!(organism.preview_deadlines.ready_at, Duration::from_secs(8));
                assert_eq!(values(organism.life.state()), before);
            }
        }
    }

    #[test]
    fn motion_change_cancels_pending_dwell_without_rearming_stationary_pointer() {
        let mut organism = Organism::default();
        let context = PreviewPose::Calm.context();
        organism.live_greeting.enter(
            1,
            organism.live_greeting.epoch(),
            Duration::ZERO,
            context,
            true,
        );
        organism.motion_changed(Some(Motion::Full), Some(Motion::Full));
        assert_eq!(
            organism.live_greeting.deadline(Duration::from_millis(200)),
            Some(Duration::from_millis(600))
        );
        organism.motion_changed(Some(Motion::Full), Some(Motion::Calm));
        assert_eq!(
            organism.live_greeting.deadline(Duration::from_millis(300)),
            None
        );
        organism.live_greeting.enter(
            1,
            organism.live_greeting.epoch(),
            Duration::from_millis(300),
            context,
            true,
        );
        organism
            .live_greeting
            .advance(Some(1), Duration::from_secs(1), context, true);
        assert_eq!(
            organism
                .live_greeting
                .apply(1, Duration::from_secs(1), context),
            context
        );
        assert_eq!(
            organism.live_greeting.deadline(Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn background_close_preserves_stable_owner_but_owner_close_retires_transients() {
        use jterm_core::organism::WatchRhythm;
        for next in [Some(10), Some(20), None] {
            let mut organism = Organism::default();
            organism.set_enabled(true);
            organism.prime_session(10, false);
            organism.prime_session(20, false);
            let context = PreviewPose::Calm.context();
            organism.watch.observe(Some(10), true, Duration::ZERO);
            for millis in [0, 100, 200, 300] {
                organism
                    .watch
                    .output(10, Duration::from_millis(millis), true);
            }
            organism.live_greeting.enter(
                10,
                organism.live_greeting.epoch(),
                Duration::ZERO,
                context,
                true,
            );
            organism
                .live_greeting
                .advance(Some(10), Duration::from_millis(600), context, true);
            let now = Duration::from_millis(700);
            assert_eq!(organism.watch.rhythm(10, now), WatchRhythm::Busy);
            assert_eq!(
                organism.live_greeting.apply(10, now, context).behavior,
                PreviewPose::Greeting.context().behavior
            );
            organism.forget_session(if next == Some(10) { 20 } else { 10 });
            organism.owner_changed(Some(10), next);
            if next == Some(10) {
                assert_eq!(organism.watch.rhythm(10, now), WatchRhythm::Busy);
                assert_eq!(
                    organism.live_greeting.apply(10, now, context).behavior,
                    PreviewPose::Greeting.context().behavior
                );
            } else {
                assert_eq!(organism.watch.rhythm(10, now), WatchRhythm::Steady);
                assert_eq!(organism.live_greeting.apply(10, now, context), context);
                assert_eq!(organism.live_greeting.deadline(now), None);
                organism.live_greeting.leave();
                organism.live_greeting.enter(
                    20,
                    organism.live_greeting.epoch(),
                    now,
                    context,
                    true,
                );
                organism.live_greeting.advance(
                    Some(20),
                    Duration::from_millis(1300),
                    context,
                    true,
                );
                assert_eq!(
                    organism
                        .live_greeting
                        .apply(20, Duration::from_millis(1300), context),
                    context
                );
            }
        }
    }

    #[test]
    fn expanded_hint_names_the_actual_bottom_bar_control_when_needed() {
        assert!(
            expanded_companion_hint(false).contains("Bottom status bar (cwd, git, last command)")
        );
        assert!(expanded_companion_hint(true).contains("stays blank while hidden"));
    }

    #[test]
    fn demo_preserves_manual_pose_life_and_both_greeting_clocks() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.select_pose(Pose(PreviewPose::Curious));
        organism.say_hello_at(Duration::ZERO);
        let context = organism.sessions[&1].context;
        let epoch = organism.live_greeting.epoch();
        organism
            .live_greeting
            .enter(1, epoch, Duration::ZERO, context, true);
        organism
            .live_greeting
            .advance(Some(1), Duration::from_millis(600), context, true);
        let live_deadline = organism.live_greeting.deadline(Duration::from_secs(1));
        assert_eq!(live_deadline, Some(Duration::from_millis(2600)));
        let before = values(organism.life.state());
        organism.start_demo_at(Duration::from_secs(1));
        organism.say_hello_at(Duration::from_secs(2));
        assert_eq!(organism.preview_deadlines.ready_at, Duration::from_secs(8));
        assert_eq!(organism.preview_deadlines.greeting_until, Duration::ZERO);
        assert_eq!(
            organism.live_greeting.deadline(Duration::from_secs(1)),
            live_deadline
        );
        for (time, pose, step) in [
            (1, PreviewPose::Calm, 1),
            (3, PreviewPose::Working, 2),
            (5, PreviewPose::Concerned, 3),
            (7, PreviewPose::Success, 4),
            (9, PreviewPose::Sleeping, 5),
        ] {
            let preview =
                organism.preview_presentation_at(Some(Motion::Static), Duration::from_secs(time));
            assert_eq!(preview.pose, pose);
            assert_eq!(preview.demo_step, Some(step));
            assert_eq!(preview.hello, HelloAvailability::Demonstrating);
            assert_eq!(preview.sprite, sprite_frame_with_context(pose.context(), 0));
        }
        assert_eq!(
            organism.preview_deadline_at(Duration::from_millis(10999)),
            Some(Duration::from_secs(11))
        );
        let restored =
            organism.preview_presentation_at(Some(Motion::Static), Duration::from_secs(11));
        assert_eq!(restored.pose, PreviewPose::Curious);
        assert_eq!(restored.demo_step, None);
        assert_eq!(organism.preview_deadline_at(Duration::from_secs(11)), None);
        assert_eq!(organism.pose, Pose(PreviewPose::Curious));
        assert_eq!(values(organism.life.state()), before);
        assert_eq!(organism.sessions.len(), 1);
    }

    #[test]
    fn demo_cancellation_and_hello_entry_guard_are_explicit() {
        for cancel in [0, 1, 2] {
            let mut organism = Organism::default();
            organism.start_demo_at(Duration::ZERO);
            organism.say_hello_at(Duration::from_secs(1));
            assert_eq!(organism.preview_deadlines.ready_at, Duration::ZERO);
            match cancel {
                0 => organism.stop_demo(),
                1 => organism.close_preview(),
                _ => organism.select_pose(Pose(PreviewPose::Curious)),
            }
            for time in [1, 9, 100] {
                let preview = organism.preview_presentation_at(None, Duration::from_secs(time));
                assert_eq!(preview.demo_step, None);
                assert_eq!(preview.pose, organism.pose.0);
                assert_eq!(
                    organism.preview_deadline_at(Duration::from_secs(time)),
                    None
                );
            }
            organism.say_hello_at(Duration::from_secs(1));
            assert_eq!(organism.preview_deadlines.ready_at, Duration::from_secs(9));
        }
        let mut cooling = Organism::default();
        cooling.say_hello_at(Duration::ZERO);
        cooling.start_demo_at(Duration::from_secs(1));
        cooling.stop_demo();
        assert_eq!(
            cooling.hello_availability_at(Duration::from_secs(2)),
            HelloAvailability::CoolingDown
        );
        assert_eq!(
            cooling.preview_deadline_at(Duration::from_secs(2)),
            Some(Duration::from_secs(8))
        );
    }

    #[test]
    fn live_sprite_and_glyph_share_the_greeting_without_changing_life() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        let context = RenderContext {
            body_language: BodyLanguage::from_state(organism.life.state()),
            ..organism.sessions[&1].context
        };
        let epoch = organism.live_greeting.epoch();
        organism
            .live_greeting
            .enter(1, epoch, Duration::ZERO, context, true);
        organism
            .live_greeting
            .advance(Some(1), Duration::from_millis(600), context, true);
        let before = values(organism.life.state());
        let now = Duration::from_millis(700);
        let final_context = organism.live_greeting.apply(1, now, context);
        assert_eq!(
            final_context.behavior,
            PreviewPose::Greeting.context().behavior
        );
        let presentation = organism.presentation_at(1, false, false, Some(Motion::Calm), now);
        assert_eq!(
            presentation.glyph,
            sticky_glyph_with_context(final_context, 0)
        );
        assert_eq!(
            presentation.sprite,
            sprite_frame_with_context(final_context, 0)
        );
        assert_eq!(
            presentation.explanation,
            jterm_core::organism_daily::behavior_explanation(final_context.behavior)
        );
        let full = organism.presentation_at(1, false, false, Some(Motion::Full), now);
        assert_eq!(full.glyph, sticky_glyph_with_context(final_context, 7));
        assert_eq!(full.sprite, sprite_frame_with_context(final_context, 7));
        assert_eq!(values(organism.life.state()), before);
        let hidden = LivePresentation::default();
        assert!(
            hidden.glyph.is_empty() && hidden.sprite.is_empty() && hidden.explanation.is_empty()
        );
    }

    #[test]
    fn motion_help_matches_the_live_capabilities() {
        assert_eq!(
            MotionChoice::Automatic.description(),
            MotionChoice::Calm.description()
        );
        assert!(MotionChoice::Full.description().contains("Animated"));
        assert!(MotionChoice::Calm.description().contains("Still"));
        assert!(MotionChoice::Static
            .description()
            .contains("no output rhythm"));
        for choice in [
            MotionChoice::Automatic,
            MotionChoice::Full,
            MotionChoice::Calm,
        ] {
            assert!(choice.interaction_hint().contains("600 ms"));
            assert!(choice.interaction_hint().contains("8-second cooldown"));
        }
        assert!(MotionChoice::Static
            .interaction_hint()
            .contains("unavailable"));
    }

    #[test]
    fn live_watch_explanation_preserves_behavior_and_names_only_observed_rhythm() {
        for behavior in [Behavior::WatchCommand, Behavior::WatchSettled] {
            let context = RenderContext::new(
                behavior,
                BodyLanguage::from_state(LifeState::default()),
                false,
            );
            let base = jterm_core::organism_daily::behavior_explanation(behavior);
            assert!(matches!(live_explanation(context), Cow::Borrowed(_)));
            for (rhythm, observation) in [
                (WatchRhythm::Busy, "Recent output activity."),
                (WatchRhythm::Waiting, "No new output observed recently."),
                (WatchRhythm::Resumed, "Output activity resumed."),
            ] {
                assert_eq!(
                    live_explanation(context.with_watch_rhythm(rhythm)),
                    format!("{base} {observation}")
                );
            }
        }
    }

    #[test]
    fn non_watch_explanations_ignore_any_stale_rhythm() {
        for behavior in [
            Behavior::Idle,
            Behavior::Approach,
            Behavior::InspectError,
            Behavior::Celebrate,
            Behavior::UnknownOutcome,
            Behavior::WatchAgent,
        ] {
            for rhythm in [
                WatchRhythm::Steady,
                WatchRhythm::Busy,
                WatchRhythm::Waiting,
                WatchRhythm::Resumed,
            ] {
                let context = RenderContext::new(
                    behavior,
                    BodyLanguage::from_state(LifeState::default()),
                    false,
                )
                .with_watch_rhythm(rhythm);
                assert_eq!(
                    live_explanation(context),
                    jterm_core::organism_daily::behavior_explanation(behavior)
                );
                assert!(matches!(live_explanation(context), Cow::Borrowed(_)));
            }
        }
    }

    #[test]
    fn live_explanation_uses_final_motion_context_without_frame_chatter() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, true);
        organism.set_remote(1, false);
        organism.watch.observe(Some(1), true, Duration::ZERO);
        let before = values(organism.life.state());
        for settled in [false, true] {
            let behavior = if settled {
                Behavior::WatchSettled
            } else {
                Behavior::WatchCommand
            };
            let base = jterm_core::organism_daily::behavior_explanation(behavior);
            for motion in [None, Some(Motion::Calm), Some(Motion::Full)] {
                let first =
                    organism.presentation_at(1, true, settled, motion, Duration::from_secs(4));
                let next =
                    organism.presentation_at(1, true, settled, motion, Duration::from_millis(4100));
                assert_eq!(
                    first.explanation,
                    format!("{base} No new output observed recently.")
                );
                assert_eq!(first.explanation, next.explanation);
            }
            let still = organism.presentation_at(
                1,
                true,
                settled,
                Some(Motion::Static),
                Duration::from_secs(4),
            );
            assert_eq!(still.explanation, base);
            let other_owner = organism.presentation_at(
                2,
                true,
                settled,
                Some(Motion::Full),
                Duration::from_secs(4),
            );
            assert_eq!(other_owner.explanation, base);
        }
        organism.reset_watch();
        let reset =
            organism.presentation_at(1, true, false, Some(Motion::Full), Duration::from_secs(4));
        assert_eq!(
            reset.explanation,
            jterm_core::organism_daily::behavior_explanation(Behavior::WatchCommand)
        );
        assert_eq!(values(organism.life.state()), before);
    }

    #[test]
    fn watch_rhythm_is_display_only_static_safe_and_owner_bound() {
        use jterm_core::organism::WatchRhythm;
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, true);
        organism.set_remote(1, false);
        organism.born = Instant::now() - Duration::from_secs(10);
        organism.watch.observe(Some(1), true, Duration::ZERO);
        let before = organism.life.state();
        for settled in [false, true] {
            let behavior = if settled {
                Behavior::WatchSettled
            } else {
                Behavior::WatchCommand
            };
            let context = RenderContext::new(behavior, BodyLanguage::from_state(before), false);
            let LivePresentation { glyph, sprite, .. } =
                organism.presentation(1, true, settled, Some(Motion::Calm));
            assert_eq!(
                sprite,
                sprite_frame_with_context(context.with_watch_rhythm(WatchRhythm::Waiting), 0)
            );
            assert_eq!(
                glyph,
                sticky_glyph_with_context(context.with_watch_rhythm(WatchRhythm::Waiting), 0)
            );
            let LivePresentation { glyph, .. } =
                organism.presentation(1, true, settled, Some(Motion::Static));
            assert_eq!(glyph, sticky_glyph_with_context(context, 0));
        }
        assert_eq!(values(organism.life.state()), values(before));
        organism.completed(2, "cargo test", Some(0), Some(10));
        assert_eq!(
            organism.watch.rhythm(1, Duration::from_secs(10)),
            WatchRhythm::Waiting
        );
        // Even a completion ignored by enable-time quarantine retires cadence.
        organism.completed(1, "cargo test", Some(0), Some(10));
        assert_eq!(
            organism.watch.rhythm(1, Duration::from_secs(10)),
            WatchRhythm::Steady
        );
        assert!(!organism.watch_batch_admitted(1));
        organism.batch_finished(1, true);
        assert!(organism.watch_batch_admitted(1));
        organism.observe_watch(Some(1), true);
        organism.set_remote(1, true);
        assert_eq!(
            organism.watch.rhythm(1, Duration::from_secs(20)),
            WatchRhythm::Steady
        );
    }

    #[test]
    fn live_greeting_has_no_physiology_or_preview_side_effects() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        let before = values(organism.life.state());
        let preview_ready = organism.preview_deadlines.ready_at;
        organism.live_greeting.enter(
            1,
            organism.live_greeting.epoch(),
            Duration::ZERO,
            PreviewPose::Calm.context(),
            true,
        );
        organism.live_greeting.advance(
            Some(1),
            Duration::from_millis(600),
            PreviewPose::Calm.context(),
            true,
        );
        let _ = organism.presentation(1, false, false, Some(Motion::Calm));
        assert_eq!(values(organism.life.state()), before);
        assert_eq!(organism.preview_deadlines.ready_at, preview_ready);
        assert_eq!(
            organism.hello_availability_at(Duration::ZERO),
            HelloAvailability::Available
        );
    }

    #[test]
    fn background_session_events_do_not_cancel_the_frontmost_greeting() {
        for event in ["remote", "forget", "completed", "running"] {
            for target in [1, 2] {
                let mut organism = Organism::default();
                organism.set_enabled(true);
                for id in [1, 2] {
                    organism.prime_session(id, false);
                    organism.set_remote(id, false);
                    organism.batch_finished(id, false);
                }
                organism.enter_live_greeting(1, organism.live_greeting_epoch(), true);
                assert!(organism.live_greeting_deadline().is_some());
                match event {
                    "remote" => organism.set_remote(target, true),
                    "forget" => organism.forget_session(target),
                    "completed" => organism.completed(target, "cargo test", Some(0), Some(1)),
                    _ => organism.batch_finished(target, true),
                }
                assert_eq!(organism.live_greeting_deadline().is_some(), target == 2);
            }
        }
    }

    #[test]
    fn live_explanation_uses_the_same_authoritative_behavior_as_the_glyph() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(3, false);
        let before = values(organism.life.state());
        for behavior in [
            Behavior::Idle,
            Behavior::InspectError,
            Behavior::SitNearError,
            Behavior::Celebrate,
            Behavior::CelebrateBig,
            Behavior::UnknownOutcome,
            Behavior::RestAfterPush,
        ] {
            organism.sessions.get_mut(&3).unwrap().context = RenderContext::new(
                behavior,
                BodyLanguage::from_state(organism.life.state()),
                false,
            );
            let LivePresentation {
                glyph,
                sprite,
                explanation,
            } = organism.presentation(3, false, false, Some(Motion::Static));
            assert!(!glyph.is_empty());
            assert_eq!(
                sprite,
                sprite_frame_with_context(organism.sessions[&3].context, 0)
            );
            assert_eq!(sprite.lines().count(), 3);
            assert_eq!(
                explanation,
                jterm_core::organism_daily::behavior_explanation(behavior)
            );
        }
        for (settled, behavior) in [
            (false, Behavior::WatchCommand),
            (true, Behavior::WatchSettled),
        ] {
            let presentation = organism.presentation(3, true, settled, Some(Motion::Static));
            let context = RenderContext::new(
                behavior,
                BodyLanguage::from_state(organism.life.state()),
                false,
            );
            assert_eq!(presentation.sprite, sprite_frame_with_context(context, 0));
            let explanation = presentation.explanation;
            assert_eq!(
                explanation,
                jterm_core::organism_daily::behavior_explanation(behavior)
            );
        }
        assert_eq!(values(organism.life.state()), before);
    }

    #[test]
    fn hello_availability_is_read_only_and_explains_busy_and_cooldown() {
        let mut organism = Organism::default();
        for _ in 0..3 {
            assert_eq!(
                organism.hello_availability_at(Duration::ZERO),
                HelloAvailability::Available
            );
        }
        organism.say_hello_at(Duration::ZERO);
        assert_eq!(
            organism.hello_availability_at(Duration::from_secs(1)),
            HelloAvailability::CoolingDown
        );
        assert_eq!(
            organism.hello_availability_at(Duration::from_secs(8)),
            HelloAvailability::Available
        );
        // Reading the future must not advance the real attention clock.
        organism.say_hello_at(Duration::from_secs(1));
        assert_eq!(
            organism.preview_deadlines.ready_at,
            GentleInteraction::COOLDOWN
        );
        organism.select_pose(Pose(PreviewPose::Working));
        assert_eq!(
            organism.hello_availability_at(Duration::from_secs(8)),
            HelloAvailability::Busy
        );
        organism.select_pose(Pose(PreviewPose::Calm));
        organism.close_preview();
        organism.set_enabled(true);
        organism.set_enabled(false);
        assert_eq!(
            organism.hello_availability_at(Duration::from_secs(7)),
            HelloAvailability::CoolingDown
        );
        assert_eq!(
            organism.hello_availability_at(Duration::from_secs(8)),
            HelloAvailability::Available
        );
    }

    #[test]
    fn calm_preview_wakes_only_at_absolute_greeting_and_cooldown_deadlines() {
        let mut deadlines = PreviewDeadlines::default();
        assert_eq!(deadlines.next(Duration::ZERO, true), None);
        deadlines.accepted(Duration::from_secs(10));
        assert_eq!(
            deadlines.next(Duration::from_secs(10), true),
            Some(Duration::from_secs(12))
        );
        // An unrelated redraw does not restart a relative timer.
        assert_eq!(
            deadlines.next(Duration::from_secs(11), true),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            deadlines.next(Duration::from_secs(12), true),
            Some(Duration::from_secs(18))
        );
        assert_eq!(deadlines.next(Duration::from_secs(18), true), None);
        deadlines.cancel();
        assert_eq!(deadlines.next(Duration::from_secs(11), false), None);
        assert_eq!(
            deadlines.next(Duration::from_secs(11), true),
            Some(Duration::from_secs(18))
        );
    }

    #[test]
    fn rejected_hello_and_preview_changes_never_extend_or_reset_cooldown() {
        let mut organism = Organism::default();
        let state = values(organism.life.state());
        organism.say_hello_at(Duration::ZERO);
        organism.say_hello_at(Duration::from_secs(1));
        assert_eq!(
            organism.preview_deadlines.greeting_until,
            GentleInteraction::HOLD
        );
        assert_eq!(
            organism.preview_deadlines.ready_at,
            GentleInteraction::COOLDOWN
        );
        organism.select_pose(Pose(PreviewPose::Working));
        assert_eq!(organism.preview_deadlines.greeting_until, Duration::ZERO);
        organism.say_hello_at(Duration::from_secs(3));
        assert_eq!(
            organism.preview_deadlines.ready_at,
            GentleInteraction::COOLDOWN
        );
        organism.select_pose(Pose(PreviewPose::Calm));
        organism.close_preview();
        organism.set_enabled(true);
        organism.set_enabled(false);
        organism.say_hello_at(Duration::from_secs(7));
        assert_eq!(organism.preview_deadlines.greeting_until, Duration::ZERO);
        organism.say_hello_at(Duration::from_secs(8));
        assert_eq!(
            organism.preview_deadlines.greeting_until,
            Duration::from_secs(10)
        );
        assert_eq!(organism.preview_deadlines.ready_at, Duration::from_secs(16));
        assert_eq!(values(organism.life.state()), state);
        assert!(organism.sessions.is_empty());
    }

    #[test]
    fn retreat_wake_is_exact_and_expires_without_a_second_heartbeat() {
        let mut organism = Organism::default();
        let now = organism.born;
        organism.retreat_until = now + Duration::from_millis(900);
        assert_eq!(
            organism.retreat_deadline_at(now + Duration::from_millis(899)),
            Some(now + Duration::from_millis(900))
        );
        assert_eq!(
            organism.retreat_deadline_at(now + Duration::from_millis(900)),
            None
        );
        organism.retreat_until = now + Duration::from_millis(1500);
        assert_eq!(
            organism.retreat_deadline_at(now + Duration::from_millis(900)),
            Some(now + Duration::from_millis(1500))
        );
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
        organism.say_hello_at(organism.born.elapsed());
        organism.preview_presentation(Some(Motion::Full));
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
            assert!(!organism.preview_presentation(None).sprite.is_empty());
        }
        organism.say_hello_at(organism.born.elapsed());
        assert!(organism.sessions.is_empty());
        assert_eq!(Organism::frame_at(None, Duration::from_secs(1)), 0);
        assert_eq!(
            Organism::frame_at(Some(Motion::Calm), Duration::from_secs(1)),
            0
        );
        assert_eq!(
            Organism::frame_at(Some(Motion::Static), Duration::from_secs(1)),
            0
        );
    }
}
