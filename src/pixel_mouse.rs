use jterm_engine::mouse::MousePosition;

/// Position inside a displayed cell before mapping it back to the live grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellOffset {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PixelViewport {
    pub generation: u64,
    pub size: (usize, usize, u16, u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PixelPosition {
    pub viewport: PixelViewport,
    pub x: u32,
    pub y: u32,
}

impl PixelPosition {
    pub fn wire(self) -> MousePosition {
        MousePosition::Pixel {
            x: self.x,
            y: self.y,
        }
    }
}

impl PixelViewport {
    pub fn position(self, col: usize, row: usize, offset: CellOffset) -> Option<PixelPosition> {
        let (cols, rows, width, height) = self.size;
        let axis = |cell: usize, cells: usize, offset: f32, pixels: u16| {
            if cell >= cells || !(0.0..1.0).contains(&offset) {
                return None;
            }
            let last = u32::from(pixels.checked_sub(1)?);
            let physical = (cell as f64 + f64::from(offset)) * f64::from(pixels) / cells as f64;
            Some((physical.floor() as u32).min(last))
        };
        Some(PixelPosition {
            viewport: self,
            x: axis(col, cols, offset.x, width)?,
            y: axis(row, rows, offset.y, height)?,
        })
    }
}

/// Coordinate space selected by the widget that sent the event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ReportCoordinates {
    Cell,
    Pixel(Option<(PixelViewport, CellOffset)>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReportPosition {
    Cell(usize, usize),
    Pixel(PixelPosition),
}

impl ReportPosition {
    pub fn wire(self) -> MousePosition {
        match self {
            Self::Cell(column, row) => MousePosition::Cell { column, row },
            Self::Pixel(position) => position.wire(),
        }
    }

    pub fn same_space(self, other: Self) -> bool {
        match (self, other) {
            (Self::Cell(..), Self::Cell(..)) => true,
            (Self::Pixel(left), Self::Pixel(right)) => left.viewport == right.viewport,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_cell_offsets_use_the_whole_measured_grid() {
        let view = PixelViewport {
            generation: 5,
            size: (3, 2, 49, 31),
        };
        let first = view
            .position(1, 1, CellOffset { x: 0.125, y: 0.5 })
            .unwrap();
        let next = view.position(1, 1, CellOffset { x: 0.5, y: 0.5 }).unwrap();
        assert_eq!(first.wire(), MousePosition::Pixel { x: 18, y: 23 });
        assert_eq!(next.wire(), MousePosition::Pixel { x: 24, y: 23 });
        assert!(view.position(3, 1, CellOffset { x: 0.0, y: 0.0 }).is_none());
        assert!(view
            .position(
                0,
                0,
                CellOffset {
                    x: f32::NAN,
                    y: 0.0
                }
            )
            .is_none());
    }
}
