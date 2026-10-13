//! Content-free native geometry and physical pointer edges only.
//! Behavior, dwell, cooldown, tickets, physiology and watch live in jterm_core.

/// Stable envelope for the shared ASCII grammar, independent of animation.
/// The renderer rejects any future shared sprite that exceeds this envelope.
pub(crate) const BODY_COLS: usize = 12;
pub(crate) const BODY_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub(crate) fn contains(self, point: (f32, f32)) -> bool {
        point.0 >= self.x
            && point.0 < self.x + self.width
            && point.1 >= self.y
            && point.1 < self.y + self.height
    }

    fn includes(self, other: Self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            other.x,
            other.y,
            other.width,
            other.height,
        ]
        .into_iter()
        .all(f32::is_finite)
            && self.width > 0.0
            && self.height > 0.0
            && other.width > 0.0
            && other.height > 0.0
            && other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Surface {
    pub card: Rect,
    pub clip: Rect,
    pub grid_origin: (f32, f32),
    pub cell: (f32, f32),
    pub start_row: usize,
    pub end_row: usize,
    pub cursor_row: usize,
    pub last_occupied_row: Option<usize>,
    pub columns: usize,
    pub complete: bool,
}

/// Find a suffix of the actual active card after all occupied/cursor rows.
/// Never extend a card, clamp to the viewport, or infer emptiness from text.
pub(crate) fn placement(surface: Surface) -> Option<Rect> {
    let Surface {
        card,
        clip,
        grid_origin: (x, y),
        cell: (cw, ch),
        start_row,
        end_row,
        cursor_row,
        last_occupied_row,
        columns,
        complete,
    } = surface;
    if !complete
        || !clip.includes(card)
        || !cw.is_finite()
        || !ch.is_finite()
        || cw <= 0.0
        || ch <= 0.0
        || cursor_row < start_row
        || cursor_row >= end_row
        || columns < BODY_COLS + 2
    {
        return None;
    }
    let row = cursor_row
        .checked_add(2)?
        .max(start_row.checked_add(1)?)
        .max(last_occupied_row.map_or(Some(start_row + 1), |r| r.checked_add(2))?);
    if row.checked_add(BODY_ROWS)? >= end_row {
        return None;
    }
    // One native cell margin, further bounded by the actual card inset.
    let right = (x + (columns - 1) as f32 * cw).min(card.x + card.width - cw);
    let rect = Rect {
        x: right - BODY_COLS as f32 * cw,
        y: y + row as f32 * ch,
        width: BODY_COLS as f32 * cw,
        height: BODY_ROWS as f32 * ch,
    };
    (rect.x >= x + cw && card.includes(rect) && clip.includes(rect)).then_some(rect)
}

pub(crate) fn sprite_fits(sprite: &str) -> bool {
    sprite.is_ascii()
        && sprite.lines().count() == BODY_ROWS
        && sprite.lines().all(|line| line.len() <= BODY_COLS)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Geometry,
    Enter,
    Exit,
}

/// Native observation bookkeeping, not another attention controller. Position
/// synchronization seeds the inside bit but can never start a physical visit.
#[derive(Debug)]
pub(crate) struct Pointer<K> {
    target: Option<K>,
    point: Option<(f32, f32)>,
    inside: bool,
}

impl<K> Default for Pointer<K> {
    fn default() -> Self {
        Self {
            target: None,
            point: None,
            inside: false,
        }
    }
}

impl<K: PartialEq + Clone> Pointer<K> {
    pub(crate) fn observe(
        &mut self,
        target: Option<K>,
        point: Option<(f32, f32)>,
        inside: bool,
        moved: bool,
    ) -> Option<Edge> {
        let changed = self.target != target;
        let real_motion = moved && self.point != point;
        let was_inside = self.inside;
        self.target = target;
        self.point = point;
        self.inside = inside;
        if changed {
            Some(Edge::Geometry)
        } else if real_motion && was_inside != inside {
            Some(if inside { Edge::Enter } else { Edge::Exit })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn surface() -> Surface {
        Surface {
            card: Rect {
                x: 4.0,
                y: 1.0,
                width: 792.0,
                height: 118.0,
            },
            clip: Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 400.0,
            },
            grid_origin: (8.0, 0.0),
            cell: (8.0, 20.0),
            start_row: 0,
            end_row: 6,
            cursor_row: 0,
            last_occupied_row: Some(0),
            columns: 98,
            complete: true,
        }
    }
    #[test]
    fn empty_input_uses_real_six_row_card_without_resizing_it() {
        let s = surface();
        let r = placement(s).unwrap();
        assert_eq!((r.y, r.width, r.height), (40.0, 96.0, 60.0));
        assert!(s.card.includes(r));
        assert!(r.y > s.cursor_row as f32 * s.cell.1);
    }
    #[test]
    fn multiline_content_and_cursor_leave_no_room_instead_of_overlaying_text() {
        for last in 1..6 {
            let mut s = surface();
            s.last_occupied_row = Some(last);
            assert!(placement(s).is_none());
        }
        let mut s = surface();
        s.cursor_row = 4;
        assert!(placement(s).is_none());
    }
    #[test]
    fn narrow_and_scaled_font_use_native_pixels_and_fail_closed() {
        let mut s = surface();
        s.columns = 13;
        assert!(placement(s).is_none());
        for scale in [0.75, 1.0, 1.5, 2.0] {
            let mut s = surface();
            s.cell.0 *= scale;
            s.cell.1 *= scale;
            s.card.width *= scale;
            s.card.height *= scale;
            s.clip.width *= scale;
            s.clip.height *= scale;
            assert_eq!(placement(s).unwrap().height, 60.0 * scale);
        }
        let mut s = surface();
        s.cell.0 = f32::NAN;
        assert!(placement(s).is_none());
    }
    #[test]
    fn clipped_or_scrolled_card_never_sticks_to_viewport_bottom() {
        let mut s = surface();
        s.complete = false;
        assert!(placement(s).is_none());
        s = surface();
        s.clip.height = 110.0;
        assert!(placement(s).is_none());
        s = surface();
        s.card.y = -1.0;
        assert!(placement(s).is_none());
    }
    #[test]
    fn layout_and_owner_changes_cannot_manufacture_pointer_entry() {
        let mut p = Pointer::default();
        assert_eq!(
            p.observe(Some(1), Some((1.0, 1.0)), true, false),
            Some(Edge::Geometry)
        );
        assert_eq!(p.observe(Some(1), Some((1.0, 1.0)), true, true), None);
        assert_eq!(
            p.observe(Some(2), Some((1.0, 1.0)), true, true),
            Some(Edge::Geometry)
        );
        assert_eq!(p.observe(Some(2), Some((2.0, 1.0)), true, true), None);
        assert_eq!(
            p.observe(Some(2), Some((9.0, 1.0)), false, true),
            Some(Edge::Exit)
        );
        assert_eq!(
            p.observe(Some(2), Some((1.0, 1.0)), true, true),
            Some(Edge::Enter)
        );
    }
    #[test]
    fn typing_hides_and_restore_seeds_geometry_without_queued_greeting() {
        let mut p = Pointer::default();
        p.observe(Some(1), Some((1.0, 1.0)), true, false);
        assert_eq!(
            p.observe(None, Some((1.0, 1.0)), false, false),
            Some(Edge::Geometry)
        );
        assert_eq!(
            p.observe(Some(2), Some((1.0, 1.0)), true, false),
            Some(Edge::Geometry)
        );
        assert_eq!(p.observe(Some(2), Some((1.0, 1.0)), true, true), None);
    }
    #[test]
    fn future_shared_sprite_growth_is_fail_closed() {
        assert!(sprite_fits(" /\\_/\\\n( o.o )\n > ^ <"));
        assert!(!sprite_fits("1234567890123\ncat\ncat"));
        assert!(!sprite_fits("cat\ncat\ncat\ncat"));
    }
}
