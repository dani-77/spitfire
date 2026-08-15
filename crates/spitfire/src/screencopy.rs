//! Server-side `wlr-screencopy-unstable-v1` — lets `grim` (and, in
//! principle, `xdg-desktop-portal-wlr`'s Screenshot/ScreenCast, though
//! spitfire doesn't wire up that portal config yet — see `doc/README.md`'s
//! xdg-desktop-portal section) read back a rendered output.
//!
//! Not provided by Smithay itself, same situation `ext_workspace.rs`
//! documents for `ext-workspace-v1` — the wire types come from
//! `wayland-protocols-wlr` (re-exported at
//! `smithay::reexports::wayland_protocols_wlr`; already generated with the
//! `server` feature Smithay's own `Cargo.toml` turns on whenever
//! `wayland_frontend` is enabled — already the case here — so no extra
//! Cargo dependency is needed), but the protocol logic in this file is new.
//! Modeled closely on niri's own `src/protocols/screencopy.rs` (MIT,
//! github.com/YaLTeR/niri), trimmed down for what spitfire actually needs
//! right now — a way to confirm what a running session looks like without
//! a physical screenshot:
//! - wl_shm buffers only, version 2 of the manager — the protocol
//!   guarantees a `buffer` event for every request at version ≤2, so there's
//!   no `linux-dmabuf`/`buffer_done` bookkeeping to do at all. No zero-copy
//!   GPU path; fine for occasional manual captures, not for a real-time
//!   screencaster.
//! - `copy` still renders and copies on the very next frame for its output,
//!   unconditionally — correct for `grim`'s single-shot use, and there's no
//!   sane way to define "damage" for a request that never promised to wait.
//!   `copy_with_damage` (2026-08-14) actually waits, though: a pending
//!   `copy_with_damage` capture is only serviced on a frame where the real
//!   render loop's own `OutputDamageTracker` reports non-empty damage for
//!   that output (see `service_pending_captures`'s `frame_damage` param,
//!   threaded in from winit.rs/udev.rs's `RenderOutputResult` — *not* this
//!   module's own throwaway per-capture tracker below, which is recreated
//!   fresh every call and so would report "everything changed" every time,
//!   useless as an incremental signal). While waiting, the compositor does
//!   no extra render/copy work for that request at all — the whole point,
//!   since a static screen (a video call sharing a slide, say) now costs
//!   nothing per frame instead of a full offscreen re-render + GPU→CPU
//!   readback every single tick. Once serviced, the real damage rects are
//!   reported back via `frame.damage()` events before `ready`, clamped to
//!   the capture's own region — matches what `xdg-desktop-portal-wlr`/
//!   PipeWire actually want from a streaming source (see the "is
//!   wlr-screencopy good enough to stream?" assessment that prompted this).
//!   Approximates "damage since the last `copy_with_damage` for this
//!   stream" as "damage on the next real frame after this request", which
//!   holds for the pattern real clients use (immediately re-request on
//!   `ready`, so there's normally at most one such request in flight); it
//!   would undercount if a client left a `copy_with_damage` unrequested for
//!   several real frames and expected the union of all of them back —
//!   not a pattern PipeWire/portal streaming actually uses.
//! - Captures a *fresh offscreen render* of the output (the same element
//!   list `render::output_elements` builds for the real frame, rendered
//!   again into a throwaway GPU texture) rather than reading back the
//!   just-presented framebuffer — this sidesteps having to prove either
//!   backend's swapchain/scanout buffer is still readable by the time a
//!   request gets serviced, and reuses `render::output_elements`/
//!   `OutputDamageTracker` completely as-is instead of new backend-specific
//!   plumbing. `output_elements` alone doesn't draw the cursor/dnd-icon/bar
//!   — winit.rs/udev.rs add those as `custom_elements` before calling it
//!   (see `output_elements`'s own call sites). Since 2026-08-14,
//!   `service_pending_captures` is handed the same raw cursor/dnd
//!   ingredients (pointer location/image/element, dnd icon, cursor status,
//!   scale) winit.rs/udev.rs already have in scope for their own real
//!   frame, and `render_and_copy` calls `render::cursor_and_dnd_elements`
//!   itself to fold the pointer into this throwaway render too — see that
//!   fn's doc comment for why a fresh call per capture rather than a
//!   pre-built, shared element list (`CustomRenderElements` isn't `Clone`).
//!   Deliberately still not the fps counter (dev-only, would leak into a
//!   capture meant to double as a screen-share/recording source) or the
//!   built-in bar (not asked for yet).
//! - Assumes `Transform::Normal` for `capture_output_region`'s coordinate
//!   math (unlike niri, which handles arbitrary output transforms) — the
//!   region is scaled from logical to physical and clamped to the output's
//!   physical size with no transform correction. `capture_output` (the
//!   whole-output case `grim` uses with no `-g`) is unaffected either way.

use std::time::{SystemTime, UNIX_EPOCH};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker, element::memory::MemoryRenderBuffer, gles::GlesTexture,
            Bind, Color32F, ExportMem, ImportAll, ImportMem, Offscreen, Renderer,
        },
    },
    desktop::Space,
    input::pointer::CursorImageStatus,
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            backend::ClientId,
            protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Buffer as BufferCoord, Logical, Physical, Point, Rectangle, Scale, Size},
    wayland::shm,
};
use tracing::trace;

use spitfire_config::WindowRule;

use crate::{
    drawing::PointerElement,
    render::{cursor_and_dnd_elements, output_elements, BorderRect, CornerMaskCache, RectCache},
    shell::WindowElement,
    state::{Backend, DndIcon, SpitfireState},
};

/// wl_shm-guaranteed version — see this module's doc comment for why
/// staying at/below this skips all `buffer_done`/`linux_dmabuf` handling.
const VERSION: u32 = 2;

pub struct ScreencopyManagerGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ScreencopyManagerGlobalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreencopyManagerGlobalData")
            .finish_non_exhaustive()
    }
}

/// What a `capture_output`/`capture_output_region` request resolved to at
/// request time — everything `copy`/`copy_with_damage` needs to know to
/// queue a `PendingCapture` later, without re-validating the output/region.
/// `pub` for the same reason `FrameData` is — see its doc comment.
#[derive(Clone)]
pub struct FrameInfo {
    output: Output,
    /// Size of the captured rect, in the output's physical pixels — the
    /// whole output for `capture_output`, the clamped sub-rect for
    /// `capture_output_region`.
    buffer_size: Size<i32, Physical>,
    /// Top-left of the captured rect within the output, in physical
    /// pixels — `(0, 0)` for `capture_output`.
    region_loc: Point<i32, Physical>,
}

/// Per-`zwlr_screencopy_frame_v1` user data. `pub` only because
/// `ScreencopyState::new`'s `Dispatch<ZwlrScreencopyFrameV1, FrameData>`
/// bound has to name it at a visibility no narrower than that `pub fn`'s
/// own — same reasoning as `ext_workspace::WorkspaceId`. Not meant to be
/// constructed or matched on outside this module.
pub enum FrameData {
    /// The output/region requested at `capture_output(_region)` time was
    /// invalid — the client already got a synchronous `failed()`; a `copy`
    /// against this frame is a client bug, silently ignored (see
    /// `Dispatch<ZwlrScreencopyFrameV1, _>::request`) rather than worth
    /// killing the client over.
    Failed,
    Pending(FrameInfo),
}

/// One `copy`/`copy_with_damage` request, waiting for its output's next
/// *eligible* frame to actually be serviced — see `service_pending_captures`,
/// called from winit.rs's render loop and udev.rs's `render_surface`. A
/// plain `copy` (`wants_damage: false`) is eligible on the very next frame,
/// unconditionally; `copy_with_damage` only once that frame actually
/// reports damage for this output — see this module's doc comment.
struct PendingCapture {
    frame: ZwlrScreencopyFrameV1,
    output: Output,
    buffer: WlBuffer,
    buffer_size: Size<i32, Physical>,
    region_loc: Point<i32, Physical>,
    wants_damage: bool,
}

#[derive(Default)]
pub struct ScreencopyState {
    pending: Vec<PendingCapture>,
}

impl std::fmt::Debug for ScreencopyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `PendingCapture` isn't `Debug` (its `WlBuffer`/`ZwlrScreencopyFrameV1`
        // fields aren't) — same workaround `ExtWorkspaceState`'s manual
        // `Debug` uses for `ClientState`: report the count, not the Vec.
        f.debug_struct("ScreencopyState")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl ScreencopyState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>
            + Dispatch<ZwlrScreencopyManagerV1, ()>
            + Dispatch<ZwlrScreencopyFrameV1, FrameData>
            + 'static,
    {
        dh.create_global::<D, ZwlrScreencopyManagerV1, _>(
            VERSION,
            ScreencopyManagerGlobalData {
                filter: Box::new(|_| true),
            },
        );
        ScreencopyState::default()
    }
}

impl<BackendData: Backend + 'static>
    GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData, SpitfireState<BackendData>>
    for ScreencopyState
{
    fn bind(
        _state: &mut SpitfireState<BackendData>,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &ScreencopyManagerGlobalData,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ScreencopyManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ZwlrScreencopyManagerV1, (), SpitfireState<BackendData>> for ScreencopyState
{
    fn request(
        _state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        let (frame, info) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, output, .. } => {
                let info = Output::from_resource(&output).and_then(|output| {
                    let buffer_size = output.current_mode()?.size;
                    Some(FrameInfo {
                        output,
                        buffer_size,
                        region_loc: (0, 0).into(),
                    })
                });
                (frame, info)
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                output,
                x,
                y,
                width,
                height,
                ..
            } => {
                let info = (width > 0 && height > 0)
                    .then(|| Output::from_resource(&output))
                    .flatten()
                    .and_then(|output| {
                        let mode = output.current_mode()?;
                        let output_scale = output.current_scale().fractional_scale();
                        let rect: Rectangle<i32, Logical> =
                            Rectangle::new((x, y).into(), (width, height).into());
                        let physical_rect = rect.to_physical_precise_round(output_scale);
                        let output_rect = Rectangle::new((0, 0).into(), mode.size);
                        let clamped = physical_rect.intersection(output_rect)?;
                        Some(FrameInfo {
                            output,
                            buffer_size: clamped.size,
                            region_loc: clamped.loc,
                        })
                    });
                (frame, info)
            }
            zwlr_screencopy_manager_v1::Request::Destroy => return,
            _ => unreachable!(),
        };

        match info {
            Some(info) => {
                let buffer_size = info.buffer_size;
                let frame = data_init.init(frame, FrameData::Pending(info));
                frame.buffer(
                    wl_shm::Format::Argb8888,
                    buffer_size.w as u32,
                    buffer_size.h as u32,
                    buffer_size.w as u32 * 4,
                );
            }
            None => {
                trace!("screencopy: invalid output or region requested");
                let frame = data_init.init(frame, FrameData::Failed);
                frame.failed();
            }
        }
    }

    fn destroyed(
        _state: &mut SpitfireState<BackendData>,
        _client: ClientId,
        _manager: &ZwlrScreencopyManagerV1,
        _data: &(),
    ) {
        // Per-protocol: destroying the manager doesn't invalidate frames
        // already created through it — nothing to clean up here.
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ZwlrScreencopyFrameV1, FrameData, SpitfireState<BackendData>> for ScreencopyState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        let (buffer, wants_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            zwlr_screencopy_frame_v1::Request::Destroy => return,
            _ => unreachable!(),
        };

        let FrameData::Pending(info) = data else {
            // Already failed at capture_output(_region) time — see
            // `FrameData::Failed`'s doc comment.
            return;
        };

        let queue = &mut state.screencopy_state.pending;
        if queue.iter().any(|p| &p.frame == frame) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "copy was already requested",
            );
            return;
        }

        queue.push(PendingCapture {
            frame: frame.clone(),
            output: info.output.clone(),
            buffer,
            buffer_size: info.buffer_size,
            region_loc: info.region_loc,
            wants_damage,
        });
    }

    fn destroyed(
        state: &mut SpitfireState<BackendData>,
        _client: ClientId,
        frame: &ZwlrScreencopyFrameV1,
        _data: &FrameData,
    ) {
        // Client destroyed the frame (or disconnected) before we got to
        // servicing it — drop it rather than rendering/copying into a
        // buffer nobody will read.
        state.screencopy_state.pending.retain(|p| &p.frame != frame);
    }
}

smithay::reexports::wayland_server::delegate_global_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ZwlrScreencopyManagerV1: ScreencopyManagerGlobalData
] => ScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ZwlrScreencopyManagerV1: ()
] => ScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ZwlrScreencopyFrameV1: FrameData
] => ScreencopyState);

/// Renders and copies every `PendingCapture` queued for `output`, then
/// drops them from the queue — call once per output per frame, after that
/// output's real composite (see this module's doc comment for why this is
/// a *second*, throwaway render rather than reading back the real one).
/// `borders`/`border_width`/`border_radius`/`border_active`/
/// `border_inactive` are the same values that frame's real
/// `output_elements`/`render_output` call was given — passing them through
/// again is what makes the capture match what was actually on screen.
/// Deliberately *not* given the real `anims` — see `render_and_copy`'s doc
/// comment for why a capture always uses the plain (non-animated) window
/// path even mid-animation.
///
/// `pointer_location`/`pointer_image`/`pointer_element`/`dnd_icon`/
/// `cursor_status`/`scale` are the same raw ingredients winit.rs/udev.rs
/// feed their own real frame's cursor into (see `render::
/// cursor_and_dnd_elements`) — passed through *raw*, not as an
/// already-built element list, since `render_and_copy` calls that fn itself
/// once per pending capture (`CustomRenderElements` isn't `Clone`, so a
/// single pre-built list couldn't be reused across more than one capture
/// due the same output this tick — see `cursor_and_dnd_elements`'s own doc
/// comment). Gives a capture the pointer visible instead of a bare desktop;
/// previously omitted entirely.
///
/// `frame_damage` is the real render loop's own damage for `output` this
/// tick — `None`/empty means nothing changed. Only gates `copy_with_damage`
/// captures (see `PendingCapture`'s doc comment and this module's own doc
/// comment); a plain `copy` is serviced regardless, as before. Also what
/// gets reported back via `frame.damage()` events for whichever
/// `copy_with_damage` captures this call does end up servicing.
///
/// `rules` is `state.config.rules()` — checked here (not baked onto a
/// window at map time) purely for `hide_from_capture`: `render_and_copy`
/// re-derives which of `output`'s windows currently match such a rule and
/// hands that list to `output_elements` as `hidden_windows`, so a match is
/// skipped from this render entirely while staying fully visible on the
/// real screen.
#[allow(clippy::too_many_arguments)]
pub fn service_pending_captures<R>(
    state: &mut ScreencopyState,
    renderer: &mut R,
    space: &Space<WindowElement>,
    output: &Output,
    locked_surface: Option<&WlSurface>,
    frame_damage: Option<&[Rectangle<i32, Physical>]>,
    pointer_location: Point<f64, Logical>,
    pointer_image: Option<&MemoryRenderBuffer>,
    pointer_element: &mut PointerElement,
    dnd_icon: Option<&DndIcon>,
    cursor_status: &mut CursorImageStatus,
    scale: Scale<f64>,
    rules: &[WindowRule],
    borders: &[BorderRect],
    border_width: i32,
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
) where
    // Offscreen target is a concrete `GlesTexture`, deliberately *not*
    // `R::TextureId` — for the udev backend `R` is `MultiRenderer<...>`,
    // whose own `TextureId` is `MultiTexture`, and the underlying
    // per-GPU renderer (`GlesRenderer`, both sides — see `udev.rs`'s
    // `UdevRenderer` alias) only implements `Offscreen<GlesTexture>`, not
    // `Offscreen<MultiTexture>`. Projecting `R::TextureId` into its own
    // bound here (`Offscreen<R::TextureId>`) also hits an unrelated rustc
    // trait-solving cycle ("cycle detected when computing the bounds for
    // type parameter R") — a fixed, unrelated `GlesTexture` sidesteps both
    // problems at once.
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    if state.pending.is_empty() {
        return;
    }
    // A capture is "due" for this output either because it's a plain `copy`
    // (always due, next frame) or because it's a `copy_with_damage` and this
    // frame actually has damage to report — otherwise it stays queued for a
    // later call. See this module's doc comment.
    let (due, rest): (Vec<_>, Vec<_>) = state.pending.drain(..).partition(|capture| {
        &capture.output == output && (!capture.wants_damage || frame_damage.is_some())
    });
    state.pending = rest;

    for capture in due {
        capture_one(
            renderer,
            space,
            output,
            locked_surface,
            frame_damage,
            pointer_location,
            pointer_image,
            pointer_element,
            dnd_icon,
            cursor_status,
            scale,
            rules,
            borders,
            border_width,
            border_radius,
            border_active,
            border_inactive,
            capture,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_one<R>(
    renderer: &mut R,
    space: &Space<WindowElement>,
    output: &Output,
    locked_surface: Option<&WlSurface>,
    frame_damage: Option<&[Rectangle<i32, Physical>]>,
    pointer_location: Point<f64, Logical>,
    pointer_image: Option<&MemoryRenderBuffer>,
    pointer_element: &mut PointerElement,
    dnd_icon: Option<&DndIcon>,
    cursor_status: &mut CursorImageStatus,
    scale: Scale<f64>,
    rules: &[WindowRule],
    borders: &[BorderRect],
    border_width: i32,
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
    capture: PendingCapture,
) where
    // See `service_pending_captures`'s doc comment for why the offscreen
    // target is a fixed `GlesTexture`.
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    let PendingCapture {
        frame,
        buffer,
        buffer_size,
        region_loc,
        wants_damage,
        ..
    } = capture;

    if !frame.is_alive() {
        return;
    }

    let result = render_and_copy(
        renderer,
        space,
        output,
        locked_surface,
        pointer_location,
        pointer_image,
        pointer_element,
        dnd_icon,
        cursor_status,
        scale,
        rules,
        borders,
        border_width,
        border_radius,
        border_active,
        border_inactive,
        &buffer,
        buffer_size,
        region_loc,
    );

    match result {
        Ok(()) => {
            frame.flags(Flags::empty());
            // Per-protocol, `damage` events only mean anything for
            // `copy_with_damage`, and must be sent before `ready` — see
            // this module's own doc comment for what `frame_damage` is and
            // why it's a reasonable approximation of "since this capture
            // was last serviced". A capture region smaller than the whole
            // output (`capture_output_region`) clips each rect down to
            // just its own slice, in its own buffer-local coordinates.
            if wants_damage {
                let capture_rect = Rectangle::new(region_loc, buffer_size);
                for rect in frame_damage.into_iter().flatten() {
                    let Some(clamped) = rect.intersection(capture_rect) else {
                        continue;
                    };
                    let local = clamped.loc - region_loc;
                    frame.damage(
                        local.x as u32,
                        local.y as u32,
                        clamped.size.w as u32,
                        clamped.size.h as u32,
                    );
                }
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            frame.ready(
                (now.as_secs() >> 32) as u32,
                (now.as_secs() & 0xFFFF_FFFF) as u32,
                now.subsec_nanos(),
            );
        }
        Err(err) => {
            trace!(%err, "screencopy capture failed");
            frame.failed();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_and_copy<R>(
    renderer: &mut R,
    space: &Space<WindowElement>,
    output: &Output,
    locked_surface: Option<&WlSurface>,
    pointer_location: Point<f64, Logical>,
    pointer_image: Option<&MemoryRenderBuffer>,
    pointer_element: &mut PointerElement,
    dnd_icon: Option<&DndIcon>,
    cursor_status: &mut CursorImageStatus,
    scale: Scale<f64>,
    rules: &[WindowRule],
    borders: &[BorderRect],
    border_width: i32,
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
    buffer: &WlBuffer,
    buffer_size: Size<i32, Physical>,
    region_loc: Point<i32, Physical>,
) -> Result<(), String>
where
    // See `service_pending_captures`'s doc comment for why the offscreen
    // target is a fixed `GlesTexture`.
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    // Fresh, throwaway caches — screencopy is an occasional manual capture,
    // not a per-frame path, so there's no reuse-across-frames win to chase
    // the way the real render loop's `RectCache`/`CornerMaskCache` do.
    let mut border_cache = RectCache::default();
    let mut corner_masks = CornerMaskCache::default();
    // Always `&[]` for `anims`, even if a window/workspace-switch animation
    // is genuinely in flight right now: `output_elements` routes non-empty
    // `anims` through `desktop::space::constrain_space_element` (the
    // "Preview" element wrapper `anim.rs` needs for a live, single
    // authoritative render pass) — reusing that path a *second* time in
    // the same frame, for this throwaway offscreen render, was found (by
    // reproducing it live: `spitfirectl workspace focus` + `grim` mid-slide)
    // to draw window content as fully blank while everything else
    // (borders/wallpaper/bar) still rendered fine — not yet root-caused
    // further than "second constrain_space_element call this frame", and
    // not worth chasing deeper: a capture showing every window at its
    // plain, settled geometry instead of mid-slide is a perfectly
    // reasonable thing for a screenshot tool to do anyway.
    // See `cursor_and_dnd_elements`'s own doc comment for why this is a
    // fresh call rather than a passed-in, pre-built element list — this
    // capture wants the pointer visible, same as the real frame did this
    // tick, but `CustomRenderElements` isn't `Clone`.
    let output_geometry = space
        .output_geometry(output)
        .ok_or("output not mapped in space")?;
    let cursor_elements = cursor_and_dnd_elements(
        renderer,
        output_geometry,
        pointer_location,
        pointer_image,
        pointer_element,
        dnd_icon,
        cursor_status,
        scale,
    );

    // `spitfire.rule({ hide_from_capture = true })` — collect this output's
    // windows matching such a rule so `output_elements` skips them below.
    // Looked up fresh per capture rather than cached on the window itself:
    // rules are re-evaluated live (`spitfire.reload()` can change them),
    // and this list is tiny/occasional, not a per-real-frame cost.
    let hidden_windows: Vec<WindowElement> = space
        .elements_for_output(output)
        .filter(|window| {
            rules
                .iter()
                .any(|rule| rule.hide_from_capture && rule.matches(window.app_id().as_deref()))
        })
        .cloned()
        .collect();

    let (elements, clear_color) = output_elements(
        output,
        space,
        cursor_elements,
        renderer,
        false,
        locked_surface,
        borders,
        border_width,
        border_radius,
        border_active,
        border_inactive,
        &mut border_cache,
        &mut corner_masks,
        &[],
        &hidden_windows,
    );

    let mode = output.current_mode().ok_or("output has no mode")?;
    let texture_size: Size<i32, BufferCoord> = (mode.size.w, mode.size.h).into();
    let mut texture = renderer
        .create_buffer(Fourcc::Argb8888, texture_size)
        .map_err(|_| "failed to create offscreen capture buffer".to_string())?;
    let mut target = renderer
        .bind(&mut texture)
        .map_err(|_| "failed to bind offscreen capture buffer".to_string())?;

    let mut damage_tracker = OutputDamageTracker::from_output(output);
    damage_tracker
        .render_output(renderer, &mut target, 0, &elements, clear_color)
        .map_err(|_| "failed to render offscreen capture".to_string())?;

    let region: Rectangle<i32, BufferCoord> = Rectangle::new(
        (region_loc.x, region_loc.y).into(),
        (buffer_size.w, buffer_size.h).into(),
    );
    let mapping = renderer
        .copy_framebuffer(&target, region, Fourcc::Argb8888)
        .map_err(|_| "failed to copy framebuffer".to_string())?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|_| "failed to map captured texture".to_string())?;

    shm::with_buffer_contents_mut(buffer, |ptr, len, data| {
        if data.format != wl_shm::Format::Argb8888
            || data.width != buffer_size.w
            || data.height != buffer_size.h
            || data.stride != buffer_size.w * 4
            || len != data.stride as usize * data.height as usize
        {
            return Err("client buffer no longer matches the announced format/size".to_string());
        }
        if bytes.len() != len {
            return Err("captured texture size mismatch".to_string());
        }
        // SAFETY: just checked `bytes.len() == len`, and `ptr` is the shm
        // pool's own mapping, valid for `len` bytes for the closure's
        // duration (that's `with_buffer_contents_mut`'s whole contract).
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len) };
        Ok(())
    })
    .map_err(|_| "target buffer is not shm-backed".to_string())?
}
