//! Server-side `ext-workspace-v1` (Phase 5) — advertises spitfire's dynamic
//! workspace list (`crate::workspace::WorkspaceSet`) to clients like
//! Utumno's `Workspaces.qml` (built on `Quickshell.WindowManager`), niri's
//! own bar, or any other `ext-workspace-v1` consumer.
//!
//! Not provided by Smithay itself (unlike wlr-layer-shell-v1 and
//! ext-session-lock-v1, both used as-is in Phase 4) — the wire types come
//! from `wayland-protocols` (re-exported at
//! `smithay::reexports::wayland_protocols`; already generated with the
//! `staging`+`server` features Smithay's own `Cargo.toml` turns on for its
//! copy of that crate, so no extra dependency is needed here), but the
//! protocol logic in this file is new. Its broadcast-on-bind/full-resync
//! shape follows the same pattern Smithay's own
//! `wayland::foreign_toplevel_list` module (MIT/Apache-2.0) uses for a
//! same-shaped protocol — a manager that pushes `new_id` objects to every
//! bound client as compositor state changes over time.
//!
//! v1 simplifications, all safe for a single-output compositor:
//! - One workspace group, shared by every client (no per-output grouping,
//!   no `output_enter`/`output_leave` — not needed until real multi-output
//!   support exists).
//! - No `id` or `coordinates` events (both optional in the protocol).
//! - Every sync is a full resync (recompute desired state from
//!   `WorkspaceSet`, reconcile) rather than fine-grained diffing — simpler,
//!   and the workspace list is small enough that this is free in practice.

use smithay::reexports::{
    wayland_protocols::ext::workspace::v1::server::{
        ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities},
        ext_workspace_handle_v1::{
            self, ExtWorkspaceHandleV1, State as WorkspaceState, WorkspaceCapabilities,
        },
        ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
    },
    wayland_server::{
        backend::{ClientId, GlobalId},
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    },
};
use tracing::warn;

use crate::{
    state::{Backend, SpitfireState},
    workspace::WorkspaceId,
};

/// Global data for the `ext_workspace_manager_v1` global — just the client
/// filter, matching the shape every other global in this codebase uses
/// (`WlrLayerShellGlobalData`, `SessionLockManagerGlobalData`, ...).
pub struct ExtWorkspaceManagerGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ExtWorkspaceManagerGlobalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtWorkspaceManagerGlobalData")
            .finish_non_exhaustive()
    }
}

/// The protocol objects created so far for one bound client: one manager,
/// one (shared, v1-only) group, and the workspace handles created for it.
struct ClientState {
    manager: ExtWorkspaceManagerV1,
    group: ExtWorkspaceGroupHandleV1,
    workspaces: Vec<(WorkspaceId, ExtWorkspaceHandleV1)>,
}

/// State of the `ext_workspace_manager_v1` global. See the module docs for
/// the v1 scoping. Kept in sync with `SpitfireState::workspaces` by
/// `SpitfireState::sync_ext_workspace_state`, called after every
/// create/remove/rename/activate.
pub struct ExtWorkspaceState {
    global: GlobalId,
    dh: DisplayHandle,
    clients: Vec<ClientState>,
}

impl std::fmt::Debug for ExtWorkspaceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtWorkspaceState")
            .field("clients", &self.clients.len())
            .finish_non_exhaustive()
    }
}

impl ExtWorkspaceState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ExtWorkspaceManagerV1, ExtWorkspaceManagerGlobalData>
            + Dispatch<ExtWorkspaceManagerV1, ()>
            + Dispatch<ExtWorkspaceGroupHandleV1, ()>
            + Dispatch<ExtWorkspaceHandleV1, WorkspaceId>
            + 'static,
    {
        let global = dh.create_global::<D, ExtWorkspaceManagerV1, _>(
            1,
            ExtWorkspaceManagerGlobalData {
                filter: Box::new(|_| true),
            },
        );
        ExtWorkspaceState {
            global,
            dh: dh.clone(),
            clients: Vec::new(),
        }
    }

    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

/// A single workspace's desired state, as seen from `WorkspaceSet` — what
/// `sync` reconciles the protocol objects against.
pub struct WorkspaceSnapshot {
    pub id: WorkspaceId,
    pub name: String,
    pub active: bool,
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    /// Recomputes the desired workspace list from `self.workspaces` and
    /// pushes whatever's changed to every bound `ext-workspace-v1` client.
    /// Call after any create/remove/rename/activate.
    pub fn sync_ext_workspace_state(&mut self) {
        let snapshot: Vec<WorkspaceSnapshot> = self
            .workspaces
            .iter()
            .map(|ws| WorkspaceSnapshot {
                id: ws.id,
                name: ws.name.clone(),
                active: ws.id == self.workspaces.active().id,
            })
            .collect();

        let dh = self.ext_workspace_state.dh.clone();
        for client in &mut self.ext_workspace_state.clients {
            client.sync::<BackendData>(&snapshot, &dh);
        }
    }

    fn request_workspace_activate(&mut self, id: WorkspaceId) {
        if let Some(idx) = self.workspaces.index_of(id) {
            self.switch_workspace(idx);
        }
    }

    fn request_workspace_remove(&mut self, id: WorkspaceId) {
        let Some(idx) = self.workspaces.index_of(id) else {
            return;
        };
        if idx == self.workspaces.active_index() {
            // Can't remove the active workspace directly — switch to the
            // first other one first (dwm/niri-ish: never leaves you on a
            // removed workspace).
            let target = if idx == 0 { 1 } else { 0 };
            self.switch_workspace(target);
        }
        // Any windows still on the doomed workspace move to whatever's now
        // active, rather than being silently dropped from the tiling order.
        let orphaned: Vec<_> = self
            .workspaces
            .get(idx)
            .map(|ws| ws.tiling.windows().to_vec())
            .unwrap_or_default();
        for window in orphaned {
            if let Some(ws) = self.workspaces.get_mut(idx) {
                ws.tiling.remove(&window);
            }
            self.workspaces.active_mut().tiling.push(window);
        }

        if self.workspaces.remove(idx) {
            self.arrange_tiling();
            self.sync_ext_workspace_state();
        }
    }

    fn request_workspace_create(&mut self, name: Option<String>) {
        self.workspaces.create(name);
        self.sync_ext_workspace_state();
    }
}

impl ClientState {
    /// Creates/updates/removes this client's workspace handles to match
    /// `snapshot`, then sends `done()`.
    fn sync<BackendData: Backend + 'static>(
        &mut self,
        snapshot: &[WorkspaceSnapshot],
        dh: &DisplayHandle,
    ) {
        // Removed workspaces: tell the client, drop our tracking.
        self.workspaces.retain(|(id, handle)| {
            let still_exists = snapshot.iter().any(|ws| ws.id == *id);
            if !still_exists {
                handle.removed();
            }
            still_exists
        });

        let Ok(client) = dh.get_client(self.manager.id()) else {
            return;
        };

        for ws in snapshot {
            let handle = match self.workspaces.iter().find(|(id, _)| *id == ws.id) {
                Some((_, handle)) => handle.clone(),
                None => {
                    let Ok(handle) = client.create_resource::<ExtWorkspaceHandleV1, WorkspaceId, SpitfireState<BackendData>>(
                        dh,
                        self.manager.version(),
                        ws.id,
                    ) else {
                        continue;
                    };
                    self.manager.workspace(&handle);
                    self.group.workspace_enter(&handle);
                    handle.capabilities(
                        WorkspaceCapabilities::Activate | WorkspaceCapabilities::Remove,
                    );
                    self.workspaces.push((ws.id, handle.clone()));
                    handle
                }
            };

            handle.name(ws.name.clone());
            let state = if ws.active {
                WorkspaceState::Active
            } else {
                WorkspaceState::empty()
            };
            handle.state(state);
        }

        self.manager.done();
    }
}

impl<BackendData: Backend + 'static>
    GlobalDispatch<ExtWorkspaceManagerV1, ExtWorkspaceManagerGlobalData, SpitfireState<BackendData>>
    for ExtWorkspaceState
{
    fn bind(
        state: &mut SpitfireState<BackendData>,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _global_data: &ExtWorkspaceManagerGlobalData,
        data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        let manager = data_init.init(resource, ());

        let Ok(group) = client
            .create_resource::<ExtWorkspaceGroupHandleV1, _, SpitfireState<BackendData>>(
                dh,
                manager.version(),
                (),
            )
        else {
            return;
        };
        manager.workspace_group(&group);
        group.capabilities(GroupCapabilities::CreateWorkspace);

        let mut client_state = ClientState {
            manager: manager.clone(),
            group,
            workspaces: Vec::new(),
        };

        let snapshot: Vec<WorkspaceSnapshot> = state
            .workspaces
            .iter()
            .map(|ws| WorkspaceSnapshot {
                id: ws.id,
                name: ws.name.clone(),
                active: ws.id == state.workspaces.active().id,
            })
            .collect();
        client_state.sync::<BackendData>(&snapshot, dh);

        state.ext_workspace_state.clients.push(client_state);
    }

    fn can_view(client: Client, global_data: &ExtWorkspaceManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<BackendData: Backend + 'static> Dispatch<ExtWorkspaceManagerV1, (), SpitfireState<BackendData>>
    for ExtWorkspaceState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        client: &Client,
        manager: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            // We send events eagerly (every sync ends in a `done()`), so
            // there's nothing extra to flush on `commit` — spitfire never
            // batches workspace changes across multiple compositor actions.
            ext_workspace_manager_v1::Request::Commit => {}
            ext_workspace_manager_v1::Request::Stop => {
                Self::destroyed(state, client.id(), manager, data);
                manager.finished();
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut SpitfireState<BackendData>,
        _client: ClientId,
        resource: &ExtWorkspaceManagerV1,
        _data: &(),
    ) {
        state
            .ext_workspace_state
            .clients
            .retain(|c| &c.manager != resource);
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ExtWorkspaceGroupHandleV1, (), SpitfireState<BackendData>> for ExtWorkspaceState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _group: &ExtWorkspaceGroupHandleV1,
        request: ext_workspace_group_handle_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_workspace_group_handle_v1::Request::CreateWorkspace { workspace } => {
                state.request_workspace_create(Some(workspace));
            }
            ext_workspace_group_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<BackendData: Backend + 'static>
    Dispatch<ExtWorkspaceHandleV1, WorkspaceId, SpitfireState<BackendData>> for ExtWorkspaceState
{
    fn request(
        state: &mut SpitfireState<BackendData>,
        _client: &Client,
        _resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        data: &WorkspaceId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, SpitfireState<BackendData>>,
    ) {
        match request {
            ext_workspace_handle_v1::Request::Activate => state.request_workspace_activate(*data),
            ext_workspace_handle_v1::Request::Remove => state.request_workspace_remove(*data),
            // Deactivating a specific workspace isn't meaningful in a
            // single-active-workspace-per-output model: there's always
            // exactly one active workspace, so "deactivate this one"
            // without saying what should become active instead has no
            // well-defined effect. Ignored, same as an unsupported
            // capability would be.
            ext_workspace_handle_v1::Request::Deactivate => {
                warn!(
                    "ext-workspace-v1: deactivate request ignored (not advertised as a capability)"
                );
            }
            // v1 has exactly one group; assigning to it is already
            // implicit.
            ext_workspace_handle_v1::Request::Assign { .. } => {}
            ext_workspace_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

smithay::reexports::wayland_server::delegate_global_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtWorkspaceManagerV1: ExtWorkspaceManagerGlobalData
] => ExtWorkspaceState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtWorkspaceManagerV1: ()
] => ExtWorkspaceState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtWorkspaceGroupHandleV1: ()
] => ExtWorkspaceState);
smithay::reexports::wayland_server::delegate_dispatch!(@<BackendData: Backend + 'static> SpitfireState<BackendData>: [
    ExtWorkspaceHandleV1: WorkspaceId
] => ExtWorkspaceState);
