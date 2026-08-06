//! Bridges the pure layout engine (`spitfire_layout`) to real compositor
//! state: keeps the tiling order of a *single workspace's* windows and
//! applies the active layout via `Space`/`xdg_toplevel.configure`. See
//! `crate::workspace` for the list-of-workspaces model built on top of
//! this (Phase 5) — each `Workspace` owns one `TilingLayout`.

use smithay::{
    desktop::{layer_map_for_output, Space},
    output::Output,
    utils::{IsAlive, Logical, Point, Rectangle, Size, SERIAL_COUNTER},
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
        pending_scratchpad_app_ids: &[String],
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
            .filter(|w| !matches_floating_rule(w, rules, pending_scratchpad_app_ids))
            .cloned()
            .collect();
        if tiled.is_empty() {
            return;
        }

        let Some(area) = usable_area(space, output, bar_height, bar_margin) else {
            return;
        };
        let area = Rect::new(area.loc.x, area.loc.y, area.size.w, area.size.h);

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
            // `Space::map_element` unconditionally moves the element to the
            // top of the z-order — that's true even when it's already
            // mapped and only its location is being updated (see its own
            // smithay doc comment: "move it to top of the stack ... can
            // safely be called on an already mapped window to update its
            // location"). This `arrange` runs every frame (see this fn's
            // doc comment above), so calling it unconditionally here would
            // re-raise every tiled window on every single frame forever —
            // which permanently buries any floating window behind them the
            // instant a tiled window's next `arrange` pass runs, typically
            // the very next frame after the floating window opened. Only
            // remap when the position actually changed, so a tiled window
            // that isn't moving doesn't keep stealing the top of the stack
            // from whatever's floating above it.
            let target_loc = Point::from((rect.x, rect.y));
            if space.element_location(&window) != Some(target_loc) {
                debug!(?rect, "placing window");
                space.map_element(window, target_loc, false);
            }
        }
    }
}

/// Marks a window as floating regardless of whether any `spitfire.rule`
/// matches its `app_id` — for windows that need excluding from tiling at
/// runtime rather than by `app_id`, e.g. `SpitfireState::toggle_scratchpad`'s
/// window while it's shown: it can be *any* app the user happened to have
/// focused when they stashed it, so it won't reliably match a `floating =
/// true` rule, but still needs the exact same "leave its geometry alone"
/// treatment a rule-matched floating window gets — otherwise the very next
/// `arrange` pass below would fold it right back into the tiling grid,
/// undoing wherever it was just centered to.
pub(crate) struct ForceFloating;

/// Checks a window's current `app_id` (see `WindowElement::app_id`, may
/// still be `None` right after mapping) against the configured rules, a
/// `ForceFloating` marker (see its own doc comment), or
/// `pending_scratchpad_app_ids` — `app_id`s a
/// `spitfire.scratchpad.toggle(...)` call is currently waiting to claim
/// (see `SpitfireState::claim_pending_named_scratchpad`).
///
/// That last check matters for timing, not just correctness: a
/// newly-spawned scratchpad window's `app_id` becomes known (via
/// `set_app_id`) well before it commits its first buffer, and this
/// `arrange` runs every frame — so without it, a scratchpad window would
/// sit in the *tiled* set for however many frames pass in between, getting
/// a real tile-slot size `send_pending_configure`d to it before
/// `claim_pending_named_scratchpad` (which only runs at that first-buffer
/// commit) ever gets a chance to mark it `ForceFloating`. A terminal
/// generally honors whatever size it's told before it ever draws, so that
/// tile-slot size is exactly what it opens at — e.g. the full height of
/// the usable area, if it was briefly the tiling set's only window.
/// Checking here closes that window (no pun intended) instead of relying
/// on `ForceFloating` alone, which only ever prevents *future* passes from
/// re-tiling it, not the one that already sent a bad initial size.
fn matches_floating_rule(
    window: &WindowElement,
    rules: &[WindowRule],
    pending_scratchpad_app_ids: &[String],
) -> bool {
    if window.0.user_data().get::<ForceFloating>().is_some() {
        return true;
    }
    let app_id = window.app_id();
    if let Some(app_id) = app_id.as_deref() {
        if pending_scratchpad_app_ids.iter().any(|id| id == app_id) {
            return true;
        }
    }
    rules
        .iter()
        .find(|rule| rule.matches(app_id.as_deref()))
        .is_some_and(|rule| rule.floating)
}

/// The portion of `output` windows should actually occupy: its full
/// geometry minus whatever `layer_map_for_output` reserves as an exclusive
/// zone (a layer-shell bar, typically) and minus the built-in bar's own
/// reserved strip — it isn't a layer-surface, so it isn't in that exclusive
/// zone at all (see `bar.rs`). `bar_height`/`bar_margin` are 0 whenever the
/// built-in bar itself is off, which makes that part a no-op.
///
/// Shared by the tiling `arrange` pass above and anything else that needs
/// "the usable screen area" to place a window relative to it — via
/// `shell::center_on_output`, that's a `spitfire.rule({ floating = true,
/// centered = true })` window's one-shot initial placement
/// (`shell::center_if_ruled`) and `SpitfireState::toggle_scratchpad`'s
/// window, re-centered every time it's shown.
pub(crate) fn usable_area(
    space: &Space<WindowElement>,
    output: &Output,
    bar_height: i32,
    bar_margin: i32,
) -> Option<Rectangle<i32, Logical>> {
    let output_geo = space.output_geometry(output)?;
    let zone = layer_map_for_output(output).non_exclusive_zone();
    let mut area = Rectangle::new(output_geo.loc + zone.loc, zone.size);
    // The bar floats now (spitfire.gaps.outer inset on top/left/right, see
    // bar.rs), so the reserved strip is bar_margin (above it) + bar_height +
    // bar_margin (below it, before windows start) — not just bar_height.
    let bar_reserved = if bar_height > 0 {
        bar_margin.max(0) + bar_height + bar_margin.max(0)
    } else {
        0
    }
    .min(area.size.h);
    area.loc.y += bar_reserved;
    area.size.h -= bar_reserved;
    Some(area)
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
        let pending_scratchpad_app_ids: Vec<String> = self
            .named_scratchpads
            .values()
            .filter_map(|slot| match slot {
                crate::workspace::NamedScratchpad::Pending { app_id, .. } => Some(app_id.clone()),
                _ => None,
            })
            .collect();
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
            &pending_scratchpad_app_ids,
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
