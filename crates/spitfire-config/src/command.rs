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
    /// `spitfire.quit()`.
    Quit,
    /// `spitfire.reload()` — drops the current Lua state and re-runs the
    /// file from scratch, same as the Phase 3 `spitfirectl reload` IPC
    /// command.
    ReloadConfig,
}
