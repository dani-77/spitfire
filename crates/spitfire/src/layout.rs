//! Bridges the pure layout engine (`spitfire_layout`) to real compositor
//! state: keeps the tiling order of a *single workspace's* windows and
//! applies the active layout via `Space`/`xdg_toplevel.configure`. See
//! `crate::workspace` for the list-of-workspaces model built on top of
//! this (Phase 5) — each `Workspace` owns one `TilingLayout`.

use smithay::{
    desktop::{layer_map_for_output, Space},
    output::Output,
    utils::{IsAlive, Point, Size, SERIAL_COUNTER},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};
use spitfire_config::WindowRule;
use spitfire_layout::{arrange as layout_arrange, LayoutParams, Rect};
use tracing::debug;

use crate::{
    focus::KeyboardFocusTarget,
    shell::WindowElement,
    state::{Backend, SpitfireState},
};

/// Tiling order + layout parameters for a workspace.
///
/// The order in `order` is what the layout engine receives: the first
/// `nmaster` windows go into the master column in `tile` mode, determine
/// the spiral order in `fibonacci`, and `order[0]` is whichever ends up "on
/// top" in `monocle`. New windows are appended at the end (like dwm — they
/// join the stack, they don't steal the master slot); reordering (swap
/// master, promote/demote) is left for a later phase.
#[derive(Debug, Default)]
pub struct TilingLayout {
    pub params: LayoutParams,
    order: Vec<WindowElement>,
}

impl TilingLayout {
    /// Registers a new window at the end of the tiling order. Idempotent —
    /// does not duplicate an entry that's already there.
    pub fn push(&mut self, window: WindowElement) {
        self.order.retain(|w| w.alive());
        if !self.order.iter().any(|w| w == &window) {
            self.order.push(window);
        }
    }

    /// Removes a window from the tiling order — used when moving a window
    /// to a different workspace (`spitfire.workspace.move_window`).
    pub fn remove(&mut self, window: &WindowElement) {
        self.order.retain(|w| w != window);
    }

    /// The windows currently managed by this workspace, in tiling order.
    /// Used to hide/show a workspace's windows when switching away from or
    /// back to it (see `SpitfireState::switch_workspace`).
    pub fn windows(&self) -> &[WindowElement] {
        &self.order
    }

    /// Reapplies the active layout to every managed window in the usable
    /// area of `output` — that area already excludes the exclusive zone
    /// reserved by layer-surfaces (a client bar, for example), via
    /// `layer_map_for_output`, and `bar_height` (the optional built-in bar,
    /// Phase 8, `spitfire.bar.enable` — 0 when it's off) on top of that,
    /// since it isn't a layer-surface and so isn't in that exclusive zone
    /// at all.
    ///
    /// Windows matched by a `spitfire.rule({ floating = true, ... })` are
    /// left out of the arrangement entirely — their geometry is never
    /// touched, same as full `Floating` mode.
    ///
    /// This also prunes dead windows from the tiling order: there is no
    /// dedicated "window destroyed" hook, so this runs every frame,
    /// alongside the `space.refresh()` call that already existed in
    /// anvil's render loop.
    pub fn arrange(
        &mut self,
        space: &mut Space<WindowElement>,
        output: &Output,
        rules: &[WindowRule],
        bar_height: i32,
        bar_margin: i32,
    ) {
        self.order.retain(|w| w.alive());
        if self.order.is_empty() {
            return;
        }

        let tiled: Vec<WindowElement> = self
            .order
            .iter()
            .filter(|w| !matches_floating_rule(w, rules))
            .cloned()
            .collect();
        if tiled.is_empty() {
            return;
        }

        let Some(output_geo) = space.output_geometry(output) else {
            return;
        };
        let zone = {
            let map = layer_map_for_output(output);
            map.non_exclusive_zone()
        };
        let mut area = Rect::new(
            output_geo.loc.x + zone.loc.x,
            output_geo.loc.y + zone.loc.y,
            zone.size.w,
            zone.size.h,
        );
        // The bar floats now (spitfire.gaps.outer inset on top/left/right,
        // see bar.rs), so the reserved strip is bar_margin (above it) +
        // bar_height + bar_margin (below it, before windows start) — not
        // just bar_height. bar_margin is 0 whenever the bar itself is (see
        // arrange_tiling), so this is a no-op with the bar disabled.
        let bar_reserved = if bar_height > 0 {
            bar_margin.max(0) + bar_height + bar_margin.max(0)
        } else {
            0
        }
        .min(area.h);
        area.y += bar_reserved;
        area.h -= bar_reserved;

        let Some(placements) = layout_arrange(&tiled, area, &self.params) else {
            // Floating layout mode: the engine deliberately doesn't touch
            // geometry — leave windows wherever the user put them (or
            // wherever place_new_window cascaded them initially).
            return;
        };

        for (window, rect) in placements {
            // A server-side-decorated window's on-screen footprint is its
            // content size *plus* HEADER_BAR_HEIGHT — see
            // shell/element.rs's AsRenderElements impl, which grows the
            // bbox by exactly that much and draws the header above the
            // content. `rect.h` here is the whole tile slot (content +
            // header), so the content itself has to be configured shorter
            // by that much, or the header sits on top of the full tile
            // height and the window ends up taller than its slot —
            // visibly overflowing past the next window/gap/screen edge.
            let ssd_height = if window.decoration_state().is_ssd {
                crate::shell::ssd::HEADER_BAR_HEIGHT
            } else {
                0
            };
            let size = Size::from((rect.w.max(1), (rect.h - ssd_height).max(1)));
            if let Some(toplevel) = window.0.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(size);
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
            debug!(?rect, "placing window");
            space.map_element(window, Point::from((rect.x, rect.y)), false);
        }
    }
}

/// Reads a window's current `app_id` (set by the client via
/// `xdg_toplevel.set_app_id`, may still be `None` right after mapping) and
/// checks it against the configured rules.
fn matches_floating_rule(window: &WindowElement, rules: &[WindowRule]) -> bool {
    let Some(surface) = window.wl_surface() else {
        return false;
    };
    let app_id = with_states(&surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .and_then(|attrs| attrs.app_id.clone())
    });
    rules
        .iter()
        .find(|rule| rule.matches(app_id.as_deref()))
        .is_some_and(|rule| rule.floating)
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    /// Reapplies the active workspace's layout on the first output — v1:
    /// a single output (matches the winit-only backend), so there's one
    /// `WorkspaceSet` rather than one per output. See `crate::workspace`.
    pub fn arrange_tiling(&mut self) {
        let Some(output) = self.space.outputs().next().cloned() else {
            return;
        };
        let rules = self.config.rules().clone();
        let bar_height = if self.config.bar.enabled {
            self.config.bar.height
        } else {
            0
        };
        let bar_margin = self.config.gaps.outer;
        self.workspaces.active_mut().tiling.arrange(
            &mut self.space,
            &output,
            &rules,
            bar_height,
            bar_margin,
        );
        self.refocus_if_dangling();
    }

    /// If keyboard focus is on nothing, or on a window that's gone (closed)
    /// or no longer part of the active workspace, hands it to whichever of
    /// the active workspace's windows was focused most recently before
    /// that — so closing the focused window falls back to the previous
    /// one (dwm/i3-style) instead of leaving focus on nothing. Runs every
    /// frame, right after the tiling order's own dead-window pruning above
    /// — same reasoning: there is no dedicated "window closed" hook.
    fn refocus_if_dangling(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };

        let focus_ok = match keyboard.current_focus() {
            Some(KeyboardFocusTarget::Window(w)) => {
                let window = WindowElement(w);
                window.alive() && self.workspaces.active().tiling.windows().contains(&window)
            }
            // Non-window focus (a layer-shell surface, a popup, the lock
            // screen, ...) is left alone — this is only about windows.
            Some(_) => return,
            None => false,
        };
        if focus_ok {
            return;
        }

        self.focus_history.retain(|w| w.alive());
        let active_windows = self.workspaces.active().tiling.windows();
        let Some(window) = self
            .focus_history
            .iter()
            .rev()
            .find(|w| active_windows.contains(w))
            .cloned()
        else {
            return;
        };

        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, Some(window.into()), serial);
    }
}
