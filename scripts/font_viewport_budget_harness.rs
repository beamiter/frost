// Font-geometry fixture only. Every @ section is verbatim production source.
// Projection rows, window state and opaque font measurement are controlled
// inputs. This is not a native renderer, PTY or physical input-method test.
use std::collections::HashMap;
use crate::glass_profile as shell;
mod toolbar {
    use crate::glass_shell_tokens as glass;
    @toolbar-constants@
}
@constants@
@clamp@
@metrics-type@
mod terminal_view {
    pub const SCROLLBAR_WIDTH: f32 = super::SCROLLBAR_WIDTH;
}
mod iced {
    #[derive(Clone, Copy)]
    pub enum Font { Measured, Missing }
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Rectangle { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }
}
fn measure_mono_ascii_advance(font: iced::Font, size: f32) -> Option<f32> {
    match font { iced::Font::Measured => Some(size * 0.625), iced::Font::Missing => None }
}
impl Metrics {
    @metrics-methods@
}
struct Config {
    font_size: f32, chrome_font_size: Option<f32>, line_spacing: f32,
    padding: f32, chat: bool, sidebar_width: Option<f32>, inspector_width: Option<f32>,
}
@config-functions@
impl Config {
    fn is_card_mode(&self) -> bool { true }
    fn is_chat_mode(&self) -> bool { self.chat }
    fn is_block_mode(&self) -> bool { !self.chat }
    @config-methods@
}
struct Size { width: f32, height: f32 }
struct Recovery { active: bool }
impl Recovery {
    fn visible(&self) -> bool { self.active }
    @banner@
}
// Only the input metadata boundary is controlled. The production function
// still finds Composer/raw rows and the real cursor before computing its cap.
mod terminal {
    @min-input-rows@
    pub enum ProjectedRowKind { Raw, Padding }
}
struct ChatRow { role: chat_mode::ChatRole }
struct Projection { chat: Vec<ChatRow>, kinds: Vec<terminal::ProjectedRowKind> }
impl Projection {
    fn chat_rows(&self) -> &[ChatRow] { &self.chat }
    fn row_kinds(&self) -> &[terminal::ProjectedRowKind] { &self.kinds }
}
struct ChatSessionState { alternate_screen: bool }
struct Terminal { alternate_screen: bool }
impl Terminal {
    fn chat_session_state(&self) -> ChatSessionState { ChatSessionState { alternate_screen: self.alternate_screen } }
}
struct Session { id: usize, read_only: bool, cursor: (usize, usize), projection: Projection, terminal: Terminal }
impl Session { fn transcript_read_only(&self) -> bool { self.read_only } }
mod native_input_surface {
    @dock-metrics@
}
@input-hold-type@
struct Frost {
    input_dock_hold: Option<InputDockHold>, input_dock_hold_generation: u64,
    win_size: Size,
    sidebar_open: bool,
    reference_inspector_open: bool,
    config: Config,
    mono: iced::Font,
    metrics: Metrics,
    reading: bool,
    startup_recovery: Recovery,
    sessions: Vec<Session>, active: usize, vim9_tabs: HashMap<usize, ()>, split: bool,
    glass_geometry_ready: bool, cols: usize, rows: usize,
}
impl Frost {
    fn block_reading_bar_visible(&self) -> bool { self.reading }
    fn is_split(&self) -> bool { self.split }
    @app-methods@
    fn apply_metrics(&mut self) { @apply-metrics@ }
}
#[cfg(test)]
mod font_viewport_budget {
    use super::*;
    fn session(id: usize, rows: usize) -> Session {
        let mut chat = vec![ChatRow { role: chat_mode::ChatRole::Output }];
        chat.extend((0..rows).map(|_| ChatRow { role: chat_mode::ChatRole::Composer }));
        Session { id, read_only: false, cursor: (rows, 0), terminal: Terminal { alternate_screen: false }, projection: Projection {
            kinds: (0..chat.len()).map(|_| terminal::ProjectedRowKind::Raw).collect(), chat,
        } }
    }
    fn app(content: f32, chrome: f32, width: f32, height: f32) -> Frost {
        let mut value=Frost { input_dock_hold: None, input_dock_hold_generation: 0, win_size: Size { width, height }, sidebar_open: true,
            reference_inspector_open: true,
            config: Config { font_size: content, chrome_font_size: Some(chrome), line_spacing: 1.0, padding: 2.0, chat: false, sidebar_width: None, inspector_width: None },
            mono: iced::Font::Measured, metrics: Metrics::new(1.0,1.0,0.0,true),
            reading: false, startup_recovery: Recovery { active: false },
            sessions: vec![session(7,0)], active: 0, vim9_tabs: HashMap::new(), split: false,
            glass_geometry_ready: true, cols: 1, rows: 1 };
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
                        close(a.term_height(),549.0);
                        let (_, rows) = m.grid_size(a.term_width()-SCROLLBAR_WIDTH, a.term_height());
                        let expected = (((549.0-padding*2.0).max(0.0)/(size*1.2*spacing)).floor() as usize).clamp(1,512);
                        assert_eq!(rows, expected, "content={size},spacing={spacing},padding={padding}");
                        assert!(rows <= previous); previous=rows;
                    }
                }
            }
        }
        let normal=app(14.0,14.0,1356.0,704.0);
        let zoomed=app(28.0,14.0,1356.0,704.0);
        let rows=|a: &Frost| a.metrics.grid_size(a.term_width()-SCROLLBAR_WIDTH,a.term_height()).1;
        assert_eq!(rows(&normal),32); assert_eq!(rows(&zoomed),16);
        close(app(14.0,14.0,1612.0,976.0).term_height(),821.0);
    }
    #[test]
    fn compact_chrome_and_dock_preserve_exact_pty_budget() {
        for (width,height) in [(1612.0,976.0),(1356.0,704.0),(800.0,640.0),(700.0,400.0)] {
            for content in [14.0,36.0,72.0] {
                for draft_rows in [None,Some(1),Some(100)] {
                    let mut a=app(content,14.0,width,height);
                    a.config.chat=draft_rows.is_some();
                    a.sessions[0]=session(7,draft_rows.unwrap_or(0));
                    close(a.effective_font_size(),content);
                    close(a.metrics.cell_h,content*1.2);
                    let layout=a.reference_layout();
                    let top=layout.header.y+layout.header.height+crate::glass_shell_tokens::TERMINAL_TOP_GAP;
                    let full=(layout.composer.y-crate::glass_shell_tokens::TERMINAL_FOOTER_GAP-top).max(0.0);
                    let joint=a.reference_native_budget();
                    close(a.term_height()+joint.input_height,full);
                    assert!(a.refresh_glass_grid_dimensions());
                    let expected_rows=(((a.term_height()-2.0*a.metrics.padding).max(0.0)/a.metrics.cell_h).floor() as usize).clamp(1,512);
                    let expected_cols=(((a.term_width()-SCROLLBAR_WIDTH-2.0*a.metrics.padding-8.0).max(0.0)/a.metrics.cell_w).floor() as usize).clamp(1,1024);
                    assert_eq!((a.cols,a.rows),(expected_cols,expected_rows));
                    if let Some(editor)=joint.editor {
                        close(editor.width,a.term_width());
                        close(joint.raw.bottom()+crate::glass_shell_tokens::TERMINAL_FOOTER_GAP,editor.y);
                        close(editor.bottom(),layout.composer.y);
                    } else {
                        close(a.term_height(),full);
                    }
                }
            }
        }
    }
    #[test]
    fn delegated_viewport_charges_actual_content_and_chrome_strips_once() {
        for (width,height) in [(320.0,400.0),(700.0,640.0),(800.0,640.0),(1356.0,704.0),(1612.0,976.0)] {
            for content in [8.0,14.0,24.0,36.0,72.0] {
                for chrome in [8.0,14.0,32.0,72.0] {
                    for reading in [false,true] {
                        for recovery in [false,true] {
                            for inspector in [false,true] {
                                let mut a=app(content,chrome,width,height);
                                a.reading=reading; a.startup_recovery.active=recovery;
                                a.reference_inspector_open=inspector;
                                let layout=a.reference_layout();
                                let bar=0.0; // Reading controls live in the existing header; never charge an extra strip.
                                let banner=if recovery {(chrome.clamp(8.0,32.0)*1.3*3.0+12.0).ceil()}else{0.0};
                                close(a.block_reading_bar_height(),bar);
                                let native_top=layout.header.y+layout.header.height+crate::glass_shell_tokens::TERMINAL_TOP_GAP+bar+banner;
                                let expected=(layout.composer.y-crate::glass_shell_tokens::TERMINAL_FOOTER_GAP-native_top).max(0.0);
                                close(a.term_height(),expected);
                                close(a.term_width(),(layout.center.width-crate::glass_shell_tokens::TERMINAL_INSET_LEFT-crate::glass_shell_tokens::TERMINAL_INSET_RIGHT).max(0.0));
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
    #[test]
    fn actual_composer_rows_cursor_and_bounded_dock_share_one_pty_budget() {
        for (width,height) in [(1612.0,976.0),(1356.0,704.0),(800.0,640.0),(700.0,400.0)] {
            for content in [8.0,14.0,24.0,36.0,72.0] {
                for chat in [false,true] {
                    for source_rows in [1,2,4,100] {
                        let mut a=app(content,14.0,width,height);
                        a.config.chat=chat; a.sessions[0]=session(7,source_rows);
                        let layout=a.reference_layout();
                        let available=(layout.composer.y-crate::glass_shell_tokens::TERMINAL_FOOTER_GAP-(layout.header.y+layout.header.height+crate::glass_shell_tokens::TERMINAL_TOP_GAP)).max(0.0);
                        let cap=(available*0.45).clamp(44.0,180.0).min(available);
                        let native_rows=source_rows.max(2);
                        let rows=if chat {native_rows.max(3)} else {native_rows};
                        let cell=content*1.2;
                        let body=rows as f32*cell;
                        let padding=if rows<=2 && body<=44.0 {((44.0-body)*0.5).min(8.0)} else {8.0};
                        let trailing=if chat {1.0} else {2.0};
                        let expected=(body+2.0*padding+trailing*cell).max(44.0).min(cap);
                        let prospective_rows=(((available-expected-2.0*a.metrics.padding).max(0.0)/cell).floor() as usize).clamp(1,512);
                        let admitted=native_rows+2<prospective_rows;
                        let input=a.reference_input_metrics();
                        assert_eq!(input.is_some(),admitted,"prompt={source_rows},font={content},chat={chat},height={height}");
                        let joint=a.reference_native_budget();
                        if let Some(input)=input {
                            close(input.input_height,expected);
                            let editor=joint.editor.unwrap();
                            close(joint.input_height,expected);
                            close(a.term_height(),(available-expected).max(0.0));
                            close(a.term_width(),editor.width);
                            close(joint.raw.height,a.term_height());
                            close(joint.raw.bottom()+crate::glass_shell_tokens::TERMINAL_FOOTER_GAP,editor.y);
                            close(editor.bottom(),layout.composer.y);
                            assert!(joint.raw.height+0.001>=available*0.55);
                            assert!(input.input_height<=180.0);
                            let visible_start=(a.sessions[0].cursor.0+1).saturating_sub(input.visible_rows).max(1);
                            assert!(visible_start<=a.sessions[0].cursor.0);
                            assert!(a.sessions[0].cursor.0<visible_start+input.visible_rows);
                        } else {
                            assert!(joint.editor.is_none());
                            close(a.term_height(),available);
                        }
                    }
                }
            }
        }
        let mut a=app(14.0,14.0,1612.0,976.0); a.config.chat=true;
        a.sessions[0]=session(7,4);
        close(a.reference_input_metrics().unwrap().input_height,100.0);
        close(a.term_height(),721.0);
        // Oversized prompts cannot create a dock/PTY-resize oscillation.
        a.sessions[0].cursor.0=99;
        assert!(a.reference_input_metrics().is_none());
        // Host padding does not pretend to be native input; the shared floor
        // still reserves three Chat rows plus one blank line below the caret.
        a.sessions[0].cursor.0=1;
        a.sessions[0].projection.kinds[2..].iter_mut().for_each(|k|*k=terminal::ProjectedRowKind::Padding);
        close(a.reference_input_metrics().unwrap().input_height,83.2);
    }
    #[test]
    fn direct_running_split_readonly_and_vim_never_allocate_a_fake_editor() {
        let mut a=app(14.0,14.0,1612.0,976.0);
        // Both public modes are card modes now. Direct rendering is selected
        // by actual ownership/projection facts, not the legacy Normal alias.
        for direct in 0..4 {
            a.config.chat=direct%2==0; a.split=direct==0;
            a.sessions[0]=session(7,4);
            if direct==1 {a.sessions[0].projection.chat.clear();}
            a.sessions[0].read_only=direct==2;
            a.vim9_tabs.clear(); if direct==3 {a.vim9_tabs.insert(7,());}
            assert!(a.reference_input_metrics().is_none());
            let joint=a.reference_native_budget();
            assert!(joint.editor.is_none()); close(joint.input_height,0.0);
            close(a.term_height(),821.0);
            assert_eq!(joint.composer,a.reference_layout().composer);
        }
    }
    #[test]
    fn prompt_hold_keeps_budget_until_matching_expiry_and_never_crosses_owners() {
        let mut a=app(14.0,14.0,1612.0,976.0);
        a.sessions[0]=session(7,2);
        assert_eq!(a.sync_input_dock_hold(),None);
        let prompt=a.reference_input_metrics().unwrap();
        let height=a.term_height();
        a.sessions[0]=session(7,0);
        let generation=a.sync_input_dock_hold().unwrap();
        assert_eq!(a.reference_input_metrics(),Some(prompt)); close(a.term_height(),height);
        a.expire_input_dock_hold(generation+1);
        assert_eq!(a.reference_input_metrics(),Some(prompt));
        a.sessions.push(session(9,0)); a.active=1;
        assert!(a.reference_input_metrics().is_none());
        a.active=0; a.sessions[0].terminal.alternate_screen=true;
        assert!(a.reference_input_metrics().is_none());
        a.sessions[0].terminal.alternate_screen=false;
        a.expire_input_dock_hold(generation);
        assert!(a.reference_input_metrics().is_none());
        close(a.term_height(),821.0);
    }
    #[test]
    fn live_owner_activation_recomputes_dimensions_but_retired_or_unallocated_do_not() {
        let mut a=app(14.0,14.0,1612.0,976.0); a.config.chat=true;
        a.sessions[0]=session(7,1);
        a.sessions.push(session(9,8));
        assert!(a.refresh_glass_grid_dimensions());
        let first=(a.cols,a.rows);
        assert!(!a.refresh_glass_grid_dimensions(),"identical dimensions must not trigger another resize");
        a.active=1;
        assert!(a.refresh_glass_grid_dimensions(),"new owner's long draft needs its own budget");
        let second=(a.cols,a.rows); assert!(second.1<first.1);
        a.active=0;
        assert!(a.refresh_glass_grid_dimensions()); assert_eq!((a.cols,a.rows),first);
        for invalid in 0..5 {
            a.active=1; a.glass_geometry_ready=invalid!=0;
            a.sessions[1].read_only=invalid==1;
            a.vim9_tabs.clear(); if invalid==2 {a.vim9_tabs.insert(9,());}
            a.win_size.width=if invalid==3 {f32::NAN}else{1612.0};
            if invalid==4 {a.active=99;}
            let unchanged=(a.cols,a.rows);
            assert!(!a.refresh_glass_grid_dimensions());
            assert_eq!((a.cols,a.rows),unchanged);
        }
    }
    #[test]
    fn actual_toolbar_width_fits_current_tools_and_compacts_narrow_windows() {
        let standard=app(14.0,14.0,1612.0,976.0).reference_layout();
        close(standard.composer.width,936.0);
        let narrow=app(14.0,14.0,800.0,640.0).reference_layout();
        close(narrow.composer.width,414.0);
        let compact=app(14.0,14.0,320.0,640.0).reference_layout();
        close(compact.composer.width,214.0);
        // Fixed-size vector buttons remain clickable at maximum text zoom.
        // Only Terminal and Tools labels depend on font scale. The source
        // checker binds every equation to the production toolbar above.
        for scale in [1.0,2.0,3.0,72.0/14.0] {
            let square=30.0;
            let gap=toolbar::METADATA_GAP+2.0;
            let trailing=3.0*square+8.0+2.0*gap;
            let fixed=toolbar::COMPOSER_CONTROL_LEFT+toolbar::COMPOSER_CONTROL_RIGHT
                +trailing+toolbar::METADATA_GAP;
            let terminal_width=84.0*scale;
            let tools_width=82.0*scale;
            let terminal_threshold=fixed+terminal_width+gap+square;
            for width in [fixed+square,terminal_threshold-1.0,terminal_threshold,
                          terminal_threshold+1.0,compact.composer.width,
                          narrow.composer.width,standard.composer.width] {
                let terminal=width>=terminal_threshold;
                let tools_threshold=fixed+tools_width+if terminal{terminal_width+gap}else{0.0};
                let icon_only=width<tools_threshold;
                let tools=if icon_only{square}else{tools_width};
                let required=fixed+tools+if terminal{terminal_width+gap}else{0.0};
                assert!(required<=width,"reachable controls width={width} scale={scale}: {required}");
                if width==terminal_threshold {assert!(terminal);}
                if width==terminal_threshold-1.0 {assert!(!terminal);}
            }
        }
    }
}
