/// A rectangle in logical coordinates.
///
/// Deliberately doesn't depend on any Smithay/Wayland type — that's what
/// makes this crate testable with `cargo test`, without needing a
/// compositor running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    /// Shrinks the rectangle by `margin` on every side (used to apply the
    /// outer gap). Saturates at 0 instead of going negative width/height.
    pub(crate) fn shrink(self, margin: i32) -> Self {
        let margin = margin.max(0);
        let w = (self.w - margin * 2).max(0);
        let h = (self.h - margin * 2).max(0);
        Rect {
            x: self.x + margin,
            y: self.y + margin,
            w,
            h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_moves_origin_and_reduces_size_by_twice_the_margin() {
        let r = Rect::new(0, 0, 1920, 1080).shrink(10);
        assert_eq!(r, Rect::new(10, 10, 1900, 1060));
    }

    #[test]
    fn shrink_saturates_at_zero_instead_of_going_negative() {
        let r = Rect::new(0, 0, 10, 10).shrink(100);
        assert_eq!(r.w, 0);
        assert_eq!(r.h, 0);
    }

    #[test]
    fn shrink_by_zero_is_a_no_op() {
        let r = Rect::new(5, 5, 100, 200);
        assert_eq!(r.shrink(0), r);
    }
}
