//! Presentation-only capabilities. No PTY, terminal VM, write, resize or close-owner API.
use crate::{
    terminal::{CursorShape, DynamicColorPalette, ProjectedViewport},
    terminal_view::KittyRender,
};
use std::sync::Arc;

pub(crate) const MAX_VIEWS: usize = 8;

#[path = "observer_identity.rs"]
mod identity;
pub(crate) use identity::{role, title, Role, Target};

pub(crate) struct Frame {
    pub projection: Arc<ProjectedViewport>,
    pub cursor: (usize, usize),
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    pub palette: DynamicColorPalette,
    pub foreground: Option<(u8, u8, u8)>,
    pub background: Option<(u8, u8, u8)>,
    pub cursor_color: Option<(u8, u8, u8)>,
    pub images: Vec<KittyRender>,
}

pub(crate) struct View {
    pub target: Target,
    pub pid: i32,
    pub latest: Frame,
    pub closed: bool,
}
