use crate::Rect;

/// dwm's classic master-stack: the first `nmaster` windows go in a column on
/// the left with width `mfact` of the available area; the rest get stacked
/// in a column on the right. If `nmaster` is 0 or covers every window, it's
/// a single full-width column.
pub(crate) fn arrange(n: usize, area: Rect, nmaster: usize, mfact: f64, gap_outer: i32, gap_inner: i32) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let area = area.shrink(gap_outer);
    let nmaster = nmaster.min(n);

    if nmaster == 0 || nmaster == n {
        return split_column(area, n, gap_inner);
    }

    let gap = gap_inner.max(0);
    let master_w = (((area.w - gap) as f64) * mfact).round() as i32;
    let master_w = master_w.clamp(0, area.w - gap);
    let stack_w = area.w - gap - master_w;

    let master_area = Rect::new(area.x, area.y, master_w, area.h);
    let stack_area = Rect::new(area.x + master_w + gap, area.y, stack_w, area.h);

    let mut rects = split_column(master_area, nmaster, gap_inner);
    rects.extend(split_column(stack_area, n - nmaster, gap_inner));
    rects
}

/// Splits `area` into `count` rows of equal height, separated by `gap`. The
/// last row absorbs the rounding remainder, so there's no "hole" left at
/// the bottom of the column.
pub(crate) fn split_column(area: Rect, count: usize, gap: i32) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let gap = gap.max(0);
    let total_gap = gap * (count as i32 - 1);
    let h = ((area.h - total_gap) as f64 / count as f64).floor() as i32;

    let mut rects = Vec::with_capacity(count);
    let mut y = area.y;
    for i in 0..count {
        let this_h = if i + 1 == count {
            (area.y + area.h) - y
        } else {
            h
        };
        rects.push(Rect::new(area.x, y, area.w, this_h.max(0)));
        y += this_h + gap;
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
        let rects = arrange(1, area(), 1, 0.55, 0, 0);
        assert_eq!(rects, vec![area()]);
    }

    #[test]
    fn nmaster_zero_is_a_single_full_width_stack() {
        let rects = arrange(2, area(), 0, 0.55, 0, 0);
        assert_eq!(rects[0].x, rects[1].x);
        assert_eq!(rects[0].w, area().w);
        assert!(rects[1].y > rects[0].y);
    }

    #[test]
    fn nmaster_covering_all_windows_is_a_single_full_width_stack() {
        let rects = arrange(3, area(), 3, 0.55, 0, 0);
        assert!(rects.iter().all(|r| r.w == area().w));
    }

    #[test]
    fn master_and_stack_columns_sit_side_by_side_with_a_gap() {
        let rects = arrange(2, area(), 1, 0.5, 0, 6);
        let master = rects[0];
        let stack = rects[1];
        assert_eq!(stack.x, master.x + master.w + 6);
        assert_eq!(master.h, area().h);
        assert_eq!(stack.h, area().h);
    }

    #[test]
    fn mfact_controls_the_master_column_width() {
        let narrow = arrange(2, area(), 1, 0.3, 0, 0)[0].w;
        let wide = arrange(2, area(), 1, 0.7, 0, 0)[0].w;
        assert!(wide > narrow);
    }

    #[test]
    fn extra_windows_beyond_nmaster_stack_vertically() {
        let rects = arrange(3, area(), 1, 0.5, 0, 6);
        // windows 1 and 2 (indices 1 and 2) are in the stack, stacked
        assert_eq!(rects[1].x, rects[2].x);
        assert_eq!(rects[1].w, rects[2].w);
        assert!(rects[2].y > rects[1].y);
    }

    #[test]
    fn windows_never_overflow_the_area() {
        for n in 1..=8usize {
            for nmaster in 0..=n {
                let rects = arrange(n, area(), nmaster, 0.55, 10, 6);
                for r in &rects {
                    assert!(r.x >= area().x && r.y >= area().y, "n={n} nmaster={nmaster} rect={r:?}");
                    assert!(r.x + r.w <= area().x + area().w, "n={n} nmaster={nmaster} rect={r:?}");
                    assert!(r.y + r.h <= area().y + area().h, "n={n} nmaster={nmaster} rect={r:?}");
                }
            }
        }
    }

    #[test]
    fn split_column_rows_tile_the_full_height_with_no_gap_at_the_bottom() {
        let rects = split_column(Rect::new(0, 0, 100, 100), 3, 4);
        assert_eq!(rects.len(), 3);
        let last = rects.last().unwrap();
        assert_eq!(last.y + last.h, 100);
    }

    #[test]
    fn zero_windows_produces_no_rects() {
        assert_eq!(arrange(0, area(), 1, 0.55, 10, 6), Vec::new());
    }
}
