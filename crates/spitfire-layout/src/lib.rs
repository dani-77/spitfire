//! spitfire's layout engine — tile, floating, fibonacci, monocle.
//!
//! Deliberately free of any Wayland/Smithay dependency: `arrange()` is a
//! pure function over a list of windows and an area, testable with
//! `cargo test` without needing a compositor running. Whoever calls this
//! (the `spitfire` crate) is responsible for translating the result into
//! real geometry via `xdg_toplevel.configure` + `Space::map_element`.

mod fibonacci;
mod geometry;
mod monocle;
mod params;
mod tile;

pub use geometry::Rect;
pub use params::{Gaps, LayoutMode, LayoutParams, MFACT_MAX, MFACT_MIN};

/// Computes each window's position/size for the layout mode active in
/// `params`.
///
/// Returns `None` in [`LayoutMode::Floating`] mode: the layout engine
/// deliberately doesn't touch floating windows' geometry — it's up to
/// whoever runs the compositor to leave them alone (normal xdg-shell
/// drag/resize).
///
/// The order of `windows` matters: in `Tile`, the first `nmaster` go into
/// the master column; in `Fibonacci`, it determines the spiral order; in
/// `Monocle`, every window gets the same area and it's up to whoever
/// consumes the result to decide which one is "on top" of the stack
/// (usually `windows[0]`).
pub fn arrange<W: Clone>(
    windows: &[W],
    area: Rect,
    params: &LayoutParams,
) -> Option<Vec<(W, Rect)>> {
    let rects = match params.mode {
        LayoutMode::Floating => return None,
        LayoutMode::Tile => tile::arrange(
            windows.len(),
            area,
            params.nmaster,
            params.mfact,
            params.gaps.outer,
            params.gaps.inner,
        ),
        LayoutMode::Fibonacci => {
            fibonacci::arrange(windows.len(), area, params.gaps.outer, params.gaps.inner)
        }
        LayoutMode::Monocle => monocle::arrange(windows.len(), area, params.gaps.outer),
    };
    debug_assert_eq!(rects.len(), windows.len());
    Some(windows.iter().cloned().zip(rects).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn floating_returns_none_it_never_touches_geometry() {
        let params = LayoutParams {
            mode: LayoutMode::Floating,
            ..Default::default()
        };
        assert!(arrange(&[1, 2, 3], area(), &params).is_none());
    }

    #[test]
    fn empty_window_list_is_some_empty_vec_not_none() {
        let params = LayoutParams::default();
        let placements: Option<Vec<(i32, Rect)>> = arrange(&[], area(), &params);
        assert_eq!(placements, Some(vec![]));
    }

    #[test]
    fn tile_single_window_fills_area_minus_outer_gap() {
        let params = LayoutParams::default();
        let placements = arrange(&[1], area(), &params).unwrap();
        let (_, rect) = placements[0];
        assert_eq!(rect.x, params.gaps.outer);
        assert_eq!(rect.y, params.gaps.outer);
        assert_eq!(rect.w, area().w - params.gaps.outer * 2);
        assert_eq!(rect.h, area().h - params.gaps.outer * 2);
    }

    #[test]
    fn tile_two_windows_master_and_stack_side_by_side() {
        let params = LayoutParams::default(); // nmaster=1, mfact=0.55
        let placements = arrange(&[1, 2], area(), &params).unwrap();
        let master = placements[0].1;
        let stack = placements[1].1;
        assert_eq!(stack.x, master.x + master.w + params.gaps.inner);
        assert_eq!(master.h, area().h - params.gaps.outer * 2);
        assert_eq!(stack.h, master.h);
    }

    #[test]
    fn switching_mode_at_runtime_changes_the_placement() {
        let mut params = LayoutParams::default();
        let tiled = arrange(&[1, 2, 3], area(), &params).unwrap();

        params.set_mode(LayoutMode::Monocle);
        let monocled = arrange(&[1, 2, 3], area(), &params).unwrap();

        assert_ne!(tiled[1].1, monocled[1].1);
        assert!(monocled.iter().all(|(_, r)| *r == monocled[0].1));
    }

    #[test]
    fn window_placements_preserve_the_input_window_identity() {
        let params = LayoutParams::default();
        let placements = arrange(&["a", "b", "c"], area(), &params).unwrap();
        let ids: Vec<_> = placements.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }
}
