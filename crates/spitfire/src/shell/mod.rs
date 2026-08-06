use std::cell::RefCell;

#[cfg(feature = "xwayland")]
use smithay::xwayland::XWaylandClientData;

#[cfg(feature = "udev")]
use smithay::wayland::drm_syncobj::DrmSyncobjCachedState;

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    desktop::{
        layer_map_for_output, space::SpaceElement, LayerSurface, PopupKind, PopupManager, Space,
        WindowSurfaceType,
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    reexports::{
        calloop::Interest,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::{IsAlive, Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            add_blocker, add_pre_commit_hook, get_parent, is_sync_subsurface, with_states,
            with_surface_tree_upward, BufferAssignment, CompositorClientState, CompositorHandler,
            CompositorState, SurfaceAttributes, TraversalAction,
        },
        dmabuf::get_dmabuf,
        session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::XdgToplevelSurfaceData,
        },
    },
};

use crate::{
    focus::KeyboardFocusTarget,
    layout::usable_area,
    state::{Backend, SpitfireState},
    ClientState,
};
use spitfire_config::WindowRule;

mod element;
mod grabs;
pub(crate) mod ssd;
#[cfg(feature = "xwayland")]
mod x11;
mod xdg;

pub use self::element::*;
pub use self::grabs::*;

fn fullscreen_output_geometry(
    wl_surface: &WlSurface,
    wl_output: Option<&wl_output::WlOutput>,
    space: &mut Space<WindowElement>,
) -> Option<Rectangle<i32, Logical>> {
    // First test if a specific output has been requested
    // if the requested output is not found ignore the request
    wl_output
        .and_then(Output::from_resource)
        .or_else(|| {
            let w = space.elements().find(|window| {
                window
                    .wl_surface()
                    .map(|s| &*s == wl_surface)
                    .unwrap_or(false)
            });
            w.and_then(|w| space.outputs_for_element(w).first().cloned())
        })
        .as_ref()
        .and_then(|o| space.output_geometry(o))
}

#[derive(Default)]
pub struct FullscreenSurface(RefCell<Option<WindowElement>>);

impl FullscreenSurface {
    pub fn set(&self, window: WindowElement) {
        *self.0.borrow_mut() = Some(window);
    }

    pub fn get(&self) -> Option<WindowElement> {
        let mut window = self.0.borrow_mut();
        if window.as_ref().map(|w| !w.alive()).unwrap_or(false) {
            *window = None;
        }
        window.clone()
    }

    pub fn clear(&self) -> Option<WindowElement> {
        self.0.borrow_mut().take()
    }
}

impl<BackendData: Backend> BufferHandler for SpitfireState<BackendData> {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl<BackendData: Backend> CompositorHandler for SpitfireState<BackendData> {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        #[cfg(feature = "xwayland")]
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        panic!("Unknown client data type")
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, move |state, _dh, surface| {
            #[cfg(feature = "udev")]
            let mut acquire_point = None;
            let maybe_dmabuf = with_states(surface, |surface_data| {
                #[cfg(feature = "udev")]
                acquire_point.clone_from(
                    &surface_data
                        .cached_state
                        .get::<DrmSyncobjCachedState>()
                        .pending()
                        .acquire_point,
                );
                surface_data
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                        _ => None,
                    })
            });
            if let Some(dmabuf) = maybe_dmabuf {
                #[cfg(feature = "udev")]
                if let Some(acquire_point) = acquire_point {
                    if let Ok((blocker, source)) = acquire_point.generate_blocker() {
                        let client = surface.client().unwrap();
                        let res = state.handle.insert_source(source, move |_, _, data| {
                            let dh = data.display_handle.clone();
                            data.client_compositor_state(&client)
                                .blocker_cleared(data, &dh);
                            Ok(())
                        });
                        if res.is_ok() {
                            add_blocker(surface, blocker);
                            return;
                        }
                    }
                }
                if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                    if let Some(client) = surface.client() {
                        let res = state.handle.insert_source(source, move |_, _, data| {
                            let dh = data.display_handle.clone();
                            data.client_compositor_state(&client)
                                .blocker_cleared(data, &dh);
                            Ok(())
                        });
                        if res.is_ok() {
                            add_blocker(surface, blocker);
                        }
                    }
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.backend_data.early_import(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.0.on_commit();

                if &root == surface {
                    let buffer_offset = with_states(surface, |states| {
                        states
                            .cached_state
                            .get::<SurfaceAttributes>()
                            .current()
                            .buffer_delta
                            .take()
                    });

                    if let Some(buffer_offset) = buffer_offset {
                        let current_loc = self.space.element_location(&window).unwrap();
                        self.space
                            .map_element(window.clone(), current_loc + buffer_offset, false);
                    }

                    // First-map focus: the moment a queued toplevel (see
                    // `new_toplevel` in shell/xdg.rs) actually commits a
                    // buffer, it's visibly "open" — hand it keyboard focus
                    // right away instead of leaving the previous window
                    // focused until the pointer happens to hover/click the
                    // new one. Skipped while locked: the lock surface keeps
                    // focus no matter what maps underneath it.
                    if let Some(pos) = self
                        .pending_initial_focus
                        .iter()
                        .position(|s| s == surface)
                    {
                        // NB: `SurfaceAttributes.buffer` is *not* usable
                        // here — `on_commit_buffer_handler` above already
                        // `take()`s it into the renderer's own
                        // `RendererSurfaceState` on every commit, so by
                        // this point it's always back to `None` (that bit
                        // me on the first attempt at this check: it never
                        // fired). `RendererSurfaceState::buffer()` is the
                        // durable copy that actually reflects "does this
                        // surface have contents right now".
                        let has_buffer = with_renderer_surface_state(surface, |state| {
                            state.buffer().is_some()
                        })
                        .unwrap_or(false);
                        if has_buffer {
                            self.pending_initial_focus.remove(pos);

                            // spitfire.rule({ floating = true, centered =
                            // true }): override place_new_window's random
                            // cascade now that the window's real size (via
                            // this first buffer) is known. See
                            // `center_if_ruled`'s docs for why this is the
                            // right point to do it. Checked ahead of that:
                            // `spitfire.scratchpad.toggle(...)`'s own
                            // "waiting for this app_id to map" claim — see
                            // `claim_pending_named_scratchpad`'s doc
                            // comment for why it lives here instead of
                            // going through a `spitfire.rule` match too.
                            let output = self.space.outputs().next().cloned();
                            if let Some(output) = output {
                                let bar_height = if self.config.bar.enabled {
                                    self.config.bar.height
                                } else {
                                    0
                                };
                                let bar_margin = self.config.gaps.outer;
                                let claimed = self.claim_pending_named_scratchpad(
                                    &window,
                                    &output,
                                    bar_height,
                                    bar_margin,
                                );
                                if !claimed {
                                    let rules = self.config.rules().clone();
                                    center_if_ruled(
                                        &mut self.space,
                                        &output,
                                        &window,
                                        &rules,
                                        bar_height,
                                        bar_margin,
                                    );
                                }
                            }

                            if !self.locked {
                                if let Some(keyboard) = self.seat.get_keyboard() {
                                    keyboard.set_focus(
                                        self,
                                        Some(KeyboardFocusTarget::from(window)),
                                        SERIAL_COUNTER.next_serial(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        self.popups.commit(surface);

        if matches!(&self.cursor_status, CursorImageStatus::Surface(cursor_surface) if cursor_surface == surface)
        {
            with_states(surface, |states| {
                let cursor_image_attributes = states.data_map.get::<CursorImageSurfaceData>();

                if let Some(mut cursor_image_attributes) =
                    cursor_image_attributes.map(|attrs| attrs.lock().unwrap())
                {
                    let buffer_delta = states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer_delta
                        .take();
                    if let Some(buffer_delta) = buffer_delta {
                        tracing::trace!(hotspot = ?cursor_image_attributes.hotspot, ?buffer_delta, "decrementing cursor hotspot");
                        cursor_image_attributes.hotspot -= buffer_delta;
                    }
                }
            });
        }

        if matches!(&self.dnd_icon, Some(icon) if &icon.surface == surface) {
            let dnd_icon = self.dnd_icon.as_mut().unwrap();
            with_states(&dnd_icon.surface, |states| {
                let buffer_delta = states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .buffer_delta
                    .take()
                    .unwrap_or_default();
                tracing::trace!(offset = ?dnd_icon.offset, ?buffer_delta, "moving dnd offset");
                dnd_icon.offset += buffer_delta;
            });
        }

        ensure_initial_configure(surface, &self.space, &mut self.popups)
    }
}

impl<BackendData: Backend> WlrLayerShellHandler for SpitfireState<BackendData> {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.space.outputs().next().unwrap().clone());
        let mut map = layer_map_for_output(&output);
        map.map_layer(&LayerSurface::new(surface, namespace))
            .unwrap();
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        if let Some((mut map, layer)) = self.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|&layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (map, layer))
        }) {
            map.unmap_layer(&layer);
        }
    }
}

impl<BackendData: Backend> SessionLockHandler for SpitfireState<BackendData> {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        tracing::info!("Session lock requested");
        self.locked = true;
        self.lock_surfaces.clear();

        // v1: no separate "cleared frame presented on every output" sync —
        // confirm right away. The render loop switches to lock-only
        // rendering the instant `self.locked` flips (see render.rs), so the
        // worst case is one stale frame still on screen before that.
        confirmation.lock();
    }

    fn unlock(&mut self) {
        tracing::info!("Session unlocked");
        self.locked = false;
        self.lock_surfaces.clear();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: wl_output::WlOutput) {
        if let Some(output) = Output::from_resource(&wl_output) {
            if let Some(geometry) = self.space.output_geometry(&output) {
                surface.with_pending_state(|state| {
                    state.size = Some(Size::from((
                        geometry.size.w.max(0) as u32,
                        geometry.size.h.max(0) as u32,
                    )));
                });
            }
        }
        surface.send_configure();

        // Grab keyboard focus for password entry — while locked, this is
        // the only surface that should ever see a keypress.
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(
                self,
                Some(KeyboardFocusTarget::from(surface.clone())),
                SERIAL_COUNTER.next_serial(),
            );
        }

        self.lock_surfaces.push(surface);
    }
}

impl<BackendData: Backend> SpitfireState<BackendData> {
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<WindowElement> {
        self.space
            .elements()
            .find(|window| window.wl_surface().map(|s| &*s == surface).unwrap_or(false))
            .cloned()
    }
}

#[derive(Default)]
pub struct SurfaceData {
    pub geometry: Option<Rectangle<i32, Logical>>,
    pub resize_state: ResizeState,
}

fn ensure_initial_configure(
    surface: &WlSurface,
    space: &Space<WindowElement>,
    popups: &mut PopupManager,
) {
    with_surface_tree_upward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| {
            states
                .data_map
                .insert_if_missing(|| RefCell::new(SurfaceData::default()));
        },
        |_, _, _| true,
    );

    if let Some(window) = space
        .elements()
        .find(|window| window.wl_surface().map(|s| &*s == surface).unwrap_or(false))
        .cloned()
    {
        // send the initial configure if relevant
        #[cfg_attr(not(feature = "xwayland"), allow(irrefutable_let_patterns))]
        if let Some(toplevel) = window.0.toplevel() {
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .initial_configure_sent
            });
            if !initial_configure_sent {
                toplevel.send_configure();
            }
        }

        with_states(surface, |states| {
            let mut data = states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut();

            // Finish resizing.
            if let ResizeState::WaitingForCommit(_) = data.resize_state {
                data.resize_state = ResizeState::NotResizing;
            }
        });

        return;
    }

    if let Some(popup) = popups.find_popup(surface) {
        let popup = match popup {
            PopupKind::Xdg(ref popup) => popup,
            // Doesn't require configure
            PopupKind::InputMethod(ref _input_popup) => {
                return;
            }
        };

        if !popup.is_initial_configure_sent() {
            // NOTE: This should never fail as the initial configure is always
            // allowed.
            popup.send_configure().expect("initial configure failed");
        }

        return;
    };

    if let Some(output) = space.outputs().find(|o| {
        let map = layer_map_for_output(o);
        map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some()
    }) {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        let mut map = layer_map_for_output(output);

        // arrange the layers before sending the initial configure
        // to respect any size the client may have sent
        map.arrange();
        // send the initial configure if relevant
        if !initial_configure_sent {
            let layer = map
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .unwrap();

            layer.layer_surface().send_configure();
        }
    };
}

fn place_new_window(
    space: &mut Space<WindowElement>,
    pointer_location: Point<f64, Logical>,
    window: &WindowElement,
    activate: bool,
) {
    // place the window at a random location on same output as pointer
    // or if there is not output in a [0;800]x[0;800] square
    use rand::distributions::{Distribution, Uniform};

    let output = space
        .output_under(pointer_location)
        .next()
        .or_else(|| space.outputs().next())
        .cloned();
    let output_geometry = output
        .and_then(|o| {
            let geo = space.output_geometry(&o)?;
            let map = layer_map_for_output(&o);
            let zone = map.non_exclusive_zone();
            Some(Rectangle::new(geo.loc + zone.loc, zone.size))
        })
        .unwrap_or_else(|| Rectangle::from_size((800, 800).into()));

    // set the initial toplevel bounds
    #[allow(irrefutable_let_patterns)]
    if let Some(toplevel) = window.0.toplevel() {
        toplevel.with_pending_state(|state| {
            state.bounds = Some(output_geometry.size);
        });
    }

    let max_x = output_geometry.loc.x + (((output_geometry.size.w as f32) / 3.0) * 2.0) as i32;
    let max_y = output_geometry.loc.y + (((output_geometry.size.h as f32) / 3.0) * 2.0) as i32;
    let x_range = Uniform::new(output_geometry.loc.x, max_x);
    let y_range = Uniform::new(output_geometry.loc.y, max_y);
    let mut rng = rand::thread_rng();
    let x = x_range.sample(&mut rng);
    let y = y_range.sample(&mut rng);

    space.map_element(window.clone(), (x, y), activate);
}

/// Moves `window` to the middle of `output`'s usable area (see
/// `layout::usable_area`), using its current `bbox().size`. Shared by
/// `center_if_ruled` below (a one-shot placement for `spitfire.rule({
/// floating = true, centered = true })` windows) and
/// `SpitfireState::toggle_scratchpad` (workspace.rs), which re-centers the
/// scratchpad window every time it's shown. Returns `false` (and does
/// nothing) if `output` has no usable area to speak of.
pub(crate) fn center_on_output(
    space: &mut Space<WindowElement>,
    output: &Output,
    window: &WindowElement,
    bar_height: i32,
    bar_margin: i32,
) -> bool {
    center_on_output_with_size(space, output, window, window.bbox().size, bar_height, bar_margin)
}

/// Same as `center_on_output`, but centers around an explicit `size`
/// instead of reading `window.bbox().size` — for a caller that's *also*
/// resizing the window in the same breath (see
/// `SpitfireState::claim_pending_named_scratchpad`'s
/// `spitfire.scratchpad.toggle(..., width_frac, height_frac)` handling):
/// `bbox()` still reflects whatever size the client committed most
/// recently, not the size a configure just asked it to resize to (that
/// only becomes true once the client redraws and commits again), so
/// centering from `bbox()` right after sending that configure would
/// center around the *old* size for one frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn center_on_output_with_size(
    space: &mut Space<WindowElement>,
    output: &Output,
    window: &WindowElement,
    size: Size<i32, Logical>,
    bar_height: i32,
    bar_margin: i32,
) -> bool {
    let Some(area) = usable_area(space, output, bar_height, bar_margin) else {
        return false;
    };
    let x = area.loc.x + (area.size.w - size.w).max(0) / 2;
    let y = area.loc.y + (area.size.h - size.h).max(0) / 2;
    space.map_element(window.clone(), (x, y), false);
    true
}

/// If `window` is matched by a `spitfire.rule({ floating = true, centered =
/// true })` rule, moves it to the middle of `output`'s usable area —
/// overriding wherever `place_new_window`'s random cascade put it. A no-op
/// for anything else, including a `floating = true` rule without
/// `centered`.
///
/// Called once, right when a window's first real buffer commits (see
/// `commit()` below) — like `floating` itself, this is a one-shot
/// placement, not a constraint kept up over the window's lifetime: resizing
/// the output afterwards won't re-center it.
pub(crate) fn center_if_ruled(
    space: &mut Space<WindowElement>,
    output: &Output,
    window: &WindowElement,
    rules: &[WindowRule],
    bar_height: i32,
    bar_margin: i32,
) {
    let app_id = window.app_id();
    let centered = rules
        .iter()
        .find(|rule| rule.matches(app_id.as_deref()))
        .is_some_and(|rule| rule.floating && rule.centered);
    if !centered {
        return;
    }
    center_on_output(space, output, window, bar_height, bar_margin);
}

pub fn fixup_positions(space: &mut Space<WindowElement>, pointer_location: Point<f64, Logical>) {
    // fixup outputs
    let mut offset = Point::<i32, Logical>::from((0, 0));
    for output in space.outputs().cloned().collect::<Vec<_>>().into_iter() {
        let size = space
            .output_geometry(&output)
            .map(|geo| geo.size)
            .unwrap_or_else(|| Size::from((0, 0)));
        space.map_output(&output, offset);
        layer_map_for_output(&output).arrange();
        offset.x += size.w;
    }

    // fixup windows
    let mut orphaned_windows = Vec::new();
    let outputs = space
        .outputs()
        .flat_map(|o| {
            let geo = space.output_geometry(o)?;
            let map = layer_map_for_output(o);
            let zone = map.non_exclusive_zone();
            Some(Rectangle::new(geo.loc + zone.loc, zone.size))
        })
        .collect::<Vec<_>>();
    for window in space.elements() {
        let window_location = match space.element_location(window) {
            Some(loc) => loc,
            None => continue,
        };
        let geo_loc = window.bbox().loc + window_location;

        if !outputs.iter().any(|o_geo| o_geo.contains(geo_loc)) {
            orphaned_windows.push(window.clone());
        }
    }
    for window in orphaned_windows.into_iter() {
        place_new_window(space, pointer_location, &window, false);
    }
}
