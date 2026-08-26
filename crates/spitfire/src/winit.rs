use std::{sync::atomic::Ordering, time::Duration};

#[cfg(feature = "egl")]
use smithay::backend::renderer::ImportEgl;
#[cfg(feature = "debug")]
use smithay::{
    backend::{allocator::Fourcc, renderer::ImportMem},
    reexports::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        egl::EGLDevice,
        renderer::{
            damage::{Error as OutputDamageTrackerError, OutputDamageTracker},
            gles::GlesRenderer,
            ImportDma, ImportMemWl,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
        SwapBuffersError,
    },
    delegate_dmabuf,
    input::{keyboard::LedState, pointer::CursorImageStatus},
    output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::EventLoop,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{protocol::wl_surface, Display},
        winit::platform::pump_events::PumpStatus,
    },
    utils::{IsAlive, Physical, Rectangle, Scale, Transform},
    wayland::{
        dmabuf::{
            DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState,
            ImportNotifier,
        },
        presentation::Refresh,
    },
};
use tracing::{error, info, warn};

use crate::state::{take_presentation_feedback, Backend, SpitfireState};
use crate::{drawing::*, render::*};

pub const OUTPUT_NAME: &str = "winit";

pub struct WinitData {
    backend: WinitGraphicsBackend<GlesRenderer>,
    damage_tracker: OutputDamageTracker,
    dmabuf_state: (DmabufState, DmabufGlobal, Option<DmabufFeedback>),
    full_redraw: u8,
    /// Backing buffers for `spitfire.border`'s rects, reused frame to frame
    /// — see `render::RectCache`.
    border_cache: crate::render::RectCache,
    /// `spitfire.border.radius`'s corner masks, reused frame to frame and
    /// only rebuilt when the radius/width/colors actually change — see
    /// `render::CornerMaskCache`.
    corner_masks: crate::render::CornerMaskCache,
    #[cfg(feature = "debug")]
    pub fps: fps_ticker::Fps,
}

impl DmabufHandler for SpitfireState<WinitData> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.backend_data.dmabuf_state.0
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self
            .backend_data
            .backend
            .renderer()
            .import_dmabuf(&dmabuf, None)
            .is_ok()
        {
            let _ = notifier.successful::<SpitfireState<WinitData>>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(SpitfireState<WinitData>);

impl Backend for WinitData {
    fn seat_name(&self) -> String {
        String::from("winit")
    }
    fn reset_buffers(&mut self, _output: &Output) {
        self.full_redraw = 4;
    }
    fn early_import(&mut self, _surface: &wl_surface::WlSurface) {}
    fn update_led_state(&mut self, _led_state: LedState) {}
}

pub fn run_winit() {
    let mut event_loop = EventLoop::try_new().unwrap();
    let display = Display::new().unwrap();
    let mut display_handle = display.handle();

    #[cfg_attr(not(feature = "egl"), allow(unused_mut))]
    let (mut backend, mut winit) = match winit::init::<GlesRenderer>() {
        Ok(ret) => ret,
        Err(err) => {
            error!("Failed to initialize Winit backend: {}", err);
            return;
        }
    };
    let size = backend.window_size();

    let mode = Mode {
        size,
        refresh: 60_000,
    };
    let output = Output::new(
        OUTPUT_NAME.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
        },
    );
    let _global = output.create_global::<SpitfireState<WinitData>>(&display.handle());
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    #[cfg(feature = "debug")]
    #[allow(deprecated)]
    let fps_image = image::io::Reader::with_format(
        std::io::Cursor::new(FPS_NUMBERS_PNG),
        image::ImageFormat::Png,
    )
    .decode()
    .unwrap();
    #[cfg(feature = "debug")]
    let fps_texture = backend
        .renderer()
        .import_memory(
            &fps_image.to_rgba8(),
            Fourcc::Abgr8888,
            (fps_image.width() as i32, fps_image.height() as i32).into(),
            false,
        )
        .expect("Unable to upload FPS texture");
    #[cfg(feature = "debug")]
    let mut fps_element = FpsElement::new(fps_texture);

    let render_node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .and_then(|device| device.try_get_render_node());

    let dmabuf_default_feedback = match render_node {
        Ok(Some(node)) => {
            let dmabuf_formats = backend.renderer().dmabuf_formats();
            let dmabuf_default_feedback = DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats)
                .build()
                .unwrap();
            Some(dmabuf_default_feedback)
        }
        Ok(None) => {
            warn!("failed to query render node, dmabuf will use v3");
            None
        }
        Err(err) => {
            warn!(?err, "failed to egl device for display, dmabuf will use v3");
            None
        }
    };

    // if we failed to build dmabuf feedback we fall back to dmabuf v3
    // Note: egl on Mesa requires either v4 or wl_drm (initialized with bind_wl_display)
    let dmabuf_state = if let Some(default_feedback) = dmabuf_default_feedback {
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = dmabuf_state
            .create_global_with_default_feedback::<SpitfireState<WinitData>>(
                &display.handle(),
                &default_feedback,
            );
        (dmabuf_state, dmabuf_global, Some(default_feedback))
    } else {
        let dmabuf_formats = backend.renderer().dmabuf_formats();
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = dmabuf_state
            .create_global::<SpitfireState<WinitData>>(&display.handle(), dmabuf_formats);
        (dmabuf_state, dmabuf_global, None)
    };

    #[cfg(feature = "egl")]
    if backend
        .renderer()
        .bind_wl_display(&display.handle())
        .is_ok()
    {
        info!("EGL hardware-acceleration enabled");
    };

    let data = {
        let damage_tracker = OutputDamageTracker::from_output(&output);

        WinitData {
            backend,
            damage_tracker,
            dmabuf_state,
            full_redraw: 0,
            border_cache: crate::render::RectCache::default(),
            corner_masks: crate::render::CornerMaskCache::default(),
            #[cfg(feature = "debug")]
            fps: fps_ticker::Fps::default(),
        }
    };
    let mut state = SpitfireState::init(display, event_loop.handle(), data, true);
    state
        .shm_state
        .update_formats(state.backend_data.backend.renderer().shm_formats());
    // `spitfire.output = { scale = ... }` — a starting value only, from here
    // on `Mod+Shift+P`/`M` rescale live the same way they always have (see
    // `KeyAction::ScaleUp`/`ScaleDown`).
    output.change_current_state(
        None,
        None,
        Some(OutputScale::Fractional(state.config.output.scale)),
        None,
    );
    state.space.map_output(&output, (0, 0));
    crate::ipc::start(&event_loop.handle());

    // `start_xwayland` sets `state.xdisplay` (and this process's own
    // `DISPLAY`) synchronously before returning — see the matching comment
    // in udev.rs. Autostart below already sees a real `DISPLAY`, no need to
    // pump the event loop and wait for `XWaylandEvent::Ready` first.
    #[cfg(feature = "xwayland")]
    state.start_xwayland();

    state.spawn_autostart();

    info!("Initialization completed, starting the main loop.");

    let mut pointer_element = PointerElement::default();

    while state.running.load(Ordering::SeqCst) {
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                // We only have one output
                let output = state.space.outputs().next().unwrap().clone();
                state.space.map_output(&output, (0, 0));
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                output.change_current_state(Some(mode), None, None, None);
                output.set_preferred(mode);
                crate::shell::fixup_positions(&mut state.space, state.pointer.current_location());
            }
            WinitEvent::Input(event) => state.process_input_event_windowed(event, OUTPUT_NAME),
            _ => (),
        });

        if let PumpStatus::Exit(_) = status {
            state.running.store(false, Ordering::SeqCst);
            break;
        }

        // drawing logic
        {
            let now = state.clock.now();
            let frame_target = now
                + output
                    .current_mode()
                    .map(|mode| Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                    .unwrap_or_default();
            state.pre_repaint(&output, frame_target);

            let border_rects = state.border_rects();
            let border = state.config.border;
            let anims = state.animated_windows();
            // Cloned out, not held as a `Ref`, so this doesn't keep
            // `state.config` borrowed across the rest of the frame (which
            // includes a later `state.post_repaint(...)` mutable borrow of
            // `state` as a whole) — same reasoning as `border_rects`/`anims`
            // just above, both already per-frame `Vec` allocations.
            let rules: Vec<spitfire_config::WindowRule> = state.config.rules().clone();
            let blur_radius = state.config.blur.radius;

            let bar_config = state.config.bar;
            let bar_margin = state.config.gaps.outer;
            let bar_data = state.bar_data();
            let bar_output_width = state
                .space
                .output_geometry(&output)
                .map(|geo| geo.size.w)
                .unwrap_or(0);
            state.bar.tick();
            let bar_status_text = state.bar.status_text();
            let bar_mut = &mut state.bar;

            let backend = &mut state.backend_data.backend;

            // draw the cursor as relevant
            // reset the cursor if the surface is no longer alive
            let mut reset = false;
            if let CursorImageStatus::Surface(ref surface) = state.cursor_status {
                reset = !surface.alive();
            }
            if reset {
                state.cursor_status = CursorImageStatus::default_named();
            }
            let cursor_visible = !matches!(state.cursor_status, CursorImageStatus::Surface(_));

            pointer_element.set_status(state.cursor_status.clone());

            #[cfg(feature = "debug")]
            let fps = state.backend_data.fps.avg().round() as u32;
            #[cfg(feature = "debug")]
            fps_element.update_fps(fps);

            let full_redraw = &mut state.backend_data.full_redraw;
            *full_redraw = full_redraw.saturating_sub(1);
            let space = &mut state.space;
            let damage_tracker = &mut state.backend_data.damage_tracker;
            let border_cache = &mut state.backend_data.border_cache;
            let corner_masks = &mut state.backend_data.corner_masks;
            let screencopy_state = &mut state.screencopy_state;
            let ext_screencopy_state = &mut state.ext_screencopy_state;
            let blur_state = &mut state.blur_state;
            let show_window_preview = state.show_window_preview;
            let locked = state.locked;
            let lock_surfaces = &state.lock_surfaces;

            let dnd_icon = state.dnd_icon.as_ref();
            let cursor_status = &mut state.cursor_status;

            let scale = Scale::from(output.current_scale().fractional_scale());
            let cursor_pos = state.pointer.current_location();
            let output_geometry = space.output_geometry(&output).unwrap();

            #[cfg(feature = "debug")]
            let mut renderdoc = state.renderdoc.as_mut();

            let age = if *full_redraw > 0 {
                0
            } else {
                backend.buffer_age().unwrap_or(0)
            };
            #[cfg(feature = "debug")]
            let window_handle = backend
                .window()
                .window_handle()
                .map(|handle| {
                    if let RawWindowHandle::Wayland(handle) = handle.as_raw() {
                        handle.surface.as_ptr()
                    } else {
                        std::ptr::null_mut()
                    }
                })
                .unwrap_or_else(|_| std::ptr::null_mut());
            let render_res = backend.bind().and_then(|(renderer, mut fb)| {
                #[cfg(feature = "debug")]
                if let Some(renderdoc) = renderdoc.as_mut() {
                    renderdoc.start_frame_capture(
                        renderer.egl_context().get_context_handle(),
                        window_handle,
                    );
                }

                let mut elements = Vec::<CustomRenderElements<GlesRenderer>>::new();

                elements.extend(cursor_and_dnd_elements(
                    renderer,
                    output_geometry,
                    cursor_pos,
                    None,
                    &mut pointer_element,
                    dnd_icon,
                    &mut *cursor_status,
                    scale,
                ));

                #[cfg(feature = "debug")]
                elements.push(CustomRenderElements::Fps(fps_element.clone()));

                // The optional built-in bar (Phase 8) — pushed into the same
                // element list as the cursor/dnd icon above rather than
                // threaded through `render_output` like `spitfire.border`,
                // since it always sits on top with no occlusion-avoidance
                // concerns. Hidden while locked: it must never show live
                // clock/workspace info over the lock screen.
                if !locked {
                    elements.extend(crate::bar::bar_elements::<GlesRenderer>(
                        &bar_config,
                        bar_output_width,
                        bar_margin,
                        &bar_data,
                        &bar_status_text,
                        scale,
                        bar_mut,
                    ));
                }

                let locked_surface = locked
                    .then(|| lock_surfaces.first())
                    .flatten()
                    .map(|s| s.wl_surface());

                // spitfire.rule({ blur = true }) — see `crate::blur`'s doc
                // comment for the whole pipeline. Skipped entirely (no GL
                // work at all) whenever nothing on this output currently
                // needs it, which is the overwhelmingly common case.
                let blur_windows = if locked {
                    Vec::new()
                } else {
                    crate::blur::blur_windows_for_output(&rules, space, &output)
                };
                // Must run before the real frame's own `render_output` call
                // below — `WindowElement::render_elements` reads this back
                // to decide whether to skip its opaque backdrop. See
                // `sync_blur_flags`'s own doc comment for why it always
                // touches every window, not just `blur_windows`.
                crate::blur::sync_blur_flags(space, &output, &blur_windows);
                let mut blur_backdrops: Vec<crate::blur::BlurBackdrop> = Vec::new();
                if !blur_windows.is_empty() {
                    // Own throwaway caches, not `border_cache`/`corner_masks`
                    // above — same reasoning `crate::screencopy::capture_one`
                    // already gives for its own fresh ones: this is a second,
                    // independent render this tick, not part of the real
                    // frame's own damage-tracked border bookkeeping.
                    let mut backdrop_border_cache = RectCache::default();
                    let mut backdrop_corner_masks = CornerMaskCache::default();
                    let (backdrop_elements, _clear_color) = output_elements(
                        &output,
                        space,
                        std::iter::empty::<CustomRenderElements<GlesRenderer>>(),
                        renderer,
                        false,
                        locked_surface,
                        &border_rects,
                        border.width,
                        border.radius,
                        crate::render::hex_to_color32f(border.active),
                        crate::render::hex_to_color32f(border.inactive),
                        &mut backdrop_border_cache,
                        &mut backdrop_corner_masks,
                        &anims,
                        &blur_windows,
                        &[],
                    );
                    if let Some((backdrop_tex, backdrop_size)) =
                        crate::blur::capture_backdrop(renderer, &output, &backdrop_elements, scale)
                    {
                        blur_backdrops = crate::blur::blur_backdrops(
                            renderer,
                            blur_state,
                            &backdrop_tex,
                            backdrop_size,
                            space,
                            &output,
                            &blur_windows,
                            blur_radius,
                            scale,
                        );
                    }
                }

                // A blurred backdrop is a brand-new `MemoryRenderBuffer` (a
                // brand-new `Id`) every single frame, at a screen position
                // that itself hasn't necessarily changed — which isn't
                // enough on its own to guarantee this region gets truly
                // repainted rather than left showing whatever the *previous*
                // frame had there. Confirmed live: without this, the
                // translucent window's own real alpha blending was
                // compositing correctly, but against genuinely stale
                // framebuffer content from before blur ever started
                // (sharp, unblurred) rather than this frame's fresh
                // backdrop — `buffer_age()`'s partial-redraw bookkeeping
                // has no way to know a *content* change happened here
                // that it should count as damage, only that a new element
                // showed up. Forcing a full redraw (`age = 0`) whenever any
                // blur backdrop is active this frame sidesteps the whole
                // question at a real but bounded cost (only paid while a
                // `blur = true` window is actually on screen).
                let age = if blur_backdrops.is_empty() { age } else { 0 };
                let res = render_output(
                    &output,
                    space,
                    elements,
                    renderer,
                    &mut fb,
                    damage_tracker,
                    age,
                    show_window_preview,
                    locked_surface,
                    &border_rects,
                    border.width,
                    border.radius,
                    crate::render::hex_to_color32f(border.active),
                    crate::render::hex_to_color32f(border.inactive),
                    border_cache,
                    corner_masks,
                    &anims,
                    &blur_backdrops,
                )
                .map_err(|err| match err {
                    OutputDamageTrackerError::Rendering(err) => err.into(),
                    _ => unreachable!(),
                });

                // The real damage this tick, cloned out of `res` before it
                // moves on below — see `service_pending_captures`'s
                // `frame_damage` doc comment for what this gates/reports.
                let frame_damage: Option<Vec<Rectangle<i32, Physical>>> =
                    res.as_ref().ok().and_then(|r| r.damage).cloned();

                // wlr-screencopy (`grim`) — a throwaway second render, not
                // dependent on `res`/`fb` above (which is about to be
                // presented via `backend.submit()` below) — see
                // `crate::screencopy`'s doc comment for why it's a fresh
                // offscreen composite rather than reading `fb` back. Given
                // the raw cursor/dnd ingredients rather than a pre-built
                // element list — `render_and_copy` calls
                // `cursor_and_dnd_elements` itself, once per pending
                // capture, since `CustomRenderElements` isn't `Clone` (see
                // that fn's doc comment).
                crate::screencopy::service_pending_captures(
                    screencopy_state,
                    renderer,
                    space,
                    &output,
                    locked_surface,
                    frame_damage.as_deref(),
                    cursor_pos,
                    None,
                    &mut pointer_element,
                    dnd_icon,
                    &mut *cursor_status,
                    scale,
                    &rules,
                    &border_rects,
                    border.width,
                    border.radius,
                    crate::render::hex_to_color32f(border.active),
                    crate::render::hex_to_color32f(border.inactive),
                );

                // ext-image-copy-capture-v1 — same throwaway render
                // approach, same inputs, see `crate::ext_screencopy`'s doc
                // comment for how it differs from wlr-screencopy above.
                crate::ext_screencopy::service_pending_captures(
                    ext_screencopy_state,
                    renderer,
                    space,
                    &output,
                    locked_surface,
                    frame_damage.as_deref(),
                    cursor_pos,
                    None,
                    &mut pointer_element,
                    dnd_icon,
                    &mut *cursor_status,
                    scale,
                    &rules,
                    &border_rects,
                    border.width,
                    border.radius,
                    crate::render::hex_to_color32f(border.active),
                    crate::render::hex_to_color32f(border.inactive),
                );

                res
            });

            match render_res {
                Ok(render_output_result) => {
                    let has_rendered = render_output_result.damage.is_some();
                    if let Some(damage) = render_output_result.damage {
                        if let Err(err) = backend.submit(Some(damage)) {
                            warn!("Failed to submit buffer: {}", err);
                        }
                    }

                    #[cfg(feature = "debug")]
                    if let Some(renderdoc) = renderdoc.as_mut() {
                        renderdoc.end_frame_capture(
                            backend.renderer().egl_context().get_context_handle(),
                            backend
                                .window()
                                .window_handle()
                                .map(|handle| {
                                    if let RawWindowHandle::Wayland(handle) = handle.as_raw() {
                                        handle.surface.as_ptr()
                                    } else {
                                        std::ptr::null_mut()
                                    }
                                })
                                .unwrap_or_else(|_| std::ptr::null_mut()),
                        );
                    }

                    backend.window().set_cursor_visible(cursor_visible);

                    let states = render_output_result.states;
                    if has_rendered {
                        let locked_surface = locked
                            .then(|| lock_surfaces.first())
                            .flatten()
                            .map(|s| s.wl_surface());
                        let mut output_presentation_feedback = take_presentation_feedback(
                            &output,
                            &state.space,
                            locked_surface,
                            &states,
                        );
                        output_presentation_feedback.presented(
                            frame_target,
                            output
                                .current_mode()
                                .map(|mode| {
                                    Refresh::fixed(Duration::from_secs_f64(
                                        1_000f64 / mode.refresh as f64,
                                    ))
                                })
                                .unwrap_or(Refresh::Unknown),
                            0,
                            wp_presentation_feedback::Kind::Vsync,
                        )
                    }

                    // Send frame events so that client start drawing their next frame
                    state.post_repaint(&output, frame_target, None, &states);
                }
                Err(SwapBuffersError::ContextLost(err)) => {
                    #[cfg(feature = "debug")]
                    if let Some(renderdoc) = renderdoc.as_mut() {
                        renderdoc.discard_frame_capture(
                            backend.renderer().egl_context().get_context_handle(),
                            backend
                                .window()
                                .window_handle()
                                .map(|handle| {
                                    if let RawWindowHandle::Wayland(handle) = handle.as_raw() {
                                        handle.surface.as_ptr()
                                    } else {
                                        std::ptr::null_mut()
                                    }
                                })
                                .unwrap_or_else(|_| std::ptr::null_mut()),
                        );
                    }

                    error!("Critical Rendering Error: {}", err);
                    state.running.store(false, Ordering::SeqCst);
                }
                Err(err) => warn!("Rendering error: {}", err),
            }
        }

        let result = event_loop.dispatch(Some(Duration::from_millis(1)), &mut state);
        if result.is_err() {
            state.running.store(false, Ordering::SeqCst);
        } else {
            // Reapply the active layout — this is also where closed windows
            // get pruned from the tiling order (there's no dedicated
            // "window destroyed" hook, see TilingLayout::arrange).
            state.arrange_tiling();
            state.space.refresh();
            state.sync_foreign_toplevels();
            state.popups.cleanup();
            display_handle.flush_clients().unwrap();
        }

        #[cfg(feature = "debug")]
        state.backend_data.fps.tick();
    }
}
