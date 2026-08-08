use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            AsRenderElements, Kind,
        },
        Renderer,
    },
    desktop::WindowSurface,
    input::Seat,
    utils::{Logical, Point, Serial},
};

use std::cell::{RefCell, RefMut};

use crate::{state::Backend, SpitfireState};

use super::WindowElement;

pub struct WindowState {
    pub is_ssd: bool,
    pub header_bar: HeaderBar,
}

#[derive(Debug, Clone)]
pub struct HeaderBar {
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub width: u32,
    pub close_button_hover: bool,
    pub background: SolidColorBuffer,
    pub close_button: SolidColorBuffer,
}

// Tokyo Night palette (same one `spitfire.border`'s defaults already draw
// from — #7aa2f7/#414868 in examples/config.lua) — the original pastel
// green/yellow read as too soft against a dark terminal to tell the header
// apart from its own content. Background is Tokyo Night's `terminal_black`
// (#414868), a blue-leaning gray; close is `red`/`red1` (#f7768e/#db4b4b).
const BG_COLOR: [f32; 4] = [0.2549f32, 0.2824f32, 0.4078f32, 1f32]; // #414868
const CLOSE_COLOR: [f32; 4] = [0.9686f32, 0.4627f32, 0.5569f32, 1f32]; // #f7768e
const CLOSE_COLOR_HOVER: [f32; 4] = [0.8588f32, 0.2941f32, 0.2941f32, 1f32]; // #db4b4b

// Both were 32 (a square button, header as tall as the button) — shrunk to
// a third of that on request, since the original read as oversized next to
// a client with no title/menu bar of its own to visually balance it (e.g.
// alacritty). `layout.rs`, `shell/element.rs` and the button hit-test math
// right below all read this constant rather than a hardcoded 32, so this
// is the only place that needs to change.
pub const HEADER_BAR_HEIGHT: i32 = 11;
const BUTTON_HEIGHT: u32 = HEADER_BAR_HEIGHT as u32;
const BUTTON_WIDTH: u32 = 11;
const BUTTON_RIGHT_MARGIN: u32 = 5;

fn is_close_hover(pointer_loc: Option<&Point<f64, Logical>>, width: u32) -> bool {
    let min_x = width.saturating_sub(BUTTON_WIDTH + BUTTON_RIGHT_MARGIN) as f64;
    let max_x = width.saturating_sub(BUTTON_RIGHT_MARGIN) as f64;
    pointer_loc.map(|l| l.x >= min_x && l.x < max_x).unwrap_or(false)
}

impl HeaderBar {
    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }

    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
    }

    pub fn clicked<BackendData: Backend>(
        &mut self,
        seat: &Seat<SpitfireState<BackendData>>,
        state: &mut SpitfireState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        if is_close_hover(self.pointer_loc.as_ref(), self.width) {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => w.send_close(),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let _ = w.close();
                }
            };
        } else if self.pointer_loc.is_some() {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => {
                    let seat = seat.clone();
                    let toplevel = w.clone();
                    state.handle.insert_idle(move |data| {
                        data.move_request_xdg(&toplevel, &seat, serial)
                    });
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let window = w.clone();
                    state
                        .handle
                        .insert_idle(move |data| data.move_request_x11(&window));
                }
            };
        }
    }

    pub fn touch_down<BackendData: Backend>(
        &mut self,
        seat: &Seat<SpitfireState<BackendData>>,
        state: &mut SpitfireState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        if !is_close_hover(self.pointer_loc.as_ref(), self.width) && self.pointer_loc.is_some() {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => {
                    let seat = seat.clone();
                    let toplevel = w.clone();
                    state.handle.insert_idle(move |data| {
                        data.move_request_xdg(&toplevel, &seat, serial)
                    });
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let window = w.clone();
                    state
                        .handle
                        .insert_idle(move |data| data.move_request_x11(&window));
                }
            };
        }
    }

    pub fn touch_up<BackendData: Backend>(
        &mut self,
        _seat: &Seat<SpitfireState<BackendData>>,
        _state: &mut SpitfireState<BackendData>,
        window: &WindowElement,
        _serial: Serial,
    ) {
        if is_close_hover(self.pointer_loc.as_ref(), self.width) {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => w.send_close(),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let _ = w.close();
                }
            };
        }
    }

    pub fn redraw(&mut self, width: u32) {
        if width == 0 {
            self.width = 0;
            return;
        }

        self.background
            .update((width as i32, HEADER_BAR_HEIGHT), BG_COLOR);

        let mut needs_redraw_buttons = false;
        if width != self.width {
            needs_redraw_buttons = true;
            self.width = width;
        }

        let close_hover = is_close_hover(self.pointer_loc.as_ref(), width);
        if close_hover && (needs_redraw_buttons || !self.close_button_hover) {
            self.close_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                CLOSE_COLOR_HOVER,
            );
            self.close_button_hover = true;
        } else if !close_hover && (needs_redraw_buttons || self.close_button_hover) {
            self.close_button
                .update((BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32), CLOSE_COLOR);
            self.close_button_hover = false;
        }
    }
}

impl<R: Renderer> AsRenderElements<R> for HeaderBar {
    type RenderElement = SolidColorRenderElement;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        _renderer: &mut R,
        location: Point<i32, smithay::utils::Physical>,
        scale: smithay::utils::Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let header_end_offset: Point<i32, Logical> =
            Point::from((self.width.saturating_sub(BUTTON_RIGHT_MARGIN) as i32, 0));
        let button_offset: Point<i32, Logical> = Point::from((BUTTON_WIDTH as i32, 0));

        vec![
            SolidColorRenderElement::from_buffer(
                &self.close_button,
                location + (header_end_offset - button_offset).to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
            SolidColorRenderElement::from_buffer(
                &self.background,
                location,
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
        ]
    }
}

impl WindowElement {
    pub fn decoration_state(&self) -> RefMut<'_, WindowState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(WindowState {
                is_ssd: false,
                header_bar: HeaderBar {
                    pointer_loc: None,
                    width: 0,
                    close_button_hover: false,
                    background: SolidColorBuffer::default(),
                    close_button: SolidColorBuffer::default(),
                },
            })
        });

        self.user_data()
            .get::<RefCell<WindowState>>()
            .unwrap()
            .borrow_mut()
    }

    pub fn set_ssd(&self, ssd: bool) {
        self.decoration_state().is_ssd = ssd;
    }
}
