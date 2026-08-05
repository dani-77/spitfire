/// The 4 layout modes, dwm/suckless-style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutMode {
    /// Classic master-stack: `nmaster` windows in a column on the left with
    /// width `mfact`, the rest stacked on the right.
    Tile,
    /// The layout engine doesn't touch geometry — position/size come from
    /// normal xdg-shell drag/resize.
    Floating,
    /// Recursive spiral split (dwm's `fibonacci`/`spiral` patch).
    Fibonacci,
    /// Every window occupies the whole area; only the top of the stack is
    /// visible.
    Monocle,
}

impl LayoutMode {
    /// Cycle order used by `spitfire.layout.cycle()` / MODKEY+space.
    pub fn cycle_next(self) -> Self {
        match self {
            LayoutMode::Tile => LayoutMode::Fibonacci,
            LayoutMode::Fibonacci => LayoutMode::Monocle,
            LayoutMode::Monocle => LayoutMode::Floating,
            LayoutMode::Floating => LayoutMode::Tile,
        }
    }
}

/// Spacing between windows (`inner`) and between windows and the output's
/// edge (`outer`), in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gaps {
    pub inner: i32,
    pub outer: i32,
}

impl Default for Gaps {
    fn default() -> Self {
        Gaps { inner: 6, outer: 10 }
    }
}

/// `mfact` bounds, same as dwm (never lets the master or stack column
/// disappear entirely).
pub const MFACT_MIN: f64 = 0.05;
pub const MFACT_MAX: f64 = 0.95;

/// A workspace's layout parameters — mutable at runtime via keybinding or
/// IPC command, replicating dwm's `MODKEY+t/f/m` and `MODKEY+i/d/h/l`
/// without needing a recompile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    pub mode: LayoutMode,
    pub nmaster: usize,
    pub mfact: f64,
    pub gaps: Gaps,
}

impl Default for LayoutParams {
    fn default() -> Self {
        LayoutParams {
            mode: LayoutMode::Tile,
            nmaster: 1,
            mfact: 0.55,
            gaps: Gaps::default(),
        }
    }
}

impl LayoutParams {
    pub fn set_mode(&mut self, mode: LayoutMode) {
        self.mode = mode;
    }

    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.cycle_next();
    }

    /// Adjusts `nmaster` by `delta`, never below 0 (dwm: MODKEY+i/d).
    pub fn inc_nmaster(&mut self, delta: i32) {
        let n = self.nmaster as i64 + delta as i64;
        self.nmaster = n.max(0) as usize;
    }

    /// Adjusts `mfact` by `delta`, clamped to [`MFACT_MIN`, `MFACT_MAX`]
    /// (dwm: MODKEY+h/l).
    pub fn inc_mfact(&mut self, delta: f64) {
        self.mfact = (self.mfact + delta).clamp(MFACT_MIN, MFACT_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_mode_visits_all_four_modes_and_returns_to_start() {
        let mut mode = LayoutMode::Tile;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..4 {
            seen.insert(mode);
            mode = mode.cycle_next();
        }
        assert_eq!(seen.len(), 4);
        assert_eq!(mode, LayoutMode::Tile);
    }

    #[test]
    fn inc_mfact_clamps_to_bounds() {
        let mut params = LayoutParams::default();
        params.inc_mfact(10.0);
        assert_eq!(params.mfact, MFACT_MAX);
        params.inc_mfact(-10.0);
        assert_eq!(params.mfact, MFACT_MIN);
    }

    #[test]
    fn inc_nmaster_never_goes_negative() {
        let mut params = LayoutParams::default();
        params.nmaster = 1;
        params.inc_nmaster(-5);
        assert_eq!(params.nmaster, 0);
        params.inc_nmaster(3);
        assert_eq!(params.nmaster, 3);
    }
}
