use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::WaylandSurfaceRenderElement,
                utils::{
                    ConstrainAlign, ConstrainScaleBehavior, CropRenderElement,
                    RelocateRenderElement, RescaleRenderElement,
                },
                AsRenderElements, Kind, RenderElement, Wrap,
            },
            Color32F, ImportAll, ImportMem, Renderer,
        },
    },
    desktop::space::{
        constrain_space_element, ConstrainBehavior, ConstrainReference, Space, SpaceElement,
        SpaceRenderElements, SurfaceTree,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    anim::AnimatedWindow,
    drawing::{PointerRenderElement, CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN, CLEAR_COLOR_LOCKED},
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    // Solid-color rectangles — shared by spitfire.border and the optional
    // built-in bar (Phase 8), both just stacks of colored rects.
    Solid=SolidColorRenderElement,
    // A whole batch of the bar's bitmap-font glyph rects (one color each),
    // bundled into a single element — see `bar::GlyphBatch`'s docs for why
    // that's not the same as `Solid` above, despite drawing the same kind
    // of rects.
    GlyphBatch=crate::bar::GlyphBatchElement,
    // spitfire.border.radius > 0: the four rounded-corner masks drawn on
    // top of a window's own square corners — see `CornerMaskCache`'s doc
    // comment for why this is a CPU-rasterized memory buffer rather than a
    // GLES pixel shader (the more obvious choice, and what niri/Strata both
    // use — see this fn's own doc comment above `border_elements` for why
    // it doesn't fit here).
    CornerMask=MemoryRenderBufferRenderElement<R>,
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
            Self::Solid(arg0) => f.debug_tuple("Solid").field(arg0).finish(),
            Self::GlyphBatch(arg0) => f.debug_tuple("GlyphBatch").field(arg0).finish(),
            Self::CornerMask(arg0) => f.debug_tuple("CornerMask").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

/// A persistent pool of `SolidColorBuffer`s, indexed positionally and
/// reused frame to frame instead of rebuilt from scratch.
///
/// `SolidColorBuffer::new` mints a brand-new `Id` every time it's called —
/// smithay's damage tracker keys on that `Id` (plus its `CommitCounter`) to
/// tell whether an element is the same one it saw last frame or new
/// content that needs damaging. Call `SolidColorBuffer::new` fresh every
/// frame (as `spitfire.border` and the bar both used to) and every rect
/// looks brand-new every single frame, even when nothing on screen actually
/// changed — which means the output never has zero damage, which means the
/// compositor never stops repainting at the full display refresh rate, even
/// sitting fully idle. Reusing the same buffer (and only touching it via
/// `update`, which no-ops when size/color haven't changed) keeps the same
/// `Id`/`CommitCounter` across frames instead, so an unchanged rect reads as
/// unchanged.
///
/// Used positionally: callers draw rects in the same order each frame
/// (`next`), then call `finish_frame` once done. As long as the *content*
/// at a given position is unchanged from the previous frame, `next` returns
/// the exact same buffer with an unchanged commit — zero damage. If a frame
/// draws fewer rects than the last one, `finish_frame` drops the leftover
/// buffers, correctly damaging whatever they used to cover.
#[derive(Debug, Default)]
pub struct RectCache {
    buffers: Vec<SolidColorBuffer>,
    used: usize,
}

impl RectCache {
    pub(crate) fn next(&mut self, size: Size<i32, Logical>, color: Color32F) -> &SolidColorBuffer {
        match self.buffers.get_mut(self.used) {
            Some(buffer) => buffer.update(size, color),
            None => self.buffers.push(SolidColorBuffer::new(size, color)),
        }
        let buffer = &self.buffers[self.used];
        self.used += 1;
        buffer
    }

    /// Call once after a frame's rects have all been drawn through `next`:
    /// drops any buffers left over from a previous frame that drew more
    /// content than this one did, and resets the cursor for next frame.
    pub fn finish_frame(&mut self) {
        self.buffers.truncate(self.used);
        self.used = 0;
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

/// One of the four corners a `spitfire.border.radius` mask sits at.
/// `corner_mask_bgra` only ever computes the `TopLeft` shape directly — the
/// other three are the same pixels mirrored, via `flips()`.
#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    /// (flip_x, flip_y) relative to the canonical `TopLeft` orientation —
    /// see `corner_mask_bgra`'s doc comment.
    fn flips(self) -> (bool, bool) {
        match self {
            Corner::TopLeft => (false, false),
            Corner::TopRight => (true, false),
            Corner::BottomLeft => (false, true),
            Corner::BottomRight => (true, true),
        }
    }
}

/// Smooth 0→1 transition over `[edge0, edge1]`, clamped outside it — the
/// standard GLSL `smoothstep`, used here to antialias `corner_mask_bgra`'s
/// two edges by hand instead of relying on a GPU rasterizer's MSAA (there
/// isn't one in play; this mask is rasterized once on the CPU, not redrawn
/// every frame).
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rasterizes one `(radius + width)`-square corner tile, premultiplied
/// `Fourcc::Argb8888` bytes (B,G,R,A in ascending memory order — that
/// fourcc names a little-endian `0xAARRGGBB` word).
///
/// The canonical (`flip_x = flip_y = false`, i.e. `Corner::TopLeft`) shape
/// has its arc centered on the tile's bottom-right corner: within `radius`
/// of that center is fully transparent (the window's own content shows
/// through there undisturbed), beyond `radius + width` is also fully
/// transparent (outside the border ring entirely), and the `width`-thick
/// band in between is `color`. Since `CornerMaskCache` places this tile
/// flush with the window's actual (square) corner and draws it in front of
/// the window (see `border_elements`'s doc comment), that band ends up
/// painting over the small triangular sliver of the window's *own* square
/// corner that pokes out past the rounded inner edge — which is what
/// actually sells the illusion that the window itself is rounded, without
/// the compositor ever clipping the client's real content.
///
/// `flip_x`/`flip_y` mirror this shape to get the other three corners —
/// see `Corner::flips`.
fn corner_mask_bgra(
    radius: i32,
    width: i32,
    color: Color32F,
    flip_x: bool,
    flip_y: bool,
) -> Vec<u8> {
    let tile = (radius + width).max(1);
    let s = tile as f32;
    let inner = radius as f32;
    let outer = (radius + width) as f32;

    let mut bytes = vec![0u8; (tile * tile * 4) as usize];
    for y in 0..tile {
        for x in 0..tile {
            // Pixel centers, so the antialiasing band straddles the true
            // radius instead of being offset by half a pixel.
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = if flip_x { s - px } else { px };
            let dy = if flip_y { s - py } else { py };
            let d = ((s - dx).powi(2) + (s - dy).powi(2)).sqrt();

            // 1: inside the outer edge. 0: past it (background shows
            // through). Mirrored for the inner edge, where 1 means "in the
            // border band, not the window-content hole".
            let outer_alpha = 1.0 - smoothstep(0.0, 1.0, d - outer);
            let inner_alpha = 1.0 - smoothstep(0.0, 1.0, inner - d);
            let alpha = outer_alpha.min(inner_alpha).clamp(0.0, 1.0);

            let i = ((y * tile + x) * 4) as usize;
            bytes[i] = (color.b() * alpha * 255.0).round() as u8;
            bytes[i + 1] = (color.g() * alpha * 255.0).round() as u8;
            bytes[i + 2] = (color.r() * alpha * 255.0).round() as u8;
            bytes[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    bytes
}

/// The 8 rounded-corner mask buffers `spitfire.border.radius` needs (one
/// per `Corner`, times active/inactive), rebuilt only when `radius`,
/// `width`, or either color actually changes — e.g. after
/// `spitfire.reload()` picks up an edited `spitfire.border` table. Baking
/// these once and reusing the same `MemoryRenderBuffer`s frame to frame
/// matters for the same reason `RectCache` reuses `SolidColorBuffer`s (see
/// its own doc comment): a fresh buffer every frame never damage-tracks as
/// "unchanged", which would keep the compositor repainting at full refresh
/// rate forever, even sitting idle.
///
/// Why a CPU-rasterized memory buffer instead of a GLES pixel shader (the
/// more obvious choice — and what both niri and Strata actually use for
/// this): `border_elements` is generic over the renderer (`R: Renderer +
/// ImportAll + ImportMem`), shared verbatim by the winit backend (`R =
/// GlesRenderer` directly) and the udev/DRM backend (`R = UdevRenderer`, a
/// `MultiRenderer` wrapping possibly multiple GPUs' `GlesRenderer`s).
/// Smithay's `PixelShaderElement` only implements `RenderElement` for a
/// concrete `GlesRenderer`/`GlowRenderer`, not generically over `R` — and
/// bridging that gap (via `render_elements!`'s `as <GlesRenderer>` syntax)
/// needs `R: AsMut<GlesRenderer>`, which `UdevRenderer` has but plain
/// `GlesRenderer` itself doesn't (there's no `impl AsMut<GlesRenderer> for
/// GlesRenderer` in smithay, and orphan rules block adding one here) — so
/// the same bound can't satisfy both backends at once without either
/// forking this function in two or wrapping winit's renderer in a
/// single-arm `MultiRenderer` just to get the impl. `MemoryRenderBuffer`/
/// `MemoryRenderBufferRenderElement<R>`, by contrast, are already generic
/// over `R` (see `drawing::PointerRenderElement`, which renders the cursor
/// through the exact same buffer type in both backends today) — no
/// GPU-side shader compilation, no cross-backend generics fighting, at the
/// cost of only being able to round a fixed radius baked in at
/// construction rather than sampling one continuously in a shader (a
/// non-issue here: `spitfire.border.radius` is one global config value,
/// not something that varies per frame).
#[derive(Default)]
pub struct CornerMaskCache {
    key: Option<(i32, i32, Color32F, Color32F)>,
    // Indexed like `Corner::ALL`.
    active: Option<[MemoryRenderBuffer; 4]>,
    inactive: Option<[MemoryRenderBuffer; 4]>,
}

impl CornerMaskCache {
    fn ensure(
        &mut self,
        radius: i32,
        width: i32,
        active_color: Color32F,
        inactive_color: Color32F,
    ) {
        let key = (radius, width, active_color, inactive_color);
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        let build = |color: Color32F| {
            Corner::ALL.map(|corner| {
                let (flip_x, flip_y) = corner.flips();
                let tile = (radius + width).max(1);
                let bytes = corner_mask_bgra(radius, width, color, flip_x, flip_y);
                MemoryRenderBuffer::from_slice(
                    &bytes,
                    Fourcc::Argb8888,
                    (tile, tile),
                    1,
                    Transform::Normal,
                    None,
                )
            })
        };
        self.active = Some(build(active_color));
        self.inactive = Some(build(inactive_color));
    }

    fn mask(&self, focused: bool, corner: Corner) -> &MemoryRenderBuffer {
        let set = if focused {
            &self.active
        } else {
            &self.inactive
        };
        &set.as_ref()
            .expect("CornerMaskCache::ensure not called before mask")[corner as usize]
    }
}

/// Builds `spitfire.border`'s ring around each entry in `borders` —
/// dwm-style, without the layout engine needing to reserve any space for
/// it. With `radius <= 0` (the default) this is four thin solid-color
/// strips (top/bottom/left/right) that never overlap the window's own
/// footprint at all, so where they land in the render order doesn't
/// matter: a single rect placed behind relies on the window's own (opaque)
/// element correctly punching a window-shaped hole through it via the
/// renderer's occlusion tracking, and in practice that undersized the
/// visible region to nothing.
///
/// With `radius > 0` (`spitfire.border.radius`), the same four strips get
/// trimmed shorter by `radius` at each end, and the resulting gaps at the
/// four corners are filled by `CornerMaskCache`'s masks — which *do*
/// overlap the window's own square corners, and are drawn on top of them
/// (see `output_elements`: border elements are always inserted at the
/// front of the render order). That overlap is exactly what makes an
/// otherwise perfectly square client surface read as rounded, without the
/// compositor ever clipping the client's actual content — see
/// `corner_mask_bgra`'s doc comment for the mechanics.
#[allow(clippy::too_many_arguments)]
pub fn border_elements<R>(
    borders: &[BorderRect],
    width: i32,
    radius: i32,
    active_color: Color32F,
    inactive_color: Color32F,
    output_scale: Scale<f64>,
    cache: &mut RectCache,
    corner_masks: &mut CornerMaskCache,
    renderer: &mut R,
) -> Vec<CustomRenderElements<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    if width <= 0 {
        cache.finish_frame();
        return Vec::new();
    }
    // Clamp: a radius covering more than half the window's own smallest
    // dimension has no sensible strip length left to trim to, and a
    // corner tile bigger than the window is nonsensical to draw at all.
    let max_radius = borders
        .iter()
        .map(|b| b.geometry.size.w.min(b.geometry.size.h) / 2)
        .min()
        .unwrap_or(0);
    let radius = radius.clamp(0, max_radius.max(0));

    let mut elements = Vec::new();

    if radius > 0 {
        corner_masks.ensure(radius, width, active_color, inactive_color);
        for b in borders {
            let g = b.geometry;
            let corners = [
                (Corner::TopLeft, (g.loc.x - width, g.loc.y - width)),
                (
                    Corner::TopRight,
                    (g.loc.x + g.size.w - radius, g.loc.y - width),
                ),
                (
                    Corner::BottomLeft,
                    (g.loc.x - width, g.loc.y + g.size.h - radius),
                ),
                (
                    Corner::BottomRight,
                    (g.loc.x + g.size.w - radius, g.loc.y + g.size.h - radius),
                ),
            ];
            for (corner, loc) in corners {
                let loc: Point<i32, Logical> = loc.into();
                let physical_loc: Point<f64, Physical> = loc.to_f64().to_physical(output_scale);
                let mask = corner_masks.mask(b.focused, corner);
                let element = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    physical_loc,
                    mask,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                )
                .expect("failed to upload spitfire.border.radius corner mask");
                elements.push(CustomRenderElements::CornerMask(element));
            }
        }
    }

    let strip_elements = borders
        .iter()
        .flat_map(|b| {
            let color = if b.focused {
                active_color
            } else {
                inactive_color
            };
            let g = b.geometry;
            // Four strips forming a hollow ring around `g`, each `width`
            // thick. With `radius == 0` they meet flush at the corners; with
            // `radius > 0` they're trimmed `radius` shorter at each end that
            // meets a corner mask (see this fn's doc comment) — either way,
            // none overlap `g` itself.
            let strips = [
                // top
                Rectangle::new(
                    (g.loc.x - width + radius, g.loc.y - width).into(),
                    (g.size.w + width * 2 - radius * 2, width).into(),
                ),
                // bottom
                Rectangle::new(
                    (g.loc.x - width + radius, g.loc.y + g.size.h).into(),
                    (g.size.w + width * 2 - radius * 2, width).into(),
                ),
                // left
                Rectangle::new(
                    (g.loc.x - width, g.loc.y + radius).into(),
                    (width, g.size.h - radius * 2).into(),
                ),
                // right
                Rectangle::new(
                    (g.loc.x + g.size.w, g.loc.y + radius).into(),
                    (width, g.size.h - radius * 2).into(),
                ),
            ];
            strips.map(|strip: Rectangle<i32, smithay::utils::Logical>| (strip, color))
        })
        .filter(|(strip, _)| strip.size.w > 0 && strip.size.h > 0)
        .map(|(strip, color)| {
            let buffer = cache.next(strip.size, color);
            let loc: Point<i32, Physical> =
                strip.loc.to_f64().to_physical(output_scale).to_i32_round();
            CustomRenderElements::Solid(SolidColorRenderElement::from_buffer(
                buffer,
                loc,
                output_scale,
                1.0,
                Kind::Unspecified,
            ))
        });
    elements.extend(strip_elements);
    cache.finish_frame();
    elements
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Window=Wrap<E>,
    Custom=CustomRenderElements<R>,
    // Also reused for a window mid open-fade or move/resize tween
    // (spitfire.anim, see `output_elements`) — same wrapped type, built by
    // the same `constrain_space_element` helper, just at a different call
    // site. `render_elements!` derives one `From<T>` impl per inner type,
    // so a same-typed second variant would conflict with this one — no
    // need for one anyway, the two never appear in the same code path.
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
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>>
        + 'a,
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
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
    border_cache: &mut RectCache,
    corner_masks: &mut CornerMaskCache,
    anims: &[AnimatedWindow],
) -> (
    Vec<OutputRenderElements<R, WindowRenderElement<R>>>,
    Color32F,
)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
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

        let output_scale: Scale<f64> = output.current_scale().fractional_scale().into();
        let space_elements = smithay::desktop::space::space_render_elements::<_, WindowElement, _>(
            renderer,
            [space],
            output,
            1.0,
        )
        .expect("output without mode?");

        if anims.is_empty() {
            // Byte-for-byte today's path when nothing is animating — idle
            // stays exactly zero-damage, see `RectCache`'s doc comment for
            // why that matters.
            output_render_elements
                .extend(space_elements.into_iter().map(OutputRenderElements::Space));
        } else {
            // spitfire.anim: a window mid open-fade or move/resize tween
            // needs a per-window transform (`constrain_space_element`,
            // below) instead of the blanket call above — but that blanket
            // call is still the simplest correct source of the layer-shell
            // elements it also produces alongside the window ones. Keep
            // those (`SpaceRenderElements::Surface` — smithay only ever
            // produces that variant for layer-shell surfaces, never for
            // windows, which come through `Element` instead — see
            // smithay's `desktop::space::space_render_elements`) and
            // rebuild only the window portion ourselves below.
            output_render_elements.extend(
                space_elements
                    .into_iter()
                    .filter(|e| matches!(e, SpaceRenderElements::Surface(_)))
                    .map(OutputRenderElements::Space),
            );

            let output_geo = space.output_geometry(output).unwrap_or_default();
            let behavior = ConstrainBehavior {
                reference: ConstrainReference::Geometry,
                behavior: ConstrainScaleBehavior::Stretch,
                align: ConstrainAlign::CENTER,
            };
            for window in space.elements_for_output(output).rev() {
                if let Some(anim) = anims.iter().find(|a| &a.window == window) {
                    let elements = constrain_space_element(
                        renderer,
                        window,
                        anim.target.loc,
                        anim.alpha,
                        output_scale,
                        anim.target,
                        behavior,
                    );
                    output_render_elements.extend(elements.map(OutputRenderElements::Preview));
                } else {
                    // Same physical-location formula `Space`'s own internal
                    // element wrapper uses (smithay's
                    // `desktop::space::mod.rs`, `InnerElement::render_location`),
                    // `output_geo` included for when this crate grows
                    // multi-output — so a non-animating window renders
                    // identically to the blanket-call path above, even on a
                    // frame where some *other* window is animating.
                    let Some(loc) = space.element_location(window) else {
                        continue;
                    };
                    let render_loc = (loc - SpaceElement::geometry(window).loc) - output_geo.loc;
                    let elements: Vec<WindowRenderElement<R>> =
                        AsRenderElements::<R>::render_elements(
                            window,
                            renderer,
                            render_loc.to_physical_precise_round(output_scale),
                            output_scale,
                            1.0,
                        );
                    output_render_elements.extend(
                        elements
                            .into_iter()
                            .map(|e| OutputRenderElements::Window(Wrap::from(e))),
                    );
                }
            }
        }

        // Borders (spitfire.border) — added at the front (top of the
        // z-order). Since each is a thin strip that never overlaps its
        // window's own footprint (see `border_elements`), where they land
        // in the render order doesn't actually matter for correctness; the
        // front avoids relying on the space elements' occlusion tracking
        // to punch a window-shaped hole through anything placed behind them.
        let border_elements = border_elements::<R>(
            borders,
            border_width,
            border_radius,
            border_active,
            border_inactive,
            output_scale,
            border_cache,
            corner_masks,
            renderer,
        );
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
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
    border_cache: &mut RectCache,
    corner_masks: &mut CornerMaskCache,
    anims: &[AnimatedWindow],
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
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
        border_radius,
        border_active,
        border_inactive,
        border_cache,
        corner_masks,
        anims,
    );
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}
