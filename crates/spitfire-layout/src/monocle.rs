use crate::Rect;

/// Every window occupies the same area — only the top of the stack is
/// actually visible; deciding which one that is and drawing the rest of the
/// stack underneath is the responsibility of whoever consumes these
/// rectangles (the z-order).
pub(crate) fn arrange(n: usize, area: Rect, gap_outer: i32) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let area = area.shrink(gap_outer);
    vec![area; n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_windows_share_the_same_rect() {
        let rects = arrange(4, Rect::new(0, 0, 1920, 1080), 10);
        let first = rects[0];
        assert!(rects.iter().all(|r| *r == first));
        assert_eq!(first, Rect::new(10, 10, 1900, 1060));
    }

    #[test]
    fn zero_windows_produces_no_rects() {
        assert_eq!(arrange(0, Rect::new(0, 0, 1920, 1080), 10), Vec::new());
    }
}
