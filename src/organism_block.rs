//! Content-free native geometry and physical pointer edges only.
//! Behavior, dwell, cooldown, tickets, physiology and watch live in jterm_core.

/// Stable envelope for the shared ASCII grammar, independent of animation.
/// The renderer rejects any future shared sprite that exceeds this envelope.
pub(crate) const BODY_COLS: usize = 12;
pub(crate) const BODY_ROWS: usize = 3;
pub(crate) const BODY_PAD_X: f32 = 6.0;
pub(crate) const BODY_PAD_Y: f32 = 3.0;
pub(crate) const BODY_BORDER: f32 = 1.0;

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

    pub(crate) fn includes(self, other: Self) -> bool {
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
pub(crate) fn travel_band(surface: Surface) -> Option<Rect> {
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
        .checked_add(1)?
        .max(start_row.checked_add(1)?)
        .max(last_occupied_row.map_or(Some(start_row + 1), |r| r.checked_add(1))?);
    if row.checked_add(BODY_ROWS)? >= end_row {
        return None;
    }
    let left = (x + cw).max(card.x + cw);
    let right = (x + (columns - 1) as f32 * cw).min(card.x + card.width - cw);
    let top = y + row as f32 * ch;
    let bottom = (y + end_row as f32 * ch).min(card.y + card.height - 3.0);
    let width = BODY_COLS as f32 * cw + 2.0 * (BODY_PAD_X + BODY_BORDER);
    let height = (BODY_ROWS as f32 * ch + 2.0 * (BODY_PAD_Y + BODY_BORDER))
        .max(jterm_core::reference_organism::SIZE);
    let rect = Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    };
    (rect.width >= width && rect.height >= height && card.includes(rect) && clip.includes(rect))
        .then_some(rect)
}

/// Validate the blank-space envelope once for both placement and wandering.
/// Callers never reconstruct a less restrictive travel area from the body.
pub(crate) fn layout(surface: Surface) -> Option<(Rect, Rect)> {
    let band = travel_band(surface)?;
    let width = BODY_COLS as f32 * surface.cell.0 + 2.0 * (BODY_PAD_X + BODY_BORDER);
    let height = (BODY_ROWS as f32 * surface.cell.1 + 2.0 * (BODY_PAD_Y + BODY_BORDER))
        .max(jterm_core::reference_organism::SIZE);
    let body = Rect {
        x: band.x + band.width - width,
        y: band.y,
        width,
        height,
    };
    Some((body, band))
}

#[cfg(test)]
fn placement(surface: Surface) -> Option<Rect> {
    layout(surface).map(|(body, _)| body)
}

/// Same quiet edge/pacing/edge/pacing cycle as the GTK reference, using the
/// existing 100 ms presentation clock. Reduced motion never calls this path.
pub(crate) fn wander(
    band: Rect,
    body: Rect,
    frame: u64,
    walk: Option<u64>,
    cell_width: f32,
) -> Rect {
    let Some(walk) = walk else {
        return body;
    };
    let step = wander_phase(frame, walk);
    let offset = (band.width - body.width) * step as f32 / 40.0;
    Rect {
        x: band.x + (offset / cell_width).floor() * cell_width,
        ..body
    }
}

fn wander_phase(frame: u64, walk: u64) -> u64 {
    if walk == 0 {
        return 0;
    }
    let walk = walk.min(120);
    let phase = frame % 800;
    let sit = (800 - 2 * walk) / 2;
    if phase < sit {
        0
    } else if phase < sit + walk {
        (phase - sit) * 40 / walk
    } else if phase < sit + walk + sit {
        40
    } else {
        40 - (phase - sit - walk - sit) * 40 / walk
    }
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
        assert_eq!((r.y, r.width, r.height), (20.0, 110.0, 68.0));
        assert!(s.card.includes(r));
        assert!(r.y > s.cursor_row as f32 * s.cell.1);
    }
    #[test]
    fn multiline_content_and_cursor_leave_no_room_instead_of_overlaying_text() {
        for last in 2..6 {
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
            assert_eq!(
                placement(s).unwrap().height,
                (60.0_f32 * scale + 8.0).max(jterm_core::reference_organism::SIZE)
            );
        }
        let mut s = surface();
        s.cell.0 = f32::NAN;
        assert!(placement(s).is_none());
    }
    #[test]
    fn vector_minimum_height_uses_only_real_blank_space() {
        let mut s = surface();
        s.cell.1 = 12.0;
        s.card.height = 72.0;
        assert!(
            layout(s).is_none(),
            "legacy 3-row room is insufficient for the 64px contour"
        );
        s.card.height = 100.0;
        s.end_row = 8;
        let (body, band) = layout(s).unwrap();
        assert_eq!(body.height, 64.0);
        assert!(band.includes(body) && s.card.includes(band));
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
    fn padded_body_and_reference_wander_stay_inside_proven_blank_band() {
        let surface = surface();
        let band = travel_band(surface).unwrap();
        let body = placement(surface).unwrap();
        for walk in [Some(0), Some(40), Some(120), None] {
            for frame in 0..1600 {
                let rect = wander(band, body, frame, walk, surface.cell.0);
                assert!(band.includes(rect));
                assert_eq!(rect.y, body.y);
                assert_eq!((rect.width, rect.height), (body.width, body.height));
                if walk.is_none() {
                    assert_eq!(rect, body);
                }
            }
        }
        assert_eq!(wander_phase(359, 40), 0);
        assert_eq!(wander_phase(380, 40), 20);
        assert_eq!(wander_phase(400, 40), 40);
        assert_eq!(wander_phase(780, 40), 20);
        assert_eq!(wander_phase(340, 120), 20);
        assert_eq!(wander_phase(800, 120), 0);
        assert_eq!(wander_phase(500, 0), 0);
    }

    #[test]
    fn future_shared_sprite_growth_is_fail_closed() {
        assert!(sprite_fits(" /\\_/\\\n( o.o )\n > ^ <"));
        assert!(!sprite_fits("1234567890123\ncat\ncat"));
        assert!(!sprite_fits("cat\ncat\ncat\ncat"));
    }
}
