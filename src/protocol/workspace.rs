//! `ext-workspace-v1`: lets bars and docks (waybar's `ext/workspaces`, ...) show the workspaces
//! and switch between them.
//!
//! By default workspaces are shared by all monitors, so there's one workspace group holding every
//! workspace and every monitor. With `workspaces { per-output }` every monitor has a workspace of
//! its own, so every monitor gets a group of its own: that group holds the monitor and a full set
//! of workspaces, and its active workspace is the one *that monitor* shows. A bar on a monitor
//! reads the group of its monitor, activating a workspace in a group switches that monitor only
//! (not the one the pointer happens to be on).
//!
//! A workspace is named after its number (1-based), carries its index as its one coordinate, and
//! is `active` while it's the shown one in its group.
//!
//! Like `jff msg watch`, changes come from comparing: once per event-loop iteration,
//! `JffState::publish_ext_workspaces` compares the workspace count, the shown workspaces, and
//! the monitors with what each client was told, and sends the difference. Turning `per-output`
//! on or off in the config replaces the groups.
//!
//! Supported: `activate` (the switch happens on `commit`, as the protocol says). Not supported:
//! creating, removing, deactivating, and moving workspaces (their count comes from the config),
//! the groups say so with empty capabilities, and the requests are ignored.

use smithay::output::Output;
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

/// The monitor a group belongs to, as part of a workspace handle's user data: empty for the one
/// group that every monitor shares.
type GroupKey = String;

/// One workspace group a client was told about.
struct Group {
    /// `Some(monitor)` for the group of one monitor (per-output workspaces), `None` for the one
    /// shared by every monitor.
    output: Option<String>,
    handle: ExtWorkspaceGroupHandleV1,
    /// The handle of workspace `i` is `workspaces[i]`.
    workspaces: Vec<ExtWorkspaceHandleV1>,
    /// The workspace announced as active.
    active: Option<usize>,
    /// Names of the monitors the group was told about.
    outputs: Vec<String>,
}

impl Group {
    fn key(&self) -> GroupKey {
        self.output.clone().unwrap_or_default()
    }
}

/// What one client that bound the protocol has been told.
struct Binding {
    manager: ExtWorkspaceManagerV1,
    /// Whether `groups` are per monitor (`true`) or the one shared group (`false`).
    per_output: bool,
    groups: Vec<Group>,
    /// `activate` requests since the last `commit`: the group (see [`GroupKey`]) and the
    /// workspace index.
    pending: Vec<(GroupKey, usize)>,
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

/// What the compositor shows, as the protocol sees it.
struct View {
    count: usize,
    per_output: bool,
    /// The workspace shown on every monitor when they share it.
    shared: usize,
    /// The workspace each monitor shows, by name (per-output workspaces).
    shown: Vec<(String, usize)>,
}

fn state_of(active: bool) -> State {
    if active {
        State::Active
    } else {
        State::empty()
    }
}

/// Take a group away: its workspaces leave it and go, then the group goes.
fn remove_group(group: Group) {
    for handle in group.workspaces.iter().rev() {
        group.handle.workspace_leave(handle);
        handle.removed();
    }
    group.handle.removed();
}

/// Bring what the client knows up to date. Returns whether anything was sent (the caller ends
/// with `done` then).
fn sync(display: &DisplayHandle, binding: &mut Binding, view: &View, outputs: &[Output]) -> bool {
    let Some(client) = binding.manager.client() else {
        return false;
    };
    let version = binding.manager.version();
    let mut sent = false;

    // `per-output` was turned on or off: start over with the other kind of group.
    if binding.per_output != view.per_output {
        for group in std::mem::take(&mut binding.groups) {
            remove_group(group);
        }
        binding.pending.clear();
        binding.per_output = view.per_output;
        sent = true;
    }

    // The groups there should be, with the workspace each shows. Shared workspaces: one group
    // (`None`). Per-output workspaces: one per monitor that is there.
    let wanted: Vec<(Option<String>, usize)> = if view.per_output {
        view.shown
            .iter()
            .map(|(name, shown)| (Some(name.clone()), *shown))
            .collect()
    } else {
        vec![(None, view.shared)]
    };

    // Groups of monitors that are gone.
    let (keep, gone): (Vec<Group>, Vec<Group>) = std::mem::take(&mut binding.groups)
        .into_iter()
        .partition(|group| wanted.iter().any(|(output, _)| *output == group.output));
    binding.groups = keep;
    for group in gone {
        remove_group(group);
        sent = true;
    }

    for (output, current) in &wanted {
        let index = match binding.groups.iter().position(|g| g.output == *output) {
            Some(index) => index,
            None => {
                let Ok(handle) = client.create_resource::<ExtWorkspaceGroupHandleV1, (), JffState>(
                    display,
                    version,
                    (),
                ) else {
                    continue;
                };
                binding.manager.workspace_group(&handle);
                // No `create_workspace`: the number of workspaces comes from the config.
                handle.capabilities(GroupCapabilities::empty());
                binding.groups.push(Group {
                    output: output.clone(),
                    handle,
                    workspaces: Vec::new(),
                    active: None,
                    outputs: Vec::new(),
                });
                sent = true;
                binding.groups.len() - 1
            }
        };
        let group = &mut binding.groups[index];
        let key = group.key();

        // Monitors: the shared group holds all of them, a monitor's group only that monitor.
        for monitor in outputs {
            let name = monitor.name();
            let belongs = output.as_ref().map_or(true, |own| *own == name);
            if belongs && !group.outputs.contains(&name) {
                for object in monitor.client_outputs(&client) {
                    group.handle.output_enter(&object);
                }
                group.outputs.push(name);
                sent = true;
            }
        }
        let before = group.outputs.len();
        group
            .outputs
            .retain(|name| outputs.iter().any(|o| o.name() == *name));
        if group.outputs.len() != before {
            // The client's `wl_output` is gone with the monitor, nothing to tell.
            sent = true;
        }

        // Workspaces that were added.
        while group.workspaces.len() < view.count {
            let at = group.workspaces.len();
            let Ok(handle) = client
                .create_resource::<ExtWorkspaceHandleV1, (GroupKey, usize), JffState>(
                    display,
                    version,
                    (key.clone(), at),
                )
            else {
                break;
            };
            binding.manager.workspace(&handle);
            // Per-output: the id has to differ between monitors, ids are unique per client.
            match output {
                Some(name) => handle.id(format!("jff-workspace-{name}-{}", at + 1)),
                None => handle.id(format!("jff-workspace-{}", at + 1)),
            }
            handle.name((at + 1).to_string());
            handle.coordinates((at as u32).to_ne_bytes().to_vec());
            handle.state(state_of(at == *current));
            handle.capabilities(WorkspaceCapabilities::Activate);
            group.handle.workspace_enter(&handle);
            group.workspaces.push(handle);
            sent = true;
        }
        if group.active.is_none() && !group.workspaces.is_empty() {
            // Announced as part of the handles just created.
            group.active = Some(*current);
        }
        // Workspaces that went away.
        while group.workspaces.len() > view.count {
            if let Some(handle) = group.workspaces.pop() {
                group.handle.workspace_leave(&handle);
                handle.removed();
                sent = true;
            }
        }

        // The shown workspace changed.
        if group.active != Some(*current) && *current < group.workspaces.len() {
            if let Some(old) = group.active.filter(|&i| i < group.workspaces.len()) {
                group.workspaces[old].state(state_of(false));
            }
            group.workspaces[*current].state(state_of(true));
            group.active = Some(*current);
            sent = true;
        }
    }
    sent
}

impl JffState {
    /// What the compositor shows now, for [`sync`].
    fn ext_workspace_view(&self) -> View {
        let per_output = self.desk.per_output();
        View {
            count: self.workspace_count(),
            per_output,
            shared: self.desk.current(),
            shown: if per_output {
                self.outputs
                    .iter()
                    .map(|output| {
                        let name = output.name();
                        let shown = self.desk.current_on(&name);
                        (name, shown)
                    })
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    /// Tell every client that bound the protocol what changed since the last call. Runs once
    /// per event-loop iteration, does nothing while nobody uses the protocol.
    pub fn publish_ext_workspaces(&mut self) {
        self.ext_workspace.bindings.retain(|b| b.manager.is_alive());
        if self.ext_workspace.bindings.is_empty() {
            return;
        }
        let view = self.ext_workspace_view();
        let outputs = self.outputs.clone();
        let display = self.display_handle.clone();
        for binding in &mut self.ext_workspace.bindings {
            if sync(&display, binding, &view, &outputs) {
                binding.manager.done();
            }
        }
    }
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for JffState {
    // Sandboxed apps must not see this.
    fn can_view(client: smithay::reexports::wayland_server::Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        state: &mut JffState,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let manager = data_init.init(resource, ());
        let view = state.ext_workspace_view();
        let mut binding = Binding {
            manager,
            per_output: view.per_output,
            groups: Vec::new(),
            pending: Vec::new(),
        };
        if sync(display, &mut binding, &view, &state.outputs) {
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
                if let Some((group, index)) = target {
                    if group.is_empty() {
                        // The shared group: every monitor (or the one in use, see
                        // `switch_workspace`).
                        state.switch_workspace(index);
                    } else {
                        // The group of one monitor: that monitor and no other.
                        state.switch_workspace_on(&group, index);
                    }
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
        // `create_workspace` isn't supported (the groups have no capabilities), `destroy` needs
        // nothing.
    }
}

impl Dispatch<ExtWorkspaceHandleV1, (GroupKey, usize)> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        (key, index): &(GroupKey, usize),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        // Only `activate` is supported, and it takes effect on the manager's `commit`.
        if let ext_workspace_handle_v1::Request::Activate = request {
            if let Some(binding) = state
                .ext_workspace
                .bindings
                .iter_mut()
                .find(|b| b.groups.iter().any(|g| g.workspaces.contains(resource)))
            {
                binding.pending.push((key.clone(), *index));
            }
        }
    }
}
