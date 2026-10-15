//! Text shares the configured terminal size. Reserve enough chrome for Iced's
//! default 1.3 line height plus each row's vertical padding; do not scale sprites.
pub fn row_height(font_size: f32, minimum: f32, padding: f32) -> f32 {
    (font_size * 1.3 + padding).ceil().max(minimum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_fits_the_entire_supported_font_range() {
        for size in [8.0, 14.0, 24.0, 36.0, 72.0] {
            for (minimum, padding) in [(30.0, 14.0), (22.0, 4.0), (20.0, 4.0), (22.0, 8.0)] {
                let height = row_height(size, minimum, padding);
                assert!(height >= minimum);
                assert!(height >= size * 1.3 + padding);
                assert!(height >= row_height(8.0, minimum, padding));
            }
        }
    }
}
