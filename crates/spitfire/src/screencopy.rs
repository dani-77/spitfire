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
//! - No screencast/damage-tracking queue — every `copy`/`copy_with_damage`
//!   request renders and copies on the very next frame for its output,
//!   unconditionally. `copy_with_damage` therefore behaves exactly like
//!   `copy` (always "damaged", the whole buffer): correct but wasteful for
//!   a real-time capture client polling every frame, harmless for `grim`'s
//!   single-shot use.
//! - Captures a *fresh offscreen render* of the output (the same element
//!   list `render::output_elements` builds for the real frame, rendered
//!   again into a throwaway GPU texture) rather than reading back the
//!   just-presented framebuffer — this sidesteps having to prove either
//!   backend's swapchain/scanout buffer is still readable by the time a
//!   request gets serviced, and reuses `render::output_elements`/
//!   `OutputDamageTracker` completely as-is instead of new backend-specific
//!   plumbing. Tradeoff: `output_elements` alone doesn't draw the
//!   cursor/dnd-icon/bar — winit.rs/udev.rs add those as `custom_elements`
//!   before calling it (see `output_elements`'s own call sites) — so a
//!   screencopy capture shows windows/borders but no cursor. Fine for
//!   debugging window compositing; revisit if a missing cursor ever
//!   actually matters.
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
            damage::OutputDamageTracker, gles::GlesTexture, Bind, Color32F, ExportMem, ImportAll,
            ImportMem, Offscreen, Renderer,
        },
    },
    desktop::Space,
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
    utils::{Buffer as BufferCoord, Logical, Physical, Point, Rectangle, Size},
    wayland::shm,
};
use tracing::trace;

use crate::{
    render::{output_elements, BorderRect, CornerMaskCache, RectCache},
    shell::WindowElement,
    state::{Backend, SpitfireState},
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
/// frame to actually be serviced — see `service_pending_captures`, called
/// from winit.rs's render loop and udev.rs's `render_surface`.
struct PendingCapture {
    frame: ZwlrScreencopyFrameV1,
    output: Output,
    buffer: WlBuffer,
    buffer_size: Size<i32, Physical>,
    region_loc: Point<i32, Physical>,
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
        let buffer = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => buffer,
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => buffer,
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
#[allow(clippy::too_many_arguments)]
pub fn service_pending_captures<R>(
    state: &mut ScreencopyState,
    renderer: &mut R,
    space: &Space<WindowElement>,
    output: &Output,
    locked_surface: Option<&WlSurface>,
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
    let (due, rest): (Vec<_>, Vec<_>) = state
        .pending
        .drain(..)
        .partition(|capture| &capture.output == output);
    state.pending = rest;

    for capture in due {
        capture_one(
            renderer,
            space,
            output,
            locked_surface,
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
    let (elements, clear_color) = output_elements(
        output,
        space,
        std::iter::empty(),
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
