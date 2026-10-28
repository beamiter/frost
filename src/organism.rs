//! Offline, volatile ASCII-organism chrome adapter.
//!
//! Deliberately does not access terminal bytes, widgets, PTYs, or files. The
//! host supplies one-shot completed commands and a content-free running flag.
//! Stable live Start identities are not public in Frost's terminal boundary,
//! so watching is display-only: no synthetic `command_started` is generated.
use crate::organism_preview::{PreviewAction, PreviewRequest, PreviewSequence};
use crate::organism_watch::WatchObservation;
use jterm_core::organism_live::{
    LiveActivityKnowledge, LiveAttention, LiveAttentionEvent, LiveAttentionInput,
    LiveAttentionOutcome, LiveAttentionRejection, LiveAttentionSnapshot, LiveAttentionSource,
    LiveAttentionTicket,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use jterm_core::organism::{
    classify_command, sprite_frame_with_context, sticky_glyph_with_context, AmbientMind, Behavior,
    BodyLanguage, CircadianPhase, LifeState, NativeOrganism, RenderContext, RepoVigil, Tone,
};
use jterm_core::organism_companion::{CompanionCare, CompanionIntent, CompanionVoice};
use jterm_core::organism_daily::{GentleInteraction, PreviewPose};
use serde::{Deserialize, Serialize};

/// Current native admission facts; no controller state or timing policy.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AttentionFacts {
    pub eligible: bool,
    pub hover_available: bool,
    pub activity: LiveActivityKnowledge,
}

impl AttentionFacts {
    #[cfg(test)]
    pub(crate) fn known_idle_for_test(eligible: bool, hover_available: bool) -> Self {
        Self {
            eligible,
            hover_available,
            activity: LiveActivityKnowledge::KnownIdle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    Full,
    Calm,
    Static,
}

// Representation-only conversion; the shared core remains the sole resolver.
impl From<Motion> for jterm_core::organism_motion::Motion {
    fn from(value: Motion) -> Self {
        match value {
            Motion::Full => Self::Full,
            Motion::Calm => Self::Calm,
            Motion::Static => Self::Static,
        }
    }
}

impl From<jterm_core::organism_motion::Motion> for Motion {
    fn from(value: jterm_core::organism_motion::Motion) -> Self {
        match value {
            jterm_core::organism_motion::Motion::Full => Self::Full,
            jterm_core::organism_motion::Motion::Calm => Self::Calm,
            jterm_core::organism_motion::Motion::Static => Self::Static,
        }
    }
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
            Self::Automatic => {
                "Follows the desktop animation preference; uses Calm when unavailable."
            }
            Self::Calm => "Still poses that respond to work.",
            Self::Full => "Animated poses that respond to work.",
            Self::Static => "Still poses that respond to work; no output rhythm.",
        }
    }

    pub fn interaction_hint(self) -> &'static str {
        match self {
            Self::Static => "Hover greetings are unavailable in Static.",
            _ => "While idle, hover over the companion's visible body or compact glyph for 600 ms to say hello. Typing or busy work cancels it. One greeting per visit, with an 8-second cooldown.",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pose(pub PreviewPose);

impl fmt::Display for Pose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.label())
    }
}

pub use jterm_core::organism_clock::WindowLife;

/// Only reducer-authored, content-free feedback survives the event drain.
/// The original hold controls its lifetime; no command or terminal text is kept.
struct CompletionFeedback {
    behavior: Behavior,
    status: String,
    tone: Tone,
}

struct SessionContext {
    native: NativeOrganism,
    /// Volatile command-scoped cwd, bounded independently of terminal history.
    work_context: Option<String>,
    /// Fresh idle-directory evidence matches the command-at-start scope.
    work_context_current: bool,
    context: RenderContext,
    /// A command already running when enabled is deliberately not replayed.
    accept_completion: bool,
    /// No event-source activation epoch exists: discard the initial PTY batch.
    quarantine_batch: bool,
    reaction_until: Option<Instant>,
    completion: Option<CompletionFeedback>,
    remote: bool,
    next_remote_probe: Instant,
}

impl SessionContext {
    fn new(running: bool, now: Instant) -> Self {
        Self {
            native: NativeOrganism::from_persisted_state(LifeState::default()),
            work_context: None,
            work_context_current: false,
            context: PreviewPose::Calm.context(),
            accept_completion: !running,
            quarantine_batch: true,
            reaction_until: None,
            completion: None,
            remote: true,
            next_remote_probe: now,
        }
    }

    fn set_work_context(&mut self, cwd: Option<&str>) {
        self.work_context_current = false;
        let bounded = cwd.filter(|cwd| !cwd.is_empty() && cwd.len() <= 2048);
        if bounded.is_none() || bounded != self.work_context.as_deref() {
            // Unknown locations cannot join two commands into one work loop.
            // Clear only repo work/habituation, retaining continuous life.
            self.native.restore_repo_context(0, false, 0, 0);
            self.work_context = bounded.map(str::to_owned);
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
    /// Same shared pose with the presentation-only native walking flag.
    pub walking_sprite: String,
    pub explanation: Cow<'static, str>,
    pub status: Cow<'static, str>,
    /// A qualitative description of the creature, never the user's state.
    pub life: &'static str,
    pub tone: Option<Tone>,
    pub wander: Option<u64>,
    pub bottom: bool,
}

impl LivePresentation {
    /// Paint-only fallback for reserved chrome, never evidence of shell idleness.
    /// No owner, live facts, physiology, or animation clock is consulted here.
    #[cfg(test)]
    pub fn passive_presence() -> Self {
        let context = PreviewPose::Calm.context();
        Self {
            glyph: sticky_glyph_with_context(context, 0).into_owned(),
            sprite: sprite_frame_with_context(context, 0).into_owned(),
            walking_sprite: sprite_frame_with_context(context, 0).into_owned(),
            explanation: Cow::Borrowed("Quietly here. Live reactions are paused in this context."),
            status: Cow::Borrowed("quiet · live reactions paused"),
            life: "quietly here",
            tone: Some(Tone::Quiet),
            wander: None,
            bottom: true,
        }
    }
}

/// Describe only the output rhythm already selected for this render snapshot.
/// These observations do not infer command progress, intent, or stalled work.
fn live_explanation(context: RenderContext) -> Cow<'static, str> {
    Cow::Borrowed(jterm_core::organism_daily::context_explanation(context))
}

pub struct PreviewPresentation {
    pub sprite: String,
    pub pose: PreviewPose,
    pub demo_step: Option<usize>,
    pub demo_paused: bool,
    pub hello: HelloAvailability,
}

/// Content-free lifecycle edge detector. Disabled observers still advance their
/// baseline, so enabling a companion never replays an old Agent reward.
#[derive(Debug, Default)]
struct AgentPulseTracker {
    present: bool,
    last: Option<jterm_core::organism::AgentPulse>,
}

impl AgentPulseTracker {
    fn observe(
        &mut self,
        state: Option<jterm_core::agent::AgentState>,
        busy: bool,
        enabled: bool,
    ) -> Option<jterm_core::organism::AgentPulse> {
        let present = state.is_some();
        let pulse = state.and_then(|state| jterm_core::organism::agent_state_pulse(state, busy));
        let pulse = if !present && self.present {
            Some(jterm_core::organism::AgentPulse::Gone)
        } else {
            pulse
        };
        let changed = pulse != self.last;
        self.present = present;
        // Preserve Gone across an absent session to avoid replaying a close.
        if present || pulse.is_some() {
            self.last = pulse;
        }
        if enabled && changed {
            pulse
        } else {
            None
        }
    }
}

/// Facts from one already validated terminal completion. Agent attribution is
/// supplied only by the terminal's local one-shot approval generation.
pub struct CommandCompletion<'a> {
    pub command: &'a str,
    pub exit: Option<i32>,
    pub duration: Option<u64>,
    pub cwd: Option<&'a str>,
    pub agent_driven: bool,
}

pub struct Organism {
    agent_pulses: AgentPulseTracker,
    enabled: bool,
    sessions: HashMap<usize, SessionContext>,
    born: Instant,
    life: WindowLife,
    /// A pane owns work observations, never a separate mind or personality.
    ambient: AmbientMind,
    voice: CompanionVoice,
    reply: &'static str,
    care: CompanionCare,
    last_human_input: Option<Duration>,
    work_day: Option<i64>,
    retreat_until: Instant,
    pub pose: Pose,
    hello: GentleInteraction,
    preview_deadlines: PreviewDeadlines,
    preview_sequence: PreviewSequence,
    preview_visible: bool,
    live_attention: LiveAttention<usize>,
    // Native pointer observation only. Entry consumption is owned by the core.
    live_pointer_inside: bool,
    watch: WatchObservation,
}

impl Default for Organism {
    fn default() -> Self {
        let now = Instant::now();
        let seed = crate::config::Config::config_path().ok().map_or_else(
            || CompanionVoice::seed_for_key("frost-companion"),
            |path| CompanionVoice::seed_for_key(&path.to_string_lossy()),
        );
        Self {
            agent_pulses: Default::default(),
            enabled: false,
            sessions: HashMap::new(),
            born: now,
            life: WindowLife::new_at(Duration::ZERO),
            ambient: AmbientMind::seeded(seed),
            voice: CompanionVoice::new(seed),
            reply: "I'm here. We can work quietly, share a story, or take a little break.",
            care: CompanionCare::default(),
            last_human_input: None,
            work_day: None,
            retreat_until: now,
            pose: Pose(PreviewPose::Calm),
            hello: GentleInteraction::default(),
            preview_deadlines: PreviewDeadlines::default(),
            preview_sequence: PreviewSequence::default(),
            preview_visible: false,
            live_attention: LiveAttention::default(),
            live_pointer_inside: false,
            watch: WatchObservation::default(),
        }
    }
}

impl Organism {
    pub fn observe_agent(&mut self, state: Option<jterm_core::agent::AgentState>, busy: bool) {
        if let Some(pulse) = self.agent_pulses.observe(state, busy, self.enabled) {
            self.life
                .replace_state(jterm_core::organism::agent_pulse(self.life.state(), pulse));
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.sessions.clear();
            self.watch.reset();
            self.close_preview();
            self.cancel_live_greeting();
            self.enabled = enabled;
            // Visibility is not birth. Retire transient command admission, but
            // keep the window's learned physiology, personality and voice.
            self.life
                .advance(self.born.elapsed(), false, false, CircadianPhase::Unlearned);
            self.ambient.interrupt();
            self.work_day = None;
            self.last_human_input = None;
        }
    }

    /// The host supplies local civil-day knowledge on existing event/tick
    /// boundaries. A new or unknown day retires old work loops, preserving the
    /// window's continuous physiology and short completion hold.
    pub fn observe_work_day(&mut self, day: Option<i64>) {
        if !self.enabled || self.work_day == day {
            return;
        }
        self.work_day = day;
        self.ambient.interrupt();
        for life in self.sessions.values_mut() {
            life.set_work_context(None);
            if life.context.behavior.is_repo_vigil() {
                life.context = PreviewPose::Calm.context();
            }
        }
    }

    /// Location evidence is supplied by the native prompt parser, never by
    /// polling a process, replaying a command or guessing from visible text.
    pub fn observe_work_context(&mut self, id: usize, idle_cwd: Option<&str>) {
        if let Some(life) = self.sessions.get_mut(&id) {
            let current = self.enabled
                && self.work_day.is_some()
                && !life.remote
                && idle_cwd.is_some_and(|cwd| {
                    cwd.starts_with('/')
                        && cwd.len() <= 2048
                        && life.work_context.as_deref() == Some(cwd)
                });
            life.work_context_current = current;
            if !current && life.context.behavior.is_repo_vigil() {
                if self.live_attention.owner() == Some(&id) {
                    self.ambient.interrupt();
                }
                life.context = PreviewPose::Calm.context();
            }
        }
    }

    /// Config reloads and the settings control share the same transient boundary.
    /// An unchanged preference must not interrupt an existing visit.
    pub fn motion_changed(&mut self, previous: Option<Motion>, next: Option<Motion>) {
        if previous != next {
            self.reset_watch();
            if next == Some(Motion::Static) {
                self.cancel_live_greeting();
            }
        }
    }

    /// Session indices can shift while the same foreground session stays active.
    pub fn owner_changed(&mut self, previous: Option<usize>, next: Option<usize>) {
        if previous != next {
            self.reset_watch();
            self.sync_live_attention_owner(next);
        }
    }

    pub fn prime_session(&mut self, id: usize, running: bool) {
        if self.enabled {
            self.sessions
                .entry(id)
                .or_insert_with(|| SessionContext::new(running, Instant::now()));
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
            self.live_attention.cancel(&id);
        }
        if let Some(life) = self.sessions.get_mut(&id) {
            life.remote = remote;
            if remote {
                life.work_context_current = false;
            }
            life.next_remote_probe = Instant::now() + Duration::from_millis(900);
        }
    }

    pub fn is_local(&self, id: usize) -> bool {
        self.sessions.get(&id).is_some_and(|life| !life.remote)
    }

    pub fn forget_session(&mut self, id: usize) {
        self.watch.reset_owner(id);
        self.live_attention.cancel(&id);
        self.sessions.remove(&id);
    }

    /// Called only from the existing completion drain, never by frame timers.
    /// No command text is retained, logged, sent, or persisted by this adapter.
    pub fn completed_event(&mut self, id: usize, event: CommandCompletion<'_>) {
        self.completed_event_at(id, event, self.born.elapsed());
    }

    #[cfg(test)]
    fn completed(
        &mut self,
        id: usize,
        command: &str,
        exit: Option<i32>,
        duration: Option<u64>,
        command_cwd: Option<&str>,
    ) {
        self.completed_at(
            id,
            command,
            exit,
            duration,
            command_cwd,
            self.born.elapsed(),
        );
    }

    #[cfg(test)]
    fn completed_at(
        &mut self,
        id: usize,
        command: &str,
        exit: Option<i32>,
        duration: Option<u64>,
        command_cwd: Option<&str>,
        now: Duration,
    ) {
        self.completed_event_at(
            id,
            CommandCompletion {
                command,
                exit,
                duration,
                cwd: command_cwd,
                agent_driven: false,
            },
            now,
        );
    }

    fn completed_event_at(&mut self, id: usize, event: CommandCompletion<'_>, now: Duration) {
        let CommandCompletion {
            command,
            exit,
            duration,
            cwd: command_cwd,
            agent_driven,
        } = event;
        self.watch.retire_owner(id);
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
        self.live_attention.cancel(&id);
        life.set_work_context(command_cwd);
        life.native.sync_state(self.life.state());
        life.native.set_agent_command(agent_driven);
        let reaction = life
            .native
            .command_finished(classify_command(command), exit, duration);
        self.life.replace_state(life.native.state());
        if self.live_attention.owner() == Some(&id) {
            self.ambient.interrupt();
        }
        life.context = RenderContext::new(
            reaction.behavior,
            BodyLanguage::from_state(life.native.state()),
            false,
        );
        life.reaction_until = Some(self.born + now + reaction.hold_duration());
        life.completion = Some(CompletionFeedback {
            behavior: reaction.behavior,
            // All text comes from the shared reducer. Bound even that text so
            // future metadata additions cannot turn the card into a transcript.
            status: reaction.description.chars().take(240).collect(),
            tone: reaction.tone,
        });
    }

    pub fn batch_finished(&mut self, id: usize, running: bool) {
        if running {
            self.live_attention.cancel(&id);
        }
        if let Some(life) = self.sessions.get_mut(&id) {
            if running {
                life.work_context_current = false;
                // A newly observed command supersedes the previous result.
                // Keep its earned physiology, but never replay that result if
                // the next shell boundary has no trustworthy completion.
                life.reaction_until = None;
                life.completion = None;
                if self.live_attention.owner() == Some(&id) {
                    self.ambient.interrupt();
                }
                life.context = RenderContext::new(
                    Behavior::Idle,
                    BodyLanguage::from_state(self.life.state()),
                    false,
                );
            }
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

    pub fn freeze_watch(&mut self, frozen: bool) {
        self.watch.freeze(frozen);
    }

    pub fn bind_watch_source(&mut self, source: Option<crate::organism_watch::SourceAuthority>) {
        self.watch.bind_source(source);
    }

    pub fn observe_watch(&mut self, eligible: Option<(usize, i32, u64)>, command: Option<u64>) {
        self.watch.sync(eligible, command, self.born.elapsed());
    }

    pub fn watch_output(
        &mut self,
        eligible: Option<(usize, i32, u64)>,
        command: Option<u64>,
        batch: crate::terminal::WatchBatch,
        stamp: crate::organism_watch::SourceStamp,
        nonempty: bool,
        admitted: bool,
    ) {
        let now = self.born.elapsed();
        if eligible.is_some_and(|source| source.2 != stamp.epoch) {
            return;
        }
        self.watch.prepare(eligible, now);
        let identity = eligible.zip(command).map(|((owner, fd, source), command)| {
            crate::organism_watch::CommandIdentity {
                owner,
                fd,
                source,
                command,
            }
        });
        if identity.is_none() {
            self.watch.sync(eligible, None, now);
            return;
        }
        let read_at = stamp
            .read_at
            .checked_duration_since(self.born)
            .unwrap_or_default();
        self.watch.batch(
            identity,
            batch,
            stamp.seq,
            (read_at, now),
            admitted,
            nonempty,
        );
    }

    pub fn note_output(&mut self, id: usize) {
        if self.enabled {
            self.life.note_output(self.born.elapsed());
            if self.live_attention.owner() == Some(&id) {
                self.ambient.interrupt();
            }
        }
    }

    pub fn accepted_input(&mut self) {
        if self.enabled {
            let now = self.born.elapsed();
            self.life.note_input(now);
            self.last_human_input = Some(now);
            // Accepted human input is an existing event source. Typing can
            // accrue active time but never surfaces an interruption itself.
            self.care.observe(now, true, false);
        }
        self.retreat();
    }

    /// An asynchronous input result belongs to its original pane, not whichever
    /// pane happens to own the live companion when delivery completes.
    pub fn retreat_for_input_attempt(
        &mut self,
        target: usize,
        active: Option<usize>,
        input_eligible: bool,
    ) {
        if active == Some(target) && input_eligible {
            self.retreat();
        }
    }

    /// Presentation-only retreat also serves blur and attempted gestures.
    pub fn retreat(&mut self) {
        self.modifier_pressed();
        self.retreat_until = Instant::now() + Duration::from_millis(900);
    }

    /// A standalone modifier does not encode terminal input. It still retires
    /// current attention and observation, without extending a real input deadline.
    pub fn modifier_pressed(&mut self) {
        self.watch.reset();
        self.cancel_live_greeting();
    }

    /// Only an identified palette-opening or greeting action may undo its own
    /// key's extension; unrelated input never calls this boundary.
    /// Already canceled visits/watch are deliberately not restored.
    pub fn restore_retreat_after_app_action(&mut self, previous: Option<Instant>) {
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

    pub fn preview_deadline(&self) -> Option<(Instant, crate::organism_wake_plan::DeadlineReason)> {
        self.preview_deadline_with_reason_at(self.born.elapsed())
            .map(|(deadline, reason)| (self.born + deadline, reason))
    }

    fn preview_deadline_with_reason_at(
        &self,
        now: Duration,
    ) -> Option<(Duration, crate::organism_wake_plan::DeadlineReason)> {
        use crate::organism_wake_plan::DeadlineReason;
        let deadline = self.preview_deadline_at(now)?;
        let reason = if self.preview_sequence.sample(now).is_some() {
            DeadlineReason::PreviewBoundary
        } else if deadline == self.preview_deadlines.greeting_until {
            DeadlineReason::PreviewGreeting
        } else {
            DeadlineReason::PreviewCooldown
        };
        Some((deadline, reason))
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

    /// The host requests this only for the current eligible observation owner.
    /// Keep the original absolute expiry; redraws must never restart a hold.
    pub fn reaction_deadline(&self, id: usize) -> Option<Instant> {
        self.reaction_deadline_at(id, self.born.elapsed())
    }

    fn reaction_deadline_at(&self, id: usize, now: Duration) -> Option<Instant> {
        if !self.enabled || !self.is_local(id) {
            return None;
        }
        self.sessions
            .get(&id)?
            .reaction_until
            .filter(|deadline| now < deadline.saturating_duration_since(self.born))
    }

    /// A background completion can expire without an owner tick. Resolve it at
    /// read time so returning to the pane cannot flash the stale reaction. The
    /// next eligible tick resumes ambient behavior without advancing it here.
    fn base_context_at(&self, id: usize, now: Duration) -> RenderContext {
        self.sessions
            .get(&id)
            .filter(|life| {
                (!life.context.behavior.is_repo_vigil() || life.work_context_current)
                    && !life.reaction_until.is_some_and(|deadline| {
                        now >= deadline.saturating_duration_since(self.born)
                    })
            })
            .map(|life| life.context)
            .unwrap_or_else(|| PreviewPose::Calm.context())
    }

    pub fn pause_clock(&mut self) {
        self.watch.reset();
        self.cancel_live_greeting();
        self.life
            .advance(self.born.elapsed(), false, false, CircadianPhase::Unlearned);
        self.care.observe(self.born.elapsed(), false, false);
    }

    pub fn tick(&mut self, owner: Option<usize>, running: bool, any_running: bool) {
        self.tick_at(owner, running, any_running, self.born.elapsed());
    }

    fn tick_at(
        &mut self,
        owner: Option<usize>,
        running: bool,
        any_running: bool,
        elapsed: Duration,
    ) {
        self.watch.heartbeat(elapsed);
        let dt = self.life.advance(
            elapsed,
            self.enabled && owner.is_some(),
            any_running,
            CircadianPhase::Unlearned,
        );
        let retreating = elapsed < self.retreat_until.saturating_duration_since(self.born);
        if let Some(life) = owner.and_then(|id| self.sessions.get_mut(&id)) {
            life.native.sync_state(self.life.state());
            if running || retreating {
                self.ambient.interrupt();
            } else if life
                .reaction_until
                .is_none_or(|deadline| elapsed >= deadline.saturating_duration_since(self.born))
            {
                life.reaction_until = None;
                life.completion = None;
                let behavior = self.ambient.step(
                    self.life.state(),
                    self.life.idle_for(elapsed).as_secs_f32(),
                    dt,
                    if life.work_context_current && self.work_day.is_some() {
                        life.native.repo_vigil()
                    } else {
                        RepoVigil::None
                    },
                );
                life.context = RenderContext::new(
                    behavior.display(),
                    BodyLanguage::from_state(self.life.state()),
                    false,
                );
            }
        }
    }

    pub fn sync_live_attention_owner(&mut self, owner: Option<usize>) {
        if self.live_attention.set_owner(owner) {
            self.live_pointer_inside = false;
        }
    }

    /// Explicit local conversation. No terminal text, shell command, or network
    /// request is involved, and switching panes never reseeds this voice.
    pub fn talk(&mut self, intent: CompanionIntent, owner: Option<usize>, running: bool) {
        let now = self.born.elapsed();
        let owner = owner.filter(|id| self.enabled && self.is_local(*id));
        if intent == CompanionIntent::Rest {
            self.care.rested(now);
        }
        let context = if running && owner.is_some() {
            RenderContext::new(
                Behavior::WatchCommand,
                BodyLanguage::from_state(self.life.state()),
                false,
            )
        } else {
            owner
                .map(|id| self.base_context_at(id, now))
                .unwrap_or_else(|| PreviewPose::Calm.context())
        };
        self.reply = self.voice.respond(
            intent,
            RenderContext {
                body_language: BodyLanguage::from_state(self.life.state()),
                ..context
            },
        );
    }

    pub fn reply(&self) -> &'static str {
        self.reply
    }

    pub fn temperament_description(&self) -> &'static str {
        self.voice.temperament().description()
    }

    pub fn set_care_reminders(&mut self, enabled: bool) {
        self.care.set_enabled(enabled && self.enabled);
    }

    pub fn snooze_care(&mut self) {
        self.care.snooze(self.born.elapsed());
        self.reply = "Of course. I'll keep the break reminders quiet for twenty minutes.";
    }

    /// Existing foreground heartbeats only. The adapter admits a text toast
    /// after real human activity and a quiet, visible native interaction slot.
    pub fn care_prompt(&mut self, owner: Option<usize>, can_offer: bool) -> Option<&'static str> {
        let now = self.born.elapsed();
        self.care_prompt_at(owner, can_offer, now)
    }

    fn care_prompt_at(
        &mut self,
        owner: Option<usize>,
        can_offer: bool,
        now: Duration,
    ) -> Option<&'static str> {
        let owner = owner.filter(|id| self.enabled && self.is_local(*id));
        let engaged = owner.is_some()
            && self
                .last_human_input
                .is_some_and(|last| now.saturating_sub(last) <= Duration::from_secs(5 * 60));
        let quiet = owner.is_some_and(|id| {
            matches!(
                self.base_context_at(id, now).behavior,
                Behavior::Idle
                    | Behavior::Sleep
                    | Behavior::Explore
                    | Behavior::Approach
                    | Behavior::GlanceAside
            )
        });
        self.care
            .observe(
                now,
                engaged,
                can_offer && quiet && !self.live_pointer_inside,
            )
            .map(|prompt| {
                self.reply = prompt.text();
                prompt.text()
            })
    }

    pub fn sync_greeting_geometry(&mut self, glyph: bool, presentable: bool) {
        if !glyph {
            self.live_pointer_inside = false;
            if let Some(owner) = self.live_attention.owner().copied() {
                self.live_attention.cancel_hover(&owner);
            }
        }
        if !presentable {
            self.pause_clock();
        }
    }

    pub fn cancel_live_greeting(&mut self) {
        if let Some(owner) = self.live_attention.owner().copied() {
            self.live_attention.cancel(&owner);
        }
    }

    fn live_input(&self, id: usize, facts: AttentionFacts) -> LiveAttentionInput {
        let base_context = self.base_context_at(id, self.born.elapsed());
        LiveAttentionInput {
            base_context,
            hard_eligible: facts.eligible && self.enabled && self.is_local(id),
            activity: facts.activity,
            hover_available: facts.hover_available,
            pointer_inside: self.live_pointer_inside,
        }
    }

    pub fn live_greeting_offer(&self, id: usize) -> Option<LiveAttentionTicket> {
        (self.live_attention.owner() == Some(&id))
            .then(|| self.live_attention.entry_offer())
            .flatten()
    }

    pub fn live_greeting_exit_reference(&self, id: usize) -> Option<LiveAttentionTicket> {
        (self.live_attention.owner() == Some(&id))
            .then(|| {
                self.live_attention
                    .entry_ticket()
                    .or(self.live_attention.entry_offer())
            })
            .flatten()
    }

    pub fn enter_live_greeting(
        &mut self,
        id: usize,
        ticket: LiveAttentionTicket,
        facts: AttentionFacts,
    ) {
        // A stale widget cannot rewrite even native pointer facts.
        if self.live_attention.owner() != Some(&id)
            || self.live_attention.entry_offer() != Some(ticket)
        {
            return;
        }
        self.live_pointer_inside = true;
        let input = self.live_input(id, facts);
        let _ = self.live_attention.advance(
            self.born.elapsed(),
            &id,
            input,
            LiveAttentionEvent::PointerEntered { ticket },
        );
    }

    pub fn exit_live_greeting(
        &mut self,
        id: usize,
        ticket: LiveAttentionTicket,
        facts: AttentionFacts,
    ) {
        if self.live_attention.owner() != Some(&id)
            || self.live_attention.entry_ticket() != Some(ticket)
        {
            return;
        }
        self.live_pointer_inside = false;
        let input = self.live_input(id, facts);
        let _ = self.live_attention.advance(
            self.born.elapsed(),
            &id,
            input,
            LiveAttentionEvent::PointerLeft { ticket },
        );
    }

    pub fn leave_live_greeting(&mut self, facts: AttentionFacts) {
        self.live_pointer_inside = false;
        if let Some(owner) = self.live_attention.owner().copied() {
            if let Some(ticket) = self.live_attention.entry_ticket() {
                let input = self.live_input(owner, facts);
                let _ = self.live_attention.advance(
                    self.born.elapsed(),
                    &owner,
                    input,
                    LiveAttentionEvent::PointerLeft { ticket },
                );
            } else {
                self.live_attention.cancel_hover(&owner);
            }
        }
    }

    pub fn poll_live_greeting(&mut self, owner: Option<usize>, facts: AttentionFacts) {
        self.sync_live_attention_owner(owner);
        if let Some(owner) = owner {
            let input = self.live_input(owner, facts);
            let _ = self.live_attention.advance(
                self.born.elapsed(),
                &owner,
                input,
                LiveAttentionEvent::Reconcile,
            );
        }
    }

    pub fn greet_live(&mut self, id: usize, facts: AttentionFacts) -> LiveAttentionOutcome {
        let input = self.live_input(id, facts);
        self.live_attention
            .advance(
                self.born.elapsed(),
                &id,
                input,
                LiveAttentionEvent::Explicit,
            )
            .map(|snapshot| snapshot.outcome)
            .unwrap_or(LiveAttentionOutcome::Rejected(
                LiveAttentionRejection::Unavailable,
            ))
    }

    pub fn live_greeting_snapshot(&self, facts: AttentionFacts) -> Option<LiveAttentionSnapshot> {
        let owner = self.live_attention.owner().copied()?;
        self.live_attention
            .snapshot(self.born.elapsed(), &owner, self.live_input(owner, facts))
            .ok()
    }

    // The reference shell has one graphical body; this compact cue is legacy test coverage.
    #[cfg(test)]
    pub fn live_greeting_candidate_visible(&self, id: usize, facts: AttentionFacts) -> bool {
        self.live_attention.owner() == Some(&id)
            && self
                .live_greeting_snapshot(facts)
                .is_some_and(|snapshot| snapshot.candidate_visible)
    }

    pub fn explicit_greeting_active(&self, facts: AttentionFacts) -> bool {
        self.live_greeting_snapshot(facts)
            .and_then(|snapshot| snapshot.active)
            .is_some_and(|active| active.source == LiveAttentionSource::Explicit)
    }

    pub fn live_greeting_deadline(&self, facts: AttentionFacts) -> Option<Instant> {
        self.live_greeting_snapshot(facts)?
            .next_deadline()
            .and_then(|deadline| self.born.checked_add(deadline))
    }

    fn frame_at(motion: Option<Motion>, now: Duration) -> u64 {
        if motion == Some(Motion::Full) {
            now.as_millis() as u64 / 100
        } else {
            0
        }
    }

    /// Paint clock only; native layout never changes the shared life state.
    pub fn paint_frame(&self, motion: Option<Motion>) -> u64 {
        Self::frame_at(motion, self.born.elapsed())
    }

    pub fn presentation_with_attention(
        &self,
        id: usize,
        running: bool,
        settled: bool,
        motion: Option<Motion>,
        facts: AttentionFacts,
    ) -> LivePresentation {
        self.presentation_at_with_attention(
            id,
            running,
            settled,
            motion,
            self.born.elapsed(),
            (facts, self.live_pointer_inside),
        )
    }

    #[cfg(test)]
    fn presentation(
        &self,
        id: usize,
        running: bool,
        settled: bool,
        motion: Option<Motion>,
    ) -> LivePresentation {
        self.presentation_at(id, running, settled, motion, self.born.elapsed())
    }

    #[cfg(test)]
    fn presentation_at(
        &self,
        id: usize,
        running: bool,
        settled: bool,
        motion: Option<Motion>,
        now: Duration,
    ) -> LivePresentation {
        self.presentation_at_with_attention(
            id,
            running,
            settled,
            motion,
            now,
            (
                AttentionFacts::known_idle_for_test(true, true),
                self.live_attention.entry_ticket().is_some(),
            ),
        )
    }

    fn presentation_at_with_attention(
        &self,
        id: usize,
        running: bool,
        settled: bool,
        motion: Option<Motion>,
        now: Duration,
        native: (AttentionFacts, bool),
    ) -> LivePresentation {
        let (facts, pointer_inside) = native;
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
            self.base_context_at(id, now)
        };
        let context = RenderContext {
            body_language: BodyLanguage::from_state(self.life.state()),
            ..context
        };
        let context = if running || motion == Some(Motion::Static) {
            context
        } else {
            self.live_attention
                .snapshot(
                    now,
                    &id,
                    LiveAttentionInput {
                        base_context: context,
                        hard_eligible: facts.eligible && self.enabled && self.is_local(id),
                        activity: facts.activity,
                        hover_available: facts.hover_available,
                        pointer_inside,
                    },
                )
                .map_or(context, |snapshot| snapshot.presentation)
        };
        let frame = Self::frame_at(motion, now);
        // Running work and an explicit greeting own the visible pose. A stale
        // completion must never relabel either, nor flash after a background
        // hold has expired before this pane receives its next tick.
        let completion = (!running)
            .then(|| self.sessions.get(&id))
            .flatten()
            .filter(|life| {
                life.reaction_until
                    .is_some_and(|deadline| now < deadline.saturating_duration_since(self.born))
            })
            .and_then(|life| life.completion.as_ref())
            .filter(|feedback| feedback.behavior == context.behavior);
        LivePresentation {
            glyph: sticky_glyph_with_context(context, frame).into_owned(),
            sprite: sprite_frame_with_context(context, frame).into_owned(),
            walking_sprite: sprite_frame_with_context(
                RenderContext {
                    walking: true,
                    ..context
                },
                frame,
            )
            .into_owned(),
            explanation: live_explanation(context),
            life: jterm_core::organism_daily::life_description(self.life.state()),
            wander: (motion == Some(Motion::Full)
                && matches!(context.behavior, Behavior::Idle | Behavior::Explore))
            .then_some(if context.behavior == Behavior::Explore {
                120
            } else if context.body_language.drowsy {
                0
            } else if context.body_language.listless {
                120
            } else {
                40
            }),
            bottom: matches!(
                context.behavior,
                Behavior::Idle
                    | Behavior::Sleep
                    | Behavior::Explore
                    | Behavior::Approach
                    | Behavior::GuardRecovery
                    | Behavior::GuardCautious
            ),
            status: completion.map_or_else(
                || Cow::Borrowed(jterm_core::organism_daily::behavior_label(context.behavior)),
                |feedback| Cow::Owned(feedback.status.clone()),
            ),
            tone: Some(completion.map_or_else(
                || match context.behavior {
                    Behavior::InspectError
                    | Behavior::SitNearError
                    | Behavior::GuardFailure
                    | Behavior::GuardStuck => Tone::Error,
                    Behavior::Celebrate
                    | Behavior::CelebrateBig
                    | Behavior::RestAfterPush
                    | Behavior::GuardRecovery => Tone::Success,
                    Behavior::UnknownOutcome | Behavior::GuardCautious => Tone::Warning,
                    Behavior::WatchCommand
                    | Behavior::WatchSettled
                    | Behavior::WatchAgent
                    | Behavior::Approach => Tone::Active,
                    _ => Tone::Quiet,
                },
                |feedback| feedback.tone,
            )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::organism::WatchRhythm;

    #[test]
    fn companion_life_and_voice_survive_pane_close_and_visibility_toggle() {
        let mut organism = completion_fixture(&[1, 2]);
        organism.owner_changed(None, Some(1));
        organism.completed(1, "cargo test", Some(101), Some(100), None);
        let state = values(organism.life.state());
        let temperament = organism.voice.temperament();
        organism.talk(CompanionIntent::Story, Some(1), false);
        let story = organism.reply();
        let next_voice = organism.voice.clone();
        organism.owner_changed(Some(1), Some(2));
        organism.forget_session(1);
        assert_eq!(values(organism.life.state()), state);
        assert_eq!(organism.voice.temperament(), temperament);
        assert_eq!(organism.reply(), story);
        organism.set_enabled(false);
        organism.set_enabled(true);
        assert_eq!(values(organism.life.state()), state);
        assert_eq!(organism.reply(), story);
        assert!(
            organism.sessions.is_empty(),
            "command admission must not replay on reenable"
        );
        organism.prime_session(3, false);
        organism.set_remote(3, false);
        organism.talk(CompanionIntent::Story, Some(3), false);
        let mut expected = next_voice;
        assert_eq!(
            organism.reply(),
            expected.respond(CompanionIntent::Story, PreviewPose::Calm.context())
        );
    }

    #[test]
    fn companion_mind_follows_active_pane_and_background_output_cannot_restart_it() {
        let mut organism = completion_fixture(&[1, 2]);
        organism.owner_changed(None, Some(1));
        let mut state = organism.life.state();
        state.energy = 0.0;
        organism.life.replace_state(state);
        organism.tick_at(Some(1), false, false, Duration::ZERO);
        assert_eq!(organism.ambient.current().display(), Behavior::Sleep);
        let mind = format!("{:?}", organism.ambient);
        organism.note_output(2);
        organism.completed(2, "cargo test", Some(0), Some(1), None);
        assert_eq!(format!("{:?}", organism.ambient), mind);
        organism.owner_changed(Some(1), Some(2));
        organism.forget_session(1);
        assert_eq!(format!("{:?}", organism.ambient), mind);
    }

    #[test]
    fn companion_talk_preserves_command_outcomes_and_uses_running_context() {
        let mut organism = completion_fixture(&[1]);
        organism.completed(1, "cargo test", Some(101), Some(100), None);
        let state = values(organism.life.state());
        let context = organism.sessions[&1].context;
        organism.talk(CompanionIntent::Hello, Some(1), true);
        assert!(organism.reply().contains("running command"));
        organism.talk(CompanionIntent::Company, Some(1), false);
        assert!(organism.reply().contains("failure"));
        assert_eq!(values(organism.life.state()), state);
        assert_eq!(organism.sessions[&1].context, context);
    }

    #[test]
    fn companion_care_uses_one_window_clock_across_many_panes() {
        let mut organism = completion_fixture(&[1, 2]);
        assert!(!organism.care.enabled());
        organism.set_care_reminders(true);
        let mut prompts = Vec::new();
        for second in (0..=45 * 60).step_by(5) {
            let now = Duration::from_secs(second);
            let owner = 1 + (second / 5) as usize % 2;
            organism.last_human_input = Some(now);
            if let Some(prompt) = organism.care_prompt_at(Some(owner), true, now) {
                prompts.push((second, prompt));
            }
        }
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].0, 45 * 60);
        assert_eq!(organism.reply(), prompts[0].1);
        assert_eq!(
            organism.care_prompt_at(Some(1), true, Duration::from_secs(45 * 60)),
            None
        );
    }

    #[test]
    fn companion_rest_clears_due_care_instead_of_only_changing_the_reply() {
        let mut organism = completion_fixture(&[1]);
        organism.set_care_reminders(true);
        for second in (0..=45 * 60).step_by(5) {
            let now = Duration::from_secs(second);
            organism.last_human_input = Some(now);
            assert_eq!(organism.care_prompt_at(Some(1), false, now), None);
        }
        organism.talk(CompanionIntent::Rest, Some(1), false);
        // Snooze expires, but a rest discarded the old debt. Keep samples
        // frequent so a long scheduling gap cannot itself reset the clock.
        for second in (45 * 60 + 5..=65 * 60 + 5).step_by(5) {
            let now = Duration::from_secs(second);
            organism.last_human_input = Some(now);
            assert_eq!(organism.care_prompt_at(Some(1), true, now), None);
        }
    }

    #[test]
    fn companion_care_does_not_infer_human_work_from_output_or_background_panes() {
        let mut organism = completion_fixture(&[1, 2]);
        organism.set_care_reminders(true);
        for second in (0..=60 * 60).step_by(5) {
            let now = Duration::from_secs(second);
            organism.life.note_output(now);
            assert_eq!(organism.care_prompt_at(Some(1), true, now), None);
        }
        for second in (60 * 60..=120 * 60).step_by(5) {
            let now = Duration::from_secs(second);
            organism.last_human_input = Some(now);
            assert_eq!(organism.care_prompt_at(None, true, now), None);
        }
        assert_eq!(
            organism.care_prompt_at(Some(2), true, Duration::from_secs(120 * 60)),
            None
        );
    }

    #[test]
    fn agent_lifecycle_edges_are_deduplicated_and_disabled_edges_are_consumed() {
        use jterm_core::agent::AgentState;
        use jterm_core::organism::AgentPulse;
        let mut tracker = AgentPulseTracker::default();
        assert_eq!(tracker.observe(None, false, true), None);
        assert_eq!(tracker.observe(Some(AgentState::Ready), false, true), None);
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), true, true),
            Some(AgentPulse::Working)
        );
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), true, true),
            None
        );
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), false, true),
            Some(AgentPulse::Finished)
        );
        assert_eq!(tracker.observe(Some(AgentState::Ready), false, true), None);
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), false, true),
            Some(AgentPulse::Finished)
        );
        assert_eq!(
            tracker.observe(Some(AgentState::Cancelled), false, true),
            Some(AgentPulse::Gone)
        );
        assert_eq!(tracker.observe(None, false, true), None);
        assert_eq!(tracker.observe(None, false, true), None);
        assert_eq!(
            tracker.observe(Some(AgentState::AwaitingModel), false, false),
            None
        );
        assert_eq!(
            tracker.observe(Some(AgentState::AwaitingModel), false, true),
            None
        );
        assert_eq!(tracker.observe(None, false, true), Some(AgentPulse::Gone));
        assert_eq!(tracker.observe(None, false, true), None);
        let mut first = AgentPulseTracker::default();
        assert_eq!(
            first.observe(Some(AgentState::Cancelled), false, true),
            Some(AgentPulse::Gone)
        );
    }

    // Native-fact fixtures delegate to the real shared controller. They contain
    // no dwell/hold/cooldown policy; host integration trajectories stay here.
    trait AttentionFixture {
        fn test_enter(&mut self, id: usize, now: Duration, context: RenderContext, eligible: bool);
        fn test_advance(
            &mut self,
            owner: Option<usize>,
            now: Duration,
            context: RenderContext,
            eligible: bool,
        );
        fn test_explicit(
            &mut self,
            id: usize,
            now: Duration,
            context: RenderContext,
        ) -> LiveAttentionOutcome;
        fn test_apply(&self, id: usize, now: Duration, context: RenderContext) -> RenderContext;
        fn test_deadline(&self, now: Duration) -> Option<Duration>;
        fn test_leave(&mut self);
    }
    fn fixture_input(context: RenderContext, eligible: bool, inside: bool) -> LiveAttentionInput {
        LiveAttentionInput {
            base_context: context,
            hard_eligible: eligible,
            activity: LiveActivityKnowledge::KnownIdle,
            hover_available: true,
            pointer_inside: inside,
        }
    }
    impl AttentionFixture for LiveAttention<usize> {
        fn test_enter(&mut self, id: usize, now: Duration, context: RenderContext, eligible: bool) {
            self.set_owner(Some(id));
            if let Some(ticket) = self.entry_offer() {
                let _ = self.advance(
                    now,
                    &id,
                    fixture_input(context, eligible, true),
                    LiveAttentionEvent::PointerEntered { ticket },
                );
            }
        }
        fn test_advance(
            &mut self,
            owner: Option<usize>,
            now: Duration,
            context: RenderContext,
            eligible: bool,
        ) {
            self.set_owner(owner);
            if let Some(id) = owner {
                let _ = self.advance(
                    now,
                    &id,
                    fixture_input(context, eligible, self.entry_ticket().is_some()),
                    LiveAttentionEvent::Reconcile,
                );
            }
        }
        fn test_explicit(
            &mut self,
            id: usize,
            now: Duration,
            context: RenderContext,
        ) -> LiveAttentionOutcome {
            self.set_owner(Some(id));
            self.advance(
                now,
                &id,
                fixture_input(context, true, self.entry_ticket().is_some()),
                LiveAttentionEvent::Explicit,
            )
            .unwrap()
            .outcome
        }
        fn test_apply(&self, id: usize, now: Duration, context: RenderContext) -> RenderContext {
            self.snapshot(
                now,
                &id,
                fixture_input(context, true, self.entry_ticket().is_some()),
            )
            .map_or(context, |snapshot| snapshot.presentation)
        }
        fn test_deadline(&self, now: Duration) -> Option<Duration> {
            let owner = self.owner()?;
            self.snapshot(
                now,
                owner,
                fixture_input(
                    PreviewPose::Calm.context(),
                    true,
                    self.entry_ticket().is_some(),
                ),
            )
            .ok()?
            .next_deadline()
        }
        fn test_leave(&mut self) {
            if let (Some(owner), Some(ticket)) = (self.owner().copied(), self.entry_ticket()) {
                let _ = self.advance(
                    Duration::ZERO,
                    &owner,
                    fixture_input(PreviewPose::Calm.context(), true, false),
                    LiveAttentionEvent::PointerLeft { ticket },
                );
            }
        }
    }

    #[test]
    fn every_shared_pose_and_transition_fits_the_three_line_companion_card() {
        use jterm_core::organism::{
            Behavior, BodyLanguage, RenderContext, VisualGrowthStage, VisualTransition, WatchRhythm,
        };
        let behaviors = [
            Behavior::Idle,
            Behavior::WatchCommand,
            Behavior::InspectError,
            Behavior::SitNearError,
            Behavior::Celebrate,
            Behavior::CelebrateBig,
            Behavior::RestAfterPush,
            Behavior::UnknownOutcome,
            Behavior::GlanceAside,
            Behavior::Sleep,
            Behavior::Explore,
            Behavior::Approach,
            Behavior::WatchAgent,
            Behavior::WatchSettled,
            Behavior::GuardFailure,
            Behavior::GuardStuck,
            Behavior::GuardRecovery,
            Behavior::GuardCautious,
        ];
        let check = |context, frame| {
            let sprite = sprite_frame_with_context(context, frame);
            assert_eq!(sprite.lines().count(), 3, "{context:?} frame={frame}");
            assert!(
                sprite.lines().all(|line| line.chars().count() <= 12),
                "{context:?} frame={frame}: {sprite}"
            );
        };
        for behavior in behaviors {
            for bits in 0..8 {
                let language = BodyLanguage {
                    drowsy: bits & 1 != 0,
                    tense: bits & 2 != 0,
                    listless: bits & 4 != 0,
                };
                for walking in [false, true] {
                    for rhythm in [
                        WatchRhythm::Steady,
                        WatchRhythm::Busy,
                        WatchRhythm::Waiting,
                        WatchRhythm::Resumed,
                    ] {
                        for stage in [
                            VisualGrowthStage::Juvenile,
                            VisualGrowthStage::Adult,
                            VisualGrowthStage::Seasoned,
                        ] {
                            let context = RenderContext::new(behavior, language, walking)
                                .with_watch_rhythm(rhythm)
                                .with_growth_stage(stage);
                            for frame in 0..130 {
                                check(context, frame);
                            }
                        }
                    }
                }
            }
            for target in behaviors {
                if let Some(transition) = VisualTransition::between(behavior, target) {
                    for stage in [
                        VisualGrowthStage::Juvenile,
                        VisualGrowthStage::Adult,
                        VisualGrowthStage::Seasoned,
                    ] {
                        let context = RenderContext::new(target, BodyLanguage::default(), false)
                            .with_growth_stage(stage)
                            .with_transition(Some(transition));
                        for frame in 0..transition.frame_count() {
                            check(context, frame);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn passive_presence_is_nonempty_static_and_does_not_claim_shell_idle() {
        let first = LivePresentation::passive_presence();
        let second = LivePresentation::passive_presence();
        assert!(!first.glyph.is_empty());
        assert!(!first.sprite.is_empty());
        assert_eq!(first.glyph, second.glyph);
        assert_eq!(first.sprite, second.sprite);
        assert!(first.glyph.chars().count() <= 9);
        assert!(first.explanation.contains("Live reactions are paused"));
        assert!(!first.explanation.contains("idle"));
        assert!(!first.explanation.contains("between commands"));
    }

    #[test]
    fn passive_presence_is_pure_rendering_without_owner_attention_or_clock() {
        let source = include_str!("organism.rs");
        let passive = source
            .split_once("impl LivePresentation {")
            .unwrap()
            .1
            .split_once("/// Describe only the output rhythm")
            .unwrap()
            .0;
        assert!(passive.contains("sticky_glyph_with_context(context, 0)"));
        assert!(passive.contains("sprite_frame_with_context(context, 0)"));
        for forbidden in [
            "LiveActivityKnowledge",
            "live_attention",
            "Instant::",
            "Duration::",
            "&mut",
            "self.",
            "command_started",
            "deadline",
            "advance(",
        ] {
            assert!(!passive.contains(forbidden));
        }
    }

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
                .live_attention
                .test_explicit(1, Duration::ZERO, PreviewPose::Calm.context());
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
                organism.live_attention.test_explicit(
                    1,
                    Duration::from_secs(1),
                    PreviewPose::Calm.context()
                ),
                LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown)
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
    fn stale_paste_attempt_preserves_current_owner_attention() {
        use jterm_core::organism::WatchRhythm;
        // Target 1 either still exists in the background or has been deleted.
        for target_exists in [true, false] {
            for (target, active, eligible, retreats) in [
                (1, Some(2), true, false),
                (1, None, true, false),
                (2, Some(2), false, false),
                (2, Some(2), true, true),
            ] {
                let mut organism = Organism::default();
                organism.set_enabled(true);
                if target_exists {
                    organism.prime_session(1, false);
                }
                organism.prime_session(2, false);
                let context = PreviewPose::Calm.context();
                organism
                    .live_attention
                    .test_explicit(2, Duration::ZERO, context);
                organism.watch.observe(Some(2), true, Duration::ZERO);
                for millis in [0, 100, 200, 300] {
                    organism
                        .watch
                        .output(2, Duration::from_millis(millis), true);
                }
                let now = Duration::from_millis(700);
                let deadline = Instant::now() + Duration::from_secs(5);
                organism.retreat_until = deadline;
                let greeting = organism.live_attention.test_deadline(now);
                assert!(greeting.is_some());
                assert_eq!(organism.watch.rhythm(2, now), WatchRhythm::Busy);
                let life = values(organism.life.state());
                organism.retreat_for_input_attempt(target, active, eligible);
                assert_eq!(values(organism.life.state()), life);
                if retreats {
                    // Active attempts retreat even if the queue subsequently refuses.
                    assert_ne!(organism.retreat_until, deadline);
                    assert!(organism.is_retreating());
                    assert_eq!(organism.live_attention.test_deadline(now), None);
                    assert_eq!(organism.watch.rhythm(2, now), WatchRhythm::Steady);
                    organism.accepted_input();
                    assert!(organism.is_retreating());
                } else {
                    assert_eq!(organism.retreat_until, deadline);
                    assert_eq!(organism.live_attention.test_deadline(now), greeting);
                    assert_eq!(organism.watch.rhythm(2, now), WatchRhythm::Busy);
                }
            }
        }
    }

    #[test]
    fn app_action_restores_only_previous_retreat_deadline_not_canceled_state() {
        let mut organism = Organism::default();
        let previous = Instant::now() + Duration::from_millis(300);
        organism.retreat_until = previous;
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, PreviewPose::Calm.context(), true);
        let snapshot = organism.retreat_deadline();
        organism.retreat();
        organism.restore_retreat_after_app_action(snapshot);
        assert_eq!(organism.retreat_until, previous);
        assert_eq!(organism.live_attention.test_deadline(Duration::ZERO), None);
        organism.retreat();
        organism.restore_retreat_after_app_action(None);
        assert!(!organism.is_retreating());
        assert_eq!(organism.live_attention.test_deadline(Duration::ZERO), None);
    }

    #[test]
    fn modifier_then_greeting_key_preserves_only_prior_real_input_retreat() {
        for has_prior_input in [false, true] {
            let mut organism = Organism::default();
            organism.set_enabled(true);
            organism.prime_session(1, false);
            organism.set_remote(1, false);
            organism.sync_live_attention_owner(Some(1));
            if has_prior_input {
                organism.retreat();
            }
            let original = organism.retreat_deadline();
            organism.modifier_pressed();
            assert_eq!(organism.retreat_deadline(), original);
            // The actual bound character follows normal Key cancellation, then
            // its recognized application action restores only the old deadline.
            let before_character = organism.retreat_deadline();
            organism.retreat();
            organism.restore_retreat_after_app_action(before_character);
            assert_eq!(organism.retreat_deadline(), original);
            let eligible = !organism.is_retreating();
            assert_eq!(eligible, !has_prior_input);
            assert_eq!(
                organism.greet_live(1, AttentionFacts::known_idle_for_test(eligible, true)),
                if has_prior_input {
                    LiveAttentionOutcome::Rejected(LiveAttentionRejection::Unavailable)
                } else {
                    LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit)
                }
            );
        }
    }

    #[test]
    fn repeated_greeting_key_cancels_hold_but_keeps_cooldown_without_replay() {
        let mut organism = Organism::default();
        let context = PreviewPose::Calm.context();
        assert_eq!(
            organism
                .live_attention
                .test_explicit(1, Duration::ZERO, context),
            LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit)
        );
        // Native key entry cancels before the binding is classified. Restoring
        // its own retreat does not recreate the accepted hold or reset cooldown.
        organism.retreat();
        organism.restore_retreat_after_app_action(None);
        assert_eq!(
            organism
                .live_attention
                .test_explicit(1, Duration::from_millis(100), context),
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown)
        );
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_millis(100)),
            None
        );
        organism
            .live_attention
            .test_advance(Some(1), Duration::from_secs(9), context, true);
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_secs(9)),
            None
        );
    }

    #[test]
    fn greeting_geometry_preserves_explicit_body_hold_but_retires_hidden_or_hover() {
        let mut organism = Organism::default();
        let context = PreviewPose::Calm.context();
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, context, true);
        organism.sync_greeting_geometry(false, true);
        assert_eq!(organism.live_attention.test_deadline(Duration::ZERO), None);
        organism.sync_greeting_geometry(true, true);
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, context, true);
        assert_eq!(organism.live_attention.test_deadline(Duration::ZERO), None);
        assert_eq!(
            organism
                .live_attention
                .test_explicit(1, Duration::ZERO, context),
            LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit)
        );
        for (glyph, presentable) in [(true, true), (false, true), (false, true), (true, true)] {
            organism.sync_greeting_geometry(glyph, presentable);
            assert_eq!(
                organism
                    .live_attention
                    .test_apply(1, Duration::from_millis(500), context)
                    .behavior,
                Behavior::Approach
            );
        }
        organism.sync_greeting_geometry(false, false);
        organism.sync_greeting_geometry(true, true);
        assert_eq!(
            organism
                .live_attention
                .test_apply(1, Duration::from_millis(500), context),
            context
        );
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_millis(500)),
            None
        );
        assert_eq!(
            organism
                .live_attention
                .test_explicit(1, Duration::from_secs(1), context),
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown)
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
        organism.sync_live_attention_owner(Some(1));
        assert_eq!(
            organism.greet_live(1, AttentionFacts::known_idle_for_test(true, true)),
            LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit)
        );
        assert_eq!(values(organism.life.state()), before);
        assert_eq!(organism.preview_deadlines.ready_at, preview_ready);
        assert_eq!(
            organism.greet_live(1, AttentionFacts::known_idle_for_test(true, true)),
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown)
        );
        assert_eq!(
            organism.greet_live(99, AttentionFacts::known_idle_for_test(true, true)),
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::Unavailable)
        );
    }

    #[test]
    fn motion_changes_preserve_live_deadlines_except_static_and_keep_watch_policy() {
        use jterm_core::organism::WatchRhythm;
        for previous in [None, Some(Motion::Full), Some(Motion::Calm)] {
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
                organism
                    .live_attention
                    .test_enter(1, Duration::ZERO, context, true);
                organism.live_attention.test_advance(
                    Some(1),
                    Duration::from_millis(600),
                    context,
                    true,
                );
                organism.say_hello_at(Duration::ZERO);
                let before = values(organism.life.state());
                let now = Duration::from_millis(700);
                let deadline = organism.live_attention.test_deadline(now);
                organism.motion_changed(previous, next);
                assert_eq!(
                    organism.watch.rhythm(1, now),
                    if previous == next {
                        WatchRhythm::Busy
                    } else {
                        WatchRhythm::Steady
                    }
                );
                if next == Some(Motion::Static) {
                    assert_eq!(organism.live_attention.test_apply(1, now, context), context);
                    assert_eq!(organism.live_attention.test_deadline(now), None);
                    assert_eq!(
                        organism.live_attention.test_explicit(2, now, context),
                        LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown)
                    );
                } else {
                    assert_eq!(
                        organism.live_attention.test_apply(1, now, context).behavior,
                        Behavior::Approach
                    );
                    assert_eq!(organism.live_attention.test_deadline(now), deadline);
                }
                assert_eq!(organism.preview_deadlines.ready_at, Duration::from_secs(8));
                assert_eq!(values(organism.life.state()), before);
            }
        }
    }

    #[test]
    fn motion_change_preserves_pending_dwell_until_static_cancels_without_rearm() {
        let mut organism = Organism::default();
        let context = PreviewPose::Calm.context();
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, context, true);
        for (previous, next) in [
            (Some(Motion::Full), Some(Motion::Calm)),
            (Some(Motion::Calm), None),
        ] {
            organism.motion_changed(previous, next);
            assert_eq!(
                organism
                    .live_attention
                    .test_deadline(Duration::from_millis(300)),
                Some(Duration::from_millis(600))
            );
        }
        organism.motion_changed(None, Some(Motion::Static));
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_millis(300)),
            None
        );
        organism.motion_changed(Some(Motion::Static), Some(Motion::Full));
        organism
            .live_attention
            .test_enter(1, Duration::from_millis(300), context, true);
        organism
            .live_attention
            .test_advance(Some(1), Duration::from_secs(1), context, true);
        assert_eq!(
            organism
                .live_attention
                .test_apply(1, Duration::from_secs(1), context),
            context
        );
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn native_entry_tickets_reject_stale_edges_before_changing_pointer_facts() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.sync_live_attention_owner(Some(1));
        let old = organism.live_greeting_offer(1).unwrap();
        organism.cancel_live_greeting();
        organism.enter_live_greeting(1, old, AttentionFacts::known_idle_for_test(true, true));
        assert!(!organism.live_pointer_inside);
        let ticket = organism.live_greeting_offer(1).unwrap();
        organism.enter_live_greeting(1, ticket, AttentionFacts::known_idle_for_test(true, true));
        assert!(organism.live_pointer_inside);
        assert_eq!(organism.live_greeting_exit_reference(1), Some(ticket));
        organism.exit_live_greeting(1, old, AttentionFacts::known_idle_for_test(true, true));
        assert!(organism.live_pointer_inside);
        // The same captured offer exits correctly without an intervening view.
        organism.exit_live_greeting(1, ticket, AttentionFacts::known_idle_for_test(true, true));
        assert!(!organism.live_pointer_inside);
        assert_eq!(organism.live_attention.entry_ticket(), None);
    }

    #[test]
    fn actual_input_cancels_due_candidate_before_tick_without_spending_cooldown() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.sync_live_attention_owner(Some(1));
        let ticket = organism.live_greeting_offer(1).unwrap();
        organism.enter_live_greeting(1, ticket, AttentionFacts::known_idle_for_test(true, true));
        organism.accepted_input();
        organism.live_attention.test_advance(
            Some(1),
            Duration::from_secs(1),
            PreviewPose::Calm.context(),
            true,
        );
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_secs(1)),
            None
        );
        assert_eq!(
            organism.live_attention.test_explicit(
                1,
                Duration::from_secs(1),
                PreviewPose::Calm.context()
            ),
            LiveAttentionOutcome::Accepted(LiveAttentionSource::Explicit)
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
            organism
                .live_attention
                .test_enter(10, Duration::ZERO, context, true);
            organism.live_attention.test_advance(
                Some(10),
                Duration::from_millis(600),
                context,
                true,
            );
            let now = Duration::from_millis(700);
            assert_eq!(organism.watch.rhythm(10, now), WatchRhythm::Busy);
            assert_eq!(
                organism
                    .live_attention
                    .test_apply(10, now, context)
                    .behavior,
                PreviewPose::Greeting.context().behavior
            );
            organism.forget_session(if next == Some(10) { 20 } else { 10 });
            organism.owner_changed(Some(10), next);
            if next == Some(10) {
                assert_eq!(organism.watch.rhythm(10, now), WatchRhythm::Busy);
                assert_eq!(
                    organism
                        .live_attention
                        .test_apply(10, now, context)
                        .behavior,
                    PreviewPose::Greeting.context().behavior
                );
            } else {
                assert_eq!(organism.watch.rhythm(10, now), WatchRhythm::Steady);
                assert_eq!(
                    organism.live_attention.test_apply(10, now, context),
                    context
                );
                assert_eq!(organism.live_attention.test_deadline(now), None);
                organism.live_attention.test_leave();
                organism.live_attention.test_enter(20, now, context, true);
                organism.live_attention.test_advance(
                    Some(20),
                    Duration::from_millis(1300),
                    context,
                    true,
                );
                assert_eq!(
                    organism
                        .live_attention
                        .test_apply(20, Duration::from_millis(1300), context),
                    context
                );
            }
        }
    }

    #[test]
    fn settings_hint_names_real_companion_control_and_safe_visibility() {
        for expanded in [false, true] {
            assert_eq!(
                crate::organism_settings_hint(false, expanded),
                "Enable Companion in Settings to allow the live companion."
            );
            let live = crate::organism_settings_hint(true, expanded);
            assert!(live.contains("Show companion status bubble"));
            assert!(live.contains("safe blank space"));
            assert!(!live.contains("Traditional"));
            assert!(!live.contains("arrow"));
        }
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
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, context, true);
        organism
            .live_attention
            .test_advance(Some(1), Duration::from_millis(600), context, true);
        let live_deadline = organism
            .live_attention
            .test_deadline(Duration::from_secs(1));
        assert_eq!(live_deadline, Some(Duration::from_millis(2600)));
        let before = values(organism.life.state());
        organism.start_demo_at(Duration::from_secs(1));
        organism.say_hello_at(Duration::from_secs(2));
        assert_eq!(organism.preview_deadlines.ready_at, Duration::from_secs(8));
        assert_eq!(organism.preview_deadlines.greeting_until, Duration::ZERO);
        assert_eq!(
            organism
                .live_attention
                .test_deadline(Duration::from_secs(1)),
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
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, context, true);
        organism
            .live_attention
            .test_advance(Some(1), Duration::from_millis(600), context, true);
        let before = values(organism.life.state());
        let now = Duration::from_millis(700);
        let final_context = organism.live_attention.test_apply(1, now, context);
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
        assert!(MotionChoice::Automatic.description().contains("desktop"));
        assert!(MotionChoice::Automatic
            .description()
            .contains("Calm when unavailable"));
        assert_ne!(
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
        for behavior in [
            Behavior::WatchCommand,
            Behavior::WatchSettled,
            Behavior::WatchAgent,
        ] {
            let context = RenderContext::new(
                behavior,
                BodyLanguage::from_state(LifeState::default()),
                false,
            );
            assert!(matches!(live_explanation(context), Cow::Borrowed(_)));
            for (rhythm, observation) in [
                (
                    WatchRhythm::Busy,
                    "Following an active stream of command output.",
                ),
                (
                    WatchRhythm::Waiting,
                    "Keeping watch while output is quiet; the command has not finished.",
                ),
                (
                    WatchRhythm::Resumed,
                    "Perking up as command output resumes.",
                ),
            ] {
                assert_eq!(
                    live_explanation(context.with_watch_rhythm(rhythm)),
                    observation,
                );
                assert!(matches!(
                    live_explanation(context.with_watch_rhythm(rhythm)),
                    Cow::Borrowed(_)
                ));
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
            Behavior::Sleep,
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
                    "Keeping watch while output is quiet; the command has not finished."
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
    fn native_block_busy_and_finished_reuse_shared_sprite_context() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.batch_finished(1, false);
        organism.sync_live_attention_owner(Some(1));
        let busy = AttentionFacts {
            eligible: false,
            hover_available: true,
            activity: LiveActivityKnowledge::Busy,
        };
        let ticket = organism.live_greeting_offer(1).unwrap();
        organism.enter_live_greeting(1, ticket, busy);
        assert!(!organism.live_greeting_candidate_visible(1, busy));
        let running = organism.presentation_at_with_attention(
            1,
            true,
            false,
            Some(Motion::Calm),
            Duration::ZERO,
            (busy, true),
        );
        let context = RenderContext::new(
            Behavior::WatchCommand,
            BodyLanguage::from_state(organism.life.state()),
            false,
        );
        assert_eq!(running.sprite, sprite_frame_with_context(context, 0));
        organism.completed(1, "cargo test", Some(1), Some(20), None);
        let facts = AttentionFacts::known_idle_for_test(true, true);
        let finished = organism.presentation_at_with_attention(
            1,
            false,
            false,
            Some(Motion::Calm),
            Duration::ZERO,
            (facts, false),
        );
        let context = RenderContext {
            body_language: BodyLanguage::from_state(organism.life.state()),
            ..organism.sessions[&1].context
        };
        assert_eq!(finished.sprite, sprite_frame_with_context(context, 0));
        assert_ne!(context.behavior, Behavior::Idle);
        assert!(crate::organism_block::sprite_fits(&running.sprite));
        assert!(crate::organism_block::sprite_fits(&finished.sprite));
    }

    #[test]
    fn native_walk_variant_reuses_shared_context_without_mutating_life() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        let before = values(organism.life.state());
        for behavior in [Behavior::Idle, Behavior::Explore] {
            organism.sessions.get_mut(&1).unwrap().context =
                RenderContext::new(behavior, BodyLanguage::default(), false);
            for motion in [Some(Motion::Full), Some(Motion::Calm), Some(Motion::Static)] {
                let now = Duration::from_secs(38);
                let presentation = organism.presentation_at_with_attention(
                    1,
                    false,
                    false,
                    motion,
                    now,
                    (AttentionFacts::known_idle_for_test(false, false), false),
                );
                let context = RenderContext::new(
                    behavior,
                    BodyLanguage::from_state(organism.life.state()),
                    true,
                );
                assert_eq!(
                    presentation.walking_sprite,
                    sprite_frame_with_context(context, Organism::frame_at(motion, now))
                );
                assert!(crate::organism_block::sprite_fits(
                    &presentation.walking_sprite
                ));
                if motion != Some(Motion::Full) {
                    assert!(presentation.wander.is_none());
                }
            }
        }
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
        organism.completed(2, "cargo test", Some(0), Some(10), None);
        assert_eq!(
            organism.watch.rhythm(1, Duration::from_secs(10)),
            WatchRhythm::Waiting
        );
        // Even a completion ignored by enable-time quarantine retires cadence.
        organism.completed(1, "cargo test", Some(0), Some(10), None);
        assert_eq!(
            organism.watch.rhythm(1, Duration::from_secs(10)),
            WatchRhythm::Steady
        );
        assert!(!organism.watch_batch_admitted(1));
        organism.batch_finished(1, true);
        assert!(organism.watch_batch_admitted(1));
        organism.observe_watch(Some((1, 1, 1)), Some(1));
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
        organism.sync_live_attention_owner(Some(1));
        organism
            .live_attention
            .test_enter(1, Duration::ZERO, PreviewPose::Calm.context(), true);
        organism.live_attention.test_advance(
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
                organism.sync_live_attention_owner(Some(1));
                let ticket = organism.live_greeting_offer(1).unwrap();
                organism.enter_live_greeting(
                    1,
                    ticket,
                    AttentionFacts::known_idle_for_test(true, true),
                );
                assert!(organism
                    .live_greeting_deadline(AttentionFacts::known_idle_for_test(true, true))
                    .is_some());
                match event {
                    "remote" => organism.set_remote(target, true),
                    "forget" => organism.forget_session(target),
                    "completed" => organism.completed(target, "cargo test", Some(0), Some(1), None),
                    _ => organism.batch_finished(target, true),
                }
                assert_eq!(
                    organism
                        .live_greeting_deadline(AttentionFacts::known_idle_for_test(true, true))
                        .is_some(),
                    target == 2
                );
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
            let life = organism.sessions.get_mut(&3).unwrap();
            life.context = RenderContext::new(
                behavior,
                BodyLanguage::from_state(organism.life.state()),
                false,
            );
            life.reaction_until = Some(organism.born + Duration::from_secs(60));
            let LivePresentation {
                glyph,
                sprite,
                explanation,
                ..
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
        // Exactly reaching the quiet threshold does not credit earlier time as rest.
        assert!((life.state().energy - before_rest.energy + 0.002).abs() < 0.000001);
        let at_threshold = life.state().energy;
        life.advance(
            Duration::from_secs(61),
            true,
            false,
            CircadianPhase::Unlearned,
        );
        assert!((life.state().energy - at_threshold - 0.030).abs() < 0.000001);
        assert!(life.state().stress < before.stress);
        life.note_output(Duration::from_secs(61));
        let before_work = life.state().energy;
        life.advance(
            Duration::from_secs(62),
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
        organism.completed(3, "cargo test", None, Some(1), None);
        organism.tick(Some(3), false, false);
        assert_eq!(
            organism.sessions[&3].context.behavior,
            Behavior::UnknownOutcome
        );
    }

    fn completion_fixture(ids: &[usize]) -> Organism {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        for &id in ids {
            organism.prime_session(id, false);
            organism.set_remote(id, false);
            organism.batch_finished(id, false);
        }
        organism
    }

    #[test]
    fn completion_readout_preserves_real_outcome_duration_and_core_tone() {
        for (command, exit, duration, phrase, tone) in [
            (
                "cargo test --token=private-value",
                Some(101),
                Some(1_200),
                "exit 101",
                Tone::Error,
            ),
            (
                "cargo test --token=private-value",
                Some(0),
                Some(1_200),
                "build/test passed",
                Tone::Success,
            ),
            (
                "git push origin private-branch",
                Some(0),
                Some(1_200),
                "git push completed",
                Tone::Success,
            ),
            (
                "echo private-value",
                None,
                Some(1_200),
                "status unknown",
                Tone::Warning,
            ),
            (
                "sleep 60 private-value",
                Some(0),
                Some(60_000),
                "command finished cleanly",
                Tone::Quiet,
            ),
        ] {
            let mut organism = completion_fixture(&[1, 2]);
            organism.completed_at(
                1,
                command,
                exit,
                duration,
                Some("/private-location"),
                Duration::ZERO,
            );
            let before = values(organism.life.state());
            for motion in [Some(Motion::Full), Some(Motion::Calm), Some(Motion::Static)] {
                let sample =
                    organism.presentation_at(1, false, false, motion, Duration::from_millis(100));
                assert!(sample.status.contains(phrase), "{}", sample.status);
                assert!(sample.status.contains(if duration == Some(60_000) {
                    "60.0s"
                } else {
                    "1.2s"
                }));
                assert_eq!(sample.tone, Some(tone));
                assert!(sample.status.chars().count() <= 240);
                assert!(!sample.status.contains("private"));
                assert!(!sample.explanation.contains("private"));
                assert!(!sample.life.contains("private"));
                let other =
                    organism.presentation_at(2, false, false, motion, Duration::from_millis(100));
                assert_ne!(other.status, sample.status);
            }
            assert_eq!(values(organism.life.state()), before);
        }
    }

    #[test]
    fn repeated_clean_builds_keep_the_shared_quiet_acknowledgment() {
        let mut organism = completion_fixture(&[1]);
        for index in 0..8 {
            let now = Duration::from_secs(index * 3);
            organism.completed_at(1, "cargo test", Some(0), Some(250), Some("/project"), now);
            let sample = organism.presentation_at(1, false, false, Some(Motion::Calm), now);
            assert_eq!(
                sample.tone,
                Some(if index >= 6 {
                    Tone::Quiet
                } else {
                    Tone::Success
                })
            );
            assert!(sample.status.contains("build/test passed"));
            assert!(sample.status.contains("250ms"));
        }
    }

    #[test]
    fn completion_readout_expires_and_cannot_relabel_new_work_or_greetings() {
        let mut organism = completion_fixture(&[1]);
        organism.completed_at(1, "echo done", Some(0), Some(15), None, Duration::ZERO);
        let at = Duration::from_millis(100);
        assert!(organism
            .presentation_at(1, false, false, Some(Motion::Calm), at)
            .status
            .contains("15ms"));
        let running = organism.presentation_at(1, true, false, Some(Motion::Full), at);
        assert_eq!(running.status, "Watching command");
        assert_eq!(running.tone, Some(Tone::Active));
        let context = organism.sessions[&1].context;
        organism.live_attention.test_explicit(1, at, context);
        let greeting = organism.presentation_at(1, false, false, Some(Motion::Calm), at);
        assert_eq!(greeting.status, "Coming closer");
        assert_eq!(greeting.tone, Some(Tone::Active));
        organism.cancel_live_greeting();
        let expired = organism.presentation_at(
            1,
            false,
            false,
            Some(Motion::Static),
            Duration::from_millis(1500),
        );
        assert_eq!(expired.status, "Keeping company");
        assert!(!expired.status.contains("15ms"));
        // No tick is necessary to invalidate an expired background readout.
        assert!(organism.sessions[&1].completion.is_some());
    }

    #[test]
    fn closing_a_pane_releases_its_feedback_without_an_animation_sweep() {
        let mut organism = completion_fixture(&[4, 11]);
        organism.completed_at(4, "cargo test", Some(101), None, None, Duration::ZERO);
        organism.completed_at(11, "git push", Some(0), None, None, Duration::ZERO);
        organism.forget_session(4);
        assert!(!organism.sessions.contains_key(&4));
        assert_eq!(organism.sessions.len(), 1);
        let remaining = organism.presentation_at(
            11,
            false,
            false,
            Some(Motion::Calm),
            Duration::from_millis(100),
        );
        assert!(remaining.status.contains("git push completed"));
        organism.set_enabled(false);
        assert!(organism.sessions.is_empty());
    }

    #[test]
    fn new_native_work_retires_feedback_without_replaying_the_previous_result() {
        let mut organism = completion_fixture(&[1]);
        organism.completed_at(1, "cargo test", Some(101), Some(50), None, Duration::ZERO);
        let before = values(organism.life.state());
        organism.batch_finished(1, true);
        assert!(organism.sessions[&1].completion.is_none());
        assert!(organism.sessions[&1].reaction_until.is_none());
        organism.batch_finished(1, false);
        let after = organism.presentation_at(
            1,
            false,
            false,
            Some(Motion::Calm),
            Duration::from_millis(100),
        );
        assert_eq!(after.status, "Keeping company");
        assert_eq!(values(organism.life.state()), before);
    }

    #[test]
    fn native_vigil_follows_failure_recovery_and_push_only_in_the_known_idle_scope() {
        let mut organism = completion_fixture(&[1]);
        organism.observe_work_day(Some(10));
        for (index, expected) in [
            Behavior::GuardFailure,
            Behavior::GuardFailure,
            Behavior::GuardStuck,
        ]
        .into_iter()
        .enumerate()
        {
            let at = Duration::from_secs(index as u64 * 12);
            organism.completed_at(1, "cargo test", Some(101), Some(50), Some("/project"), at);
            organism.observe_work_context(1, Some("/project"));
            organism.tick_at(Some(1), false, false, at + Duration::from_secs(11));
            assert_eq!(organism.sessions[&1].context.behavior, expected);
            let sample = organism.presentation_at(
                1,
                false,
                false,
                Some(Motion::Calm),
                at + Duration::from_secs(11),
            );
            assert!(sample.status.contains("Keeping watch"));
            assert!(!sample.status.contains("50ms"));
        }
        organism.completed_at(
            1,
            "cargo test",
            Some(0),
            Some(50),
            Some("/project"),
            Duration::from_secs(36),
        );
        organism.observe_work_context(1, Some("/project"));
        organism.tick_at(Some(1), false, false, Duration::from_secs(44));
        assert_eq!(
            organism.sessions[&1].context.behavior,
            Behavior::GuardRecovery
        );
        organism.completed_at(
            1,
            "git push",
            Some(0),
            Some(50),
            Some("/project"),
            Duration::from_secs(45),
        );
        organism.observe_work_context(1, Some("/project"));
        organism.tick_at(Some(1), false, false, Duration::from_secs(51));
        assert!(!organism.sessions[&1].context.behavior.is_repo_vigil());
    }

    #[test]
    fn missing_changed_remote_or_new_day_scope_retires_the_vigil_without_life_rewards() {
        for (cwd, day, remote) in [
            (None, Some(10), false),
            (Some("/other"), Some(10), false),
            (Some("/project"), Some(11), false),
            (Some("/project"), None, false),
            (Some("/project"), Some(10), true),
        ] {
            let mut organism = completion_fixture(&[1]);
            organism.observe_work_day(Some(10));
            organism.completed_at(
                1,
                "cargo test",
                Some(101),
                None,
                Some("/project"),
                Duration::ZERO,
            );
            organism.observe_work_context(1, Some("/project"));
            organism.tick_at(Some(1), false, false, Duration::from_secs(6));
            assert_eq!(
                organism.sessions[&1].context.behavior,
                Behavior::GuardFailure
            );
            let before = values(organism.life.state());
            organism.observe_work_day(day);
            organism.set_remote(1, remote);
            organism.observe_work_context(1, cwd);
            assert!(!organism
                .base_context_at(1, Duration::from_secs(6))
                .behavior
                .is_repo_vigil());
            assert_eq!(values(organism.life.state()), before);
            if day != Some(10) {
                assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
            }
        }
    }

    #[test]
    fn idle_readout_describes_the_painted_body_without_allocating_watch_text() {
        let mut organism = completion_fixture(&[1]);
        let mut life = organism.life.state();
        life.energy = 0.1;
        organism.life.replace_state(life);
        let sample =
            organism.presentation_at(1, false, false, Some(Motion::Static), Duration::ZERO);
        assert!(sample.life.contains("Low energy"));
        assert_eq!(sample.explanation, "Dozing while its energy recovers.");
        assert!(matches!(sample.explanation, Cow::Borrowed(_)));
        assert_eq!(sample.status, "Keeping company");
    }

    #[test]
    fn local_agent_completions_keep_recovery_quiet_without_claiming_later_human_work() {
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        let mut organism = completion_fixture(&[1]);
        for (step, (generation, exit)) in [
            (Some(1), 101),
            (Some(2), 101),
            (Some(3), 101),
            (Some(4), 0),
            (None, 101),
            (None, 101),
            (None, 101),
            (None, 0),
        ]
        .into_iter()
        .enumerate()
        {
            terminal.process_batch(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            if let Some(generation) = generation {
                terminal
                    .arm_agent_execution(generation, "cargo test")
                    .unwrap();
            }
            terminal.process_batch(format!(
                "cargo test\r\n\x1b]133;C;cmdline_url=cargo%20test;cwd=/project\x07\x1b]133;D;{exit};duration_ms=100\x07"
            ).as_bytes());
            let completed = terminal.take_completed_commands();
            assert_eq!(completed.len(), 1);
            let completed = &completed[0];
            assert_eq!(completed.agent_generation, generation);
            let confidence = organism.life.state().confidence;
            organism.completed_event(
                1,
                CommandCompletion {
                    command: &completed.command,
                    exit: completed.exit_code,
                    duration: completed.duration_ms,
                    cwd: completed.command_cwd.as_deref(),
                    agent_driven: completed.agent_generation.is_some(),
                },
            );
            if generation.is_some() && exit != 0 {
                assert_eq!(organism.life.state().confidence, confidence);
            } else if exit != 0 {
                assert!(organism.life.state().confidence < confidence);
            }
            if step == 3 {
                assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
            } else if step == 7 {
                assert_eq!(
                    organism.sessions[&1].context.behavior,
                    Behavior::CelebrateBig
                );
            }
        }
    }

    #[test]
    fn agent_completion_without_authoritative_outcome_stays_unknown() {
        let mut organism = completion_fixture(&[1]);
        organism.completed_event(
            1,
            CommandCompletion {
                command: "cargo test",
                exit: None,
                duration: Some(60_000),
                cwd: Some("/project"),
                agent_driven: true,
            },
        );
        assert_eq!(
            organism.sessions[&1].context.behavior,
            Behavior::UnknownOutcome
        );
    }

    #[test]
    fn completed_commands_do_not_recover_another_working_directory() {
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        let mut organism = completion_fixture(&[1]);
        for (cwd, exit) in [
            ("/workspace/project-a", 101),
            ("/workspace/project-a", 101),
            ("/workspace/project-a", 101),
            ("/workspace/project-b", 0),
        ] {
            terminal.process_batch(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C;cmdline_url=cargo%20test;cwd={cwd}\x07\x1b]133;D;{exit};duration_ms=100\x07"
                )
                .as_bytes(),
            );
            assert_eq!(
                terminal.command_zones.back().unwrap().cwd.as_deref(),
                Some(cwd)
            );
            let completed = terminal.take_completed_commands();
            assert_eq!(completed.len(), 1);
            let completed = &completed[0];
            organism.completed(
                1,
                &completed.command,
                completed.exit_code,
                completed.duration_ms,
                completed.command_cwd.as_deref(),
            );
            organism.batch_finished(1, terminal.is_command_running());
        }
        assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
        assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
    }

    fn fail_builds(organism: &mut Organism, id: usize, cwd: Option<&str>) {
        for _ in 0..3 {
            organism.completed(id, "cargo test", Some(101), Some(100), cwd);
        }
    }

    #[test]
    fn same_command_cwd_keeps_real_recovery_across_running_directory_changes() {
        let mut organism = completion_fixture(&[1]);
        fail_builds(&mut organism, 1, Some("/source"));
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        terminal.process_batch(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C;cwd=/source\x07",
        );
        terminal.process_batch(
            b"\x1b]7;file://localhost/destination\x07\x1b]133;D;0;cwd=/destination\x07",
        );
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        let completed = &completed[0];
        organism.completed(
            1,
            &completed.command,
            completed.exit_code,
            completed.duration_ms,
            completed.command_cwd.as_deref(),
        );
        assert_eq!(
            organism.sessions[&1].context.behavior,
            Behavior::CelebrateBig
        );
        assert_eq!(
            organism.sessions[&1].native.repo_vigil(),
            RepoVigil::Recovery
        );
    }

    #[test]
    fn unknown_or_oversized_command_cwd_cannot_join_build_work_loops() {
        let oversized = format!("/{}", "x".repeat(2048));
        for cwd in [None, Some(""), Some(oversized.as_str())] {
            let mut organism = completion_fixture(&[1]);
            fail_builds(&mut organism, 1, Some("/known"));
            organism.completed(1, "cargo test", Some(0), Some(100), cwd);
            assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
            assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
            assert!(organism.sessions[&1].work_context.is_none());
            fail_builds(&mut organism, 1, cwd);
            organism.completed(1, "cargo test", Some(0), Some(100), cwd);
            assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
            assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
        }
        let boundary = format!("/{}", "x".repeat(2047));
        let mut organism = completion_fixture(&[1]);
        fail_builds(&mut organism, 1, Some(&boundary));
        organism.completed(1, "cargo test", Some(0), Some(100), Some(&boundary));
        assert_eq!(
            organism.sessions[&1].context.behavior,
            Behavior::CelebrateBig
        );
    }

    #[test]
    fn work_context_reset_keeps_continuous_life_and_other_sessions_work() {
        let mut organism = completion_fixture(&[1, 2]);
        fail_builds(&mut organism, 1, Some("/first"));
        fail_builds(&mut organism, 2, Some("/second"));
        let before_window = values(organism.life.state());
        let life = organism.sessions.get_mut(&1).unwrap();
        let before_native = values(life.native.state());
        life.set_work_context(Some("/third"));
        assert_eq!(values(life.native.state()), before_native);
        assert_eq!(values(organism.life.state()), before_window);
        assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
        organism.completed(2, "cargo test", Some(0), Some(100), Some("/second"));
        assert_eq!(
            organism.sessions[&2].context.behavior,
            Behavior::CelebrateBig
        );
    }

    #[test]
    fn ignored_completions_cannot_select_work_context_or_replay_recovery() {
        let mut organism = Organism::default();
        organism.completed(1, "cargo test", Some(101), None, Some("/old"));
        assert!(organism.sessions.is_empty());
        organism.set_enabled(true);
        organism.prime_session(1, true);
        organism.set_remote(1, false);
        fail_builds(&mut organism, 1, Some("/old"));
        assert!(organism.sessions[&1].work_context.is_none());
        organism.batch_finished(1, true);
        // The command that was running when enabled is still inadmissible.
        organism.completed(1, "cargo test", Some(101), None, Some("/old"));
        assert!(organism.sessions[&1].work_context.is_none());
        organism.batch_finished(1, false);
        organism.completed(1, "cargo test", Some(0), None, Some("/new"));
        assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
        organism.set_enabled(false);
        fail_builds(&mut organism, 1, Some("/old"));
        assert!(organism.sessions.is_empty());
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.batch_finished(1, false);
        organism.completed(1, "cargo test", Some(0), None, Some("/old"));
        assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
        assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
    }

    #[test]
    fn context_changes_keep_long_command_quiet_nod_and_shared_life_rewards() {
        let mut organism = completion_fixture(&[1]);
        fail_builds(&mut organism, 1, Some("/old"));
        let mut expected = NativeOrganism::from_persisted_state(organism.life.state());
        let reaction =
            expected.command_finished(classify_command("sleep 60"), Some(0), Some(60_000));
        assert_eq!(reaction.tone, Tone::Quiet);
        assert!(reaction.speech.is_none());
        organism.completed_at(
            1,
            "sleep 60",
            Some(0),
            Some(60_000),
            Some("/new"),
            Duration::ZERO,
        );
        assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
        assert_eq!(organism.sessions[&1].native.repo_vigil(), RepoVigil::None);
        assert_eq!(values(organism.life.state()), values(expected.state()));
        assert_eq!(
            organism.reaction_deadline_at(1, Duration::ZERO),
            Some(organism.born + reaction.hold_duration())
        );
    }

    #[test]
    fn sustained_completion_expires_at_exact_shared_hold_without_waiting_for_heartbeat() {
        let mut organism = completion_fixture(&[1]);
        let completed_at = Duration::from_secs(1);
        let expires_at = Duration::from_millis(2800);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, completed_at);
        assert_eq!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
        let deadline = organism.born + expires_at;
        let before = values(organism.life.state());
        for now in [
            completed_at,
            Duration::from_millis(1900),
            expires_at - Duration::from_nanos(1),
        ] {
            assert_eq!(organism.reaction_deadline_at(1, now), Some(deadline));
            assert_eq!(
                organism.base_context_at(1, now).behavior,
                Behavior::Celebrate
            );
        }
        assert_eq!(organism.reaction_deadline_at(1, expires_at), None);
        assert_eq!(
            organism.base_context_at(1, expires_at).behavior,
            Behavior::Idle
        );
        let expired = organism.presentation_at(1, false, false, Some(Motion::Calm), expires_at);
        assert_eq!(
            expired.explanation,
            live_explanation(PreviewPose::Calm.context())
        );
        assert_eq!(values(organism.life.state()), before);
        // A deadline tick retires the cache and resumes ordinary ambient state.
        organism.tick_at(Some(1), false, false, expires_at);
        assert_eq!(organism.sessions[&1].reaction_until, None);
        assert_ne!(organism.sessions[&1].context.behavior, Behavior::Celebrate);
        assert_eq!(
            organism.base_context_at(1, expires_at + Duration::from_secs(60)),
            organism.sessions[&1].context
        );
    }

    #[test]
    fn newer_completion_replaces_deadline_and_old_wake_cannot_retire_it() {
        let mut organism = completion_fixture(&[1]);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        let first_expiry = Duration::from_millis(1800);
        assert_eq!(
            organism.reaction_deadline_at(1, Duration::ZERO),
            Some(organism.born + first_expiry)
        );
        let next_completion = Duration::from_millis(500);
        organism.completed_at(1, "cargo test", Some(1), Some(100), None, next_completion);
        let next_expiry = Duration::from_millis(5500);
        assert_eq!(
            organism.sessions[&1].context.behavior,
            Behavior::InspectError
        );
        assert_eq!(
            organism.reaction_deadline_at(1, next_completion),
            Some(organism.born + next_expiry)
        );
        organism.tick_at(Some(1), false, false, first_expiry);
        assert_eq!(
            organism.base_context_at(1, first_expiry).behavior,
            Behavior::InspectError
        );
        assert_eq!(
            organism.reaction_deadline_at(1, first_expiry),
            Some(organism.born + next_expiry)
        );
        assert_eq!(organism.reaction_deadline_at(1, next_expiry), None);
        assert_eq!(
            organism.base_context_at(1, next_expiry).behavior,
            Behavior::Idle
        );
    }

    #[test]
    fn expired_background_completion_does_not_flash_when_owner_returns_before_tick() {
        let mut organism = completion_fixture(&[1, 2]);
        organism.owner_changed(None, Some(1));
        organism.completed_at(2, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        let returned_at = Duration::from_secs(3);
        organism.tick_at(Some(1), false, false, returned_at);
        assert_eq!(organism.sessions[&2].context.behavior, Behavior::Celebrate);
        organism.owner_changed(Some(1), Some(2));
        let before = values(organism.life.state());
        assert_eq!(organism.reaction_deadline_at(2, returned_at), None);
        for motion in [
            None,
            Some(Motion::Full),
            Some(Motion::Calm),
            Some(Motion::Static),
        ] {
            let sample = organism.presentation_at(2, false, false, motion, returned_at);
            assert_eq!(
                sample.explanation,
                live_explanation(PreviewPose::Calm.context())
            );
            let context = RenderContext::new(
                Behavior::Idle,
                BodyLanguage::from_state(organism.life.state()),
                false,
            );
            let frame = Organism::frame_at(motion, returned_at);
            assert_eq!(sample.sprite, sprite_frame_with_context(context, frame));
            assert_eq!(sample.glyph, sticky_glyph_with_context(context, frame));
        }
        assert_eq!(values(organism.life.state()), before);
        // The read-time fallback is pure; it neither queues nor renews a hold.
        assert_eq!(organism.sessions[&2].context.behavior, Behavior::Celebrate);
        assert_eq!(organism.reaction_deadline_at(2, returned_at), None);
    }

    #[test]
    fn new_running_command_overrides_completion_before_and_after_expiry() {
        let mut organism = completion_fixture(&[1]);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        let before = values(organism.life.state());
        for now in [Duration::from_millis(500), Duration::from_millis(1800)] {
            for motion in [None, Some(Motion::Calm), Some(Motion::Static)] {
                for (settled, behavior) in [
                    (false, Behavior::WatchCommand),
                    (true, Behavior::WatchSettled),
                ] {
                    let sample = organism.presentation_at(1, true, settled, motion, now);
                    let context = RenderContext::new(
                        behavior,
                        BodyLanguage::from_state(organism.life.state()),
                        false,
                    );
                    assert_eq!(sample.sprite, sprite_frame_with_context(context, 0));
                    assert_eq!(sample.glyph, sticky_glyph_with_context(context, 0));
                    assert_eq!(sample.explanation, live_explanation(context));
                }
            }
        }
        assert_eq!(values(organism.life.state()), before);
        organism.tick_at(Some(1), true, true, Duration::from_millis(1800));
        assert_eq!(
            organism.reaction_deadline_at(1, Duration::from_millis(1800)),
            None
        );
        assert_eq!(
            organism
                .base_context_at(1, Duration::from_millis(1800))
                .behavior,
            Behavior::Idle
        );
    }

    #[test]
    fn completion_deadlines_are_local_finite_and_disabled_holds_cannot_replay() {
        let mut organism = completion_fixture(&[1, 2]);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        assert_eq!(organism.reaction_deadline_at(99, Duration::ZERO), None);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        assert!(organism.reaction_deadline_at(1, Duration::ZERO).is_some());
        assert_eq!(organism.reaction_deadline_at(2, Duration::ZERO), None);
        organism.set_remote(1, true);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        organism.set_remote(1, false);
        organism.set_enabled(false);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        assert!(organism.sessions.is_empty());
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        assert_eq!(
            organism.base_context_at(1, Duration::ZERO).behavior,
            Behavior::Idle
        );
        // The initial batch remains quarantined after re-enabling.
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        assert_eq!(
            organism.base_context_at(1, Duration::ZERO).behavior,
            Behavior::Idle
        );
        organism.batch_finished(1, false);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        assert!(organism.reaction_deadline_at(1, Duration::ZERO).is_some());
        organism.forget_session(1);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        assert_eq!(organism.reaction_deadline_at(1, Duration::ZERO), None);
        assert_eq!(
            organism.base_context_at(1, Duration::ZERO).behavior,
            Behavior::Idle
        );
    }

    #[test]
    fn sustained_completion_reduced_motion_uses_one_fixed_fitting_frame() {
        let mut organism = completion_fixture(&[1]);
        organism.completed_at(1, "sleep 60", Some(0), Some(60_000), None, Duration::ZERO);
        let context = organism.sessions[&1].context;
        assert_eq!(context.behavior, Behavior::Celebrate);
        for motion in [None, Some(Motion::Calm), Some(Motion::Static)] {
            for millis in [0, 400, 900, 1799] {
                let sample = organism.presentation_at(
                    1,
                    false,
                    false,
                    motion,
                    Duration::from_millis(millis),
                );
                assert_eq!(sample.sprite, sprite_frame_with_context(context, 0));
                assert_eq!(sample.glyph, sticky_glyph_with_context(context, 0));
                assert_eq!(sample.sprite.lines().count(), 3);
                assert!(sample.sprite.lines().all(|line| line.chars().count() <= 12));
                assert!(sample.glyph.chars().count() <= 9);
                assert!(crate::organism_block::sprite_fits(&sample.sprite));
            }
        }
    }

    #[test]
    fn completion_context_expiry_is_shared_by_presentation_and_attention_admission() {
        let source = include_str!("organism.rs");
        let input = source
            .split_once("    fn live_input(")
            .unwrap()
            .1
            .split_once("    pub fn live_greeting_offer(")
            .unwrap()
            .0;
        assert!(input.contains("self.base_context_at(id, self.born.elapsed())"));
        let presentation = source
            .split_once("    fn presentation_at_with_attention(")
            .unwrap()
            .1
            .split_once("    pub fn preview_presentation(")
            .unwrap()
            .0;
        assert!(presentation.contains("self.base_context_at(id, now)"));
        assert!(!presentation.contains(".map(|life| life.context)"));
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
        organism.completed(7, "cargo test", Some(0), Some(100), None);
        assert_eq!(organism.sessions[&7].context.behavior, Behavior::Idle);
        organism.completed(7, "cargo test", None, Some(100), None);
        assert_eq!(
            organism.sessions[&7].context.behavior,
            Behavior::UnknownOutcome
        );
        organism.set_enabled(false);
        organism.completed(7, "cargo test", Some(0), None, None);
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
        organism.completed(11, "cargo test", Some(0), Some(1), None);
        assert!(organism.sessions.is_empty());
    }

    #[test]
    fn initial_batch_is_quarantined_even_when_cached_running_is_false() {
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(9, false);
        organism.set_remote(9, false);
        organism.completed(9, "cargo test", Some(0), Some(100), None);
        organism.completed(9, "cargo test", Some(1), Some(100), None);
        assert_eq!(organism.sessions[&9].context.behavior, Behavior::Idle);
        organism.batch_finished(9, false);
        organism.completed(9, "cargo test", None, Some(100), None);
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

#[cfg(test)]
mod motion_reason_tests {
    use super::*;
    use crate::organism_wake_plan::DeadlineReason;

    #[test]
    fn representation_conversion_round_trips_all_explicit_values() {
        for value in [Motion::Full, Motion::Calm, Motion::Static] {
            assert_eq!(
                Motion::from(jterm_core::organism_motion::Motion::from(value)),
                value
            );
        }
    }

    #[test]
    fn preview_reason_keeps_the_same_absolute_deadline_for_every_pose() {
        let mut organism = Organism::default();
        let start = Duration::from_secs(10);
        organism.preview_deadlines.accepted(start);
        for pose in PreviewPose::ALL {
            organism.pose = Pose(pose);
            for now in [
                start,
                start + GentleInteraction::HOLD,
                start + GentleInteraction::COOLDOWN,
            ] {
                let previous = organism.preview_deadline_at(now);
                let tagged = organism.preview_deadline_with_reason_at(now);
                assert_eq!(tagged.map(|(at, _)| at), previous);
                if let Some((at, reason)) = tagged {
                    assert_eq!(
                        reason,
                        if at == organism.preview_deadlines.greeting_until {
                            DeadlineReason::PreviewGreeting
                        } else {
                            DeadlineReason::PreviewCooldown
                        }
                    );
                }
            }
        }
    }
}
