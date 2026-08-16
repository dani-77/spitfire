//! Server-side `ext-foreign-toplevel-list-v1` — advertises every open
//! window (across every workspace, tiled/floating/scratchpad alike) to
//! clients like a pager, a taskbar, or — the reason this landed now — a
//! per-window `ext-image-copy-capture-v1` capture source (`crate::ext_screencopy`),
//! which needs exactly this protocol's `ext_foreign_toplevel_handle_v1` as
//! its capture target.
//!
//! Unlike `ext_workspace.rs`/`screencopy.rs`/`ext_screencopy.rs`, this file
//! doesn't hand-roll `GlobalDispatch`/`Dispatch` impls at all — Smithay
//! ships a complete, ready-to-use implementation
//! (`smithay::wayland::foreign_toplevel_list`), so this is just the glue:
//! `ForeignToplevelListHandler`, `delegate_foreign_toplevel_list!`, and
//! `sync_foreign_toplevels`, the per-frame full-resync that tells it about
//! spitfire's own windows.
//!
//! **Source of truth is deliberately not `Space::elements()`**: switching
//! workspaces unmaps every window that isn't on the newly active one (see
//! `SpitfireState::switch_workspace` in workspace.rs), so `Space::elements()`
//! only ever holds the *active* workspace's windows. Sourcing from it would
//! spuriously `send_closed()` every other workspace's windows on every
//! switch, then hand out a *new* identifier for the same window when
//! switching back — a real, protocol-visible bug: the spec requires a
//! toplevel's identifier to stay unique and never be reused for as long as
//! it's mapped. The real source is every `Workspace::tiling`'s window list
//! (tiled *and* floating both live there — see `new_toplevel` in
//! shell/xdg.rs and shell/x11.rs, which push into it unconditionally) plus
//! the two scratchpad slots (`SpitfireState::scratchpad`,
//! `SpitfireState::named_scratchpads`), which pull a window out of every
//! workspace's tiling list while stashed but keep the client itself alive
//! — see `all_windows` below.
//!
//! Full resync every frame (same "small enough to be free" reasoning
//! `ext_workspace.rs`'s own doc comment already gives for its own full
//! resync), rather than hooking every individual `xdg_toplevel.set_title`/
//! `set_app_id` request — a title/app_id change lands within one frame
//! either way, and this avoids needing a second hook in shell/x11.rs for
//! the equivalent X11 property changes.

use smithay::{
    utils::IsAlive,
    wayland::foreign_toplevel_list::{ForeignToplevelListHandler, ForeignToplevelListState},
};

use crate::{
    shell::WindowElement,
    state::{Backend, SpitfireState},
    workspace::NamedScratchpad,
};

impl<BackendData: Backend + 'static> ForeignToplevelListHandler for SpitfireState<BackendData> {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list_state
    }
}

smithay::delegate_foreign_toplevel_list!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    /// Every currently-open window, regardless of which workspace it's on
    /// or whether it's stashed in a scratchpad slot — see this module's doc
    /// comment for why `Space::elements()` alone is the wrong source here.
    fn all_windows(&self) -> Vec<WindowElement> {
        let mut windows: Vec<WindowElement> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tiling.windows().iter().cloned())
            .collect();
        windows.extend(self.scratchpad.clone());
        windows.extend(
            self.named_scratchpads
                .values()
                .filter_map(|slot| match slot {
                    NamedScratchpad::Shown(w) | NamedScratchpad::Hidden(w) => Some(w.clone()),
                    NamedScratchpad::Pending { .. } => None,
                }),
        );
        windows
    }

    /// Full resync: creates an `ext_foreign_toplevel_handle_v1` for every
    /// window `all_windows` now has that wasn't already tracked, sends
    /// `closed()` and drops tracking for any tracked window that's gone,
    /// and re-sends `title`/`app_id` (plus `done()`) for any whose live
    /// value no longer matches what was last sent. Call once per real
    /// frame — winit.rs's main loop / udev.rs's render loop, right
    /// alongside `space.refresh()` (same per-frame liveness-check spirit;
    /// there's no dedicated "window destroyed" hook to react to instead,
    /// see the comment already next to that `space.refresh()` call).
    pub fn sync_foreign_toplevels(&mut self) {
        let windows = self.all_windows();

        self.foreign_toplevel_handles.retain(|(window, handle)| {
            let still_open = window.alive() && windows.iter().any(|w| w == window);
            if !still_open {
                handle.send_closed();
            }
            still_open
        });

        for window in &windows {
            if !window.alive() {
                continue;
            }
            let title = window.title().unwrap_or_default();
            let app_id = window.app_id().unwrap_or_default();

            match self
                .foreign_toplevel_handles
                .iter()
                .find(|(w, _)| w == window)
            {
                Some((_, handle)) => {
                    let mut changed = false;
                    if handle.title() != title {
                        handle.send_title(&title);
                        changed = true;
                    }
                    if handle.app_id() != app_id {
                        handle.send_app_id(&app_id);
                        changed = true;
                    }
                    if changed {
                        handle.send_done();
                    }
                }
                None => {
                    let handle = self
                        .foreign_toplevel_list_state
                        .new_toplevel::<Self>(title, app_id);
                    self.foreign_toplevel_handles.push((window.clone(), handle));
                }
            }
        }
    }
}
