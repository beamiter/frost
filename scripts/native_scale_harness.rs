//! Observer template. Every @ section is actual source, inserted verbatim.
#![allow(dead_code)]
extern crate self as jterm_core;
#[path = @core-path@]
pub mod reference_shell;
#[path = @viewport-path@]
mod native_viewport;
#[path = @typography-path@]
mod ui_typography;
use reference_shell as shell;
@terminal-constants@
@clamp@
@font-system@
@font-advance@
@metrics@
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
}
impl Frost {
    fn effective_font_size(&self) -> f32 { self.font_size }
    fn chrome_font_size(&self) -> f32 { self.font_size }
    fn block_reading_bar_visible(&self) -> bool { self.reading }
    @methods@
}
fn main() {
    let a: Vec<_> = std::env::args().collect();
    assert_eq!(a.len(), 9);
    let num = |i: usize| a[i].parse::<f32>().unwrap();
    let app = Frost {
        win_size: Size { width: num(1), height: num(2) },
        sidebar_open: true, reference_inspector_open: true,
        font_size: num(4), reading: a[7] == "1",
        startup_recovery: Recovery { active: a[8] == "1" },
    };
    let family: &'static str = Box::leak(a[3].clone().into_boxed_str());
    let m = Metrics::with_font(iced::Font::with_name(family), num(4), num(5), num(6), true);
    let l = app.reference_layout();
    let raw = native_viewport::raw_rect(l.center, app.block_reading_bar_height(),
                                      app.startup_recovery.banner_height(app.chrome_font_size()));
    let (cols, rows) = m.grid_size((app.term_width() - SCROLLBAR_WIDTH).max(0.0), app.term_height());
    println!(r#"{{"grid":[{},{}],"center":[{},{},{},{}],"raw":[{},{},{},{}],"navigation_width":{},"inspector_width":{},"rail_width":{},"titlebar_height":{},"palette":{{"rail":{:?},"navigation":{:?},"inspector":{:?}}},"metrics":{{"cell_w":{},"cell_h":{},"padding":{},"mono_advance_exact":{}}}}}"#,
             cols, rows, l.center.x,l.center.y,l.center.width,l.center.height,
             raw.x,raw.y,raw.width,raw.height,l.navigation.map_or(0.0,|r|r.width),
             l.inspector.map_or(0.0,|r|r.width),l.rail.width,l.titlebar.height,
             shell::color::RAIL,shell::color::NAVIGATION,shell::color::INSPECTOR,
             m.cell_w,m.cell_h,m.padding,m.mono_advance_exact);
}
