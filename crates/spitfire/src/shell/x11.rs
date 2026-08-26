//! `XwmHandler`: the compositor-side half of XWayland support — the X11
//! window manager duties Smithay's `X11Wm` needs a host for (mapping,
//! configure requests, maximize/fullscreen, drag-move/resize, selection
//! forwarding to/from the Wayland clipboard). Adapted from anvil's own
//! `shell/x11.rs` — see `NOTICE.md`.
//!
//! Every X11 top-level ends up wrapped in the same [`WindowElement`] (via
//! `Window::new_x11_window`) that xdg-shell windows use, so once it's
//! mapped it's managed by the same tiling/border/focus code as anything
//! else — nothing downstream needs to know a window came from XWayland
//! rather than xdg-shell.

use std::{cell::RefCell, os::unix::io::OwnedFd};

use smithay::{
    desktop::{space::SpaceElement, Window},
    input::pointer::Focus,
    utils::{Logical, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        selection::{
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata,
                request_primary_client_selection, set_primary_selection,
            },
            SelectionTarget,
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge as X11ResizeEdge, WmWindowType, XwmId},
        X11Surface, X11Wm, XwmHandler,
    },
};
use tracing::{error, trace};

use crate::{
    focus::KeyboardFocusTarget,
    state::{Backend, SpitfireState},
};

use super::{
    place_new_window, FullscreenSurface, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab,
    ResizeData, ResizeState, SurfaceData, TouchMoveSurfaceGrab, WindowElement,
};

/// An X11 window's geometry before it was maximized/fullscreened, so
/// `unmaximize_request`/`unfullscreen_request` can restore it — X11 clients
/// don't remember this themselves the way xdg-shell's own state machine
/// does.
#[derive(Debug, Default)]
struct OldGeometry(RefCell<Option<Rectangle<i32, Logical>>>);
impl OldGeometry {
    pub fn save(&self, geo: Rectangle<i32, Logical>) {
        *self.0.borrow_mut() = Some(geo);
    }

    pub fn restore(&self) -> Option<Rectangle<i32, Logical>> {
        self.0.borrow_mut().take()
    }
}

impl<BackendData: Backend + 'static> XWaylandShellHandler for SpitfireState<BackendData> {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

impl<BackendData: Backend + 'static> XwmHandler for SpitfireState<BackendData> {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        window.set_mapped(true).unwrap();

        // A window that's positioned relative to something else (a menu
        // under the button that opened it, a submenu next to its parent, a
        // dialog centered on its owner) needs to end up wherever it
        // actually asked to be, not wherever `place_new_window`'s random
        // cascade or `TilingLayout::arrange`'s tiling puts it. Genuinely
        // override-redirect windows already get this — they never reach
        // this function at all, see `mapped_override_redirect_window` —
        // but plenty of real-world popups/menus (Steam's own CEF-based UI
        // among them) map as ordinary, non-override-redirect windows that
        // just happen to be transient-for their opener or carry an EWMH
        // `_NET_WM_WINDOW_TYPE` marking them as a menu/tooltip/notification.
        // Treating those the same way here — keep their own geometry,
        // don't tile them — is what actually fixes "Steam's menu opens in
        // the wrong place" (a plain floating-window fix doesn't cover it,
        // since without this check it's `place_new_window` and then
        // `TilingLayout::arrange` fighting over where the window sits, not
        // just one or the other).
        if is_positioned_by_client(&window) {
            let location = window.geometry().loc;
            let window = WindowElement(Window::new_x11_window(window));
            self.space.map_element(window, location, true);
            return;
        }

        let window = WindowElement(Window::new_x11_window(window));
        place_new_window(
            &mut self.space,
            self.pointer.current_location(),
            &window,
            true,
        );
        // Phase 5: joins the active workspace's tiling order — same as a
        // Wayland toplevel does in shell/xdg.rs's `new_toplevel`. Without
        // this, `hide_inactive_workspaces` (workspace.rs) has no idea this
        // window exists (it only ever looks at each workspace's `tiling`
        // list), so it never gets unmapped when switching away: an
        // XWayland window would stay mapped in `self.space` and visible on
        // every single workspace forever, instead of just the one it
        // opened on.
        self.workspaces.active_mut().tiling.push(window.clone());
        let bbox = self.space.element_bbox(&window).unwrap();
        let Some(xsurface) = window.0.x11_surface() else {
            unreachable!()
        };
        xsurface.configure(Some(bbox)).unwrap();
        // Deliberately never SSD an X11 window (unlike xdg-shell's
        // `new_toplevel` in shell/xdg.rs, which does base this on whether
        // the client asked for its own decorations). A Wayland client has
        // no idea where it sits on screen, so a header bar composited
        // above/around its surface — extra space added only to this
        // WindowElement's own bbox, never told to the client — is
        // invisible to it and harmless. An X11 client, by contrast, *is*
        // its own on-screen coordinate system: `xsurface.configure()`
        // above already told the real X11 window (and hence Xwayland's own
        // bookkeeping of it) its exact, un-inflated position and size. Any
        // header height added only on our side afterward is space the X11
        // window itself is never aware of, so `XTranslateCoordinates` (or
        // any other absolute-position query) returns coordinates that
        // don't match where we actually render its content — every
        // popup/menu the client positions off of that (Steam's own
        // CEF-based context menus among them) lands off-target by exactly
        // the header height, no matter the window's on-screen position or
        // the output's scale. Matches wasp (our dwl fork): it never draws
        // SSD for XWayland clients at all, only for xdg-shell ones via the
        // wlr-xdg-decoration protocol.
        window.set_ssd(false);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let location = window.geometry().loc;
        let window = WindowElement(Window::new_x11_window(window));
        self.space.map_element(window, location, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let maybe = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned();
        if let Some(elem) = maybe {
            self.space.unmap_elem(&elem);
            // Mirrors the push in `map_window_request` above — without
            // this, the tiling order keeps a dangling entry for a window
            // that's now unmapped (not necessarily dead: an X11 client can
            // remap the same surface later), which `TilingLayout::arrange`'s
            // per-frame pruning wouldn't catch on its own since that only
            // drops windows that are no longer `alive()`.
            for ws in self.workspaces.iter_mut() {
                ws.tiling.remove(&elem);
            }
        }
        if !window.is_override_redirect() {
            window.set_mapped(false).unwrap();
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // Override-redirect windows (menus, dropdowns, tooltips) position
        // and size themselves directly over X11 — the window manager isn't
        // in the loop for them by definition (that's what "override
        // redirect" means). Confirmed in smithay's own source
        // (xwayland/xwm/surface.rs): `X11Surface::configure()` immediately
        // errors out and does *nothing* — no XCB request goes out at all —
        // if called with `Some(rect)` on one of these. The code below used
        // to call it unconditionally and silently drop that `Err` via
        // `let _ =`, meaning every override-redirect ConfigureRequest —
        // Steam's own menu asking to be placed under the button that
        // opened it, a submenu asking to be placed next to its parent —
        // was a complete no-op: the window just stayed wherever it was
        // originally created (the screen's top-left corner), and hovering
        // into a submenu did nothing since its own "move over here"
        // request was silently swallowed the exact same way.
        //
        // These windows still end up wherever they asked to be regardless
        // of what we do here — the X server applies their own
        // ConfigureWindow request immediately and reports the result via
        // ConfigureNotify either way, which `configure_notify` below is
        // what actually keeps `self.space` in sync with. There's nothing
        // left for us to usefully do with a ConfigureRequest from one.
        //
        // `is_positioned_by_client` extends the same treatment to
        // non-override-redirect windows that are still clearly
        // self-positioned — transient-for another window, or EWMH-typed
        // as a menu/tooltip/notification — for the same reason
        // `map_window_request` skips tiling them: see that function's
        // doc comment.
        if window.is_override_redirect() || is_positioned_by_client(&window) {
            return;
        }

        // Just set the new size, but don't let windows move themselves
        // around freely — same tiling-friendly restriction xdg-shell
        // windows are already under.
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned()
        else {
            return;
        };
        // `Space::map_element` also raises an already-mapped element.  X11
        // clients can send ConfigureNotify without having moved (notably
        // while their activation state changes), so mapping unconditionally
        // here makes an unrelated XWayland window jump above a newly shown
        // Wayland scratchpad.  The X11 surface itself owns its size; `Space`
        // only needs updating when its compositor-side location changed.
        if self.space.element_location(&elem) != Some(geometry.loc) {
            self.space.map_element(elem, geometry.loc, false);
            self.raise_floating_windows();
        }
        // Override-redirect window stacking order isn't tracked here —
        // they're always mapped on top and never reordered afterward. Same
        // known limitation anvil ships with.
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.maximize_request_x11(&window);
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned()
        else {
            return;
        };

        window.set_maximized(false).unwrap();
        if let Some(old_geo) = window
            .user_data()
            .get::<OldGeometry>()
            .and_then(|data| data.restore())
        {
            window.configure(old_geo).unwrap();
            self.space.map_element(elem, old_geo.loc, false);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        {
            let outputs_for_window = self.space.outputs_for_element(elem);
            let output = outputs_for_window
                .first()
                // The window hasn't been mapped yet, use the primary output instead.
                .or_else(|| self.space.outputs().next())
                // Assumes that at least one output exists.
                .expect("No outputs found");
            let geometry = self.space.output_geometry(output).unwrap();

            window.set_fullscreen(true).unwrap();
            elem.set_ssd(false);
            window.configure(geometry).unwrap();
            output
                .user_data()
                .insert_if_missing(FullscreenSurface::default);
            output
                .user_data()
                .get::<FullscreenSurface>()
                .unwrap()
                .set(elem.clone());
            trace!("Fullscreening: {:?}", elem);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        {
            window.set_fullscreen(false).unwrap();
            // Never SSD an X11 window — see the doc comment on
            // `set_ssd(false)` in `map_window_request` above.
            elem.set_ssd(false);
            if let Some(output) = self.space.outputs().find(|o| {
                o.user_data()
                    .get::<FullscreenSurface>()
                    .and_then(|f| f.get())
                    .map(|w| &w == elem)
                    .unwrap_or(false)
            }) {
                trace!("Unfullscreening: {:?}", elem);
                output
                    .user_data()
                    .get::<FullscreenSurface>()
                    .unwrap()
                    .clear();
                window.configure(self.space.element_bbox(elem)).unwrap();
                self.backend_data.reset_buffers(output);
            }
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        edges: X11ResizeEdge,
    ) {
        // Single-seat only, same as the rest of spitfire so far.
        let start_data = self.pointer.grab_start_data().unwrap();

        let Some(element) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        else {
            return;
        };

        let geometry = element.geometry();
        let loc = self.space.element_location(element).unwrap();
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        with_states(&element.wl_surface().unwrap(), move |states| {
            states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut()
                .resize_state = ResizeState::Resizing(ResizeData {
                edges: edges.into(),
                initial_window_location,
                initial_window_size,
            });
        });

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window: element.clone(),
            edges: edges.into(),
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        self.move_request_x11(&window)
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        if let Some(keyboard) = self.seat.get_keyboard() {
            // Only forward the selection while an X11 window actually has
            // keyboard focus.
            if let Some(KeyboardFocusTarget::Window(w)) = keyboard.current_focus() {
                if let Some(surface) = w.x11_surface() {
                    if surface.xwm_id().unwrap() == xwm {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    error!(
                        ?err,
                        "Failed to request current Wayland clipboard for XWayland"
                    );
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    error!(
                        ?err,
                        "Failed to request current Wayland primary selection for XWayland"
                    );
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        trace!(?selection, ?mime_types, "Got selection from X11");
        // TODO: verify the currently-focused window is actually an X11
        // window before accepting this (matches anvil's own known gap).
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types, ())
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types, ())
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    pub fn maximize_request_x11(&mut self, window: &X11Surface) {
        let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == window))
            .cloned()
        else {
            return;
        };

        let old_geo = self.space.element_bbox(&elem).unwrap();
        let outputs_for_window = self.space.outputs_for_element(&elem);
        let output = outputs_for_window
            .first()
            // The window hasn't been mapped yet, use the primary output instead.
            .or_else(|| self.space.outputs().next())
            // Assumes that at least one output exists.
            .expect("No outputs found");
        let geometry = self.space.output_geometry(output).unwrap();

        window.set_maximized(true).unwrap();
        window.configure(geometry).unwrap();
        window.user_data().insert_if_missing(OldGeometry::default);
        window
            .user_data()
            .get::<OldGeometry>()
            .unwrap()
            .save(old_geo);
        self.space.map_element(elem, geometry.loc, false);
    }

    pub fn move_request_x11(&mut self, window: &X11Surface) {
        if let Some(touch) = self.seat.get_touch() {
            if let Some(start_data) = touch.grab_start_data() {
                let element = self
                    .space
                    .elements()
                    .find(|e| matches!(e.0.x11_surface(), Some(w) if w == window));

                if let Some(element) = element {
                    let mut initial_window_location = self.space.element_location(element).unwrap();

                    // If the surface is maximized, unmaximize it first.
                    if window.is_maximized() {
                        window.set_maximized(false).unwrap();
                        let pos = start_data.location;
                        initial_window_location = (pos.x as i32, pos.y as i32).into();
                        if let Some(old_geo) = window
                            .user_data()
                            .get::<OldGeometry>()
                            .and_then(|data| data.restore())
                        {
                            window
                                .configure(Rectangle::new(initial_window_location, old_geo.size))
                                .unwrap();
                        }
                    }

                    let grab = TouchMoveSurfaceGrab {
                        start_data,
                        window: element.clone(),
                        initial_window_location,
                    };

                    touch.set_grab(self, grab, SERIAL_COUNTER.next_serial());
                    return;
                }
            }
        }

        // Single-seat only, same as the rest of spitfire so far.
        let Some(start_data) = self.pointer.grab_start_data() else {
            return;
        };

        let Some(element) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == window))
        else {
            return;
        };

        let mut initial_window_location = self.space.element_location(element).unwrap();

        // If the surface is maximized, unmaximize it first.
        if window.is_maximized() {
            window.set_maximized(false).unwrap();
            let pos = self.pointer.current_location();
            initial_window_location = (pos.x as i32, pos.y as i32).into();
            if let Some(old_geo) = window
                .user_data()
                .get::<OldGeometry>()
                .and_then(|data| data.restore())
            {
                window
                    .configure(Rectangle::new(initial_window_location, old_geo.size))
                    .unwrap();
            }
        }

        let grab = PointerMoveSurfaceGrab {
            start_data,
            window: element.clone(),
            initial_window_location,
        };

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }
}

/// Whether `window` positions itself and should be left alone by
/// `place_new_window`'s cascade and `TilingLayout::arrange`'s tiling —
/// true override-redirect windows already get this treatment
/// unconditionally (they never reach `map_window_request`/
/// `configure_request` at all, see `mapped_override_redirect_window`), but
/// plenty of real popups/menus/dialogs (Steam's own CEF-based UI among
/// them) map as ordinary windows instead. `is_transient_for` covers dialogs
/// positioned relative to their owner; the EWMH window-type check covers
/// menus, dropdowns, tooltips and notifications, which are positioned
/// relative to whatever opened them rather than the owner window itself.
fn is_positioned_by_client(window: &X11Surface) -> bool {
    window.is_transient_for().is_some()
        || matches!(
            window.window_type(),
            Some(
                WmWindowType::Menu
                    | WmWindowType::DropdownMenu
                    | WmWindowType::PopupMenu
                    | WmWindowType::Tooltip
                    | WmWindowType::Notification
            )
        )
}
