//! Exercise the exact shaper shared with Iced, not a substitute width model.
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

#[test]
fn measurement_and_iced_use_identical_buffer_types() {
    fn renderer_buffer(buffer: &iced::advanced::graphics::text::cosmic_text::Buffer) -> &Buffer {
        buffer
    }
    let mut fonts = FontSystem::new();
    let buffer = Buffer::new(&mut fonts, Metrics::new(16.0, 20.0));
    assert!(std::ptr::eq(&buffer, renderer_buffer(&buffer)));
}

#[test]
fn unicode_layout_scales_without_changing_cluster_boundaries() {
    let mut fonts = FontSystem::new();
    assert!(
        fonts.db().faces().next().is_some(),
        "install fonts-dejavu-core and fonts-noto-cjk"
    );
    for text in [
        "ASCII grid",
        "中文界Ａ",
        "e\u{301}o\u{308}",
        "A界e\u{301}Ｂ",
    ] {
        for font_size in [12.0, 20.0, 32.0] {
            let mut reference = None;
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let size = font_size * scale;
                let mut buffer = Buffer::new(&mut fonts, Metrics::new(size, size * 1.2));
                buffer.set_size(Some(4096.0), Some(size * 2.0));
                buffer.set_text(
                    text,
                    &Attrs::new().family(Family::Monospace),
                    Shaping::Advanced,
                    None,
                );
                buffer.shape_until_scroll(&mut fonts, false);
                let runs: Vec<_> = buffer.layout_runs().collect();
                assert_eq!(runs.len(), 1, "{text:?} at {size}");
                let glyphs: Vec<_> = runs[0]
                    .glyphs
                    .iter()
                    .map(|g| {
                        assert_ne!(
                            g.glyph_id, 0,
                            "missing glyph for {text:?}; install fonts-noto-cjk"
                        );
                        assert!(text.is_char_boundary(g.start) && text.is_char_boundary(g.end));
                        (g.start, g.end, g.x / scale, g.w / scale)
                    })
                    .collect();
                if let Some(ref baseline) = reference {
                    let baseline: &Vec<(usize, usize, f32, f32)> = baseline;
                    assert_eq!(baseline.len(), glyphs.len());
                    for (a, b) in baseline.iter().zip(&glyphs) {
                        assert_eq!((a.0, a.1), (b.0, b.1));
                        assert!((a.2 - b.2).abs() < 0.02 && (a.3 - b.3).abs() < 0.02);
                    }
                } else {
                    reference = Some(glyphs);
                }
            }
        }
    }
}

#[test]
fn dirs_seven_retains_path_api_types() {
    let _: [fn() -> Option<std::path::PathBuf>; 5] = [
        dirs::home_dir,
        dirs::config_dir,
        dirs::data_dir,
        dirs::data_local_dir,
        dirs::state_dir,
    ];
}

#[test]
fn iced_cache_and_paragraph_flush_lazy_layout_before_reading() {
    use iced::advanced::graphics::text::{cache, Cache, Paragraph};
    use iced::advanced::text::{self, Paragraph as _, Text};
    use iced::{alignment, Font, Pixels, Size};
    let mut fonts = FontSystem::new();
    let mut cache = Cache::new();
    for size in [12.0, 20.0, 32.0] {
        let (_, entry) = cache.allocate(
            &mut fonts,
            cache::Key {
                content: "界 e\u{301} Ａ",
                size,
                line_height: size * 1.2,
                font: Font::MONOSPACE,
                bounds: Size::new(600.0, 100.0),
                shaping: text::Shaping::Advanced,
                align_x: text::Alignment::Left,
            },
        );
        assert!(entry.min_bounds.width > 0.0);
        assert_eq!(entry.buffer.layout_runs().count(), 1);
        let mut paragraph = Paragraph::with_text(Text {
            content: "hello world hello world",
            bounds: Size::new(600.0, 200.0),
            size: Pixels(size),
            line_height: text::LineHeight::Absolute(Pixels(size * 1.2)),
            font: Font::MONOSPACE,
            align_x: text::Alignment::Center,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::Word,
        });
        assert_eq!(paragraph.buffer().layout_runs().count(), 1);
        paragraph.resize(Size::new(70.0, 500.0));
        assert!(paragraph.buffer().layout_runs().count() > 1);
        assert!(paragraph.min_bounds().width > 0.0);
        paragraph.resize(Size::new(600.0, 200.0));
        assert_eq!(paragraph.buffer().layout_runs().count(), 1);
    }
}
