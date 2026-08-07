/// Effects a Lua call (a bind firing, or the script's own top-level body
/// running) can ask the compositor for. The functions exposed in
/// `spitfire.*` don't touch compositor state directly — they don't have
/// access to it — instead they push a `Command`, which whoever loaded the
/// config (the `spitfire` crate) drains and applies to `SpitfireState`
/// after each invocation.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `spitfire.spawn(cmd)` — runs `cmd` through `sh -c`.
    Spawn(String),
    /// `spitfire.layout.set(mode)`.
    LayoutSet(spitfire_layout::LayoutMode),
    /// `spitfire.layout.cycle()`.
    LayoutCycle,
    /// `spitfire.mfact.inc(delta)` — dwm: MODKEY+h/l.
    MfactInc(f64),
    /// `spitfire.nmaster.inc(delta)` — dwm: MODKEY+i/d.
    NmasterInc(i32),
    /// `spitfire.workspace.focus(n)` — 1-based, switches the view;
    /// creates workspace `n` if it doesn't exist yet.
    WorkspaceFocus(usize),
    /// `spitfire.workspace.move_window(n)` — 1-based, moves the focused
    /// window to workspace `n` without switching the view there.
    WorkspaceMoveWindow(usize),
    /// `spitfire.window.close()` — closes whichever window has keyboard
    /// focus.
    WindowClose,
    /// `spitfire.window.focus_next()` — dwm-style MODKEY+j: moves keyboard
    /// focus to the next window in the active workspace's tiling order.
    WindowFocusNext,
    /// `spitfire.window.focus_prev()` — dwm-style MODKEY+k: same as
    /// `WindowFocusNext`, other direction.
    WindowFocusPrev,
    /// `spitfire.window.swap_next()` — moves focused window next in tiling order.
    WindowSwapNext,
    /// `spitfire.window.swap_prev()` — moves focused window previous in tiling order.
    WindowSwapPrev,
    /// `spitfire.window.toggle_scratchpad()` — hides the focused window
    /// into the (single-slot) scratchpad if it's currently visible, or
    /// re-shows/refocuses whatever's already stashed there if nothing
    /// relevant is focused. See `SpitfireState::toggle_scratchpad`.
    WindowToggleScratchpad,
    /// `spitfire.scratchpad.toggle(name, spawn_cmd, app_id, width_frac?,
    /// height_frac?)` — a named, app-specific scratchpad slot
    /// (XMonad/LeftWM-style): spawns `spawn_cmd` if nothing's registered
    /// yet under `name`, claiming the next window that maps with the given
    /// `app_id` as that slot's window; otherwise shows or hides whichever
    /// window is already registered there. `width_frac`/`height_frac`
    /// (each `0.0..=1.0`, either or both omittable) size the window, as a
    /// fraction of the output's usable area, the moment it's claimed —
    /// omitted entirely, it just keeps whatever size it opened itself at.
    /// See `SpitfireState::toggle_named_scratchpad`.
    ScratchpadToggle {
        name: String,
        spawn_cmd: String,
        app_id: String,
        width_frac: Option<f64>,
        height_frac: Option<f64>,
    },
    /// `spitfire.quit()`.
    Quit,
    /// `spitfire.reload()` — drops the current Lua state and re-runs the
    /// file from scratch, same as the Phase 3 `spitfirectl reload` IPC
    /// command.
    ReloadConfig,
}
