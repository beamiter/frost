//! Admission shared by the native body, observations and attention reducer.
//! These predicates have no clock, renderer, input writer or independent owner.
use jterm_core::organism_live::LiveActivityKnowledge;

#[derive(Clone, Copy)]
pub(crate) struct SessionOwner {
    pub id: usize,
    pub managed_remote: bool,
    pub read_only: bool,
    pub alternate_screen: bool,
    pub local: bool,
    pub remote_foreground: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Observation {
    pub enabled: bool,
    pub card_mode: bool,
    pub bottom_bar: bool,
    pub focused: bool,
    pub input_active: bool,
    pub presentable: bool,
    pub session: Option<SessionOwner>,
}

impl Observation {
    pub(crate) fn owner(self) -> Option<usize> {
        if !self.enabled
            || (!self.card_mode && !self.bottom_bar)
            || !self.focused
            || !self.input_active
            || !self.presentable
        {
            return None;
        }
        self.session
            .filter(|s| {
                !s.managed_remote
                    && !s.read_only
                    && !s.alternate_screen
                    && s.local
                    && !s.remote_foreground
            })
            .map(|s| s.id)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct BodyGuards {
    pub card_mode: bool,
    pub epoch_exhausted: bool,
    pub pointer_down: bool,
    pub dragging: bool,
    pub application_gesture: bool,
    pub selected: bool,
    pub mouse_reporting: bool,
    pub preedit: bool,
    pub images: bool,
}

impl BodyGuards {
    pub(crate) fn allows(self) -> bool {
        self.card_mode
            && !self.epoch_exhausted
            && !self.pointer_down
            && !self.dragging
            && !self.application_gesture
            && !self.selected
            && !self.mouse_reporting
            && !self.preedit
            && !self.images
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Interaction {
    pub presentable: bool,
    pub has_owner: bool,
    pub pointer_down: bool,
    pub dragging: bool,
    pub application_gesture: bool,
    pub running: bool,
}

impl Interaction {
    pub(crate) fn available(self) -> bool {
        self.presentable
            && self.has_owner
            && !self.pointer_down
            && !self.dragging
            && !self.application_gesture
            && !self.running
    }
}

pub(crate) fn care_available(
    enabled: bool,
    interaction: bool,
    native_visible: bool,
    input_clean: bool,
    knowledge: impl FnOnce() -> LiveActivityKnowledge,
) -> bool {
    enabled
        && interaction
        && native_visible
        && input_clean
        && matches!(knowledge(), LiveActivityKnowledge::KnownIdle)
}

/// Preserve the lazy prompt-grid capture: busy/ineligible panes need no scan.
pub(crate) fn activity(
    running: bool,
    eligible: bool,
    knowledge: impl FnOnce() -> LiveActivityKnowledge,
) -> LiveActivityKnowledge {
    if running {
        LiveActivityKnowledge::Busy
    } else if eligible {
        knowledge()
    } else {
        LiveActivityKnowledge::Unknown
    }
}

pub(crate) fn greeting_target(
    requested: Option<usize>,
    owner: Option<usize>,
    interaction: bool,
) -> bool {
    requested.is_some() && requested == owner && interaction
}

pub(crate) struct Snapshot<'a, Projection, Block> {
    pub owner: usize,
    pub epoch: u64,
    pub projection: &'a Projection,
    pub block: &'a Block,
}

impl<P: PartialEq, B: PartialEq> Snapshot<'_, P, B> {
    pub(crate) fn matches(&self, current: &Self) -> bool {
        self.owner == current.owner
            && self.epoch == current.epoch
            && self.projection == current.projection
            && self.block == current.block
    }
}

/// The active PTY identity is checked before terminal activity reconciliation.
pub(crate) fn active_output(active: Option<(usize, i32)>, id: usize, fd: i32) -> bool {
    active == Some((id, fd))
}

pub(crate) fn attention_available(
    interaction: bool,
    motion: Option<crate::organism::Motion>,
) -> bool {
    interaction && motion != Some(crate::organism::Motion::Static)
}
