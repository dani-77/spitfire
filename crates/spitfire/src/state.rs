use std::{
    collections::HashMap,
    os::unix::io::OwnedFd,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use tracing::{error, info, warn};

use smithay::{
    backend::{
        input::TabletToolDescriptor,
        renderer::element::{
            default_primary_scanout_output_compare, utils::select_dmabuf_feedback,
            RenderElementStates,
        },
    },
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_fractional_scale,
    delegate_input_method_manager, delegate_keyboard_shortcuts_inhibit, delegate_layer_shell,
    delegate_output, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_presentation, delegate_primary_selection, delegate_relative_pointer, delegate_seat,
    delegate_security_context, delegate_session_lock, delegate_shm, delegate_tablet_manager,
    delegate_text_input_manager, delegate_viewporter, delegate_virtual_keyboard_manager,
    delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        space::SpaceElement,
        utils::{
            send_dmabuf_feedback_surface_tree, send_frames_surface_tree,
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            take_presentation_feedback_surface_tree, update_surface_primary_scanout_output,
            with_surfaces_surface_tree, OutputPresentationFeedback,
        },
        PopupKind, PopupManager, Space,
    },
    input::{
        keyboard::{Keysym, LedState, XkbConfig},
        pointer::{CursorImageStatus, CursorImageSurfaceData, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::Output,
    reexports::{
        calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction},
        wayland_protocols::xdg::decoration::{
            self as xdg_decoration,
            zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
            Client, Display, DisplayHandle, Resource,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle, Time},
    wayland::{
        commit_timing::{CommitTimerBarrierStateUserData, CommitTimingManagerState},
        compositor::{
            get_parent, with_states, CompositorClientState, CompositorHandler, CompositorState,
        },
        dmabuf::DmabufFeedback,
        fifo::{FifoBarrierCachedState, FifoManagerState},
        fractional_scale::{
            with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
        },
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::{
            with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
        },
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        seat::WaylandFocus,
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
            SecurityContextState,
        },
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler,
        },
        session_lock::{LockSurface, SessionLockManagerState},
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{
                decoration::{XdgDecorationHandler, XdgDecorationState},
                ToplevelSurface, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        tablet_manager::{TabletManagerState, TabletSeatHandler},
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
    },
};

#[cfg(feature = "xwayland")]
use crate::cursor::Cursor;
use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    shell::WindowElement,
};
#[cfg(feature = "xwayland")]
use smithay::{
    delegate_xwayland_keyboard_grab, delegate_xwayland_shell,
    utils::Size,
    wayland::selection::{SelectionSource, SelectionTarget},
    wayland::xwayland_keyboard_grab::{XWaylandKeyboardGrabHandler, XWaylandKeyboardGrabState},
    wayland::xwayland_shell,
    xwayland::{X11Wm, XWayland, XWaylandEvent},
};

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub security_context: Option<SecurityContext>,
}
impl ClientData for ClientState {
    /// Notification that a client was initialized
    fn initialized(&self, _client_id: ClientId) {}
    /// Notification that a client is disconnected
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

#[derive(Debug)]
pub struct SpitfireState<BackendData: Backend + 'static> {
    pub backend_data: BackendData,
    pub socket_name: Option<String>,
    pub display_handle: DisplayHandle,
    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, SpitfireState<BackendData>>,

    // desktop
    pub space: Space<WindowElement>,
    pub popups: PopupManager,
    /// Toplevels waiting for their first buffer commit — the moment a
    /// window in here actually has something to show, it gets keyboard
    /// focus (see `shell/mod.rs`'s `commit()`), dwm-style, instead of
    /// requiring the pointer to hover or click it first. Populated in
    /// `new_toplevel`, drained on that first commit (or on destroy, so a
    /// toplevel closed before ever mapping doesn't linger here).
    pub pending_initial_focus: Vec<WlSurface>,
    /// Every window that's held keyboard focus, oldest first, most
    /// recently focused last — updated from `SeatHandler::focus_changed`
    /// below, so it stays accurate regardless of *why* focus moved (click,
    /// `spitfire.window.focus_next`, a newly-mapped window stealing it,
    /// ...). Read by `arrange_tiling` to refocus the previously-focused
    /// window whenever the current one vanishes (closed) instead of
    /// leaving focus on nothing.
    pub focus_history: Vec<WindowElement>,
    /// `spitfire.anim`: windows currently mid open-scale ("pop") or
    /// move/resize tween, purely for the render path — see `crate::anim`.
    pub window_anims: crate::anim::WindowAnimations,
    /// `spitfire.anim`: a workspace switch's slide, if one is currently in
    /// flight — see `crate::workspace::WorkspaceSlide` and
    /// `SpitfireState::switch_workspace`'s doc comment.
    pub workspace_slide: Option<crate::workspace::WorkspaceSlide>,
    /// Phase 5: dynamic per-output workspace list (v1: a single output, so
    /// a single `WorkspaceSet`) — each workspace owns its own
    /// tile/floating/fibonacci/monocle layout state. See `crate::workspace`.
    pub workspaces: crate::workspace::WorkspaceSet,
    /// `spitfire.window.toggle_scratchpad()`'s single hidden slot — `Some`
    /// exactly when a window is currently stashed there (unmapped, off any
    /// workspace's tiling order). See `SpitfireState::toggle_scratchpad`
    /// (workspace.rs).
    pub scratchpad: Option<WindowElement>,
    /// `spitfire.scratchpad.toggle(name, ...)`'s named slots, keyed by
    /// `name`. See `SpitfireState::toggle_named_scratchpad` (workspace.rs).
    pub named_scratchpads: std::collections::HashMap<String, crate::workspace::NamedScratchpad>,
    /// Phase 5: `ext-workspace-v1` protocol state, kept in sync with
    /// `workspaces` — see `crate::ext_workspace`.
    pub ext_workspace_state: crate::ext_workspace::ExtWorkspaceState,
    /// `wlr-screencopy-unstable-v1` (`grim` and friends) — queued
    /// `copy`/`copy_with_damage` requests, serviced once per frame by
    /// winit.rs/udev.rs. See `crate::screencopy`.
    pub screencopy_state: crate::screencopy::ScreencopyState,
    /// `ext-image-copy-capture-v1` + `ext-image-capture-source-v1` — the
    /// protocol pair `xdg-desktop-portal-wlr` prefers over
    /// `wlr-screencopy-unstable-v1` once both are advertised. Queued
    /// `capture()` requests, serviced once per frame alongside
    /// `screencopy_state` above. See `crate::ext_screencopy`.
    pub ext_screencopy_state: crate::ext_screencopy::ExtScreencopyState,
    /// `ext-foreign-toplevel-list-v1` — Smithay's own ready-made
    /// implementation (`smithay::wayland::foreign_toplevel_list`); this
    /// only holds the global's state, kept in sync with every window
    /// across every workspace by `SpitfireState::sync_foreign_toplevels`,
    /// called once per frame by winit.rs/udev.rs. See `crate::foreign_toplevel`.
    pub foreign_toplevel_list_state:
        smithay::wayland::foreign_toplevel_list::ForeignToplevelListState,
    /// One entry per window `sync_foreign_toplevels` currently has a live
    /// `ext_foreign_toplevel_handle_v1` for — a plain `Vec`, not a
    /// `HashMap`, since `WindowElement` isn't `Hash` (same reasoning
    /// `ext_workspace::ClientState::workspaces` already gives for its own
    /// small linear-scan `Vec`).
    pub foreign_toplevel_handles: Vec<(
        WindowElement,
        smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle,
    )>,
    /// `spitfire.rule({ blur = true })`'s compiled GLES blur shader —
    /// compiled once, lazily, and reused every frame after. See
    /// `crate::blur`.
    pub blur_state: crate::blur::BlurState,
    /// Phase 2: Lua config loaded from `spitfire_config::Config::default_path()`.
    pub config: spitfire_config::Config,
    /// Phase 8: the optional built-in bar's own runtime state (currently
    /// just the clock/date text) — off entirely unless `spitfire.bar.enable`
    /// is set, see `crate::bar`.
    pub bar: crate::bar::Bar,

    // smithay state
    pub compositor_state: CompositorState,
    pub data_device_state: DataDeviceState,
    pub layer_shell_state: WlrLayerShellState,
    /// Phase 4: ext-session-lock-v1 (the Utumno lockscreen and any other
    /// `WlSessionLock`-based client).
    pub session_lock_state: SessionLockManagerState,
    /// `true` for as long as a client holds an active session lock — the
    /// render loop switches to lock-surfaces-only while this is set, see
    /// `render.rs`/`winit.rs`.
    pub locked: bool,
    /// The lock surfaces currently shown while `locked` — one per output,
    /// normally. Cleared on `unlock`.
    pub lock_surfaces: Vec<LockSurface>,
    pub output_manager_state: OutputManagerState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub seat_state: SeatState<SpitfireState<BackendData>>,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub shm_state: ShmState,
    pub viewporter_state: ViewporterState,
    pub xdg_activation_state: XdgActivationState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_shell_state: XdgShellState,
    pub presentation_state: PresentationState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub xdg_foreign_state: XdgForeignState,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: xwayland_shell::XWaylandShellState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fifo_manager_state: FifoManagerState,
    pub commit_timing_manager_state: CommitTimingManagerState,

    pub dnd_icon: Option<DndIcon>,

    // input-related fields
    pub suppressed_keys: Vec<Keysym>,
    pub cursor_status: CursorImageStatus,
    /// A touchpad swipe currently in progress, from `GestureSwipeBegin` to
    /// `GestureSwipeEnd`/cancel — `None` whenever no swipe is live. See
    /// `PendingGesture`'s own doc comment and `input_handler.rs`'s
    /// `on_gesture_swipe_*` handlers.
    pub pending_gesture: Option<PendingGesture>,
    pub seat_name: String,
    pub seat: Seat<SpitfireState<BackendData>>,
    pub clock: Clock<Monotonic>,
    pub pointer: PointerHandle<SpitfireState<BackendData>>,

    #[cfg(feature = "xwayland")]
    pub xwm: Option<X11Wm>,
    #[cfg(feature = "xwayland")]
    pub xdisplay: Option<u32>,

    #[cfg(feature = "debug")]
    pub renderdoc: Option<renderdoc::RenderDoc<renderdoc::V141>>,

    pub show_window_preview: bool,
}

#[derive(Debug)]
pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: Point<i32, Logical>,
}

/// State for a touchpad swipe currently between `GestureSwipeBegin` and
/// `GestureSwipeEnd`/cancel — see `SpitfireState::pending_gesture`.
///
/// `intercepted`, decided once at `GestureSwipeBegin` (before a direction is
/// knowable — only `fingers` is available yet), is why this exists at all:
/// `spitfire.gesture` needs the *whole* sequence either kept from the
/// focused client or handed to it, never a partial one (a client that saw
/// `begin`+`update` but never an `end` would be left with a gesture stuck
/// mid-flight forever). `Config::has_gesture_for_fingers` answers "would
/// *any* registered `spitfire.gesture` ever fire for this many fingers,
/// regardless of which way it ends up going" — if yes, the whole sequence
/// is swallowed (never forwarded to `self.pointer` at all) and `dx`/`dy`
/// accumulate silently until `GestureSwipeEnd`, where `GestureDirection::
/// classify` picks a direction and `Config::find_gesture` looks up the
/// actual match (if the finger count matched but no entry wants *this*
/// direction, nothing fires — a shrug, not an error). If no, every event in
/// the sequence is forwarded to `self.pointer` exactly as before
/// `spitfire.gesture` existed — zero behavior change for anyone not using
/// it, including any client speaking `zwp_pointer_gestures` itself.
#[derive(Debug)]
pub struct PendingGesture {
    pub fingers: u32,
    pub dx: f64,
    pub dy: f64,
    pub intercepted: bool,
}

delegate_compositor!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> DataDeviceHandler for SpitfireState<BackendData> {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl<BackendData: Backend> ClientDndGrabHandler for SpitfireState<BackendData> {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        let offset = if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_states(surface, |states| {
                let hotspot = states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .hotspot;
                Point::from((-hotspot.x, -hotspot.y))
            })
        } else {
            (0, 0).into()
        };
        self.dnd_icon = icon.map(|surface| DndIcon { surface, offset });
    }
    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.dnd_icon = None;
    }
}
impl<BackendData: Backend> ServerDndGrabHandler for SpitfireState<BackendData> {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {
        unreachable!("Spitfire doesn't do server-side grabs");
    }
}
delegate_data_device!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> OutputHandler for SpitfireState<BackendData> {}
delegate_output!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> SelectionHandler for SpitfireState<BackendData> {
    type SelectionUserData = ();

    #[cfg(feature = "xwayland")]
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
                warn!(?err, ?ty, "Failed to set Xwayland selection");
            }
        }
    }

    #[cfg(feature = "xwayland")]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd, self.handle.clone()) {
                warn!(?err, "Failed to send primary (X11 -> Wayland)");
            }
        }
    }
}

impl<BackendData: Backend> PrimarySelectionHandler for SpitfireState<BackendData> {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}
delegate_primary_selection!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> DataControlHandler for SpitfireState<BackendData> {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

delegate_data_control!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> ShmHandler for SpitfireState<BackendData> {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> SeatHandler for SpitfireState<BackendData> {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = PointerFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<SpitfireState<BackendData>> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&KeyboardFocusTarget>) {
        // Record window focus for `arrange_tiling`'s "refocus the previous
        // window when the current one closes" — see `focus_history`'s own
        // docs. Re-focusing an already-topmost entry (e.g. clicking the
        // already-focused window) is deliberately a no-op below rather
        // than a no-op dedup-and-repush, so it doesn't reorder anything.
        if let Some(KeyboardFocusTarget::Window(w)) = target {
            let window = WindowElement(w.clone());
            if self.focus_history.last() != Some(&window) {
                self.focus_history.retain(|w| w != &window);
                self.focus_history.push(window);
            }
        }

        let dh = &self.display_handle;

        let wl_surface = target.and_then(WaylandFocus::wl_surface);

        let focus = wl_surface.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, focus.clone());
        set_primary_focus(dh, seat, focus);
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.backend_data.update_led_state(led_state)
    }
}
delegate_seat!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> TabletSeatHandler for SpitfireState<BackendData> {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        // TODO: tablet tools should have their own cursors
        self.cursor_status = image;
    }
}
delegate_tablet_manager!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

delegate_text_input_manager!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> InputMethodHandler for SpitfireState<BackendData> {
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn popup_repositioned(&mut self, _: PopupSurface) {}

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|parent| parent.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, smithay::utils::Logical> {
        self.space
            .elements()
            .find_map(|window| {
                (window.wl_surface().as_deref() == Some(parent)).then(|| window.geometry())
            })
            .unwrap_or_default()
    }
}

delegate_input_method_manager!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> KeyboardShortcutsInhibitHandler for SpitfireState<BackendData> {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Just grant the wish for everyone
        inhibitor.activate();
    }
}

delegate_keyboard_shortcuts_inhibit!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

delegate_virtual_keyboard_manager!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

delegate_pointer_gestures!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

delegate_relative_pointer!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> PointerConstraintsHandler for SpitfireState<BackendData> {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // XXX region
        let Some(current_focus) = pointer.current_focus() else {
            return;
        };
        if current_focus.wl_surface().as_deref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                constraint.unwrap().activate();
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        if with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        }) {
            let origin = self
                .space
                .elements()
                .find_map(|window| {
                    (window.wl_surface().as_deref() == Some(surface)).then(|| window.geometry())
                })
                .unwrap_or_default()
                .loc
                .to_f64();

            pointer.set_location(origin + location);
        }
    }
}
delegate_pointer_constraints!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

delegate_viewporter!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> XdgActivationHandler for SpitfireState<BackendData> {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        if let Some((serial, seat)) = data.serial {
            let keyboard = self.seat.get_keyboard().unwrap();
            Seat::from_resource(&seat) == Some(self.seat.clone())
                && keyboard
                    .last_enter()
                    .map(|last_enter| serial.is_no_older_than(&last_enter))
                    .unwrap_or(false)
        } else {
            false
        }
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed().as_secs() < 10 {
            // Just grant the wish
            let w = self
                .space
                .elements()
                .find(|window| window.wl_surface().map(|s| *s == surface).unwrap_or(false))
                .cloned();
            if let Some(window) = w {
                self.space.raise_element(&window, true);
            }
        }
    }
}
delegate_xdg_activation!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> XdgDecorationHandler for SpitfireState<BackendData> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        // Set the default to client side
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ClientSide);
        });
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(match mode {
                DecorationMode::ServerSide => Mode::ServerSide,
                _ => Mode::ClientSide,
            });
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ClientSide);
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

delegate_xdg_shell!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);
delegate_layer_shell!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);
delegate_session_lock!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);
delegate_presentation!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> FractionalScaleHandler for SpitfireState<BackendData> {
    fn new_fractional_scale(
        &mut self,
        surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        // Here we can set the initial fractional scale
        //
        // First we look if the surface already has a primary scan-out output, if not
        // we test if the surface is a subsurface and try to use the primary scan-out output
        // of the root surface. If the root also has no primary scan-out output we just try
        // to use the first output of the toplevel.
        // If the surface is the root we also try to use the first output of the toplevel.
        //
        // If all the above tests do not lead to a output we just use the first output
        // of the space (which in case of spitfire will also be the output a toplevel will
        // initially be placed on)
        #[allow(clippy::redundant_clone)]
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        with_states(&surface, |states| {
            let primary_scanout_output = surface_primary_scanout_output(&surface, states)
                .or_else(|| {
                    if root != surface {
                        with_states(&root, |states| {
                            surface_primary_scanout_output(&root, states).or_else(|| {
                                self.window_for_surface(&root).and_then(|window| {
                                    self.space.outputs_for_element(&window).first().cloned()
                                })
                            })
                        })
                    } else {
                        self.window_for_surface(&root).and_then(|window| {
                            self.space.outputs_for_element(&window).first().cloned()
                        })
                    }
                })
                .or_else(|| self.space.outputs().next().cloned());
            if let Some(output) = primary_scanout_output {
                with_fractional_scale(states, |fractional_scale| {
                    fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        });
    }
}
delegate_fractional_scale!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend + 'static> SecurityContextHandler for SpitfireState<BackendData> {
    fn context_created(
        &mut self,
        source: SecurityContextListenerSource,
        security_context: SecurityContext,
    ) {
        self.handle
            .insert_source(source, move |client_stream, _, data| {
                let client_state = ClientState {
                    security_context: Some(security_context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = data
                    .display_handle
                    .insert_client(client_stream, Arc::new(client_state))
                {
                    warn!("Error adding wayland client: {}", err);
                };
            })
            .expect("Failed to init wayland socket source");
    }
}
delegate_security_context!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

#[cfg(feature = "xwayland")]
impl<BackendData: Backend + 'static> XWaylandKeyboardGrabHandler for SpitfireState<BackendData> {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<KeyboardFocusTarget> {
        let elem = self
            .space
            .elements()
            .find(|elem| elem.wl_surface().as_deref() == Some(surface))?;
        Some(KeyboardFocusTarget::Window(elem.0.clone()))
    }
}
#[cfg(feature = "xwayland")]
delegate_xwayland_keyboard_grab!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

#[cfg(feature = "xwayland")]
delegate_xwayland_shell!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend> XdgForeignHandler for SpitfireState<BackendData> {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}
smithay::delegate_xdg_foreign!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

smithay::delegate_single_pixel_buffer!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

smithay::delegate_fifo!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

smithay::delegate_commit_timing!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

/// `spitfire.keyboard = { layout, variant, model, options, rules }` →
/// smithay/xkbcommon's own `XkbConfig` — same fields, just borrowed
/// instead of owned (`XkbConfig` borrows its strings, so this can't be a
/// `From` impl on the config type itself without giving it a lifetime).
/// Used both at startup (`SpitfireState::init`) and on every
/// `spitfire.reload()` (`reload_config`, so a layout change takes effect
/// without restarting the compositor).
pub(crate) fn xkb_config_from(keyboard: &spitfire_config::KeyboardConfig) -> XkbConfig<'_> {
    XkbConfig {
        rules: &keyboard.rules,
        model: &keyboard.model,
        layout: &keyboard.layout,
        variant: &keyboard.variant,
        options: keyboard.options.clone(),
    }
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    pub fn init(
        display: Display<SpitfireState<BackendData>>,
        handle: LoopHandle<'static, SpitfireState<BackendData>>,
        backend_data: BackendData,
        listen_on_socket: bool,
    ) -> SpitfireState<BackendData> {
        let dh = display.handle();

        let clock = Clock::new();

        // init wayland clients
        let socket_name = if listen_on_socket {
            let source = ListeningSocketSource::new_auto().unwrap();
            let socket_name = source.socket_name().to_string_lossy().into_owned();
            handle
                .insert_source(source, |client_stream, _, data| {
                    if let Err(err) = data
                        .display_handle
                        .insert_client(client_stream, Arc::new(ClientState::default()))
                    {
                        warn!("Error adding wayland client: {}", err);
                    };
                })
                .expect("Failed to init wayland socket source");
            info!(name = socket_name, "Listening on wayland socket");
            Some(socket_name)
        } else {
            None
        };
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, data| {
                    profiling::scope!("dispatch_clients");
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(data).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("Failed to init wayland server source");

        // init globals
        let compositor_state = CompositorState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_client| true);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let data_control_state =
            DataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| true);
        let mut seat_state = SeatState::new();
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fifo_manager_state = FifoManagerState::new::<Self>(&dh);
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&dh);
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);
        // Expose global only if backend supports relative motion events
        if BackendData::HAS_RELATIVE_MOTION {
            RelativePointerManagerState::new::<Self>(&dh);
        }
        PointerConstraintsState::new::<Self>(&dh);
        if BackendData::HAS_GESTURES {
            PointerGesturesState::new::<Self>(&dh);
        }
        TabletManagerState::new::<Self>(&dh);
        SecurityContextState::new::<Self, _>(&dh, |client| {
            client
                .get_data::<ClientState>()
                .is_none_or(|client_state| client_state.security_context.is_none())
        });

        // Loaded before the keyboard below needs it (spitfire.keyboard =
        // { layout, variant, ... }).
        let config_path = spitfire_config::Config::default_path();
        let config = spitfire_config::Config::load(&config_path).unwrap_or_else(|err| {
            error!(path = %config_path.display(), %err, "error in Lua config, starting with defaults");
            // The path is already in mlua's error above; Config::load
            // reading from a nonexistent file never fails (it only warns),
            // so getting here only happens on a genuine Lua syntax/runtime
            // error — not worth aborting the compositor over.
            spitfire_config::Config::load(std::path::Path::new("/dev/null"))
                .expect("loading an empty config should never fail")
        });

        // init input
        let seat_name = backend_data.seat_name();
        let mut seat = seat_state.new_wl_seat(&dh, seat_name.clone());

        let pointer = seat.add_pointer();
        // spitfire.keyboard.repeat_delay/repeat_rate — see KeyboardConfig's
        // doc comment for why the default isn't 200ms.
        seat.add_keyboard(
            xkb_config_from(&config.keyboard),
            config.keyboard.repeat_delay,
            config.keyboard.repeat_rate,
        )
        .expect("Failed to initialize the keyboard");

        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<Self>(&dh);

        #[cfg(feature = "xwayland")]
        let xwayland_shell_state = xwayland_shell::XWaylandShellState::new::<Self>(&dh.clone());

        #[cfg(feature = "xwayland")]
        XWaylandKeyboardGrabState::new::<Self>(&dh.clone());

        let mut workspaces = crate::workspace::WorkspaceSet::default();
        workspaces.apply_gaps(config.gaps);
        // Deliberately *not* pre-creating up to spitfire.workspace.max here
        // — tried, briefly (2026-08-17, reverted the same day): every
        // workspace 1..max existing from the very first frame, wasp-style.
        // Decided against it on reflection — spitfire's own niri-style
        // "nothing exists until you ask for it" growth (`.focus(n)`/
        // `.next()`/`spitfire.rule({ workspace = n })` all still create on
        // demand via `Workspaces::ensure_len`) is the intended behavior,
        // `max` stays purely a *ceiling* on `.next()`'s growth, not also a
        // startup floor. `Workspaces::ensure` is still what a caller wanting
        // the old eager behavior back would reach for.
        let ext_workspace_state = crate::ext_workspace::ExtWorkspaceState::new::<Self>(&dh);
        let screencopy_state = crate::screencopy::ScreencopyState::new::<Self>(&dh);
        let ext_screencopy_state = crate::ext_screencopy::ExtScreencopyState::new::<Self>(&dh);
        let foreign_toplevel_list_state =
            smithay::wayland::foreign_toplevel_list::ForeignToplevelListState::new::<Self>(&dh);

        SpitfireState {
            backend_data,
            config,
            bar: crate::bar::Bar::default(),
            workspaces,
            scratchpad: None,
            named_scratchpads: std::collections::HashMap::new(),
            ext_workspace_state,
            screencopy_state,
            ext_screencopy_state,
            foreign_toplevel_list_state,
            foreign_toplevel_handles: Vec::new(),
            blur_state: crate::blur::BlurState::default(),
            display_handle: dh,
            socket_name,
            running: Arc::new(AtomicBool::new(true)),
            handle,
            space: Space::default(),
            popups: PopupManager::default(),
            pending_initial_focus: Vec::new(),
            focus_history: Vec::new(),
            window_anims: crate::anim::WindowAnimations::default(),
            workspace_slide: None,
            compositor_state,
            data_device_state,
            layer_shell_state,
            session_lock_state,
            locked: false,
            lock_surfaces: Vec::new(),
            output_manager_state,
            primary_selection_state,
            data_control_state,
            seat_state,
            keyboard_shortcuts_inhibit_state,
            shm_state,
            viewporter_state,
            xdg_activation_state,
            xdg_decoration_state,
            xdg_shell_state,
            presentation_state,
            fractional_scale_manager_state,
            xdg_foreign_state,
            single_pixel_buffer_state,
            fifo_manager_state,
            commit_timing_manager_state,
            dnd_icon: None,
            suppressed_keys: Vec::new(),
            cursor_status: CursorImageStatus::default_named(),
            pending_gesture: None,
            seat_name,
            seat,
            pointer,
            clock,

            #[cfg(feature = "xwayland")]
            xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xdisplay: None,
            #[cfg(feature = "debug")]
            renderdoc: renderdoc::RenderDoc::new().ok(),
            show_window_preview: false,
        }
    }

    /// Starts XWayland (X11 application support) if the `xwayland` feature
    /// is compiled in. Not fatal if it can't: this only ever runs the
    /// `Xwayland` binary lazily, on demand, so a system that doesn't have
    /// it installed just means X11-only apps won't work — every native
    /// Wayland client is unaffected either way.
    #[cfg(feature = "xwayland")]
    pub fn start_xwayland(&mut self) {
        use std::process::Stdio;

        use smithay::wayland::compositor::CompositorHandler;

        let (xwayland, client) = match XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        ) {
            Ok(pair) => pair,
            Err(err) => {
                warn!(%err, "Failed to start XWayland — is the Xwayland binary installed? X11-only apps won't work this session, everything else is unaffected");
                return;
            }
        };

        // `display_number()` is known the instant `spawn()` returns — no
        // need to wait for `XWaylandEvent::Ready` for this part. Setting
        // `DISPLAY` on our own process now (not just handing it explicitly
        // to each `Command` we spawn ourselves, as `xdisplay_env()`
        // elsewhere does) means every descendant of spitfire inherits it
        // from here on, transitively — including a frontend launched via
        // `spitfire.autostart` (Utumno) and whatever *it* in turn spawns
        // (its app launcher, e.g. Steam), which never went through our own
        // `Command::envs()` calls at all. Without this, such a frontend
        // that happened to start before the async `Ready` event landed —
        // or that outlives a slow/failed one — stays without `DISPLAY` for
        // its entire lifetime, breaking every XWayland-only app it tries to
        // launch. Mirrors wasp's plain `setenv("DISPLAY", ...)` right after
        // `wlr_xwayland_create()` in wasp.c.
        let display_number = xwayland.display_number();
        std::env::set_var("DISPLAY", format!(":{display_number}"));
        self.xdisplay = Some(display_number);

        let ret = self
            .handle
            .insert_source(xwayland, move |event, _, data| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => {
                    let xwayland_scale = std::env::var("SPITFIRE_XWAYLAND_SCALE")
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(1.);
                    data.client_compositor_state(&client)
                        .set_client_scale(xwayland_scale);
                    let mut wm = X11Wm::start_wm(data.handle.clone(), x11_socket, client.clone())
                        .expect("Failed to attach X11 Window Manager");

                    let cursor = Cursor::load();
                    let image = cursor.get_image(1, Duration::ZERO);
                    wm.set_cursor(
                        &image.pixels_rgba,
                        Size::from((image.width as u16, image.height as u16)),
                        Point::from((image.xhot as u16, image.yhot as u16)),
                    )
                    .expect("Failed to set xwayland default cursor");
                    data.xwm = Some(wm);
                    data.xdisplay = Some(display_number);
                }
                XWaylandEvent::Error => {
                    warn!("XWayland crashed on startup");
                }
            });
        if let Err(e) = ret {
            tracing::error!(
                "Failed to insert the XWaylandSource into the event loop: {}",
                e
            );
        }
    }
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    pub fn pre_repaint(&mut self, output: &Output, frame_target: impl Into<Time<Monotonic>>) {
        let frame_target = frame_target.into();

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();
        self.space.elements().for_each(|window| {
            window.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        });

        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }
        // Drop the lock to the layer map before calling blocker_cleared, which might end up
        // calling the commit handler which in turn again could access the layer map.
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        if self.locked {
            if let Some(surface) = self.lock_surfaces.first().map(|s| s.wl_surface().clone()) {
                with_surfaces_surface_tree(&surface, |surface, states| {
                    if let Some(mut commit_timer_state) = states
                        .data_map
                        .get::<CommitTimerBarrierStateUserData>()
                        .map(|commit_timer| commit_timer.lock().unwrap())
                    {
                        commit_timer_state.signal_until(frame_target);
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                });
            }
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }

    pub fn post_repaint(
        &mut self,
        output: &Output,
        time: impl Into<Duration>,
        dmabuf_feedback: Option<SurfaceDmabufFeedback>,
        render_element_states: &RenderElementStates,
    ) {
        let time = time.into();
        let throttle = Some(Duration::from_secs(1));

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        self.space.elements().for_each(|window| {
            window.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });

            if self.space.outputs_for_element(window).contains(output) {
                window.send_frame(output, time, throttle, surface_primary_scanout_output);
                if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                    window.send_dmabuf_feedback(
                        output,
                        surface_primary_scanout_output,
                        |surface, _| {
                            select_dmabuf_feedback(
                                surface,
                                render_element_states,
                                &dmabuf_feedback.render_feedback,
                                &dmabuf_feedback.scanout_feedback,
                            )
                        },
                    );
                }
            }
        });
        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });

            layer_surface.send_frame(output, time, throttle, surface_primary_scanout_output);
            if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                layer_surface.send_dmabuf_feedback(
                    output,
                    surface_primary_scanout_output,
                    |surface, _| {
                        select_dmabuf_feedback(
                            surface,
                            render_element_states,
                            &dmabuf_feedback.render_feedback,
                            &dmabuf_feedback.scanout_feedback,
                        )
                    },
                );
            }
        }
        // Drop the lock to the layer map before calling blocker_cleared, which might end up
        // calling the commit handler which in turn again could access the layer map.
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

        // Session lock: the actual fix for "typing into the lock screen
        // never shows anything" — `send_frames_surface_tree` below is what
        // sends the `wl_surface.frame` done callback a frame-throttled
        // client (Quickshell/Qt among them) waits on before painting its
        // next frame. The lock surface never went through any of
        // `window`/`layer_surface`'s equivalent (it's neither), so it got
        // exactly one paint — the first one, at map time — and then stuck
        // forever afterwards: the password dots, a shake-on-failure
        // animation, all of it silently never rendered again under a
        // backend that (unlike winit's continuous redraw loop) only
        // repaints in response to real frame/vblank scheduling.
        if self.locked {
            if let Some(surface) = self.lock_surfaces.first().map(|s| s.wl_surface().clone()) {
                with_surfaces_surface_tree(&surface, |surface, states| {
                    let primary_scanout_output = surface_primary_scanout_output(surface, states);

                    if let Some(output) = primary_scanout_output.as_ref() {
                        with_fractional_scale(states, |fraction_scale| {
                            fraction_scale
                                .set_preferred_scale(output.current_scale().fractional_scale());
                        });
                    }

                    if primary_scanout_output
                        .as_ref()
                        .map(|o| o == output)
                        .unwrap_or(true)
                    {
                        let fifo_barrier = states
                            .cached_state
                            .get::<FifoBarrierCachedState>()
                            .current()
                            .barrier
                            .take();

                        if let Some(fifo_barrier) = fifo_barrier {
                            fifo_barrier.signal();
                            let client = surface.client().unwrap();
                            clients.insert(client.id(), client);
                        }
                    }
                });

                send_frames_surface_tree(
                    &surface,
                    output,
                    time,
                    throttle,
                    surface_primary_scanout_output,
                );
                if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                    send_dmabuf_feedback_surface_tree(
                        &surface,
                        output,
                        surface_primary_scanout_output,
                        |surface, _| {
                            select_dmabuf_feedback(
                                surface,
                                render_element_states,
                                &dmabuf_feedback.render_feedback,
                                &dmabuf_feedback.scanout_feedback,
                            )
                        },
                    );
                }
            }
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }
}

pub fn update_primary_scanout_output(
    space: &Space<WindowElement>,
    output: &Output,
    dnd_icon: &Option<DndIcon>,
    cursor_status: &CursorImageStatus,
    locked_surface: Option<&WlSurface>,
    render_element_states: &RenderElementStates,
) {
    space.elements().for_each(|window| {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let CursorImageStatus::Surface(ref surface) = cursor_status {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let Some(surface) = dnd_icon.as_ref().map(|icon| &icon.surface) {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    // Session lock: without this, the lock surface never got a primary
    // scanout output, and — more to the point — the `send_frame`/
    // `take_presentation_feedback` calls below key off exactly this same
    // set of surfaces, so it never received frame-done callbacks either.
    // A frame-throttled client (Qt/Quickshell among them) waits for that
    // callback before painting its next frame, so every redraw after the
    // first (typing into the password field, an unlock-failed shake, ...)
    // silently never happened under this backend.
    if let Some(surface) = locked_surface {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

#[derive(Debug, Clone)]
pub struct SurfaceDmabufFeedback {
    pub render_feedback: DmabufFeedback,
    pub scanout_feedback: DmabufFeedback,
}

#[profiling::function]
pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<WindowElement>,
    locked_surface: Option<&WlSurface>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut output_presentation_feedback,
                surface_primary_scanout_output,
                |surface, _| {
                    surface_presentation_feedback_flags_from_states(surface, render_element_states)
                },
            );
        }
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, render_element_states)
            },
        );
    }

    if let Some(surface) = locked_surface {
        take_presentation_feedback_surface_tree(
            surface,
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, render_element_states)
            },
        );
    }

    output_presentation_feedback
}

pub trait Backend {
    const HAS_RELATIVE_MOTION: bool = false;
    const HAS_GESTURES: bool = false;
    fn seat_name(&self) -> String;
    fn reset_buffers(&mut self, output: &Output);
    fn early_import(&mut self, surface: &WlSurface);
    fn update_led_state(&mut self, led_state: LedState);
}
