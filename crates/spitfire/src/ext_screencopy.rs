//! Server-side `ext-image-copy-capture-v1` + `ext-image-capture-source-v1` —
//! the protocol pair that supersedes `wlr-screencopy-unstable-v1`
//! (`crate::screencopy`) for whole-output capture. Not provided by Smithay
//! itself (same "wire types free, protocol logic is new" situation
//! `crate::screencopy`'s own doc comment describes) — the wire types come
//! from the already-vendored `wayland-protocols` crate (re-exported at
//! `smithay::reexports::wayland_protocols`, `staging`+`server` features
//! already on via smithay's own `wayland_frontend` feature), but everything
//! in this file is new.
//!
//! Why this protocol pair, and why now: see `doc/README.md`'s own account —
//! a prior attempt at a dmabuf zero-copy path on `wlr-screencopy` v3 crashed the real render
//! loop, because that protocol's `linux_dmabuf` event carries a bare fourcc
//! with **no modifier list**, leaving a client to guess a modifier the
//! compositor can't actually import. `ext_image_copy_capture_session_v1`'s
//! `dmabuf_format` event carries a real `modifiers: array`, closing exactly
//! that hole — and the user's installed `xdg-desktop-portal-wlr` (0.8.2)
//! already prefers this protocol pair over `wlr-screencopy` automatically
//! once both `ext_image_copy_capture_manager_v1` and
//! `ext_output_image_capture_source_manager_v1` are advertised.
//!
//! **Scope of this first pass — deliberately SHM-only**: given that exact
//! crash history, this implementation advertises `shm_format` only, never
//! `dmabuf_device`/`dmabuf_format` — a client that only gets shm formats
//! offered has no way to attempt a dmabuf-backed buffer at all, so the
//! failure mode above is structurally unreachable here, not just avoided by
//! care. `xdg-desktop-portal-wlr` still prefers this protocol pair the
//! moment both globals are advertised (its version-preference check is
//! based on global presence, not on which buffer types those globals go on
//! to offer), so the practical win — being picked automatically over
//! `wlr-screencopy` for real screen-sharing — lands immediately even
//! without dmabuf. Real dmabuf support (querying the renderer's actual
//! importable modifiers per format and advertising exactly those) is a
//! deliberately separate follow-up, not attempted blind here — it needs the
//! same live GPU-encoder testing (`wf-recorder -c h264_vaapi`) the reverted
//! attempt used, which risks the same class of crash if anything is missed.
//!
//! **Per-window capture** (`ext_foreign_toplevel_image_capture_source_manager_v1.create_source`,
//! taking an `ext_foreign_toplevel_handle_v1` from `crate::foreign_toplevel` as its target)
//! is also supported, alongside the whole-output source above — `CaptureSource` below is the
//! `Output`/`Toplevel` split that threads through the rest of this module. A toplevel
//! session's `buffer_size` is the window's own geometry, not the output mode; its render path
//! (`render_window_and_copy`) renders just that window's own elements (via
//! `WindowElement::render_elements`, the same per-window element set the real on-screen
//! per-window loop in `render.rs` already uses) into a transparent-backed buffer sized to it —
//! deliberately *not* `crate::screencopy::render_and_copy` (that fn is output/`Space`-shaped
//! start to finish), and deliberately *not* including spitfire's own border/gaps chrome, only
//! the window's actual content — matching what a "share this window" picker wants. No cursor
//! compositing for a per-window capture either (simpler; a stray systemwide cursor baked into
//! a single-window stream isn't generally what's wanted).
//!
//! **Other simplifications, all safe to revisit later without a wire
//! break**: no `ext_image_copy_capture_cursor_session_v1` support yet (the
//! object is created — it's a protocol-mandated `new_id` — but never emits
//! `enter`/`leave`/`position`/`hotspot`; a client asking for a separate
//! cursor stream just never sees one become active, rather than the
//! request failing outright) — cursor visibility for an *output* session is
//! still available via that session's own `paint_cursors` option, composited into the frame
//! same as `crate::screencopy` already does. Client-submitted `damage_buffer` hints
//! are accepted (validated, not ignored outright — an invalid rect still
//! raises the protocol error) but not used to narrow the actual copy: same
//! "copies unconditionally, reports real derived damage back" shape
//! `crate::screencopy`'s `copy_with_damage` already uses, reusing that
//! module's own damage-gating logic (see `service_pending_captures` here) — for a toplevel
//! session this approximates "the window's content changed" as "the output had damage this
//! tick", a conservative superset (correct, just occasionally over-eager: it can also fire on
//! a *different* window's damage). A session's `create_frame` while a previous frame on the
//! same session is still un-destroyed doesn't raise the spec's `duplicate_frame` error — not
//! enforced, since no client this was tested against triggers it.
//!
//! The actual render + shm copy for the whole-output path is *not* duplicated here — every
//! such capture calls straight into `crate::screencopy::render_and_copy`, the exact same
//! offscreen-render-and-`memcpy` routine `wlr-screencopy` uses (fresh throwaway render each
//! time, `hide_from_capture` rules honored, cursor/dnd composited in). The per-window path's
//! `render_window_and_copy` duplicates that routine's general *shape* (create an offscreen
//! GLES texture, render into it, `copy_framebuffer`, `map_texture`, `memcpy` into the client's
//! shm buffer) rather than sharing code with it — the element-gathering step is fundamentally
//! different (one window's own elements vs. a whole `Space`/`output_elements` composite), so
//! there wasn't a clean single function to share; same "structurally parallel, not factored
//! into one shared abstraction" choice `capture_one`/`service_pending_captures` already make
//! relative to their `crate::screencopy` counterparts.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{memory::MemoryRenderBuffer, AsRenderElements},
            gles::GlesTexture,
            Bind, Color32F, ExportMem, ImportAll, ImportMem, Offscreen, Renderer,
        },
    },
    desktop::{space::SpaceElement, Space},
    input::pointer::CursorImageStatus,
    output::Output,
    reexports::{
        wayland_protocols::ext::{
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::{
                    self, ExtForeignToplevelImageCaptureSourceManagerV1,
                },
                ext_image_capture_source_v1::ExtImageCaptureSourceV1,
                ext_output_image_capture_source_manager_v1::{
                    self, ExtOutputImageCaptureSourceManagerV1,
                },
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_cursor_session_v1::{
                    self, ExtImageCopyCaptureCursorSessionV1,
                },
                ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
                ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
                ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{
            backend::ClientId,
            protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
        },
    },
    utils::{
        Buffer as BufferCoord, IsAlive, Logical, Physical, Point, Rectangle, Scale, Size, Transform,
    },
    wayland::{foreign_toplevel_list::ForeignToplevelHandle, shm},
};
use tracing::trace;

use spitfire_config::WindowRule;

use crate::{
    drawing::PointerElement,
    screencopy::render_and_copy,
    shell::{WindowElement, WindowRenderElement},
    state::{Backend, DndIcon, SpitfireState},
};

/// What an `ext_image_capture_source_v1` actually captures — a whole
/// output (`ext_output_image_capture_source_manager_v1`) or a single
/// window (`ext_foreign_toplevel_image_capture_source_manager_v1`) — see
/// this module's doc comment for how the two render paths differ.
#[derive(Clone)]
enum CaptureSource {
    Output(Output),
    Toplevel(WindowElement),
}

/// Both new globals sit at version 1 — that's the only version either
/// protocol has.
const VERSION: u32 = 1;

pub struct ExtScreencopyGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ExtScreencopyGlobalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtScreencopyGlobalData")
            .finish_non_exhaustive()
    }
}

/// Per-`ext_image_capture_source_v1` user data — what this source
/// captures. `pub` for the same visibility reason `screencopy::FrameData`
/// is (named in a `Dispatch` bound at least as visible as `ExtScreencopyState::new`).
pub struct SourceData {
    source: CaptureSource,
}

/// Per-`ext_image_copy_capture_session_v1` user data.
struct SessionInner {
    /// `None` for a session that can never actually capture anything — an
    /// unknown source, or the inert `ext_image_copy_capture_cursor_session_v1`
    /// stub (see this module's doc comment) — rather than a fake
    /// placeholder. `capture()` on such a session fails immediately instead
    /// of ever reaching `ExtScreencopyState::pending`.
    source: Option<CaptureSource>,
    /// `ext_image_copy_capture_manager_v1.create_session`'s `options`
    /// bitfield, `paint_cursors` bit — see `render_and_copy`'s `paint_cursor`
    /// param.
    paint_cursors: bool,
    /// Set the moment this session's first `capture()` request is queued —
    /// gates whether the *next* `capture()` is due unconditionally (a
    /// session's first successful frame, per-protocol, must not be
    /// arbitrarily delayed) or only once the output actually has fresh
    /// damage (every capture after that), mirroring
    /// `crate::screencopy`'s `copy` vs `copy_with_damage` gating exactly —
    /// see `service_pending_captures`'s doc comment there for why "queued"
    /// is treated as "will succeed" rather than waiting for a real `ready`.
    captured_once: bool,
}

pub struct SessionData(Mutex<SessionInner>);

/// Per-`ext_image_copy_capture_frame_v1` user data — accumulates
/// `attach_buffer`/`damage_buffer` until `capture()` finalizes it into a
/// `PendingCapture` pushed onto `ExtScreencopyState::pending`.
struct FrameInner {
    session: Weak<ExtImageCopyCaptureSessionV1>,
    buffer: Option<WlBuffer>,
    /// Set once `capture()` has been requested — enforces the protocol's
    /// `already_captured` error on any further `attach_buffer`/
    /// `damage_buffer`/`capture` against the same frame.
    captured: bool,
}

pub struct FrameData(Mutex<FrameInner>);

/// One `capture()` request, waiting for its output's next eligible frame —
/// identical shape and gating to `crate::screencopy::PendingCapture`, see
/// that struct's doc comment.
struct PendingCapture {
    frame: ExtImageCopyCaptureFrameV1,
    source: CaptureSource,
    buffer: WlBuffer,
    paint_cursors: bool,
    wants_damage: bool,
}

#[derive(Default)]
pub struct ExtScreencopyState {
    pending: Vec<PendingCapture>,
}

impl std::fmt::Debug for ExtScreencopyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtScreencopyState")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl ExtScreencopyState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ExtScreencopyGlobalData>
            + GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ExtScreencopyGlobalData>
            + GlobalDispatch<ExtImageCopyCaptureManagerV1, ExtScreencopyGlobalData>
            + Dispatch<ExtOutputImageCaptureSourceManagerV1, ()>
            + Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()>
            + Dispatch<ExtImageCaptureSourceV1, SourceData>
            + Dispatch<ExtImageCopyCaptureManagerV1, ()>
            + Dispatch<ExtImageCopyCaptureSessionV1, SessionData>
            + Dispatch<ExtImageCopyCaptureFrameV1, FrameData>
            + Dispatch<ExtImageCopyCaptureCursorSessionV1, ()>
            + 'static,
    {
        dh.create_global::<D, ExtOutputImageCaptureSourceManagerV1, _>(
            VERSION,
            ExtScreencopyGlobalData {
                filter: Box::new(|_| true),
            },
        );
        dh.create_global::<D, ExtForeignToplevelImageCaptureSourceManagerV1, _>(
            VERSION,
            ExtScreencopyGlobalData {
                filter: Box::new(|_| true),
            },
        );
        dh.create_global::<D, ExtImageCopyCaptureManagerV1, _>(
            VERSION,
            ExtScreencopyGlobalData {
                filter: Box::new(|_| true),
            },
        );
        ExtScreencopyState::default()
    }
}

// --- ext_output_image_capture_source_manager_v1 ---

impl<BackendData: Backend + 'static>
    GlobalDispatch<
        ExtOutputImageCaptureSourceManagerV1,
        ExtScreencopyGlobalData,
        SpitfireState<BackendData>,
    > for ExtScreencopyState
{
    fn bind(
        _state: &mut SpitfireState<BackendData>,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _global_data: &ExtScreencopyGlobalData,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ExtScreencopyGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ExtOutputImageCaptureSourceManagerV1, (), SpitfireState<BackendData>>
    for ExtScreencopyState
{
    fn request(
        _state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_output_image_capture_source_manager_v1::Request::CreateSource {
                source,
                output,
            } => {
                let Some(output) = Output::from_resource(&output) else {
                    // No sane recovery — the source object still has to
                    // exist (it's a `new_id`), just permanently unusable.
                    // `create_session` against it will fail below.
                    trace!("ext-image-capture-source: create_source for an unknown wl_output");
                    return;
                };
                data_init.init(
                    source,
                    SourceData {
                        source: CaptureSource::Output(output),
                    },
                );
            }
            ext_output_image_capture_source_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ExtImageCaptureSourceV1, SourceData, SpitfireState<BackendData>>
    for ExtScreencopyState
{
    fn request(
        _state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _source: &ExtImageCaptureSourceV1,
        _request: smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_image_capture_source_v1::Request,
        _data: &SourceData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        // Only request is `destroy` (a destructor) — nothing to do.
    }
}

// --- ext_foreign_toplevel_image_capture_source_manager_v1 ---

impl<BackendData: Backend + 'static>
    GlobalDispatch<
        ExtForeignToplevelImageCaptureSourceManagerV1,
        ExtScreencopyGlobalData,
        SpitfireState<BackendData>,
    > for ExtScreencopyState
{
    fn bind(
        _state: &mut SpitfireState<BackendData>,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _global_data: &ExtScreencopyGlobalData,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ExtScreencopyGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, (), SpitfireState<BackendData>>
    for ExtScreencopyState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource {
                source,
                toplevel_handle,
            } => {
                // The wire object only identifies *which* toplevel — the
                // window it actually refers to lives in
                // `state.foreign_toplevel_handles` (`crate::foreign_toplevel`),
                // matched by the protocol's own stable `identifier` (no
                // other equality is exposed on `ForeignToplevelHandle`).
                let window = ForeignToplevelHandle::from_resource(&toplevel_handle)
                    .map(|handle| handle.identifier())
                    .and_then(|id| {
                        state
                            .foreign_toplevel_handles
                            .iter()
                            .find(|(_, h)| h.identifier() == id)
                            .map(|(w, _)| w.clone())
                    });
                let Some(window) = window else {
                    // Closed/unknown toplevel — same "still has to exist,
                    // permanently unusable" handling as an unknown wl_output
                    // above.
                    trace!(
                        "ext-image-capture-source: create_source for an unknown/closed toplevel"
                    );
                    return;
                };
                data_init.init(
                    source,
                    SourceData {
                        source: CaptureSource::Toplevel(window),
                    },
                );
            }
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

// --- ext_image_copy_capture_manager_v1 ---

impl<BackendData: Backend + 'static>
    GlobalDispatch<
        ExtImageCopyCaptureManagerV1,
        ExtScreencopyGlobalData,
        SpitfireState<BackendData>,
    > for ExtScreencopyState
{
    fn bind(
        _state: &mut SpitfireState<BackendData>,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _global_data: &ExtScreencopyGlobalData,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ExtScreencopyGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ExtImageCopyCaptureManagerV1, (), SpitfireState<BackendData>> for ExtScreencopyState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession {
                session,
                source,
                options,
            } => {
                let capture_source = source.data::<SourceData>().map(|d| d.source.clone());
                let paint_cursors = options
                    .into_result()
                    .map(|options| {
                        options.contains(ext_image_copy_capture_manager_v1::Options::PaintCursors)
                    })
                    .unwrap_or(false);

                let Some(capture_source) = capture_source else {
                    // `create_source` above already failed for this source
                    // (unknown wl_output/toplevel) — send no constraints,
                    // just stop the session immediately so a well-behaved
                    // client tears it down rather than waiting forever.
                    let session = data_init.init(
                        session,
                        SessionData(Mutex::new(SessionInner {
                            source: None,
                            paint_cursors,
                            captured_once: false,
                        })),
                    );
                    session.stopped();
                    return;
                };

                let session = data_init.init(
                    session,
                    SessionData(Mutex::new(SessionInner {
                        source: Some(capture_source.clone()),
                        paint_cursors,
                        captured_once: false,
                    })),
                );
                let output_scale = state
                    .space
                    .outputs()
                    .next()
                    .map(|o| o.current_scale().fractional_scale())
                    .unwrap_or(1.0);
                send_constraints(&session, &capture_source, output_scale);
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                ..
            } => {
                // Not implemented yet (see this module's doc comment) — the
                // object has to exist (a mandatory `new_id`), it just never
                // emits `enter`/`position`/etc, so a client asking for a
                // separate cursor stream never sees one become active.
                data_init.init(session, ());
            }
            ext_image_copy_capture_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

/// Sends `buffer_size` + `shm_format(Argb8888)` + `done` for a freshly
/// created session — the full "SHM-only" constraint batch this module's
/// doc comment describes (no `dmabuf_device`/`dmabuf_format` at all).
/// Called once, right at `create_session` time — spitfire doesn't yet
/// re-send an updated batch if the output's mode (or, for a toplevel
/// source, the window's geometry) changes later (same limitation
/// `crate::screencopy` already has for `wlr-screencopy`).
///
/// A toplevel source's `buffer_size` is the window's own logical geometry
/// converted to physical pixels at `output_scale` — v1's "single output,
/// take the first" simplification (same one `shell/mod.rs`'s
/// `place_new_window`/`center_if_ruled` already make) rather than tracking
/// which output the window actually happens to be on.
fn send_constraints(
    session: &ExtImageCopyCaptureSessionV1,
    source: &CaptureSource,
    output_scale: f64,
) {
    let size = match source {
        CaptureSource::Output(output) => {
            let Some(mode) = output.current_mode() else {
                session.stopped();
                return;
            };
            mode.size
        }
        CaptureSource::Toplevel(window) => {
            if !window.alive() {
                session.stopped();
                return;
            }
            window
                .geometry()
                .size
                .to_physical_precise_round(output_scale)
        }
    };
    session.buffer_size(size.w as u32, size.h as u32);
    session.shm_format(wl_shm::Format::Argb8888);
    session.done();
}

// --- ext_image_copy_capture_session_v1 ---

impl<BackendData: Backend + 'static>
    Dispatch<ExtImageCopyCaptureSessionV1, SessionData, SpitfireState<BackendData>>
    for ExtScreencopyState
{
    fn request(
        _state: &mut SpitfireState<BackendData>,
        _client: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        _data: &SessionData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_image_copy_capture_session_v1::Request::CreateFrame { frame } => {
                data_init.init(
                    frame,
                    FrameData(Mutex::new(FrameInner {
                        session: session.downgrade(),
                        buffer: None,
                        captured: false,
                    })),
                );
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

// --- ext_image_copy_capture_frame_v1 ---

impl<BackendData: Backend + 'static>
    Dispatch<ExtImageCopyCaptureFrameV1, FrameData, SpitfireState<BackendData>>
    for ExtScreencopyState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        _client: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                let mut inner = data.0.lock().unwrap();
                if inner.captured {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "attach_buffer after capture",
                    );
                    return;
                }
                inner.buffer = Some(buffer);
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                let inner = data.0.lock().unwrap();
                if inner.captured {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "damage_buffer after capture",
                    );
                    return;
                }
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage,
                        "invalid damage_buffer rect",
                    );
                }
                // Accepted but not otherwise used — see this module's doc
                // comment for why (it's an optimization hint, not a
                // constraint on what gets copied).
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                let mut inner = data.0.lock().unwrap();
                if inner.captured {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "capture sent twice",
                    );
                    return;
                }
                let Some(buffer) = inner.buffer.take() else {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::NoBuffer,
                        "capture without attach_buffer",
                    );
                    return;
                };
                let Some(session) = inner.session.upgrade().ok() else {
                    frame.failed(ext_image_copy_capture_frame_v1::FailureReason::Stopped);
                    return;
                };
                let Some(session_data) = session.data::<SessionData>() else {
                    frame.failed(ext_image_copy_capture_frame_v1::FailureReason::Unknown);
                    return;
                };
                inner.captured = true;
                drop(inner);

                let mut session_inner = session_data.0.lock().unwrap();
                let Some(source) = session_inner.source.clone() else {
                    // A stopped/inert session (unknown source, or the
                    // cursor-session stub — see `SessionInner::source`'s
                    // doc comment) can never actually capture anything.
                    drop(session_inner);
                    frame.failed(ext_image_copy_capture_frame_v1::FailureReason::Stopped);
                    return;
                };
                let wants_damage = session_inner.captured_once;
                session_inner.captured_once = true;
                let paint_cursors = session_inner.paint_cursors;
                drop(session_inner);

                state.ext_screencopy_state.pending.push(PendingCapture {
                    frame: frame.clone(),
                    source,
                    buffer,
                    paint_cursors,
                    wants_damage,
                });
            }
            ext_image_copy_capture_frame_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut SpitfireState<BackendData>,
        _client: ClientId,
        frame: &ExtImageCopyCaptureFrameV1,
        _data: &FrameData,
    ) {
        // Same reasoning as `screencopy::FrameData`'s `destroyed`: drop it
        // rather than rendering/copying into a buffer nobody will read.
        state
            .ext_screencopy_state
            .pending
            .retain(|p| &p.frame != frame);
    }
}

// --- ext_image_copy_capture_cursor_session_v1 (inert stub — see doc comment) ---

impl<BackendData: Backend + 'static>
    Dispatch<ExtImageCopyCaptureCursorSessionV1, (), SpitfireState<BackendData>>
    for ExtScreencopyState
{
    fn request(
        _state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _cursor_session: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession {
                session,
                ..
            } => {
                // Never sent any constraints/`done` — a client waiting on
                // this session's own `buffer_size`/`done` batch before
                // calling `create_frame` just never gets one, matching
                // "cursor session support not implemented yet" above.
                data_init.init(
                    session,
                    SessionData(Mutex::new(SessionInner {
                        source: None,
                        paint_cursors: false,
                        captured_once: false,
                    })),
                );
            }
            ext_image_copy_capture_cursor_session_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

smithay::reexports::wayland_server::delegate_global_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtOutputImageCaptureSourceManagerV1: ExtScreencopyGlobalData
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_global_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtForeignToplevelImageCaptureSourceManagerV1: ExtScreencopyGlobalData
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_global_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtImageCopyCaptureManagerV1: ExtScreencopyGlobalData
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtOutputImageCaptureSourceManagerV1: ()
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtForeignToplevelImageCaptureSourceManagerV1: ()
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtImageCaptureSourceV1: SourceData
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtImageCopyCaptureManagerV1: ()
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtImageCopyCaptureSessionV1: SessionData
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtImageCopyCaptureFrameV1: FrameData
] => ExtScreencopyState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtImageCopyCaptureCursorSessionV1: ()
] => ExtScreencopyState);

/// Renders and copies every queued `capture()` for `output`, then drops
/// them from the queue — call once per output per frame, right alongside
/// `crate::screencopy::service_pending_captures` (same call sites in
/// winit.rs/udev.rs, same params — see that fn's doc comment for what each
/// one means, they're identical here). The only behavioral difference from
/// that fn: `wants_damage`/`paint_cursors` come from this protocol's own
/// per-frame/per-session state instead of always being `false`/`true`.
#[allow(clippy::too_many_arguments)]
pub fn service_pending_captures<R>(
    state: &mut ExtScreencopyState,
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
    borders: &[crate::render::BorderRect],
    border_width: i32,
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
) where
    // See `crate::screencopy::service_pending_captures`'s doc comment for
    // why the offscreen target is a fixed `GlesTexture`.
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    if state.pending.is_empty() {
        return;
    }
    let (due, rest): (Vec<_>, Vec<_>) = state.pending.drain(..).partition(|capture| {
        // A toplevel-sourced capture isn't tied to any particular output —
        // v1 is single-output anyway, so it's always "due for this call".
        let matches_output = match &capture.source {
            CaptureSource::Output(o) => o == output,
            CaptureSource::Toplevel(_) => true,
        };
        matches_output && (!capture.wants_damage || frame_damage.is_some())
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

/// What a successful render produced: the buffer size actually used, and
/// where its region sits within that (always `(0, 0)` in this module — no
/// sub-rect capture, unlike `wlr-screencopy`'s `capture_output_region`) —
/// what `capture_one`'s damage/`ready` reporting needs, regardless of
/// which `CaptureSource` branch produced it.
type CaptureResult = Result<(Size<i32, Physical>, Point<i32, Physical>), String>;

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
    borders: &[crate::render::BorderRect],
    border_width: i32,
    border_radius: i32,
    border_active: Color32F,
    border_inactive: Color32F,
    capture: PendingCapture,
) where
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    let PendingCapture {
        frame,
        source,
        buffer,
        paint_cursors,
        wants_damage,
    } = capture;

    if !frame.is_alive() {
        return;
    }

    // Both branches converge on the same `(buffer_size, region_loc)` shape
    // so the damage/ready reporting below doesn't need its own split — see
    // this module's doc comment for why the two render paths themselves
    // aren't unified any further than that.
    let result: CaptureResult = match &source {
        CaptureSource::Output(_) => {
            let Some(mode) = output.current_mode() else {
                frame.failed(ext_image_copy_capture_frame_v1::FailureReason::Unknown);
                return;
            };
            let buffer_size: Size<i32, Physical> = mode.size;
            let region_loc: Point<i32, Physical> = (0, 0).into();
            render_and_copy(
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
                paint_cursors,
                rules,
                borders,
                border_width,
                border_radius,
                border_active,
                border_inactive,
                &buffer,
                buffer_size,
                region_loc,
            )
            .map(|()| (buffer_size, region_loc))
        }
        CaptureSource::Toplevel(window) => render_window_and_copy(renderer, window, scale, &buffer)
            .map(|buffer_size| (buffer_size, (0, 0).into())),
    };

    match result {
        Ok((buffer_size, region_loc)) => {
            frame.transform(
                smithay::reexports::wayland_server::protocol::wl_output::Transform::Normal,
            );
            // First capture in a session always carries full damage
            // (per-protocol); later ones report the real frame damage,
            // same source `crate::screencopy`'s `copy_with_damage` uses.
            // `frame_damage` is in the *output's* physical coordinate
            // space — meaningless to intersect against a toplevel
            // capture's own (0,0)-based buffer, which has no relation to
            // where the window sits on screen, so that source always just
            // reports full-buffer damage instead (protocol-legal — a
            // client must accept more damage than the true minimum).
            if wants_damage && matches!(source, CaptureSource::Output(_)) {
                for rect in frame_damage.into_iter().flatten() {
                    let Some(clamped) = rect.intersection(Rectangle::new(region_loc, buffer_size))
                    else {
                        continue;
                    };
                    let local = clamped.loc - region_loc;
                    frame.damage(local.x, local.y, clamped.size.w, clamped.size.h);
                }
            } else {
                frame.damage(0, 0, buffer_size.w, buffer_size.h);
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs();
            frame.presentation_time(
                (secs >> 32) as u32,
                (secs & 0xFFFF_FFFF) as u32,
                now.subsec_nanos(),
            );
            frame.ready();
        }
        Err(err) => {
            trace!(%err, "ext-image-copy-capture: capture failed");
            frame.failed(ext_image_copy_capture_frame_v1::FailureReason::Unknown);
        }
    }
}

/// Renders `window`'s own elements — content plus SSD decoration if any,
/// via `WindowElement::render_elements`, the exact same per-window element
/// set `render.rs`'s real on-screen per-window loop already uses — into an
/// offscreen texture sized to the window's own geometry, then copies that
/// into `buffer`. Deliberately *not* `crate::screencopy::render_and_copy`:
/// no `Space`/output involved at all, no compositor border/gaps chrome, no
/// cursor — see this module's doc comment. Background is fully transparent
/// (`Color32F::TRANSPARENT`) rather than opaque, so any part of the buffer
/// the window's own elements don't cover (rounded corners, an
/// unusually-shaped surface) reads as transparent rather than black.
///
/// Returns the buffer size actually used on success, so the caller can
/// report `damage`/`ready` against it the same way the output path does.
fn render_window_and_copy<R>(
    renderer: &mut R,
    window: &WindowElement,
    scale: Scale<f64>,
    buffer: &WlBuffer,
) -> Result<Size<i32, Physical>, String>
where
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture> + Bind<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
{
    if !window.alive() {
        return Err("window no longer alive".to_string());
    }

    let buffer_size: Size<i32, Physical> = SpaceElement::geometry(window)
        .size
        .to_physical_precise_round(scale);
    if buffer_size.w <= 0 || buffer_size.h <= 0 {
        return Err("window has empty geometry".to_string());
    }

    let elements: Vec<WindowRenderElement<R>> =
        window.render_elements(renderer, (0, 0).into(), scale, 1.0);

    let texture_size: Size<i32, BufferCoord> = (buffer_size.w, buffer_size.h).into();
    let mut texture = renderer
        .create_buffer(Fourcc::Argb8888, texture_size)
        .map_err(|_| "failed to create offscreen capture buffer".to_string())?;
    let mut target = renderer
        .bind(&mut texture)
        .map_err(|_| "failed to bind offscreen capture buffer".to_string())?;

    let mut damage_tracker = OutputDamageTracker::new(buffer_size, scale, Transform::Normal);
    damage_tracker
        .render_output(renderer, &mut target, 0, &elements, Color32F::TRANSPARENT)
        .map_err(|_| "failed to render window capture".to_string())?;

    let region: Rectangle<i32, BufferCoord> =
        Rectangle::new((0, 0).into(), (buffer_size.w, buffer_size.h).into());
    let mapping = renderer
        .copy_framebuffer(&target, region, Fourcc::Argb8888)
        .map_err(|_| "failed to copy framebuffer".to_string())?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|_| "failed to map captured texture".to_string())?;

    let copy_result: Result<(), String> =
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
            // duration (that's `with_buffer_contents_mut`'s whole contract) —
            // same as `crate::screencopy::render_and_copy`'s identical copy.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len) };
            Ok(())
        })
        .map_err(|_| "target buffer is not shm-backed".to_string())?;
    copy_result?;

    Ok(buffer_size)
}
