//! Observer template. Every @ section is actual source, inserted verbatim.
//! The fixture's short Block prompt has two source rows, no active hold, split
//! or editor. Real font measurement and all native budget methods are retained.
#![allow(dead_code)]
extern crate self as jterm_core;
#[path = @core-path@]
pub mod reference_shell;
#[path = @viewport-path@]
mod native_viewport;
#[path = @typography-path@]
mod ui_typography;
#[path = @layout-path@]
mod glass_layout;
#[path = @tokens-path@]
mod glass_shell_tokens;
#[path = @profile-path@]
mod glass_profile;
use glass_profile as shell;
use std::collections::HashMap;
@terminal-constants@
@clamp@
@font-system@
@font-advance@
@metrics@
mod terminal_view { pub const SCROLLBAR_WIDTH: f32 = super::SCROLLBAR_WIDTH; }
mod chat_mode { #[derive(PartialEq)] pub enum ChatRole { Output, Composer } }
mod terminal { @min-input-rows@ pub enum ProjectedRowKind { Raw } }
struct ChatRow { role: chat_mode::ChatRole }
struct Projection { rows: Vec<ChatRow>, kinds: Vec<terminal::ProjectedRowKind> }
impl Projection {
    fn chat_rows(&self) -> &[ChatRow] { &self.rows }
    fn row_kinds(&self) -> &[terminal::ProjectedRowKind] { &self.kinds }
}
struct Session { id: usize, cursor: (usize,usize), projection: Projection }
impl Session { fn transcript_read_only(&self) -> bool { false } }
mod native_input_surface {
    pub(crate) const INPUT_STRIP_HEIGHT: f32 = crate::native_viewport::INPUT_STRIP_HEIGHT;
    @dock-type@
    @dock-metrics@
}
struct Config { sidebar_width: Option<f32>, inspector_width: Option<f32> }
impl Config {
    fn is_card_mode(&self) -> bool { true }
    fn is_chat_mode(&self) -> bool { false }
    fn is_block_mode(&self) -> bool { true }
}
#[derive(Clone, Copy)]
struct Size { width: f32, height: f32 }
struct Recovery { active: bool }
impl Recovery {
    fn visible(&self) -> bool { self.active }
    @banner@
}
struct Frost {
    win_size: Size,
    sidebar_open: bool,
    reference_inspector_open: bool,
    font_size: f32,
    reading: bool,
    startup_recovery: Recovery,
    config: Config,
    metrics: Metrics,
    active: usize,
    sessions: Vec<Session>,
    vim9_tabs: HashMap<usize, ()>,
}
impl Frost {
    fn effective_font_size(&self) -> f32 { self.font_size }
    fn chrome_font_size(&self) -> f32 { self.font_size }
    fn block_reading_bar_visible(&self) -> bool { self.reading }
    fn is_split(&self) -> bool { false }
    fn held_input_metrics(&self) -> Option<native_input_surface::DockMetrics> { None }
    @methods@
}
fn main() {
    let a: Vec<_> = std::env::args().collect();
    assert_eq!(a.len(), 9);
    let num = |i: usize| a[i].parse::<f32>().unwrap();
    let family: &'static str = Box::leak(a[3].clone().into_boxed_str());
    let m = Metrics::with_font(iced::Font::with_name(family), num(4), num(5), num(6), true);
    let app = Frost {
        win_size: Size { width: num(1), height: num(2) },
        sidebar_open: true, reference_inspector_open: false,
        font_size: num(4), reading: a[7] == "1",
        startup_recovery: Recovery { active: a[8] == "1" },
        config: Config { sidebar_width: None, inspector_width: None }, metrics: m,
        active: 0, vim9_tabs: HashMap::new(),
        sessions: vec![Session { id: 1, cursor: (1,0), projection: Projection {
            rows: vec![ChatRow { role: chat_mode::ChatRole::Composer }, ChatRow { role: chat_mode::ChatRole::Composer }],
            kinds: vec![terminal::ProjectedRowKind::Raw, terminal::ProjectedRowKind::Raw],
        } }],
    };
    let l = app.reference_layout();
    let raw = app.reference_native_budget().raw;
    let (cols, rows) = m.grid_size((app.term_width() - SCROLLBAR_WIDTH).max(0.0), app.term_height());
    println!(r#"{{"grid":[{},{}],"center":[{},{},{},{}],"raw":[{},{},{},{}],"navigation_width":{},"inspector_width":{},"rail_width":{},"titlebar_height":{},"palette":{{"rail":{:?},"navigation":{:?},"inspector":{:?}}},"metrics":{{"cell_w":{},"cell_h":{},"padding":{},"mono_advance_exact":{}}}}}"#,
             cols, rows, l.center.x,l.center.y,l.center.width,l.center.height,
             raw.x,raw.y,raw.width,raw.height,l.navigation.map_or(0.0,|r|r.width),
             l.inspector.map_or(0.0,|r|r.width),l.rail.width,l.titlebar.height,
             glass_shell_tokens::RAIL,glass_shell_tokens::NAVIGATION,glass_shell_tokens::INSPECTOR,
             m.cell_w,m.cell_h,m.padding,m.mono_advance_exact);
}
