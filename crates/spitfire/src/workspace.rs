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

use crate::{
    focus::KeyboardFocusTarget,
    layout::TilingLayout,
    shell::WindowElement,
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

    /// The active workspace's windows, current on-screen geometry plus
    /// whether each is focused — what `render::border_elements` needs to
    /// draw `spitfire.border`. Only windows still actually mapped in
    /// `space` (e.g. not one that's floating and never got positioned) are
    /// included.
    pub fn border_rects(&self) -> Vec<crate::render::BorderRect> {
        let focused = self.seat.get_keyboard().and_then(|kb| kb.current_focus()).and_then(|f| match f {
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
