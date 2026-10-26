//! Session-local motion and exact pixel geometry for native transcripts.
//!
//! The terminal owns row provenance and reading anchors. This small adapter
//! keeps the fractional part between those rows and never feeds it to a PTY.

use jterm_core::transcript_canvas::CanvasMotion;
use std::time::Instant;

// A stable logical-pixel scale keeps motion tolerance independent of font zoom.
const ROW: f64 = 24.0;

#[derive(Debug, Default)]
pub struct TranscriptCanvas {
    motion: CanvasMotion,
    enabled: bool,
    parked: bool,
    following: bool,
    applied_row: usize,
    extent: usize,
    viewport_rows: usize,
    frame: Option<Instant>,
}

impl TranscriptCanvas {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn following(&self) -> bool {
        self.enabled && self.following
    }
    pub fn animating(&self) -> bool {
        self.enabled && self.motion.is_animating()
    }
    pub fn row(&self) -> usize {
        (self.motion.position() / ROW).floor().max(0.0) as usize
    }
    pub fn fraction(&self) -> f32 {
        ((self.motion.position() / ROW).fract()) as f32
    }
    pub fn position_rows(&self) -> f64 {
        self.motion.position() / ROW
    }

    pub fn disable(&mut self) {
        self.motion.cancel();
        self.enabled = false;
        self.parked = false;
        self.frame = None;
    }

    /// OSC completion and the next prompt can arrive in different reads. The
    /// native interval still owns input; park only the reader's sub-row offset.
    pub fn park(&mut self) {
        if self.enabled {
            self.motion.cancel();
            self.parked = true;
            self.enabled = false;
            self.frame = None;
        }
    }

    /// `anchored_row` comes from the real raw/synthetic document anchor. When
    /// output grows above a reader, both presented position and destination
    /// move by that exact amount; the fractional screen position stays fixed.
    pub fn reconcile(&mut self, anchored_row: usize, extent: usize, viewport_rows: usize) {
        let reset = !self.enabled || self.viewport_rows != viewport_rows;
        // Keep the old coordinate available until the anchor rebase has been
        // applied. Clamping a trimmed document first would lose its fraction.
        self.motion
            .set_bounds(0.0, extent.max(self.extent) as f64 * ROW);
        if reset {
            // Resize and parked prompt transitions retain the reader's exact
            // sub-row anchor. A zero whole-row anchor is not proof that the
            // reader wants to follow: they may be only a fraction above live.
            let retain_owner = self.enabled || self.parked;
            let retain_fraction = retain_owner && !self.following;
            let fraction = if retain_fraction {
                f64::from(self.fraction())
            } else {
                0.0
            };
            let position = if retain_owner && self.following {
                0.0
            } else {
                anchored_row as f64 + fraction
            };
            self.motion.jump_to(position * ROW);
            if !retain_owner {
                self.following = anchored_row == 0;
            }
        } else if self.following {
            let growth = extent as f64 - self.extent as f64;
            // A burst of thousands of lines must not replay hidden output.
            // Two visible screens are sufficient to retain a spatial origin.
            self.motion.shift(growth * ROW);
            let limit = (viewport_rows.saturating_mul(2).max(1) as f64) * ROW;
            if self.motion.position() > limit {
                self.motion.jump_to(limit);
            }
            self.motion.set_target(0.0);
        } else {
            self.motion
                .shift((anchored_row as f64 - self.applied_row as f64) * ROW);
        }
        self.motion.set_bounds(0.0, extent as f64 * ROW);
        self.extent = extent;
        self.viewport_rows = viewport_rows;
        self.enabled = true;
        self.parked = false;
        if self.motion.is_animating() && self.frame.is_none() {
            self.frame = Some(Instant::now());
        }
    }

    pub fn applied(&mut self, row: usize) {
        self.applied_row = row;
    }

    /// Mouse wheels provide notches; touchpads provide already integrated
    /// physical movement. Never add a second inertia filter to the latter.
    pub fn scroll(&mut self, delta_rows: f64, precise: bool, now: Instant) -> bool {
        if !self.enabled || !delta_rows.is_finite() {
            return false;
        }
        let before = self.row();
        if precise {
            self.motion
                .jump_to(self.motion.position() + delta_rows * ROW);
        } else {
            self.motion.scroll_by(delta_rows * ROW);
        }
        self.following = self.motion.target() <= 0.0;
        self.frame = Some(now);
        before != self.row()
    }

    pub fn advance(&mut self, now: Instant) -> bool {
        if !self.animating() {
            self.frame = None;
            return false;
        }
        let before = self.row();
        // A presented-frame subscription can still deliver an event queued
        // before the latest wheel input. Never move the integration clock
        // backwards: the next frame must include only time after that input.
        let Some(previous) = self.frame else {
            self.frame = Some(now);
            return false;
        };
        if now <= previous {
            return false;
        }
        let dt = now.duration_since(previous).as_secs_f64();
        self.frame = Some(now);
        self.motion.advance(dt);
        if !self.motion.is_animating() {
            self.frame = None;
        }
        before != self.row()
    }

    pub fn cancel(&mut self) {
        self.motion.cancel();
        self.following = false;
        self.frame = None;
    }

    pub fn set_reduced_motion(&mut self, reduced: bool) -> bool {
        let before = self.row();
        self.motion.set_reduced_motion(reduced);
        before != self.row()
    }

    pub fn follow_live(&mut self, now: Instant) {
        self.following = true;
        self.motion.set_target(0.0);
        self.frame = self.motion.is_animating().then_some(now);
    }

    /// Explicit navigation, editing and scrollbar grabs are immediate. Their
    /// destination must not inherit an older wheel or live-follow trajectory.
    pub fn navigate(&mut self, row: usize) {
        self.parked = false;
        self.motion.jump_to(row as f64 * ROW);
        self.following = row == 0;
        self.applied_row = row;
        self.frame = None;
    }
}

/// The same affine map drives glyphs, cards, cursor, IME and pointer mapping.
/// `dock_start` is in the materialized array (including the extra head row).
#[derive(Clone, Copy, Debug, Default)]
pub struct CanvasGeometry {
    pub head: bool,
    pub fraction: f32,
    pub dock_start: Option<usize>,
}

impl CanvasGeometry {
    pub fn offset(self, row: usize, cell_h: f32) -> f32 {
        if !self.head {
            return 0.0;
        }
        if self.dock_start.is_some_and(|start| row >= start) {
            -cell_h
        } else {
            (self.fraction - 1.0) * cell_h
        }
    }

    pub fn y(self, origin: f32, row: usize, cell_h: f32) -> f32 {
        origin + row as f32 * cell_h + self.offset(row, cell_h)
    }

    pub fn row(self, relative_y: f32, cell_h: f32, rows: usize) -> usize {
        let cell_h = cell_h.max(1.0);
        let dock = self
            .dock_start
            .filter(|start| self.head && relative_y >= start.saturating_sub(1) as f32 * cell_h);
        let shift = dock.map_or_else(|| self.offset(0, cell_h), |_| -cell_h);
        ((relative_y - shift).max(0.0) / cell_h)
            .floor()
            .min(rows.saturating_sub(1) as f32) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn touchpad_retains_every_fraction_without_a_timer() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(0, 100, 25);
        canvas.scroll(0.125, true, Instant::now());
        assert_eq!(canvas.row(), 0);
        assert_eq!(canvas.fraction(), 0.125);
        assert!(!canvas.following());
        assert!(!canvas.animating());
    }

    #[test]
    fn resize_preserves_subrow_reader_without_reenabling_follow() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(0, 100, 25);
        canvas.scroll(0.375, true, Instant::now());
        canvas.applied(0);
        canvas.reconcile(0, 95, 30);
        assert_eq!(canvas.position_rows(), 0.375);
        assert!(!canvas.following());
        assert!(!canvas.animating());
        canvas.reconcile(5, 100, 30);
        assert_eq!(canvas.position_rows(), 5.375);
    }

    #[test]
    fn resize_rebases_reader_and_retires_old_wheel_destination() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(7, 100, 25);
        canvas.scroll(0.375, true, Instant::now());
        canvas.scroll(3.0, false, Instant::now());
        assert!(canvas.animating());
        canvas.reconcile(10, 103, 30);
        assert_eq!(canvas.position_rows(), 10.375);
        assert!(!canvas.animating());
        assert!(!canvas.following());
    }

    #[test]
    fn resize_keeps_live_follow_ownership_during_streaming_animation() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(0, 100, 25);
        canvas.reconcile(0, 105, 25);
        assert!(canvas.animating());
        canvas.reconcile(5, 100, 30);
        assert_eq!(canvas.position_rows(), 0.0);
        assert!(canvas.following());
        assert!(!canvas.animating());
    }

    #[test]
    fn streamed_output_preserves_readers_fraction_and_pending_destination() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(4, 100, 25);
        canvas.applied(4);
        canvas.scroll(0.375, true, Instant::now());
        canvas.reconcile(11, 107, 25);
        assert_eq!(canvas.position_rows(), 11.375);
        assert!(!canvas.animating());
    }

    #[test]
    fn live_output_moves_continuously_and_settles_without_idle_frames() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(0, 10, 25);
        canvas.reconcile(0, 12, 25);
        assert_eq!(canvas.position_rows(), 2.0);
        assert!(canvas.animating());
        let start = Instant::now();
        for tick in 1..=100 {
            canvas.advance(start + Duration::from_secs_f64(tick as f64 / 120.0));
        }
        assert_eq!(canvas.position_rows(), 0.0);
        assert!(!canvas.animating());
        assert!(canvas.following());
    }

    #[test]
    fn queued_frame_before_input_does_not_rewind_the_motion_clock() {
        let mut canvas = TranscriptCanvas::default();
        let mut reference = TranscriptCanvas::default();
        let now = Instant::now();
        for motion in [&mut canvas, &mut reference] {
            motion.reconcile(0, 100, 25);
            motion.scroll(3.0, false, now);
        }
        assert!(!canvas.advance(now - Duration::from_millis(40)));
        assert_eq!(canvas.position_rows(), 0.0);
        canvas.advance(now + Duration::from_millis(16));
        reference.advance(now + Duration::from_millis(16));
        assert_eq!(canvas.position_rows(), reference.position_rows());
        assert!(canvas.animating());
    }

    #[test]
    fn wheel_reversal_and_explicit_navigation_retire_old_motion() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(20, 100, 25);
        canvas.applied(20);
        let now = Instant::now();
        canvas.scroll(6.0, false, now);
        canvas.advance(now + Duration::from_millis(16));
        let before = canvas.position_rows();
        canvas.scroll(-3.0, false, now + Duration::from_millis(16));
        canvas.advance(now + Duration::from_millis(32));
        assert!(canvas.position_rows() < before);
        canvas.navigate(0);
        assert!(!canvas.animating());
        assert!(canvas.following());
    }

    #[test]
    fn trimmed_history_rebases_before_clamping_and_preserves_fraction() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(99, 100, 25);
        canvas.applied(99);
        canvas.scroll(0.375, true, Instant::now());
        canvas.reconcile(89, 90, 25);
        assert_eq!(canvas.position_rows(), 89.375);
    }

    #[test]
    fn return_to_live_is_animated_unless_desktop_requests_reduced_motion() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(20, 100, 25);
        canvas.follow_live(Instant::now());
        assert!(canvas.following());
        assert!(canvas.animating());
        assert_eq!(canvas.position_rows(), 20.0);
        assert!(canvas.set_reduced_motion(true));
        assert_eq!(canvas.position_rows(), 0.0);
        assert!(!canvas.animating());
        canvas.scroll(0.125, true, Instant::now());
        assert_eq!(canvas.fraction(), 0.125);
    }

    #[test]
    fn between_prompt_reads_park_fraction_without_replaying_motion() {
        let mut canvas = TranscriptCanvas::default();
        canvas.reconcile(7, 100, 25);
        canvas.applied(7);
        canvas.scroll(0.375, true, Instant::now());
        canvas.park();
        assert!(!canvas.enabled());
        assert!(!canvas.animating());
        canvas.reconcile(10, 103, 25);
        assert_eq!(canvas.position_rows(), 10.375);
        assert!(!canvas.animating());
        assert!(!canvas.following());
    }

    #[test]
    fn geometry_keeps_dock_fixed_and_hit_testing_matches_fractional_ink() {
        for fraction in [0.0, 0.125, 0.5, 0.999] {
            let geometry = CanvasGeometry {
                head: true,
                fraction,
                dock_start: Some(8),
            };
            for row in (1..7).chain(8..11) {
                let y = geometry.y(6.0, row, 21.5);
                assert_eq!(geometry.row(y - 6.0 + 1.0, 21.5, 11), row);
            }
            assert_eq!(geometry.y(6.0, 8, 21.5), 6.0 + 7.0 * 21.5);
        }
    }
}
