use crate::Rect;

/// Recursive spiral split, alternating horizontal/vertical — like dwm's
/// `fibonacci`/`spiral` patch. Each window gets half of whatever space is
/// left; the last window takes whatever remains.
pub(crate) fn arrange(n: usize, area: Rect, gap_outer: i32, gap_inner: i32) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let gap = gap_inner.max(0);
    let mut remaining = area.shrink(gap_outer);
    let mut rects = Vec::with_capacity(n);

    for i in 0..n {
        if i + 1 == n {
            rects.push(remaining);
            break;
        }
        if i % 2 == 0 {
            // split vertically: this window gets the left half
            let w = (((remaining.w - gap) as f64) / 2.0).floor() as i32;
            let w = w.max(0);
            rects.push(Rect::new(remaining.x, remaining.y, w, remaining.h));
            remaining = Rect::new(remaining.x + w + gap, remaining.y, (remaining.w - w - gap).max(0), remaining.h);
        } else {
            // split horizontally: this window gets the top half
            let h = (((remaining.h - gap) as f64) / 2.0).floor() as i32;
            let h = h.max(0);
            rects.push(Rect::new(remaining.x, remaining.y, remaining.w, h));
            remaining = Rect::new(remaining.x, remaining.y + h + gap, remaining.w, (remaining.h - h - gap).max(0));
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn single_window_fills_area() {
        assert_eq!(arrange(1, area(), 0, 0), vec![area()]);
    }

    #[test]
    fn two_windows_split_left_right() {
        let rects = arrange(2, area(), 0, 6);
        assert_eq!(rects[0].x, area().x);
        assert_eq!(rects[1].x, rects[0].x + rects[0].w + 6);
        assert_eq!(rects[0].h, area().h);
        assert_eq!(rects[1].h, area().h);
    }

    #[test]
    fn three_windows_second_split_is_vertical() {
        let rects = arrange(3, area(), 0, 6);
        // window 1 (the right half of window 0) splits into top/bottom
        // to make room for windows 1 and 2
        assert_eq!(rects[1].x, rects[2].x);
        assert_eq!(rects[1].w, rects[2].w);
        assert!(rects[2].y > rects[1].y);
    }

    #[test]
    fn consecutive_windows_never_get_identical_rects() {
        for n in 2..=6usize {
            let rects = arrange(n, area(), 10, 6);
            for pair in rects.windows(2) {
                assert_ne!(pair[0], pair[1], "n={n}");
            }
        }
    }

    #[test]
    fn windows_never_overflow_the_area() {
        for n in 1..=8usize {
            let rects = arrange(n, area(), 10, 6);
            for r in &rects {
                assert!(r.x >= area().x + 10 && r.y >= area().y + 10, "n={n} rect={r:?}");
                assert!(r.x + r.w <= area().x + area().w - 10, "n={n} rect={r:?}");
                assert!(r.y + r.h <= area().y + area().h - 10, "n={n} rect={r:?}");
            }
        }
    }

    #[test]
    fn zero_windows_produces_no_rects() {
        assert_eq!(arrange(0, area(), 10, 6), Vec::new());
    }
}
