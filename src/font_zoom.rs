//! Two persistent font groups, with independent high-resolution wheel remainder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Chrome,
    Content,
}

#[derive(Default)]
pub(crate) struct WheelZoom {
    remainder: [f32; 2],
}

impl WheelZoom {
    pub(crate) fn clear(&mut self) {
        self.remainder = [0.0; 2];
    }

    /// A fixed pixel threshold keeps trackpad sensitivity independent of zoom.
    /// Reversals discard the old direction's fractional remainder.
    pub(crate) fn steps(&mut self, group: Group, delta: f32, pixels: bool) -> f32 {
        if !delta.is_finite() || delta == 0.0 {
            return 0.0;
        }
        let remainder = &mut self.remainder[match group {
            Group::Chrome => 0,
            Group::Content => 1,
        }];
        let lines = (if pixels { delta / 40.0 } else { delta }).clamp(-64.0, 64.0);
        if remainder.signum() != lines.signum() {
            *remainder = 0.0;
        }
        *remainder += lines;
        let whole = remainder.trunc();
        *remainder -= whole;
        whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_lines_and_pixels_are_isolated_between_groups() {
        let mut zoom = WheelZoom::default();
        assert_eq!(zoom.steps(Group::Chrome, 20.0, true), 0.0);
        assert_eq!(zoom.steps(Group::Content, 0.75, false), 0.0);
        assert_eq!(zoom.steps(Group::Chrome, 20.0, true), 1.0);
        assert_eq!(zoom.steps(Group::Content, 0.25, false), 1.0);
    }

    #[test]
    fn reversal_release_and_invalid_deltas_do_not_leak_remainders() {
        let mut zoom = WheelZoom::default();
        assert_eq!(zoom.steps(Group::Content, 0.75, false), 0.0);
        assert_eq!(zoom.steps(Group::Content, -0.5, false), 0.0);
        assert_eq!(zoom.steps(Group::Content, -0.5, false), -1.0);
        assert_eq!(zoom.steps(Group::Chrome, f32::NAN, false), 0.0);
        assert_eq!(zoom.steps(Group::Chrome, f32::INFINITY, true), 0.0);
        assert_eq!(zoom.steps(Group::Chrome, 30.0, true), 0.0);
        zoom.clear();
        assert_eq!(zoom.steps(Group::Chrome, 10.0, true), 0.0);
        assert_eq!(zoom.steps(Group::Content, f32::MAX, false), 64.0);
    }
}
