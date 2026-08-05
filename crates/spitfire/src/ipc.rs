//! Wires spitfire-ipc's control socket into the compositor's calloop event
//! loop, and turns each `Request` into a real effect on `SpitfireState`.

use std::{os::unix::net::UnixListener, sync::atomic::Ordering};

use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};
use spitfire_ipc::{Request, Response};
use tracing::{error, info, warn};

use crate::state::{Backend, SpitfireState};

/// Binds the control socket and registers it on `handle`. Each accepted
/// connection is handled to completion synchronously inside the callback —
/// one small JSON line in, one out — which is fine given how rarely this
/// fires compared to the render loop.
///
/// Binding failures (e.g. `$XDG_RUNTIME_DIR` unwritable) are logged and
/// otherwise ignored: `spitfirectl` just won't work, which shouldn't take
/// the whole compositor down with it.
pub fn start<BackendData: Backend + 'static>(
    handle: &LoopHandle<'static, SpitfireState<BackendData>>,
) {
    let path = spitfire_ipc::default_socket_path();
    let listener = match spitfire_ipc::bind_listener(&path) {
        Ok(listener) => listener,
        Err(err) => {
            error!(path = %path.display(), %err, "failed to bind the control socket, spitfirectl won't work");
            return;
        }
    };
    info!(path = %path.display(), "Listening on control socket");

    let result = handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, state: &mut SpitfireState<BackendData>| {
            // Safety: we don't drop the listener.
            unsafe {
                accept_all(listener.get_mut(), state);
            }
            Ok(PostAction::Continue)
        },
    );
    if let Err(err) = result {
        error!(%err, "failed to register the control socket on the event loop");
    }
}

fn accept_all<BackendData: Backend + 'static>(
    listener: &mut UnixListener,
    state: &mut SpitfireState<BackendData>,
) {
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _addr)) => stream,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(err) => {
                warn!(%err, "control socket accept() failed");
                return;
            }
        };

        // The accepted connection is short-lived (one request, one
        // response, then the client closes it) — block on it directly
        // instead of also driving it through calloop.
        if let Err(err) = stream.set_nonblocking(false) {
            warn!(%err, "failed to set an accepted connection to blocking mode");
            continue;
        }

        let cloned = match stream.try_clone() {
            Ok(cloned) => cloned,
            Err(err) => {
                warn!(%err, "failed to clone an accepted connection");
                continue;
            }
        };
        let mut reader = std::io::BufReader::new(cloned);
        let request: Request = match spitfire_ipc::read_message(&mut reader) {
            Ok(req) => req,
            Err(err) => {
                warn!(%err, "failed to read a request from the control socket");
                continue;
            }
        };

        let response = handle_request(state, request);
        if let Err(err) = spitfire_ipc::write_message(&mut stream, &response) {
            warn!(%err, "failed to write a response to the control socket");
        }
    }
}

fn handle_request<BackendData: Backend + 'static>(
    state: &mut SpitfireState<BackendData>,
    request: Request,
) -> Response {
    match request {
        Request::ReloadConfig => {
            state.reload_config();
            Response::ok()
        }

        Request::Quit => {
            info!("spitfirectl quit");
            state.running.store(false, Ordering::SeqCst);
            Response::ok()
        }

        Request::SetLayout { mode } => match parse_layout_mode(&mode) {
            Some(mode) => {
                info!(?mode, "spitfirectl layout");
                state.workspaces.active_mut().tiling.params.set_mode(mode);
                state.arrange_tiling();
                Response::ok()
            }
            None => Response::err(format!(
                "unknown layout mode '{mode}' (expected tile, floating, fibonacci, or monocle)"
            )),
        },

        Request::FocusWorkspace { index } => {
            if index == 0 {
                return Response::err("workspace index is 1-based, 0 is not valid");
            }
            info!(index, "spitfirectl workspace focus");
            state.switch_workspace(index - 1);
            Response::ok()
        }

        Request::ListWorkspaces => {
            let active = state.workspaces.active_index();
            let workspaces: Vec<_> = state
                .workspaces
                .iter()
                .enumerate()
                .map(|(i, ws)| {
                    serde_json::json!({
                        "index": i + 1,
                        "name": ws.name,
                        "active": i == active,
                        "layout": format!("{:?}", ws.tiling.params.mode).to_lowercase(),
                        "windows": ws.tiling.windows().len(),
                    })
                })
                .collect();
            Response::ok_data(serde_json::Value::Array(workspaces))
        }

        Request::ListWindows => {
            let windows: Vec<_> = state
                .space
                .elements()
                .map(|window| {
                    serde_json::json!({
                        "app_id": window.app_id(),
                        "title": window.title(),
                    })
                })
                .collect();
            Response::ok_data(serde_json::Value::Array(windows))
        }

        Request::ListOutputs => {
            let outputs: Vec<_> = state
                .space
                .outputs()
                .map(|output| {
                    let geometry = state.space.output_geometry(output);
                    serde_json::json!({
                        "name": output.name(),
                        "geometry": geometry.map(|g| serde_json::json!({
                            "x": g.loc.x, "y": g.loc.y, "w": g.size.w, "h": g.size.h,
                        })),
                    })
                })
                .collect();
            Response::ok_data(serde_json::Value::Array(outputs))
        }
    }
}

fn parse_layout_mode(name: &str) -> Option<spitfire_layout::LayoutMode> {
    use spitfire_layout::LayoutMode;
    match name.to_ascii_lowercase().as_str() {
        "tile" => Some(LayoutMode::Tile),
        "floating" => Some(LayoutMode::Floating),
        "fibonacci" | "spiral" => Some(LayoutMode::Fibonacci),
        "monocle" => Some(LayoutMode::Monocle),
        _ => None,
    }
}
