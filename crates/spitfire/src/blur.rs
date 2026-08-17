//! `spitfire.rule({ blur = true })` — a frosted-glass backdrop rendered
//! right behind a window's own content, so a window with real per-pixel
//! alpha in its own buffer (a terminal with `background_opacity`/`opacity`
//! set, a launcher like rofi/wofi) shows a blurred version of whatever's
//! behind it through the translucent parts, instead of the sharp original.
//!
//! wasp gets this "for free" from SceneFX, a drop-in wlroots scene-graph
//! replacement (see the roadmap this item comes from). spitfire has no such
//! layer — this is a hand-rolled two-pass separable GLES Gaussian blur,
//! wired into `render::output_elements`'s existing per-window loop rather
//! than a scene-graph feature. Real screen only for now: `screencopy.rs`/
//! `ext_screencopy.rs` don't call into this module, so a capture currently
//! shows a `blur = true` window as if the rule weren't set (a known,
//! documented gap, not a bug — reproducing this for captures too would mean
//! computing a second backdrop during every capture, which none of the
//! existing capture call sites do today for anything render-cost-shaped
//! like this).
//!
//! ## Pipeline, once per frame a `blur = true` window is visible
//!
//! 1. [`blur_windows_for_output`] — which of this output's windows actually
//!    need a backdrop this frame (rule match, alive, mapped, not the
//!    fullscreen surface). Empty is the overwhelmingly common case (nobody
//!    using `blur`, or nothing currently blurred is on screen) and short-
//!    circuits everything below — no GL work at all when it is.
//! 2. Caller re-invokes `render::output_elements` with those windows passed
//!    as `hidden_windows` — same trick `crate::screencopy` already uses for
//!    `hide_from_capture`, just aimed the other way (excluding only the
//!    windows *about to get* a blur backdrop, keeping every other window,
//!    border, and layer-shell surface in the list) — producing the exact
//!    element set that should show up blurred behind them.
//! 3. [`capture_backdrop`] renders that element list into an offscreen
//!    `GlesTexture` the size of the whole output — generic over the
//!    backend's own renderer type (`R`, `GlesRenderer` for winit,
//!    `UdevRenderer` for udev), reusing the exact `Offscreen<GlesTexture>`+
//!    `Bind<GlesTexture>` pattern `crate::screencopy`'s own offscreen
//!    captures already rely on, for the same reason (see that module's
//!    `service_pending_captures` doc comment on why the target is always a
//!    concrete `GlesTexture`, never `R::TextureId` itself).
//! 4. [`blur_backdrops`] does the actual shader work — for each window,
//!    crop the backdrop texture to that window's own on-screen rect (with
//!    natural bleed from neighboring background, sampled straight out of
//!    the full backdrop texture, not clamped to the window's own edges —
//!    see its own doc comment), then two GLES passes (horizontal, then
//!    vertical) of a separable Gaussian, and reads the result back to a
//!    `MemoryRenderBuffer` per window.
//! 5. Caller re-invokes `render::output_elements` a *second* time, this
//!    time for real (no `hidden_windows`), passing the `Vec<BlurBackdrop>`
//!    from step 4 as its new `blur_backdrops` param — the per-window loop
//!    there pushes each one as a plain `MemoryRenderBufferRenderElement`
//!    immediately behind that window's own content (see that fn's own
//!    comment for the exact push-order reasoning).
//! 6. Caller forces a full redraw for that same real frame (winit.rs:
//!    `age = 0` passed to `render_output` instead of the real buffer age;
//!    udev.rs: see the known-gap note left where it *would* call
//!    `DrmCompositor::reset_buffer_ages`). Found the hard way, live: a
//!    brand-new `MemoryRenderBuffer` (a brand-new `Id`) every frame, at a
//!    screen position that hasn't necessarily moved, isn't on its own
//!    enough to guarantee the region actually gets repainted rather than
//!    left showing whatever the *previous* frame had there — the window's
//!    own real alpha blending was compositing correctly, just against
//!    stale (pre-blur, sharp) framebuffer content instead of this frame's
//!    backdrop. `buffer_age()`-based partial redraw has no way to know a
//!    *content* change happened here that should count as damage, only
//!    that some element's identity changed.
//!
//! Step 3's shader work needs `GlesRenderer`'s own inherent methods
//! (`compile_custom_texture_shader`, `render_texture_from_to` with a custom
//! program) — none of that exists on the generic `Renderer`/`Frame` traits,
//! so unlike steps 2/3 this genuinely can't stay generic over `R`. For
//! winit `R` already *is* `GlesRenderer`, nothing to convert. For udev `R`
//! is `MultiRenderer`, which has its own `AsMut<GlesRenderer>` (used to
//! reach the real per-GPU renderer for exactly this kind of thing) — but
//! plain `GlesRenderer` has no `impl AsMut<GlesRenderer> for GlesRenderer`
//! to lean on for the winit case (`render::CornerMaskCache`'s doc comment
//! hit this same gap first, choosing a CPU-rasterized buffer instead of a
//! GLES shader for exactly this reason). Blur can't dodge needing the real
//! shader entry points the way border corners did, so each backend's own
//! render loop is expected to get `&mut GlesRenderer` however it can
//! (winit.rs: already has it; udev.rs: `renderer.as_mut()`) and call
//! [`blur_backdrops`] with that directly, rather than this module trying to
//! abstract the difference away itself.
//!
//! **Untested on real hardware (`--udev`)**: every reproduction so far is
//! nested `spitfire --winit` (see `nested-winit-testing-blind-spots` in
//! this project's own notes on that environment's limits). The multi-GPU
//! case in particular rests on an unverified assumption: that
//! `MultiRenderer`'s `Offscreen<GlesTexture>` (used for the whole-output
//! backdrop capture) and its `AsMut<GlesRenderer>` (used for the shader
//! passes) resolve to the *same* GPU context, so a `GlesTexture` made via
//! one is safely sampled from the other. True on any single-GPU box
//! (the common case) by construction; unverified for a real multi-GPU
//! `--udev` session.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::memory::MemoryRenderBuffer,
            gles::{
                GlesError, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
                UniformType,
            },
            Bind, Color32F, ImportAll, ImportMem, Offscreen, Renderer,
        },
    },
    desktop::space::Space,
    output::Output,
    utils::{Buffer as BufferCoord, IsAlive, Physical, Rectangle, Scale, Size, Transform},
};

use crate::{
    render::OutputRenderElements,
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};
use spitfire_config::WindowRule;

/// `//_DEFINES_` is smithay's own placeholder line — `GlesRenderer::
/// compile_custom_texture_shader` replaces it with the real `#define`s for
/// the `EXTERNAL`/`NO_ALPHA`/`DEBUG_FLAGS` variants it compiles from this
/// one source (see that fn's own doc comment). `blur_step` is the one
/// uniform this shader adds beyond the standard `tex`/`alpha`/`v_coords`
/// every custom texture shader gets for free — a texture-space UV offset
/// *per tap*, already carrying both the blur radius and which axis this
/// pass runs along (see `blur_backdrops`'s horizontal/vertical calls). The
/// Gaussian weight itself is computed in-shader from a fixed sigma tied to
/// the (also fixed) 15-tap spread, not from a `radius` uniform — spreading
/// `blur_step` further apart already widens the effective blur without the
/// shader needing to know the pixel radius at all.
const BLUR_FRAG_SHADER: &str = r#"
#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
uniform vec2 blur_step;
varying vec2 v_coords;

void main() {
    vec4 sum = vec4(0.0);
    float wsum = 0.0;
    for (int i = 0; i < 15; i++) {
        float x = float(i) - 7.0;
        // sigma == 3.0 (2*sigma^2 == 18.0) — a fixed shape across the fixed
        // 15-tap spread; `blur_step`'s own magnitude is what actually
        // controls how wide the blur reads on screen.
        float w = exp(-(x * x) / 18.0);
        vec4 s = texture2D(tex, v_coords + blur_step * x);
#if defined(NO_ALPHA)
        s = vec4(s.rgb, 1.0);
#endif
        sum += s * w;
        wsum += w;
    }
    gl_FragColor = (sum / wsum) * alpha;
}
"#;

/// Compiled once per `GlesRenderer` context and reused every frame after —
/// see `ensure_program`. Lives on `SpitfireState` (both backends): for
/// winit that's one persistent context for the whole session; for udev it
/// rides along with whichever GPU `AsMut<GlesRenderer>` keeps resolving to
/// (see this module's own doc comment for the untested-on-real-multi-GPU
/// caveat). If the context is ever lost and recreated from under a cached
/// program handle, the failure mode is the same class of already-known,
/// already-tracked risk as `egl-context-loss-second-offscreen-capture` —
/// not something this module tries to detect or recover from itself.
#[derive(Debug, Default)]
pub struct BlurState {
    program: Option<GlesTexProgram>,
}

fn ensure_program<'a>(
    gles: &mut GlesRenderer,
    state: &'a mut BlurState,
) -> Result<&'a GlesTexProgram, GlesError> {
    if state.program.is_none() {
        let program = gles.compile_custom_texture_shader(
            BLUR_FRAG_SHADER,
            &[UniformName::new("blur_step", UniformType::_2f)],
        )?;
        state.program = Some(program);
    }
    Ok(state.program.as_ref().expect("just set above"))
}

/// One window's blurred backdrop, ready to feed straight into
/// `render::output_elements`'s `blur_backdrops` param. `physical_loc`/
/// `logical_size` describe where and how big to draw `buffer` — `buffer`'s
/// own raw pixel dimensions are physical (device) pixels (see
/// `blur_backdrops`), so `logical_size` is passed to
/// `MemoryRenderBufferRenderElement::from_buffer`'s `size` override rather
/// than left to that fn's own scale-1 default, which would otherwise
/// double-apply the output scale on top of pixels that already have it
/// baked in.
pub struct BlurBackdrop {
    pub window: WindowElement,
    pub buffer: MemoryRenderBuffer,
    pub physical_loc: smithay::utils::Point<i32, Physical>,
    pub logical_size: Size<i32, smithay::utils::Logical>,
}

/// Which of `output`'s windows currently need a blur backdrop this frame —
/// `spitfire.rule({ blur = true })` matches that are alive, actually mapped
/// on `output`'s space, and not the one window rendered by output_elements'
/// separate fullscreen branch (which bypasses the per-window loop
/// `blur_backdrops` elements get pushed into entirely, so a backdrop
/// computed for it would just be wasted GL work — same reasoning
/// `hide_from_capture` doesn't special-case fullscreen either, except here
/// it actually matters for cost, not just correctness).
///
/// Empty in the common case (no `blur = true` rule, or nothing currently
/// matching one is on screen) — every caller treats that as "skip the rest
/// of this module's pipeline entirely, zero extra GL work", which is the
/// whole reason this exists as its own cheap up-front check.
pub fn blur_windows_for_output(
    rules: &[WindowRule],
    space: &Space<WindowElement>,
    output: &Output,
) -> Vec<WindowElement> {
    if output
        .user_data()
        .get::<FullscreenSurface>()
        .and_then(|f| f.get())
        .is_some()
    {
        return Vec::new();
    }
    space
        .elements_for_output(output)
        .filter(|window| {
            rules
                .iter()
                .any(|rule| rule.blur && rule.matches(window.app_id().as_deref()))
        })
        .cloned()
        .collect()
}

/// Renders `elements` (the whole output, minus whichever windows are about
/// to get a blur backdrop — see this module's own doc comment, step 2) into
/// an offscreen `GlesTexture` the size of `output`'s own mode, and hands
/// back the texture plus its physical size. `None` on any failure (no
/// current mode, GL error) — callers treat that the same as "no blur
/// windows this frame": skip the rest of the pipeline for this frame rather
/// than propagate an error up through the real render path over what's
/// fundamentally an optional visual extra.
///
/// Generic over `R` exactly like `crate::screencopy::render_and_copy` —
/// the offscreen *target* is a fixed concrete `GlesTexture` (`Offscreen<
/// GlesTexture>`/`Bind<GlesTexture>`, implemented by `MultiRenderer` too,
/// not just `GlesRenderer` directly) so this works unchanged on both
/// backends, while `elements` stays typed by `R::TextureId` so each
/// window's real client buffer gets imported correctly for whichever GPU
/// `R` actually is.
pub fn capture_backdrop<R>(
    renderer: &mut R,
    output: &Output,
    elements: &[OutputRenderElements<R, WindowRenderElement<R>>],
    scale: Scale<f64>,
) -> Option<(GlesTexture, Size<i32, Physical>)>
where
    R: Renderer + ImportAll + ImportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    let mode = output.current_mode()?;
    let size: Size<i32, Physical> = mode.size;
    let texture_size: Size<i32, BufferCoord> = (size.w, size.h).into();

    let mut texture = renderer
        .create_buffer(Fourcc::Argb8888, texture_size)
        .ok()?;
    let mut target = renderer.bind(&mut texture).ok()?;

    let mut damage_tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
    damage_tracker
        .render_output(
            renderer,
            &mut target,
            0,
            elements,
            crate::drawing::CLEAR_COLOR,
        )
        .ok()?;
    drop(target);

    Some((texture, size))
}

/// Crops `backdrop` to each of `blur_windows`' own on-screen rect and runs
/// the two-pass separable Gaussian on it — see this module's doc comment
/// for the overall pipeline and why this step specifically needs a
/// concrete `&mut GlesRenderer` rather than staying generic.
///
/// The horizontal pass samples straight out of `backdrop` (the *whole*
/// output, not pre-cropped) with the destination sized to just the
/// window's rect — `render_texture_from_to`'s own src/dest mapping does
/// the cropping, and critically, sampling `blur_step * x` for the taps
/// nearest a window's edge naturally reads a few pixels of whatever's
/// *outside* that window's own bounds too (still well within `backdrop`,
/// which covers the full output). That's deliberate: it's what makes the
/// blur read as "the background bleeding through/around the window" rather
/// than the window's edge itself smearing into flat color. The vertical
/// pass, sampling the (now window-sized) intermediate texture instead,
/// doesn't get that same margin — GLES's default edge-clamping quietly
/// repeats the nearest already-blurred edge pixel there instead, a minor,
/// accepted asymmetry rather than carrying padding through both passes.
#[allow(clippy::too_many_arguments)]
pub fn blur_backdrops(
    gles: &mut GlesRenderer,
    state: &mut BlurState,
    backdrop: &GlesTexture,
    backdrop_size: Size<i32, Physical>,
    space: &Space<WindowElement>,
    output: &Output,
    blur_windows: &[WindowElement],
    radius: i32,
    scale: Scale<f64>,
) -> Vec<BlurBackdrop> {
    if blur_windows.is_empty() || radius <= 0 {
        return Vec::new();
    }
    let Ok(program) = ensure_program(gles, state).cloned() else {
        return Vec::new();
    };

    let output_geo = space.output_geometry(output).unwrap_or_default();
    let backdrop_bounds: Rectangle<i32, Physical> = Rectangle::from_size(backdrop_size);

    let mut results = Vec::with_capacity(blur_windows.len());
    for window in blur_windows {
        if !window.alive() {
            continue;
        }
        let Some(geo) = space.element_geometry(window) else {
            continue;
        };
        let rect: Rectangle<i32, Physical> =
            Rectangle::new(geo.loc - output_geo.loc, geo.size).to_physical_precise_round(scale);
        let Some(clipped) = rect.intersection(backdrop_bounds) else {
            continue;
        };
        if clipped.size.w <= 0 || clipped.size.h <= 0 {
            continue;
        }

        let Some(buffer) = blur_one(gles, &program, backdrop, backdrop_size, clipped, radius)
        else {
            continue;
        };

        results.push(BlurBackdrop {
            window: window.clone(),
            buffer,
            physical_loc: clipped.loc,
            logical_size: clipped.size.to_f64().to_logical(scale).to_i32_round(),
        });
    }
    results
}

/// The actual two-pass render for one window's already-clipped rect — split
/// out of `blur_backdrops` just so the `?`-heavy GL sequence (any failure
/// here just skips this one window's backdrop, not the whole frame) doesn't
/// have to fight that fn's per-window loop control flow.
fn blur_one(
    gles: &mut GlesRenderer,
    program: &GlesTexProgram,
    backdrop: &GlesTexture,
    backdrop_size: Size<i32, Physical>,
    clipped: Rectangle<i32, Physical>,
    radius: i32,
) -> Option<MemoryRenderBuffer> {
    use smithay::backend::renderer::{ExportMem, Frame};

    let size = clipped.size;
    let dst = Rectangle::from_size(size);
    let damage = [dst];
    let region: Rectangle<i32, BufferCoord> =
        Rectangle::new((0, 0).into(), (size.w, size.h).into());

    // Pass 1 (horizontal): backdrop -> tmp, cropped to `clipped`, stepping
    // in `backdrop`'s own texel space (see this module's doc comment on why
    // that's deliberate — it lets the blur bleed in neighboring background
    // beyond the window's own edge on this pass).
    let mut tmp: GlesTexture = gles
        .create_buffer(Fourcc::Argb8888, (size.w, size.h).into())
        .ok()?;
    let mut target = gles.bind(&mut tmp).ok()?;
    {
        let mut frame = gles.render(&mut target, size, Transform::Normal).ok()?;
        frame.clear(Color32F::TRANSPARENT, &[dst]).ok()?;
        let src: Rectangle<f64, BufferCoord> = Rectangle::new(
            (clipped.loc.x as f64, clipped.loc.y as f64).into(),
            (size.w as f64, size.h as f64).into(),
        );
        let step_x = (radius as f32 / 7.0) / backdrop_size.w.max(1) as f32;
        frame
            .render_texture_from_to(
                backdrop,
                src,
                dst,
                &damage,
                &[],
                Transform::Normal,
                1.0,
                Some(program),
                &[Uniform::new("blur_step", (step_x, 0.0f32))],
            )
            .ok()?;
        let _ = frame.finish().ok()?;
    }
    drop(target);

    // Pass 2 (vertical): tmp -> blurred, same size, stepping in `tmp`'s own
    // (window-rect-sized) texel space this time — see this fn's doc comment
    // on the resulting edge-clamp asymmetry. Read back to CPU straight out
    // of the same binding once this pass finishes — same "bind once, render,
    // then copy_framebuffer from that same target" shape
    // `ext_screencopy::render_window_and_copy` already uses.
    let mut blurred: GlesTexture = gles
        .create_buffer(Fourcc::Argb8888, (size.w, size.h).into())
        .ok()?;
    let mut target = gles.bind(&mut blurred).ok()?;
    {
        let mut frame = gles.render(&mut target, size, Transform::Normal).ok()?;
        frame.clear(Color32F::TRANSPARENT, &[dst]).ok()?;
        let src: Rectangle<f64, BufferCoord> =
            Rectangle::new((0.0, 0.0).into(), (size.w as f64, size.h as f64).into());
        let step_y = (radius as f32 / 7.0) / size.h.max(1) as f32;
        frame
            .render_texture_from_to(
                &tmp,
                src,
                dst,
                &damage,
                &[],
                Transform::Normal,
                1.0,
                Some(program),
                &[Uniform::new("blur_step", (0.0f32, step_y))],
            )
            .ok()?;
        let _ = frame.finish().ok()?;
    }

    let mapping = gles
        .copy_framebuffer(&target, region, Fourcc::Argb8888)
        .ok()?;
    let bytes = gles.map_texture(&mapping).ok()?.to_vec();

    Some(MemoryRenderBuffer::from_slice(
        &bytes,
        Fourcc::Argb8888,
        (size.w, size.h),
        1,
        Transform::Normal,
        None,
    ))
}
