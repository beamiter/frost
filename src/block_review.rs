//! Bounded, pane-local reading history. IDs are terminal identities, never indices.
use jterm_engine::archived_text::ArchiveAnnotations;

const PREVIEW_CLIPPED: &str = "\n[Preview clipped; copy/export retains the available capture]";

#[derive(Debug, Default)]
pub struct ReadingHistory {
    visits: Vec<u64>,
    cursor: usize,
    pub seen_through: Option<u64>,
}

impl ReadingHistory {
    pub fn visit(&mut self, id: u64) {
        if self.visits.get(self.cursor) == Some(&id) {
            return;
        }
        self.visits.truncate(self.cursor.saturating_add(1));
        self.visits.push(id);
        if self.visits.len() > 64 {
            self.visits.remove(0);
        }
        self.cursor = self.visits.len() - 1;
    }

    pub fn can_navigate(&self, live: &[u64], older: bool) -> bool {
        self.next_index(live, older).is_some()
    }

    fn next_index(&self, live: &[u64], older: bool) -> Option<usize> {
        // Keep visit positions (including repeated IDs) while skipping evicted
        // targets. Pruning first would move the cursor or skip its nearest neighbor.
        if older {
            (0..self.cursor)
                .rev()
                .find(|index| live.contains(&self.visits[*index]))
        } else {
            (self.cursor.saturating_add(1)..self.visits.len())
                .find(|index| live.contains(&self.visits[*index]))
        }
    }

    pub fn navigate(&mut self, live: &[u64], older: bool) -> Option<u64> {
        let next = self.next_index(live, older)?;
        self.cursor = next;
        Some(self.visits[next])
    }

    pub fn unseen(&self, live: impl Iterator<Item = u64>) -> usize {
        live.filter(|id| self.seen_through.is_none_or(|seen| *id > seen))
            .count()
    }
}

/// Bound UTF-8 review text without splitting a scalar; display-only clipping
/// never changes the exact command payload validated at the insert boundary.
pub fn preview(value: &str, limit: usize) -> String {
    let mut text = String::with_capacity(value.len().min(limit));
    let mut clipped = false;
    for character in value.chars() {
        if character == '\n'
            || (!character.is_control()
                && !jterm_core::review_input::is_terminal_visual_spoofing_character(character))
        {
            if character.len_utf8() > limit.saturating_sub(text.len()) {
                clipped = true;
                break;
            }
            text.push(character);
        } else {
            let shown = crate::review_text::visible_bounded(&character.to_string(), 16);
            if shown.len() > limit.saturating_sub(text.len()) {
                clipped = true;
                break;
            }
            text.push_str(&shown);
        }
    }
    if clipped {
        text.push_str(PREVIEW_CLIPPED);
    }
    text
}

pub fn preview_annotations(
    value: &str,
    preview: &str,
    annotations: &ArchiveAnnotations,
) -> Option<ArchiveAnnotations> {
    let prefix = if value.starts_with(preview) {
        preview
    } else {
        let prefix = preview.strip_suffix(PREVIEW_CLIPPED)?;
        value.starts_with(prefix).then_some(prefix)?
    };
    let mut projected = annotations.clone();
    projected.retain_head_tail(prefix.len(), 0..0, 0);
    (!projected.is_empty() && projected.valid_for(preview)).then_some(projected)
}

pub fn styled_spans<'a>(
    text: &'a str,
    annotations: &ArchiveAnnotations,
    theme: &crate::theme::Theme,
) -> Vec<iced::widget::text::Span<'a>> {
    use crate::color::{resolve_bg_with_palette, resolve_fg_with_palette};
    use iced::{
        font::{Style, Weight},
        widget::text::Span,
        Font,
    };
    use jterm_engine::{Color, UnderlineStyle};

    let mut spans = Vec::new();
    let mut offset = 0;
    for span in &annotations.spans {
        spans.push(Span::new(&text[offset..span.bytes.start]));
        let style = span.style;
        let flags = style.flags;
        let mut foreground =
            resolve_fg_with_palette(style.foreground, theme, None, flags.bold(), flags.dim());
        let mut background = resolve_bg_with_palette(style.background, theme, None);
        if flags.inverse() {
            std::mem::swap(&mut foreground, &mut background);
        }
        let font = Font {
            weight: if flags.bold() {
                Weight::Bold
            } else {
                Weight::Normal
            },
            style: if flags.italic() {
                Style::Italic
            } else {
                Style::Normal
            },
            ..Font::MONOSPACE
        };
        spans.push(
            Span::new(&text[span.bytes.clone()])
                .font(font)
                .color(foreground)
                .background_maybe(
                    (flags.inverse() || style.background != Color::Default).then_some(background),
                )
                .underline(flags.underline() != UnderlineStyle::None)
                .strikethrough(flags.strikethrough()),
        );
        offset = span.bytes.end;
    }
    spans.push(Span::new(&text[offset..]));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_branches_and_is_bounded() {
        let mut h = ReadingHistory::default();
        for id in 0..100 {
            h.visit(id);
        }
        assert_eq!(h.visits.len(), 64);
        assert_eq!(h.navigate(&(0..100).collect::<Vec<_>>(), true), Some(98));
        h.visit(50);
        assert_eq!(h.navigate(&(0..100).collect::<Vec<_>>(), false), None);
        assert_eq!(h.navigate(&[50, 97], true), Some(97));
    }
    #[test]
    fn eviction_never_returns_stale_ids() {
        let mut h = ReadingHistory::default();
        h.visit(1);
        h.visit(2);
        assert_eq!(h.navigate(&[3], true), None);
        h.visit(3);
        assert_eq!(h.navigate(&[3], true), None);
        h.visit(4);
        h.visit(5);
        assert_eq!(h.navigate(&[3, 5], true), Some(3));
        h.seen_through = Some(3);
        assert_eq!(h.unseen([2, 3, 4, 5].into_iter()), 2);
    }
    #[test]
    fn navigation_availability_tracks_boundaries_without_moving_cursor() {
        let mut h = ReadingHistory::default();
        assert!(!h.can_navigate(&[], true));
        assert!(!h.can_navigate(&[], false));
        h.visit(10);
        h.visit(20);
        h.visit(30);
        assert!(h.can_navigate(&[10, 30], true));
        assert!(!h.can_navigate(&[10, 30], false));
        assert_eq!(h.navigate(&[10, 30], true), Some(10));
        assert!(!h.can_navigate(&[10, 30], true));
        assert!(h.can_navigate(&[10, 30], false));
        assert_eq!(h.navigate(&[10, 30], false), Some(30));
        assert!(!h.can_navigate(&[30], true));
    }

    #[test]
    fn unicode_preview_is_bounded_and_explicit() {
        let s = preview("你好世界", 7);
        assert!(s.starts_with("你好"));
        assert!(s.contains("Preview clipped"));
        assert!(!preview("abc", 3).contains("clipped"));
        assert_eq!(preview("a\nb\tc", 20), "a\nb\\tc");
        assert!(preview(&"x".repeat(16000), 20000).len() == 16000);
    }
}
