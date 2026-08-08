//! The dynamic per-output workspace model (Phase 5) — niri-style: an
//! ordered, growable list of workspaces, one active at a time. Each
//! workspace owns its own [`TilingLayout`], so switching workspaces means
//! swapping which set of windows is tiled/visible, not touching the
//! layout engine itself.
//!
//! v1 scope: a single output (matches the winit-only backend), so there is
//! exactly one [`WorkspaceSet`], not one per output. The struct is shaped
//! so that moving to real multi-output later means keying a
//! `HashMap<Output, WorkspaceSet>` instead of rewriting this.

use smithay::{
    desktop::{space::SpaceElement, WindowSurface},
    output::Output,
    utils::{IsAlive, Logical, Size, SERIAL_COUNTER},
};
use tracing::info;

use crate::{
    focus::KeyboardFocusTarget,
    layout::{usable_area, ForceFloating, TilingLayout},
    shell::{center_on_output, center_on_output_with_size, WindowElement},
    state::{Backend, SpitfireState},
};

/// A stable identifier for a workspace, assigned once at creation and never
/// reused — unlike its `Vec` index, which shifts when an earlier workspace
/// is removed. `ext_workspace.rs` keys its protocol objects by this instead
/// of by index, so a client's `ExtWorkspaceHandleV1` never silently starts
/// pointing at the wrong workspace after a removal elsewhere in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(u64);

/// One workspace: a name (shown by `ext-workspace-v1`'s `name` event and
/// any bar) plus its own tiling state.
#[derive(Debug)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub tiling: TilingLayout,
}

impl Workspace {
    fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Workspace {
            id,
            name: name.into(),
            tiling: TilingLayout::default(),
        }
    }
}

/// An ordered list of workspaces with one active at a time. Always has at
/// least one workspace — there is no "no workspace" state.
#[derive(Debug)]
pub struct WorkspaceSet {
    workspaces: Vec<Workspace>,
    active: usize,
    next_id: u64,
}

impl Default for WorkspaceSet {
    fn default() -> Self {
        WorkspaceSet {
            workspaces: vec![Workspace::new(WorkspaceId(0), "1")],
            active: 0,
            next_id: 1,
        }
    }
}

impl WorkspaceSet {
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    pub fn is_empty(&self) -> bool {
        false // always has at least one workspace
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    pub fn get(&self, idx: usize) -> Option<&Workspace> {
        self.workspaces.get(idx)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Workspace> {
        self.workspaces.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Workspace> {
        self.workspaces.iter_mut()
    }

    pub fn index_of(&self, id: WorkspaceId) -> Option<usize> {
        self.workspaces.iter().position(|ws| ws.id == id)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Workspace> {
        self.workspaces.get_mut(idx)
    }

    /// Grows the list until index `idx` is valid, without changing which
    /// workspace is active (unlike [`Self::switch_to`]).
    pub fn ensure(&mut self, idx: usize) {
        self.ensure_len(idx);
    }

    fn alloc_id(&mut self) -> WorkspaceId {
        let id = WorkspaceId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Grows the list with freshly-numbered workspaces until index `idx` is
    /// valid — niri-style dynamic growth: asking to focus workspace 5 when
    /// only 2 exist just creates 3, 4, and 5.
    fn ensure_len(&mut self, idx: usize) {
        while self.workspaces.len() <= idx {
            let n = self.workspaces.len() + 1;
            let id = self.alloc_id();
            self.workspaces.push(Workspace::new(id, n.to_string()));
        }
    }

    /// Switches the active workspace to `idx`, growing the list first if
    /// needed. Returns `true` if the active workspace actually changed
    /// (`false` if `idx` was already active — callers use this to skip
    /// re-arranging/re-hiding for a no-op switch).
    pub fn switch_to(&mut self, idx: usize) -> bool {
        self.ensure_len(idx);
        if self.active == idx {
            return false;
        }
        self.active = idx;
        true
    }

    /// Appends a new, empty workspace and returns its index. Used by both
    /// `spitfire.workspace.focus()` growth and the `create_workspace`
    /// request from an `ext-workspace-v1` client.
    pub fn create(&mut self, name: Option<String>) -> usize {
        let idx = self.workspaces.len();
        let name = name.unwrap_or_else(|| (idx + 1).to_string());
        let id = self.alloc_id();
        self.workspaces.push(Workspace::new(id, name));
        idx
    }

    /// Removes workspace `idx`, unless it's the only one left (an empty
    /// `WorkspaceSet` isn't a representable state) or it's the active one
    /// (the caller must switch away first — this type doesn't own the
    /// `Space`/output needed to move or hide windows, see
    /// `SpitfireState::remove_workspace`). Returns whether it was removed.
    pub fn remove(&mut self, idx: usize) -> bool {
        if self.workspaces.len() <= 1 || idx >= self.workspaces.len() || idx == self.active {
            return false;
        }
        self.workspaces.remove(idx);
        if self.active > idx {
            self.active -= 1;
        }
        true
    }

    pub fn rename(&mut self, idx: usize, name: impl Into<String>) -> bool {
        match self.workspaces.get_mut(idx) {
            Some(ws) => {
                ws.name = name.into();
                true
            }
            None => false,
        }
    }
}

/// One `spitfire.scratchpad.toggle(name, spawn_cmd, app_id, width_frac?,
/// height_frac?)` slot's state — see
/// `SpitfireState::toggle_named_scratchpad`'s doc comment for the whole
/// flow this drives.
#[derive(Debug)]
pub enum NamedScratchpad {
    /// `spawn_cmd` has already been run; waiting for a window whose
    /// `app_id` matches to map so `SpitfireState::claim_pending_named_scratchpad`
    /// can claim it. `width_frac`/`height_frac` are what it'll be resized
    /// to (each a fraction of the output's usable area) once claimed, if
    /// given at all — `None` leaves that axis at whatever size the window
    /// opened itself at.
    Pending {
        app_id: String,
        width_frac: Option<f64>,
        height_frac: Option<f64>,
    },
    /// Currently visible.
    Shown(WindowElement),
    /// Currently stashed (unmapped, out of every workspace's tiling order).
    Hidden(WindowElement),
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    /// `spitfire.workspace.focus(n)` — switches to workspace `idx` (0-based
    /// here; the Lua API is 1-based and subtracts 1 before calling this),
    /// creating it if it doesn't exist yet (niri-style dynamic growth).
    /// Windows on the outgoing workspace are hidden, the incoming one's are
    /// shown and re-tiled.
    pub fn switch_workspace(&mut self, idx: usize) {
        if !self.workspaces.switch_to(idx) {
            return;
        }
        self.hide_inactive_workspaces();
        self.arrange_tiling();
        self.sync_ext_workspace_state();
    }

    /// `spitfire.workspace.move_window(n)` — moves the currently
    /// keyboard-focused window to workspace `idx` (0-based) without
    /// switching the view there (dwm convention: the window leaves, you
    /// stay put). No-op if nothing is focused or it's already on `idx`.
    pub fn move_focused_window_to_workspace(&mut self, idx: usize) {
        let current = self.workspaces.active_index();
        if current == idx {
            return;
        }
        let Some(KeyboardFocusTarget::Window(window)) =
            self.seat.get_keyboard().and_then(|kb| kb.current_focus())
        else {
            return;
        };
        let window = WindowElement(window);

        self.workspaces.active_mut().tiling.remove(&window);
        self.workspaces.ensure(idx);
        if let Some(ws) = self.workspaces.get_mut(idx) {
            ws.tiling.push(window.clone());
        }
        self.space.unmap_elem(&window);
        self.arrange_tiling();
        self.sync_ext_workspace_state();
    }

    /// `spitfire.window.close()` — closes whichever window currently has
    /// keyboard focus: `xdg_toplevel.close` for a Wayland client, the
    /// equivalent X11 request for an XWayland one. A no-op if nothing is
    /// focused (e.g. an empty workspace).
    pub fn close_focused_window(&mut self) {
        let Some(KeyboardFocusTarget::Window(window)) =
            self.seat.get_keyboard().and_then(|kb| kb.current_focus())
        else {
            return;
        };
        match window.underlying_surface() {
            WindowSurface::Wayland(w) => w.send_close(),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let _ = w.close();
            }
        }
    }

    /// `spitfire.window.focus_next()` / `.focus_prev()` — dwm-style
    /// MODKEY+j/k: moves keyboard focus to the next/previous window in the
    /// active workspace's tiling order (wraps around). If nothing was
    /// focused (or focus was on something other than a window, e.g. a
    /// layer-shell surface), lands on the first window instead. A no-op
    /// with fewer than 2 windows in the workspace.
    pub fn cycle_window_focus(&mut self, forward: bool) {
        let windows = self.workspaces.active().tiling.windows();
        if windows.len() < 2 {
            return;
        }
        let windows: Vec<WindowElement> = windows.to_vec();

        let current_idx = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| match f {
                KeyboardFocusTarget::Window(w) => Some(WindowElement(w)),
                _ => None,
            })
            .and_then(|current| windows.iter().position(|w| w == &current));

        let next_idx = match current_idx {
            Some(idx) if forward => (idx + 1) % windows.len(),
            Some(idx) => (idx + windows.len() - 1) % windows.len(),
            None => 0,
        };
        let window = windows[next_idx].clone();

        self.space.raise_element(&window, true);
        #[cfg(feature = "xwayland")]
        if let Some(surface) = window.0.x11_surface() {
            self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
        }
        let serial = SERIAL_COUNTER.next_serial();
        self.seat
            .get_keyboard()
            .unwrap()
            .set_focus(self, Some(window.into()), serial);
    }

    /// `spitfire.window.swap_next()` / `.swap_prev()` — swaps the position of
    /// the currently focused window with the next/previous window in the
    /// active workspace's tiling layout.
    pub fn swap_focused_window(&mut self, forward: bool) {
        let Some(KeyboardFocusTarget::Window(window)) =
            self.seat.get_keyboard().and_then(|kb| kb.current_focus())
        else {
            return;
        };
        let window = WindowElement(window);
        self.workspaces
            .active_mut()
            .tiling
            .swap_window(&window, forward);
        self.arrange_tiling();
    }

    /// `spitfire.window.toggle_scratchpad()` — a single-slot, dwm-style
    /// scratchpad. If the slot is empty, stashes whichever window
    /// currently has keyboard focus: unmaps it and takes it out of its
    /// workspace's tiling order, same as switching away from a workspace
    /// does (see `hide_inactive_workspaces` below). If the slot is already
    /// occupied, brings that window back instead — centered on the active
    /// output (same placement `center_if_ruled` gives a `spitfire.rule({
    /// floating = true, centered = true })` window), rejoining the active
    /// workspace's tiling order, and focused.
    ///
    /// Only ever tracks one window: toggling while the slot is occupied
    /// always shows *that* window, regardless of what's currently focused
    /// — it doesn't replace the stashed window with whatever's focused.
    /// Show it first (which empties the slot), then toggle again on
    /// something else to stash that instead. A no-op if the slot is empty
    /// and nothing (or something other than a window) is focused.
    pub fn toggle_scratchpad(&mut self) {
        if let Some(window) = self.scratchpad.take() {
            self.show_scratchpad_window(window);
            return;
        }

        let Some(KeyboardFocusTarget::Window(window)) =
            self.seat.get_keyboard().and_then(|kb| kb.current_focus())
        else {
            return;
        };
        let window = WindowElement(window);
        self.hide_scratchpad_window(&window);
        self.scratchpad = Some(window);
    }

    /// Marks `window` floating for good, centers it on the (first) output,
    /// rejoins the active workspace's tiling order, and focuses it — the
    /// "bring a stashed window back" half of both `toggle_scratchpad` and
    /// `toggle_named_scratchpad`.
    fn show_scratchpad_window(&mut self, window: WindowElement) {
        // `ForceFloating` never gets removed once set — see its own doc
        // comment for why that's the right call here, not a bug.
        window.0.user_data().insert_if_missing(|| ForceFloating);
        let output = self.space.outputs().next().cloned();
        if let Some(output) = output {
            let bar_height = if self.config.bar.enabled {
                self.config.bar.height
            } else {
                0
            };
            let bar_margin = self.config.gaps.outer;
            center_on_output(&mut self.space, &output, &window, bar_height, bar_margin);
        }
        // `Space` owns the compositor's render order, but XWayland keeps
        // a separate X11 stacking order too.  Re-mapping only in `Space`
        // leaves an X11 scratchpad below an already-mapped X11 window: its
        // compositor-drawn border is visible, while its client buffer is
        // hidden underneath.  Keep both orders in sync, as the normal
        // focus paths do.
        self.space.raise_element(&window, true);
        #[cfg(feature = "xwayland")]
        if let Some(surface) = window.0.x11_surface() {
            self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
        }
        self.workspaces.active_mut().tiling.push(window.clone());
        self.raise_floating_windows();
        let serial = SERIAL_COUNTER.next_serial();
        self.seat
            .get_keyboard()
            .unwrap()
            .set_focus(self, Some(window.into()), serial);
    }

    /// Unmaps `window` and takes it out of every workspace's tiling order —
    /// the "stash a window away" half of both `toggle_scratchpad` and
    /// `toggle_named_scratchpad`.
    fn hide_scratchpad_window(&mut self, window: &WindowElement) {
        for ws in self.workspaces.iter_mut() {
            ws.tiling.remove(window);
        }
        self.space.unmap_elem(window);
    }

    /// `spitfire.scratchpad.toggle(name, spawn_cmd, app_id, width_frac?,
    /// height_frac?)` — a named, app-specific scratchpad slot
    /// (XMonad/LeftWM-style "named scratchpad"), as opposed to
    /// `toggle_scratchpad`'s single anonymous one: `name` identifies the
    /// slot, `spawn_cmd` is what launches the app the first time (or
    /// again, if the window it previously held died), and `app_id` is how
    /// the freshly spawned window gets recognized and claimed once it
    /// maps — see `claim_pending_named_scratchpad`, called from
    /// `shell::commit` right when a new window gets its first real buffer.
    /// `width_frac`/`height_frac` (each `0.0..=1.0`, either omittable) size
    /// it, as a fraction of the output's usable area, at that same claim
    /// point — omitted, it just keeps whatever size it opened itself at.
    ///
    /// - Nothing registered under `name` yet, or the window it held died:
    ///   spawns `spawn_cmd` and starts waiting for `app_id` to map. The
    ///   window ends up shown (not stashed) the moment it's claimed —
    ///   pressing the bind on an app that isn't running yet opens it
    ///   visibly, matching XMonad/LeftWM.
    /// - Registered and currently shown: hides it.
    /// - Registered and currently hidden: shows it (at whatever size it
    ///   currently is — `width_frac`/`height_frac` only ever apply once,
    ///   right when the window is first claimed).
    ///
    /// A second toggle while still waiting for the spawned window to map
    /// is a deliberate no-op — otherwise a fast double-press would spawn
    /// the app twice.
    pub fn toggle_named_scratchpad(
        &mut self,
        name: &str,
        spawn_cmd: &str,
        app_id: &str,
        width_frac: Option<f64>,
        height_frac: Option<f64>,
    ) {
        let entry = match self.named_scratchpads.remove(name) {
            // Normalize a dead window (closed while shown or hidden) to
            // "nothing registered" so it falls into the same respawn path
            // below, instead of needing a second toggle to notice it's
            // gone before a third one actually respawns it.
            Some(NamedScratchpad::Shown(w)) | Some(NamedScratchpad::Hidden(w)) if !w.alive() => {
                None
            }
            other => other,
        };

        match entry {
            None => {
                info!(
                    name,
                    spawn_cmd, app_id, "Spawning (spitfire.scratchpad.toggle)"
                );
                self.spawn(spawn_cmd);
                self.named_scratchpads.insert(
                    name.to_string(),
                    NamedScratchpad::Pending {
                        app_id: app_id.to_string(),
                        width_frac,
                        height_frac,
                    },
                );
            }
            Some(pending @ NamedScratchpad::Pending { .. }) => {
                self.named_scratchpads.insert(name.to_string(), pending);
            }
            Some(NamedScratchpad::Shown(window)) => {
                self.hide_scratchpad_window(&window);
                self.named_scratchpads
                    .insert(name.to_string(), NamedScratchpad::Hidden(window));
            }
            Some(NamedScratchpad::Hidden(window)) => {
                self.show_scratchpad_window(window.clone());
                self.named_scratchpads
                    .insert(name.to_string(), NamedScratchpad::Shown(window));
            }
        }
    }

    /// Checks whether `window`'s `app_id` matches any
    /// `NamedScratchpad::Pending` slot, and if so, claims it: marks it
    /// floating, resizes it to that slot's `width_frac`/`height_frac` (if
    /// either was given), centers it on `output`, joins the active
    /// workspace's tiling order, and transitions the slot to `Shown`.
    /// Doesn't touch keyboard focus — the caller (`shell::commit`'s
    /// first-buffer path) already hands every newly mapped window focus
    /// regardless, so doing it here too would just be a redundant second
    /// `set_focus` call.
    ///
    /// A no-op (returns `false`) if nothing's waiting for this `app_id`.
    pub(crate) fn claim_pending_named_scratchpad(
        &mut self,
        window: &WindowElement,
        output: &Output,
        bar_height: i32,
        bar_margin: i32,
    ) -> bool {
        let Some(window_app_id) = window.app_id() else {
            return false;
        };
        let Some((name, width_frac, height_frac)) =
            self.named_scratchpads
                .iter()
                .find_map(|(name, slot)| match slot {
                    NamedScratchpad::Pending {
                        app_id,
                        width_frac,
                        height_frac,
                    } if *app_id == window_app_id => {
                        Some((name.clone(), *width_frac, *height_frac))
                    }
                    _ => None,
                })
        else {
            return false;
        };

        window.0.user_data().insert_if_missing(|| ForceFloating);

        if width_frac.is_some() || height_frac.is_some() {
            if let Some(area) = usable_area(&self.space, output, bar_height, bar_margin) {
                let natural = window.bbox().size;
                let size = Size::from((
                    width_frac.map_or(natural.w, |f| ((area.size.w as f64) * f).round() as i32),
                    height_frac.map_or(natural.h, |f| ((area.size.h as f64) * f).round() as i32),
                ));
                resize_window(window, size);
                center_on_output_with_size(
                    &mut self.space,
                    output,
                    window,
                    size,
                    bar_height,
                    bar_margin,
                );
            }
        } else {
            center_on_output(&mut self.space, output, window, bar_height, bar_margin);
        }

        self.space.raise_element(window, true);
        #[cfg(feature = "xwayland")]
        if let Some(surface) = window.0.x11_surface() {
            self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
        }

        self.workspaces.active_mut().tiling.push(window.clone());
        self.named_scratchpads
            .insert(name, NamedScratchpad::Shown(window.clone()));
        self.raise_floating_windows();
        true
    }

    /// The active workspace's windows, current on-screen geometry plus
    /// whether each is focused — what `render::border_elements` needs to
    /// draw `spitfire.border`. Only windows still actually mapped in
    /// `space` (e.g. not one that's floating and never got positioned) are
    /// included.
    pub fn border_rects(&self) -> Vec<crate::render::BorderRect> {
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| match f {
                KeyboardFocusTarget::Window(w) => Some(WindowElement(w)),
                _ => None,
            });

        self.workspaces
            .active()
            .tiling
            .windows()
            .iter()
            .filter_map(|w| {
                let geometry = self.space.element_geometry(w)?;
                Some(crate::render::BorderRect {
                    geometry,
                    focused: focused.as_ref() == Some(w),
                })
            })
            .collect()
    }

    /// Unmaps every window belonging to a workspace other than the active
    /// one from `space`, so it isn't rendered or clickable. The active
    /// workspace's windows get remapped by the very next `arrange_tiling`.
    fn hide_inactive_workspaces(&mut self) {
        let active = self.workspaces.active_index();
        let to_hide: Vec<WindowElement> = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != active)
            .flat_map(|(_, ws)| ws.tiling.windows().iter().cloned())
            .collect();
        for window in to_hide {
            self.space.unmap_elem(&window);
        }
    }
}

/// Sends `window` a configure requesting `size` — used only by
/// `SpitfireState::claim_pending_named_scratchpad`'s
/// `width_frac`/`height_frac` handling. The resize itself isn't
/// synchronous: the client redraws and commits a new buffer at its own
/// pace, same as any other resize (see `center_on_output_with_size`'s doc
/// comment for why the caller centers around the *requested* size rather
/// than waiting for that commit).
fn resize_window(window: &WindowElement, size: Size<i32, Logical>) {
    match window.0.underlying_surface() {
        WindowSurface::Wayland(toplevel) => {
            toplevel.with_pending_state(|state| {
                state.size = Some(size);
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(x11) => {
            let loc = x11.geometry().loc;
            let _ = x11.configure(smithay::utils::Rectangle::new(loc, size));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_one_workspace_named_1() {
        let ws = WorkspaceSet::default();
        assert_eq!(ws.len(), 1);
        assert_eq!(ws.active_index(), 0);
        assert_eq!(ws.active().name, "1");
    }

    #[test]
    fn switch_to_grows_the_list_dynamically() {
        let mut ws = WorkspaceSet::default();
        assert!(ws.switch_to(4));
        assert_eq!(ws.len(), 5);
        assert_eq!(ws.active_index(), 4);
        assert_eq!(ws.get(4).unwrap().name, "5");
    }

    #[test]
    fn switch_to_the_already_active_workspace_is_a_no_op() {
        let mut ws = WorkspaceSet::default();
        assert!(!ws.switch_to(0));
    }

    #[test]
    fn create_appends_and_returns_its_index() {
        let mut ws = WorkspaceSet::default();
        let idx = ws.create(None);
        assert_eq!(idx, 1);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws.get(1).unwrap().name, "2");
    }

    #[test]
    fn create_with_explicit_name() {
        let mut ws = WorkspaceSet::default();
        let idx = ws.create(Some("scratch".into()));
        assert_eq!(ws.get(idx).unwrap().name, "scratch");
    }

    #[test]
    fn cannot_remove_the_last_remaining_workspace() {
        let mut ws = WorkspaceSet::default();
        assert!(!ws.remove(0));
        assert_eq!(ws.len(), 1);
    }

    #[test]
    fn cannot_remove_the_active_workspace() {
        let mut ws = WorkspaceSet::default();
        ws.create(None);
        assert!(!ws.remove(0)); // 0 is still active
    }

    #[test]
    fn remove_shifts_active_index_down_when_removing_an_earlier_workspace() {
        let mut ws = WorkspaceSet::default();
        ws.create(None);
        ws.create(None);
        assert!(ws.switch_to(2));
        assert!(ws.remove(0));
        assert_eq!(ws.len(), 2);
        assert_eq!(ws.active_index(), 1); // was 2, shifted down by one
    }

    #[test]
    fn rename_updates_the_given_workspace_only() {
        let mut ws = WorkspaceSet::default();
        ws.create(None);
        assert!(ws.rename(1, "web"));
        assert_eq!(ws.active().name, "1");
        assert_eq!(ws.get(1).unwrap().name, "web");
    }

    #[test]
    fn rename_out_of_bounds_returns_false() {
        let mut ws = WorkspaceSet::default();
        assert!(!ws.rename(5, "nope"));
    }

    #[test]
    fn ids_stay_stable_across_removals_of_other_workspaces() {
        let mut ws = WorkspaceSet::default();
        ws.create(None); // id 1, index 1
        ws.create(None); // id 2, index 2
        let id_of_ws2 = ws.get(2).unwrap().id;

        assert!(ws.switch_to(2));
        assert!(ws.switch_to(0));
        assert!(ws.remove(1)); // removes index 1 (id 1); ws with id 2 shifts to index 1

        assert_eq!(ws.index_of(id_of_ws2), Some(1));
    }
}
