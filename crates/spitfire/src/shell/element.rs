use std::{borrow::Cow, time::Duration};

use smithay::{
    backend::renderer::{
        element::{
            solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement, AsRenderElements,
            Kind,
        },
        ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::{
        space::SpaceElement, utils::OutputPresentationFeedback, Window, WindowSurface,
        WindowSurfaceType,
    },
    input::{
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent,
            GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
            GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, MotionEvent,
            PointerTarget, RelativeMotionEvent,
        },
        touch::TouchTarget,
        Seat,
    },
    output::Output,
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    render_elements,
    utils::{
        user_data::UserDataMap, IsAlive, Logical, Physical, Point, Rectangle, Scale, Serial, Size,
    },
    wayland::{
        compositor::{with_states, SurfaceData as WlSurfaceData},
        dmabuf::DmabufFeedback,
        seat::WaylandFocus,
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use super::ssd::{BG_COLOR, HEADER_BAR_HEIGHT};
use crate::{focus::PointerFocusTarget, state::Backend, SpitfireState};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowElement(pub Window);

impl WindowElement {
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
        window_type: WindowSurfaceType,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        let state = self.decoration_state();
        if state.is_ssd && location.y < HEADER_BAR_HEIGHT as f64 {
            return Some((PointerFocusTarget::SSD(SSD(self.clone())), Point::default()));
        }
        let offset = if state.is_ssd {
            Point::from((0, HEADER_BAR_HEIGHT))
        } else {
            Point::default()
        };

        let surface_under = self
            .0
            .surface_under(location - offset.to_f64(), window_type);
        let (under, loc) = match self.0.underlying_surface() {
            WindowSurface::Wayland(_) => {
                surface_under.map(|(surface, loc)| (PointerFocusTarget::WlSurface(surface), loc))
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => {
                surface_under.map(|(_, loc)| (PointerFocusTarget::X11Surface(s.clone()), loc))
            }
        }?;
        Some((under, loc + offset))
    }

    pub fn with_surfaces<F>(&self, processor: F)
    where
        F: FnMut(&WlSurface, &WlSurfaceData),
    {
        self.0.with_surfaces(processor);
    }

    pub fn send_frame<T, F>(
        &self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
    {
        self.0
            .send_frame(output, time, throttle, primary_scan_out_output)
    }

    pub fn send_dmabuf_feedback<'a, P, F>(
        &self,
        output: &Output,
        primary_scan_out_output: P,
        select_dmabuf_feedback: F,
    ) where
        P: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F: Fn(&WlSurface, &WlSurfaceData) -> &'a DmabufFeedback + Copy,
    {
        self.0
            .send_dmabuf_feedback(output, primary_scan_out_output, select_dmabuf_feedback)
    }

    pub fn take_presentation_feedback<F1, F2>(
        &self,
        output_feedback: &mut OutputPresentationFeedback,
        primary_scan_out_output: F1,
        presentation_feedback_flags: F2,
    ) where
        F1: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F2: FnMut(&WlSurface, &WlSurfaceData) -> wp_presentation_feedback::Kind + Copy,
    {
        self.0.take_presentation_feedback(
            output_feedback,
            primary_scan_out_output,
            presentation_feedback_flags,
        )
    }

    #[cfg(feature = "xwayland")]
    #[inline]
    pub fn is_x11(&self) -> bool {
        self.0.is_x11()
    }

    #[inline]
    pub fn is_wayland(&self) -> bool {
        self.0.is_wayland()
    }

    #[inline]
    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }

    /// The app_id set by the client via `xdg_toplevel.set_app_id`, if any
    /// (may still be `None` right after mapping, before the client has had
    /// a chance to send it). Used by window rules (`spitfire.rule`) and
    /// `spitfirectl list-windows`.
    pub fn app_id(&self) -> Option<String> {
        let surface = self.wl_surface()?;
        with_states(&surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok())
                .and_then(|attrs| attrs.app_id.clone())
        })
    }

    /// The title set by the client via `xdg_toplevel.set_title`, if any.
    pub fn title(&self) -> Option<String> {
        let surface = self.wl_surface()?;
        with_states(&surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok())
                .and_then(|attrs| attrs.title.clone())
        })
    }

    #[inline]
    pub fn user_data(&self) -> &UserDataMap {
        self.0.user_data()
    }
}

impl IsAlive for WindowElement {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SSD(WindowElement);

impl IsAlive for SSD {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for SSD {
    #[inline]
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl<BackendData: Backend> PointerTarget<SpitfireState<BackendData>> for SSD {
    fn enter(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn motion(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn relative_motion(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &RelativeMotionEvent,
    ) {
    }
    fn button(
        &self,
        seat: &Seat<SpitfireState<BackendData>>,
        data: &mut SpitfireState<BackendData>,
        event: &ButtonEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.clicked(seat, data, &self.0, event.serial);
        }
    }
    fn axis(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _frame: AxisFrame,
    ) {
    }
    fn frame(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
    ) {
    }
    fn leave(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _serial: Serial,
        _time: u32,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_leave();
        }
    }
    fn gesture_swipe_begin(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GestureSwipeBeginEvent,
    ) {
    }
    fn gesture_swipe_update(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GestureSwipeUpdateEvent,
    ) {
    }
    fn gesture_swipe_end(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GestureSwipeEndEvent,
    ) {
    }
    fn gesture_pinch_begin(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GesturePinchBeginEvent,
    ) {
    }
    fn gesture_pinch_update(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GesturePinchUpdateEvent,
    ) {
    }
    fn gesture_pinch_end(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GesturePinchEndEvent,
    ) {
    }
    fn gesture_hold_begin(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GestureHoldBeginEvent,
    ) {
    }
    fn gesture_hold_end(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &GestureHoldEndEvent,
    ) {
    }
}

impl<BackendData: Backend> TouchTarget<SpitfireState<BackendData>> for SSD {
    fn down(
        &self,
        seat: &Seat<SpitfireState<BackendData>>,
        data: &mut SpitfireState<BackendData>,
        event: &smithay::input::touch::DownEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
            state
                .header_bar
                .touch_down(seat, data, &self.0, event.serial);
        }
    }

    fn up(
        &self,
        seat: &Seat<SpitfireState<BackendData>>,
        data: &mut SpitfireState<BackendData>,
        event: &smithay::input::touch::UpEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.touch_up(seat, data, &self.0, event.serial);
        }
    }

    fn motion(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        event: &smithay::input::touch::MotionEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }

    fn frame(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn cancel(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn shape(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &smithay::input::touch::ShapeEvent,
        _seq: Serial,
    ) {
    }

    fn orientation(
        &self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _data: &mut SpitfireState<BackendData>,
        _event: &smithay::input::touch::OrientationEvent,
        _seq: Serial,
    ) {
    }
}

impl SpaceElement for WindowElement {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        let mut geo = SpaceElement::geometry(&self.0);
        if self.decoration_state().is_ssd {
            geo.size.h += HEADER_BAR_HEIGHT;
        }
        geo
    }
    fn bbox(&self) -> Rectangle<i32, Logical> {
        let mut bbox = SpaceElement::bbox(&self.0);
        if self.decoration_state().is_ssd {
            bbox.size.h += HEADER_BAR_HEIGHT;
        }
        bbox
    }
    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        if self.decoration_state().is_ssd {
            point.y < HEADER_BAR_HEIGHT as f64
                || SpaceElement::is_in_input_region(
                    &self.0,
                    &(*point - Point::from((0.0, HEADER_BAR_HEIGHT as f64))),
                )
        } else {
            SpaceElement::is_in_input_region(&self.0, point)
        }
    }
    fn z_index(&self) -> u8 {
        SpaceElement::z_index(&self.0)
    }

    fn set_activate(&self, activated: bool) {
        SpaceElement::set_activate(&self.0, activated);
    }
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        SpaceElement::output_enter(&self.0, output, overlap);
    }
    fn output_leave(&self, output: &Output) {
        SpaceElement::output_leave(&self.0, output);
    }
    #[profiling::function]
    fn refresh(&self) {
        SpaceElement::refresh(&self.0);
    }
}

render_elements!(
    pub WindowRenderElement<R> where R: ImportAll + ImportMem;
    Window=WaylandSurfaceRenderElement<R>,
    Decoration=SolidColorRenderElement,
);

impl<R: Renderer> std::fmt::Debug for WindowRenderElement<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Decoration(arg0) => f.debug_tuple("Decoration").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

impl<R> AsRenderElements<R> for WindowElement
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
{
    type RenderElement = WindowRenderElement<R>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        mut location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let window_bbox = SpaceElement::bbox(&self.0);

        // Sized to exactly the content geometry, drawn *behind* the
        // client's own content — see `WindowState::backdrop`'s doc comment
        // (shell/ssd.rs) for the real bug this papers over (a client
        // attaching a real-alpha buffer and relying on compositor blur
        // spitfire doesn't have).
        let backdrop_element = |location: Point<i32, Physical>, size: Size<i32, Logical>| {
            let mut state = self.decoration_state();
            state
                .backdrop
                .update((size.w.max(1), size.h.max(1)), BG_COLOR);
            WindowRenderElement::<R>::from(SolidColorRenderElement::from_buffer(
                &state.backdrop,
                location,
                scale,
                alpha,
                Kind::Unspecified,
            ))
        };

        if self.decoration_state().is_ssd && !window_bbox.is_empty() {
            let window_geo = SpaceElement::geometry(&self.0);

            let mut vec = {
                let mut state = self.decoration_state();
                state.header_bar.redraw(window_geo.size.w as u32);
                AsRenderElements::<R>::render_elements::<WindowRenderElement<R>>(
                    &state.header_bar,
                    renderer,
                    location,
                    scale,
                    alpha,
                )
            };

            location.y += (scale.y * HEADER_BAR_HEIGHT as f64) as i32;

            let window_elements: Vec<WindowRenderElement<R>> =
                AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha);
            vec.extend(window_elements);
            vec.push(backdrop_element(location, window_geo.size));
            vec.into_iter().map(C::from).collect()
        } else {
            let content_size = SpaceElement::geometry(&self.0).size;
            let mut vec: Vec<WindowRenderElement<R>> =
                AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha);
            vec.push(backdrop_element(location, content_size));
            vec.into_iter().map(C::from).collect()
        }
    }
}
