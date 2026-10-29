// Font-geometry fixture only. Every @ section is verbatim production source.
// The window/state and opaque font-measurement boundary are controlled inputs;
// no glyph shaping, native renderer, PTY or display-server behavior is claimed.
use crate::reference_shell as shell;
@constants@
@clamp@
@metrics-type@
mod iced {
    #[derive(Clone, Copy)]
    pub enum Font { Measured, Missing }
}
fn measure_mono_ascii_advance(font: iced::Font, size: f32) -> Option<f32> {
    match font { iced::Font::Measured => Some(size * 0.625), iced::Font::Missing => None }
}
impl Metrics {
    @metrics-methods@
}
struct Config { font_size: f32, chrome_font_size: Option<f32>, line_spacing: f32, padding: f32 }
@config-functions@
impl Config {
    fn is_card_mode(&self) -> bool { true }
    @config-methods@
}
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
    config: Config,
    mono: iced::Font,
    metrics: Metrics,
    reading: bool,
    startup_recovery: Recovery,
}
impl Frost {
    fn block_reading_bar_visible(&self) -> bool { self.reading }
    @app-methods@
    fn apply_metrics(&mut self) { @apply-metrics@ }
}
#[cfg(test)]
mod font_viewport_budget {
    use super::*;
    fn app(content: f32, chrome: f32, width: f32, height: f32) -> Frost {
        let mut value=Frost { win_size: Size { width, height }, sidebar_open: true,
            reference_inspector_open: true,
            config: Config { font_size: content, chrome_font_size: Some(chrome), line_spacing: 1.0, padding: 2.0 },
            mono: iced::Font::Measured, metrics: Metrics::new(1.0,1.0,0.0,true),
            reading: false, startup_recovery: Recovery { active: false } };
        value.apply_metrics(); value
    }
    fn close(a: f32, b: f32) { assert!((a-b).abs() < 0.001, "{a} != {b}"); }
    #[test]
    fn measured_and_fallback_metrics_keep_scaled_content_rows() {
        for font in [iced::Font::Measured, iced::Font::Missing] {
            for spacing in [0.8, 1.0, 1.5, 3.0] {
                for padding in [0.0, 2.0, 8.0] {
                    let mut previous = usize::MAX;
                    for size in [8.0, 14.0, 24.0, 28.0, 36.0, 72.0] {
                        let mut a = app(size, 14.0, 1356.0, 704.0);
                        a.mono=font; a.config.line_spacing=spacing; a.config.padding=padding;
                        a.apply_metrics(); let m=a.metrics;
                        close(m.font_size, size);
                        close(m.cell_h, size * 1.2 * spacing);
                        let (_, rows) = m.grid_size(a.term_width()-SCROLLBAR_WIDTH, a.term_height());
                        let expected = (((506.0-padding*2.0).max(0.0)/(size*1.2*spacing)).floor() as usize).clamp(1,512);
                        assert_eq!(rows, expected, "content={size},spacing={spacing},padding={padding}");
                        assert!(rows <= previous); previous=rows;
                    }
                }
            }
        }
        let normal=app(14.0,14.0,1356.0,704.0);
        let zoomed=app(28.0,14.0,1356.0,704.0);
        let rows=|a: &Frost| a.metrics.grid_size(a.term_width()-SCROLLBAR_WIDTH,a.term_height()).1;
        assert_eq!(rows(&normal),29); assert_eq!(rows(&zoomed),14);
    }
    #[test]
    fn delegated_viewport_charges_actual_content_and_chrome_strips_once() {
        for (width,height) in [(320.0,400.0),(700.0,640.0),(800.0,640.0),(1356.0,704.0),(1600.0,950.0)] {
            for content in [8.0,14.0,24.0,36.0,72.0] {
                for chrome in [8.0,14.0,32.0,72.0] {
                    for reading in [false,true] {
                        for recovery in [false,true] {
                            for inspector in [false,true] {
                                let mut a=app(content,chrome,width,height);
                                a.reading=reading; a.startup_recovery.active=recovery;
                                a.reference_inspector_open=inspector;
                                let center=a.reference_layout().center;
                                let bar=if reading { (content*1.3+14.0).ceil().max(36.0) }else{0.0};
                                let banner=if recovery {(chrome.clamp(8.0,32.0)*1.3*3.0+12.0).ceil()}else{0.0};
                                close(a.block_reading_bar_height(),bar);
                                let expected=(center.height-shell::CANVAS_HEADER_HEIGHT-native_viewport::CONTENT_TOP
                                    -shell::COMPOSER_HEIGHT-shell::COMPOSER_BOTTOM-bar-banner).max(0.0);
                                close(a.term_height(),expected);
                                close(a.term_width(),(center.width-2.0*shell::COMPOSER_INSET).max(0.0));
                                assert!(a.term_height()>=0.0);
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn chrome_and_content_groups_keep_distinct_geometry_authority() {
        let mut a=app(14.0,14.0,1356.0,704.0);
        a.config.chrome_font_size=None;
        a.startup_recovery.active=true;
        let old_height=a.term_height(); let old_header=a.pane_header_height();
        a.config.set_content_font_size(36.0); a.apply_metrics();
        assert_eq!(a.effective_font_size(),36.0);
        assert_eq!(a.chrome_font_size(),14.0,"content setter must freeze legacy chrome fallback");
        close(a.term_height(),old_height);
        assert!(a.pane_header_height()>old_header,"split header text follows content zoom");
        let metrics=a.metrics;
        a.config.chrome_font_size=Some(32.0); a.apply_metrics();
        assert_eq!(a.effective_font_size(),36.0);
        let other=a.metrics;
        close(metrics.cell_h,other.cell_h); close(metrics.cell_w,other.cell_w);
        assert!(a.term_height()<old_height,"UI recovery banner follows chrome font only");
    }
}
