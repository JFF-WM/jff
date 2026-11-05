//! `ext-workspace-v1`: lets bars and docks (waybar's `ext/workspaces`, ...) show the workspaces
//! and switch between them.
//!
//! jff's workspaces are shared across all monitors, so there's one workspace group holding every
//! workspace and every monitor. A workspace is named after its number (1-based), carries its
//! index as its one coordinate, and is `active` while it's the shown one.
//!
//! Like `jff msg watch`, changes come from comparing: once per event-loop iteration,
//! `JffState::publish_ext_workspaces` compares the workspace count, the shown workspace, and the
//! monitors with what each client was told, and sends the difference.
//!
//! Supported: `activate` (the switch happens on `commit`, as the protocol says). Not supported:
//! creating, removing, deactivating, and moving workspaces (their count comes from the config),
//! the group says so with empty capabilities, and the requests are ignored.

use smithay::reexports::wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, State, WorkspaceCapabilities},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::JffState;

/// What one client that bound the protocol has been told.
struct Binding {
    manager: ExtWorkspaceManagerV1,
    group: Option<ExtWorkspaceGroupHandleV1>,
    /// The handle of workspace `i` is `workspaces[i]`.
    workspaces: Vec<ExtWorkspaceHandleV1>,
    /// The workspace announced as active.
    active: Option<usize>,
    /// Names of the monitors the group was told about.
    outputs: Vec<String>,
    /// `activate` requests since the last `commit` (workspace indices).
    pending: Vec<usize>,
}

/// Everything the protocol needs to keep between requests.
pub struct ExtWorkspace {
    bindings: Vec<Binding>,
    _global: GlobalId,
}

impl ExtWorkspace {
    /// Create the protocol global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            bindings: Vec::new(),
            _global: display.create_global::<JffState, ExtWorkspaceManagerV1, ()>(1, ()),
        }
    }
}

fn state_of(active: bool) -> State {
    if active {
        State::Active
    } else {
        State::empty()
    }
}

/// Bring what the client knows up to date. Returns whether anything was sent (the caller ends
/// with `done` then).
fn sync(
    display: &DisplayHandle,
    binding: &mut Binding,
    count: usize,
    current: usize,
    outputs: &[smithay::output::Output],
) -> bool {
    let Some(client) = binding.manager.client() else {
        return false;
    };
    let version = binding.manager.version();
    let mut sent = false;

    if binding.group.is_none() {
        let Ok(group) =
            client.create_resource::<ExtWorkspaceGroupHandleV1, (), JffState>(display, version, ())
        else {
            return false;
        };
        binding.manager.workspace_group(&group);
        // No `create_workspace`: the number of workspaces comes from the config.
        group.capabilities(GroupCapabilities::empty());
        binding.group = Some(group);
        sent = true;
    }
    let group = binding.group.clone().expect("created above");

    // Monitors: every monitor shows the same workspaces, so all of them are in the group.
    for output in outputs {
        let name = output.name();
        if !binding.outputs.contains(&name) {
            for object in output.client_outputs(&client) {
                group.output_enter(&object);
            }
            binding.outputs.push(name);
            sent = true;
        }
    }
    let before = binding.outputs.len();
    binding
        .outputs
        .retain(|name| outputs.iter().any(|o| o.name() == *name));
    if binding.outputs.len() != before {
        // The client's `wl_output` is gone with the monitor, nothing to tell.
        sent = true;
    }

    // Workspaces that were added.
    while binding.workspaces.len() < count {
        let index = binding.workspaces.len();
        let Ok(handle) = client
            .create_resource::<ExtWorkspaceHandleV1, usize, JffState>(display, version, index)
        else {
            break;
        };
        binding.manager.workspace(&handle);
        handle.id(format!("jff-workspace-{}", index + 1));
        handle.name((index + 1).to_string());
        handle.coordinates((index as u32).to_ne_bytes().to_vec());
        handle.state(state_of(index == current));
        handle.capabilities(WorkspaceCapabilities::Activate);
        group.workspace_enter(&handle);
        binding.workspaces.push(handle);
        sent = true;
    }
    if binding.active.is_none() && !binding.workspaces.is_empty() {
        // Announced as part of the handles just created.
        binding.active = Some(current);
    }
    // Workspaces that went away.
    while binding.workspaces.len() > count {
        if let Some(handle) = binding.workspaces.pop() {
            group.workspace_leave(&handle);
            handle.removed();
            sent = true;
        }
    }

    // The shown workspace changed.
    if binding.active != Some(current) && current < binding.workspaces.len() {
        if let Some(old) = binding.active.filter(|&i| i < binding.workspaces.len()) {
            binding.workspaces[old].state(state_of(false));
        }
        binding.workspaces[current].state(state_of(true));
        binding.active = Some(current);
        sent = true;
    }
    sent
}

impl JffState {
    /// Tell every client that bound the protocol what changed since the last call. Runs once
    /// per event-loop iteration, does nothing while nobody uses the protocol.
    pub fn publish_ext_workspaces(&mut self) {
        self.ext_workspace.bindings.retain(|b| b.manager.is_alive());
        if self.ext_workspace.bindings.is_empty() {
            return;
        }
        let (count, current) = (self.workspace_count(), self.desk.current());
        let outputs = self.outputs.clone();
        let display = self.display_handle.clone();
        for binding in &mut self.ext_workspace.bindings {
            if sync(&display, binding, count, current, &outputs) {
                binding.manager.done();
            }
        }
    }
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for JffState {
    fn bind(
        state: &mut JffState,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let manager = data_init.init(resource, ());
        let mut binding = Binding {
            manager,
            group: None,
            workspaces: Vec::new(),
            active: None,
            outputs: Vec::new(),
            pending: Vec::new(),
        };
        let (count, current) = (state.workspace_count(), state.desk.current());
        if sync(display, &mut binding, count, current, &state.outputs) {
            binding.manager.done();
        }
        state.ext_workspace.bindings.push(binding);
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                let target = state
                    .ext_workspace
                    .bindings
                    .iter_mut()
                    .find(|b| b.manager == *resource)
                    .and_then(|b| std::mem::take(&mut b.pending).pop());
                // The last one asked for wins.
                if let Some(index) = target {
                    state.switch_workspace(index);
                }
            }
            ext_workspace_manager_v1::Request::Stop => {
                state
                    .ext_workspace
                    .bindings
                    .retain(|b| b.manager != *resource);
                resource.finished();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ExtWorkspaceGroupHandleV1,
        _request: ext_workspace_group_handle_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        // `create_workspace` isn't supported (the group has no capabilities), `destroy` needs
        // nothing.
    }
}

impl Dispatch<ExtWorkspaceHandleV1, usize> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        index: &usize,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        // Only `activate` is supported, and it takes effect on the manager's `commit`.
        if let ext_workspace_handle_v1::Request::Activate = request {
            if let Some(binding) = state
                .ext_workspace
                .bindings
                .iter_mut()
                .find(|b| b.workspaces.contains(resource))
            {
                binding.pending.push(*index);
            }
        }
    }
}
