use std::collections::HashSet;

use smithay::{
    backend::input::InputTime,
    desktop::{space::SpaceElement, utils::bbox_from_surface_tree},
    input::pointer::{CursorImageStatus, MotionEvent},
    output::Output,
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER},
};
use spitfire_layout::Rect;

use crate::{
    shell::{FullscreenSurface, WindowElement},
    state::Backend,
    workspace::{last_window_location, WorkspaceId},
    SpitfireState,
};

const LEFT_BUTTON: u32 = 0x110;
const DRAG_THRESHOLD_SQUARED: f64 = 64.0;

#[derive(Debug, Default)]
pub(crate) struct Overview {
    selected: Option<WorkspaceId>,
    press: Option<Press>,
    buttons: HashSet<u32>,
    scroll: f64,
}

#[derive(Debug)]
struct Press {
    workspace: WorkspaceId,
    window: Option<WindowElement>,
    location: Point<f64, Logical>,
}

pub(crate) struct Scene {
    pub viewport: Rectangle<i32, Logical>,
    pub cards: Vec<Card>,
}

pub(crate) struct Card {
    pub rect: Rectangle<i32, Logical>,
    pub zoom: f64,
    pub selected: bool,
    pub highlighted: bool,
    workspace: WorkspaceId,
    pub windows: Vec<PreviewWindow>,
}

pub(crate) struct PreviewWindow {
    pub window: WindowElement,
    /// Surface-tree origin relative to the unscaled output.
    pub origin: Point<i32, Logical>,
    pub rect: Rectangle<i32, Logical>,
}

impl<BackendData: Backend> SpitfireState<BackendData> {
    pub(crate) fn overview_open(&self) -> bool {
        !self.locked && self.overview.selected.is_some()
    }

    pub(crate) fn dismiss_overview(&mut self) {
        self.overview.selected = None;
        self.overview.press = None;
        self.overview.scroll = 0.0;
    }

    pub(crate) fn toggle_overview(&mut self) {
        if self.overview_open() {
            self.close_overview();
        } else if !self.locked
            && self.space.outputs().next().is_some()
            && !self.pointer.is_grabbed()
            && !self.seat.get_keyboard().is_some_and(|kb| kb.is_grabbed())
            && !self
                .seat
                .get_touch()
                .is_some_and(|touch| touch.is_grabbed())
        {
            self.overview.selected = Some(self.workspaces.active().id);
            self.refresh_overview_pointer();
            self.cursor_status = CursorImageStatus::default_named();
        }
    }

    pub(crate) fn close_overview(&mut self) {
        self.dismiss_overview();
        self.refresh_overview_pointer();
    }

    fn refresh_overview_pointer(&mut self) {
        let pointer = self.pointer.clone();
        let location = pointer.current_location();
        pointer.motion(
            self,
            self.surface_under(location),
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    fn overview_index(&self) -> Option<usize> {
        self.overview.selected.filter(|_| !self.locked).map(|id| {
            self.workspaces
                .index_of(id)
                .unwrap_or(self.workspaces.active_index())
        })
    }

    pub(crate) fn step_overview(&mut self, delta: isize) {
        if let Some(index) = self.overview_index() {
            let target = index
                .saturating_add_signed(delta)
                .min(self.workspaces.len() - 1);
            self.overview.selected = self.workspaces.get(target).map(|ws| ws.id);
        }
    }

    pub(crate) fn scroll_overview(&mut self, amount: f64, threshold: f64) {
        if amount.signum() != self.overview.scroll.signum() {
            self.overview.scroll = 0.0;
        }
        self.overview.scroll += amount;
        let steps = (self.overview.scroll / threshold).trunc();
        self.overview.scroll %= threshold;
        self.step_overview(steps as isize);
    }

    pub(crate) fn activate_overview_selection(&mut self) {
        if let Some(index) = self.overview_index() {
            self.activate_overview_workspace(index, None);
        }
    }

    fn activate_overview_workspace(&mut self, index: usize, window: Option<WindowElement>) {
        self.switch_workspace(index);
        if let Some(window) = window.filter(|window| self.space.element_location(window).is_some())
        {
            self.space.raise_element(&window, true);
            #[cfg(feature = "xwayland")]
            if let (Some(xwm), Some(surface)) = (self.xwm.as_mut(), window.0.x11_surface()) {
                let _ = xwm.raise_window(surface);
            }
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Some(window.into()), SERIAL_COUNTER.next_serial());
            }
        }
        self.close_overview();
    }

    pub(crate) fn overview_button(&mut self, button: u32, pressed: bool) -> bool {
        if !pressed && !self.overview.buttons.remove(&button) {
            return false;
        }
        if !self.overview_open() {
            return !pressed;
        }
        if pressed {
            self.overview.buttons.insert(button);
        }
        if button != LEFT_BUTTON {
            return true;
        }

        let location = self.pointer.current_location();
        let hit = self.space.output_under(location).next().and_then(|output| {
            let scene = self.overview_scene(output)?;
            let point =
                (location - self.space.output_geometry(output)?.loc.to_f64()).to_i32_floor();
            let card = scene.cards.iter().find(|card| card.rect.contains(point))?;
            let window = card
                .windows
                .iter()
                .find(|window| window.rect.contains(point));
            Some((card.workspace, window.map(|preview| preview.window.clone())))
        });
        if pressed {
            self.overview.press = hit.map(|(workspace, window)| Press {
                workspace,
                window,
                location,
            });
        } else if let (Some(press), Some((workspace, _))) = (self.overview.press.take(), hit) {
            let Some(target) = self.workspaces.index_of(workspace) else {
                return true;
            };
            if workspace == press.workspace {
                if distance_squared(location, press.location) < DRAG_THRESHOLD_SQUARED {
                    self.activate_overview_workspace(target, press.window);
                }
            } else if let (Some(window), Some(source)) =
                (press.window, self.workspaces.index_of(press.workspace))
            {
                let fullscreen = self.space.outputs().any(|output| {
                    output
                        .user_data()
                        .get::<FullscreenSurface>()
                        .and_then(FullscreenSurface::get)
                        .as_ref()
                        == Some(&window)
                });
                if distance_squared(location, press.location) >= DRAG_THRESHOLD_SQUARED
                    && !fullscreen
                {
                    self.move_window_between_workspaces(&window, source, target);
                    self.overview.selected = Some(workspace);
                }
            }
        }
        true
    }

    pub(crate) fn overview_scene(&self, output: &Output) -> Option<Scene> {
        let selected = self.overview_index()?;
        let geo = self.space.output_geometry(output)?;
        let viewport = Rectangle::from_size(geo.size);
        if viewport.is_empty() {
            return None;
        }
        let zoom = 0.58;
        let card_size = geo.size.to_f64().upscale(zoom).to_i32_round();
        let center = (geo.size.to_point() - card_size.to_point()).downscale(2);
        let spacing = card_size.h + (geo.size.h / 32).clamp(10, 32);
        let pointer = (self.pointer.current_location() - geo.loc.to_f64()).to_i32_floor();
        let dragging = self.overview.press.as_ref().and_then(|press| {
            press.window.as_ref().filter(|_| {
                distance_squared(self.pointer.current_location(), press.location)
                    >= DRAG_THRESHOLD_SQUARED
            })?;
            Some(press.workspace)
        });
        let cards = (selected.saturating_sub(1)..=(selected + 1).min(self.workspaces.len() - 1))
            .filter_map(|index| {
                let workspace = self.workspaces.get(index)?;
                let offset = (index as isize - selected as isize) as i32 * spacing;
                let rect = Rectangle::new(center + Point::from((0, offset)), card_size);
                let highlighted = dragging.is_some_and(|source| workspace.id != source)
                    && viewport.contains(pointer)
                    && rect.contains(pointer);
                let windows = self
                    .overview_windows(index, output, geo)
                    .into_iter()
                    .filter_map(|(window, origin)| {
                        let origin = origin - geo.loc;
                        let geometry = window.geometry();
                        let bounds = if geometry.is_empty() {
                            // Some clients still have empty cached XDG geometry after
                            // a workspace remap, even with a drawable buffer.
                            bbox_from_surface_tree(window.wl_surface()?.as_ref(), Point::default())
                        } else {
                            geometry
                        };
                        let window_rect = Rectangle::new(origin + bounds.loc, bounds.size);
                        let window_rect = Rectangle::new(
                            rect.loc + window_rect.loc.to_f64().upscale(zoom).to_i32_round(),
                            window_rect.size.to_f64().upscale(zoom).to_i32_round(),
                        )
                        .intersection(rect)
                        .filter(|rect| !rect.is_empty())?;
                        Some(PreviewWindow {
                            window,
                            origin,
                            rect: window_rect,
                        })
                    })
                    .collect();
                Some(Card {
                    rect,
                    zoom,
                    selected: index == selected,
                    highlighted,
                    workspace: workspace.id,
                    windows,
                })
            })
            .collect();
        Some(Scene { viewport, cards })
    }

    fn overview_windows(
        &self,
        index: usize,
        output: &Output,
        geo: Rectangle<i32, Logical>,
    ) -> Vec<(WindowElement, Point<i32, Logical>)> {
        let workspace = self.workspaces.get(index).unwrap();
        if index == self.workspaces.active_index() {
            if let Some(window) = output
                .user_data()
                .get::<FullscreenSurface>()
                .and_then(|state| state.get())
            {
                if workspace.tiling.windows().contains(&window) {
                    return vec![(window, geo.loc)];
                }
            }
            return self
                .space
                .elements_for_output(output)
                .rev()
                .filter_map(|window| {
                    workspace
                        .tiling
                        .windows()
                        .contains(window)
                        .then(|| {
                            self.space
                                .element_location(window)
                                .map(|pos| (window.clone(), pos - window.geometry().loc))
                        })
                        .flatten()
                })
                .collect();
        }

        let Some(primary) = self.space.outputs().next() else {
            return Vec::new();
        };
        let bar_height = if self.config.bar.enabled {
            self.config.bar.height
        } else {
            0
        };
        let Some(area) =
            crate::layout::usable_area(&self.space, primary, bar_height, self.config.gaps.outer)
        else {
            return Vec::new();
        };
        let tiled: Vec<_> = workspace
            .tiling
            .windows()
            .iter()
            .filter(|window| window.alive() && self.is_tiled(window))
            .collect();
        let placements = spitfire_layout::arrange(
            &tiled,
            Rect::new(area.loc.x, area.loc.y, area.size.w, area.size.h),
            &workspace.tiling.params,
        )
        .unwrap_or_default();

        let mut windows = Vec::new();
        let mut tiles = Vec::new();
        for window in workspace
            .tiling
            .windows()
            .iter()
            .rev()
            .filter(|window| window.alive())
        {
            if let Some((_, rect)) = placements.iter().find(|(tiled, _)| *tiled == window) {
                tiles.push((
                    window.clone(),
                    Point::from((rect.x, rect.y)) - window.geometry().loc,
                ));
            } else {
                let position = self
                    .space
                    .element_location(window)
                    .or_else(|| last_window_location(window))
                    .unwrap_or_else(|| {
                        area.loc
                            + (area.size.to_point() - window.geometry().size.to_point())
                                .downscale(2)
                    });
                windows.push((window.clone(), position - window.geometry().loc));
            }
        }
        windows.extend(tiles);
        windows
    }
}

fn distance_squared(a: Point<f64, Logical>, b: Point<f64, Logical>) -> f64 {
    let delta = a - b;
    delta.x * delta.x + delta.y * delta.y
}
