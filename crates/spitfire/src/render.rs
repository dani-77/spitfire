use smithay::{
    backend::renderer::{
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            utils::{
                ConstrainAlign, ConstrainScaleBehavior, CropRenderElement, RelocateRenderElement,
                RescaleRenderElement,
            },
            AsRenderElements, Kind, RenderElement, Wrap,
        },
        Color32F, ImportAll, ImportMem, Renderer,
    },
    desktop::space::{
        constrain_space_element, ConstrainBehavior, ConstrainReference, Space, SpaceRenderElements,
        SurfaceTree,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Physical, Point, Rectangle, Scale, Size},
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    drawing::{PointerRenderElement, CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN, CLEAR_COLOR_LOCKED},
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    Border=SolidColorRenderElement,
    #[cfg(feature = "debug")]
    // Note: We would like to borrow this element instead, but that would introduce
    // a feature-dependent lifetime, which introduces a lot more feature bounds
    // as the whole type changes and we can't have an unused lifetime (for when "debug" is disabled)
    // in the declaration.
    Fps=FpsElement<R::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            Self::Border(arg0) => f.debug_tuple("Border").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

/// Converts a `0xRRGGBB` color (as parsed from `spitfire.border.active`/
/// `.inactive` in `config.lua`) into the renderer's `Color32F`.
pub fn hex_to_color32f(hex: u32) -> Color32F {
    let r = ((hex >> 16) & 0xff) as f32 / 255.0;
    let g = ((hex >> 8) & 0xff) as f32 / 255.0;
    let b = (hex & 0xff) as f32 / 255.0;
    Color32F::new(r, g, b, 1.0)
}

/// A window's current on-screen geometry plus whether it's the focused one
/// — all `border_elements` needs to know to draw `spitfire.border` behind
/// each managed window. Computed by `SpitfireState::border_rects`.
pub struct BorderRect {
    pub geometry: Rectangle<i32, smithay::utils::Logical>,
    pub focused: bool,
}

/// Builds four thin solid-color strips (top/bottom/left/right) around each
/// entry in `borders` — dwm-style, without the layout engine needing to
/// reserve any space for it. Four non-overlapping strips rather than one
/// bigger rect behind the window: a single element placed behind relies on
/// the window's own (opaque) element correctly punching a window-shaped
/// hole through it via the renderer's occlusion tracking, and in practice
/// that undersized the visible region to nothing — these strips never
/// overlap the window's own footprint at all, so where they land in the
/// render order doesn't matter.
pub fn border_elements<R>(
    borders: &[BorderRect],
    width: i32,
    active_color: Color32F,
    inactive_color: Color32F,
    output_scale: Scale<f64>,
) -> Vec<CustomRenderElements<R>>
where
    R: Renderer + ImportAll + ImportMem,
{
    if width <= 0 {
        return Vec::new();
    }
    borders
        .iter()
        .flat_map(|b| {
            let color = if b.focused { active_color } else { inactive_color };
            let g = b.geometry;
            // Four strips forming a hollow ring around `g`, each `width`
            // thick, meeting at the corners — none overlap `g` itself.
            let strips = [
                // top
                Rectangle::new((g.loc.x - width, g.loc.y - width).into(), (g.size.w + width * 2, width).into()),
                // bottom
                Rectangle::new((g.loc.x - width, g.loc.y + g.size.h).into(), (g.size.w + width * 2, width).into()),
                // left
                Rectangle::new((g.loc.x - width, g.loc.y).into(), (width, g.size.h).into()),
                // right
                Rectangle::new((g.loc.x + g.size.w, g.loc.y).into(), (width, g.size.h).into()),
            ];
            strips.into_iter().map(move |strip: Rectangle<i32, smithay::utils::Logical>| {
                let buffer = SolidColorBuffer::new(strip.size, color);
                let loc: Point<i32, Physical> = strip.loc.to_f64().to_physical(output_scale).to_i32_round();
                CustomRenderElements::Border(SolidColorRenderElement::from_buffer(
                    &buffer,
                    loc,
                    output_scale,
                    1.0,
                    Kind::Unspecified,
                ))
            })
        })
        .collect()
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Window=Wrap<E>,
    Custom=CustomRenderElements<R>,
    Preview=CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>,
}

impl<R: Renderer + ImportAll + ImportMem, E: RenderElement<R> + std::fmt::Debug> std::fmt::Debug
    for OutputRenderElements<R, E>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(arg0) => f.debug_tuple("Space").field(arg0).finish(),
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Custom(arg0) => f.debug_tuple("Custom").field(arg0).finish(),
            Self::Preview(arg0) => f.debug_tuple("Preview").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

pub fn space_preview_elements<'a, R, C>(
    renderer: &'a mut R,
    space: &'a Space<WindowElement>,
    output: &'a Output,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>> + 'a,
{
    let constrain_behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let preview_padding = 10;

    let elements_on_space = space.elements_for_output(output).count();
    let output_scale = output.current_scale().fractional_scale();
    let output_transform = output.current_transform();
    let output_size = output
        .current_mode()
        .map(|mode| {
            output_transform
                .transform_size(mode.size)
                .to_f64()
                .to_logical(output_scale)
        })
        .unwrap_or_default();

    let max_elements_per_row = 4;
    let elements_per_row = usize::min(elements_on_space, max_elements_per_row);
    let rows = f64::ceil(elements_on_space as f64 / elements_per_row as f64);

    let preview_size = Size::from((
        f64::round(output_size.w / elements_per_row as f64) as i32 - preview_padding * 2,
        f64::round(output_size.h / rows) as i32 - preview_padding * 2,
    ));

    space
        .elements_for_output(output)
        .enumerate()
        .flat_map(move |(element_index, window)| {
            let column = element_index % elements_per_row;
            let row = element_index / elements_per_row;
            let preview_location = Point::from((
                preview_padding + (preview_padding + preview_size.w) * column as i32,
                preview_padding + (preview_padding + preview_size.h) * row as i32,
            ));
            let constrain = Rectangle::new(preview_location, preview_size);
            constrain_space_element(
                renderer,
                window,
                preview_location,
                1.0,
                output_scale,
                constrain,
                constrain_behavior,
            )
        })
}

#[profiling::function]
#[allow(clippy::too_many_arguments)]
pub fn output_elements<R>(
    output: &Output,
    space: &Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &mut R,
    show_window_preview: bool,
    locked_surface: Option<&WlSurface>,
    borders: &[BorderRect],
    border_width: i32,
    border_active: Color32F,
    border_inactive: Color32F,
) -> (Vec<OutputRenderElements<R, WindowRenderElement<R>>>, Color32F)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    if let Some(surface) = locked_surface {
        // Session locked (Phase 4, ext-session-lock-v1): render only the
        // lock surface, covering the whole output over an opaque black
        // backdrop. Nothing else — not even a fullscreen app underneath —
        // should ever be visible while locked. `custom_elements` (the
        // cursor) still gets drawn, so the pointer stays visible.
        let scale = output.current_scale().fractional_scale().into();
        let mut elements: Vec<OutputRenderElements<R, WindowRenderElement<R>>> = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .collect();
        let lock_elements: Vec<CustomRenderElements<R>> = AsRenderElements::<R>::render_elements(
            &SurfaceTree::from_surface(surface),
            renderer,
            (0, 0).into(),
            scale,
            1.0,
        );
        elements.extend(lock_elements.into_iter().map(OutputRenderElements::from));
        return (elements, CLEAR_COLOR_LOCKED);
    }

    if let Some(window) = output
        .user_data()
        .get::<FullscreenSurface>()
        .and_then(|f| f.get())
    {
        let scale = output.current_scale().fractional_scale().into();
        let window_render_elements: Vec<WindowRenderElement<R>> =
            AsRenderElements::<R>::render_elements(&window, renderer, (0, 0).into(), scale, 1.0);

        let elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .chain(
                window_render_elements
                    .into_iter()
                    .map(|e| OutputRenderElements::Window(Wrap::from(e))),
            )
            .collect::<Vec<_>>();
        (elements, CLEAR_COLOR_FULLSCREEN)
    } else {
        let mut output_render_elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .collect::<Vec<_>>();

        if show_window_preview && space.elements_for_output(output).count() > 0 {
            output_render_elements.extend(space_preview_elements(renderer, space, output));
        }

        let space_elements = smithay::desktop::space::space_render_elements::<_, WindowElement, _>(
            renderer,
            [space],
            output,
            1.0,
        )
        .expect("output without mode?");
        output_render_elements.extend(space_elements.into_iter().map(OutputRenderElements::Space));

        // Borders (spitfire.border) — added at the front (top of the
        // z-order). Since each is a thin strip that never overlaps its
        // window's own footprint (see `border_elements`), where they land
        // in the render order doesn't actually matter for correctness; the
        // front avoids relying on the space elements' occlusion tracking
        // to punch a window-shaped hole through anything placed behind them.
        let scale = output.current_scale().fractional_scale().into();
        let border_elements = border_elements::<R>(borders, border_width, border_active, border_inactive, scale);
        for e in border_elements.into_iter().rev() {
            output_render_elements.insert(0, OutputRenderElements::from(e));
        }

        (output_render_elements, CLEAR_COLOR)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'a, 'd, R>(
    output: &'a Output,
    space: &'a Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    framebuffer: &'a mut R::Framebuffer<'_>,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    show_window_preview: bool,
    locked_surface: Option<&WlSurface>,
    borders: &[BorderRect],
    border_width: i32,
    border_active: Color32F,
    border_inactive: Color32F,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let (elements, clear_color) = output_elements(
        output,
        space,
        custom_elements,
        renderer,
        show_window_preview,
        locked_surface,
        borders,
        border_width,
        border_active,
        border_inactive,
    );
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}
