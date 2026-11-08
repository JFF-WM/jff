//! `wlr-output-management-unstable-v1`: tools like wlr-randr and kanshi list the monitors and
//! change their mode, position, and on/off state while jff runs.
//!
//! Changes go through the in-memory `Config`, exactly as if they'd been written in the `outputs`
//! block of the config file, so they last until the config is reloaded. Scale, transform, and
//! adaptive sync aren't supported: a configuration asking for them is reported as failed.
//!
//! Units: the protocol speaks mHz for refresh rates, the config speaks whole Hz. Conversion
//! happens at the edge (see `mhz_to_hz`).

use std::sync::Mutex;

use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::ZwlrOutputHeadV1,
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::ZwlrOutputModeV1,
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_output::Transform;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use crate::app::Backend;
use crate::config::ModeRequest;
use crate::feature::outputs::{mhz_to_hz, Monitor, MonitorMode};
use crate::state::JffState;

/// Highest version jff speaks. 4 adds adaptive sync, which we don't have.
const VERSION: u32 = 3;

/// One manager object a client bound, and the heads announced on it.
struct ManagerEntry {
    manager: ZwlrOutputManagerV1,
    heads: Vec<(ZwlrOutputHeadV1, Vec<ZwlrOutputModeV1>)>,
}

/// Everything the protocol needs to keep between requests.
pub struct OutputManagement {
    managers: Vec<ManagerEntry>,
    /// Bumped on every announcement, so clients can tell their view is out of date.
    serial: u32,
    /// What clients were told last, so unchanged state isn't announced again.
    last_heads: Vec<HeadState>,
    _global: GlobalId,
}

impl OutputManagement {
    /// Create the protocol global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            managers: Vec::new(),
            serial: 0,
            last_heads: Vec::new(),
            _global: display.create_global::<JffState, ZwlrOutputManagerV1, ()>(VERSION, ()),
        }
    }
}

/// What a client sees of one monitor.
#[derive(Clone, PartialEq)]
struct HeadState {
    monitor: Monitor,
    enabled: bool,
    /// Index into `monitor.modes`.
    current: Option<usize>,
    position: (i32, i32),
}

/// User data of a mode object.
pub struct ModeData {
    width: i32,
    height: i32,
    /// Refresh in mHz.
    refresh: i32,
}

/// What a client asked for one head in a configuration.
#[derive(Clone, Debug, Default)]
pub struct HeadRequest {
    name: String,
    enabled: bool,
    mode: Option<ModeRequest>,
    position: Option<(i32, i32)>,
    /// Something jff can't do (scale, transform) was asked for.
    unsupported: bool,
}

/// User data of a configuration object.
pub struct ConfigState {
    serial: u32,
    used: bool,
    requests: Vec<HeadRequest>,
}

/// User data of a configuration head object.
pub struct HeadConfigData {
    config: ZwlrOutputConfigurationV1,
    name: String,
}

/// Create head and mode objects for `heads` on `manager`, then `done`.
fn announce(
    display: &DisplayHandle,
    manager: &ZwlrOutputManagerV1,
    heads: &[HeadState],
    serial: u32,
) -> Vec<(ZwlrOutputHeadV1, Vec<ZwlrOutputModeV1>)> {
    let mut announced = Vec::new();
    let Some(client) = manager.client() else {
        return announced;
    };
    let version = manager.version();

    for state in heads {
        let monitor = &state.monitor;
        let Ok(head) = client.create_resource::<ZwlrOutputHeadV1, String, JffState>(
            display,
            version,
            monitor.name.clone(),
        ) else {
            continue;
        };
        manager.head(&head);
        head.name(monitor.name.clone());
        head.description(monitor.name.clone());
        if monitor.size_mm != (0, 0) {
            head.physical_size(monitor.size_mm.0, monitor.size_mm.1);
        }

        let mut modes = Vec::new();
        for mode in &monitor.modes {
            let Ok(resource) = client.create_resource::<ZwlrOutputModeV1, ModeData, JffState>(
                display,
                version,
                ModeData {
                    width: mode.width,
                    height: mode.height,
                    refresh: mode.refresh,
                },
            ) else {
                continue;
            };
            head.mode(&resource);
            resource.size(mode.width, mode.height);
            resource.refresh(mode.refresh);
            if mode.preferred {
                resource.preferred();
            }
            modes.push(resource);
        }

        head.enabled(state.enabled as i32);
        if state.enabled {
            if let Some(current) = state.current.and_then(|i| modes.get(i)) {
                head.current_mode(current);
            }
            head.position(state.position.0, state.position.1);
            head.transform(Transform::Normal);
            head.scale(1.0);
        }
        announced.push((head, modes));
    }

    manager.done(serial);
    announced
}

impl JffState {
    /// Every connected monitor, lit or not.
    pub fn monitors(&self) -> Vec<Monitor> {
        match &self.backend {
            Backend::Tty(tty) => tty.monitors().to_vec(),
            Backend::Winit(_) => self
                .outputs
                .iter()
                .map(|output| Monitor {
                    name: output.name(),
                    size_mm: (0, 0),
                    modes: output
                        .current_mode()
                        .into_iter()
                        .map(|mode| MonitorMode {
                            width: mode.size.w,
                            height: mode.size.h,
                            refresh: mode.refresh,
                            preferred: true,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// The monitors as clients should see them right now.
    fn head_states(&self) -> Vec<HeadState> {
        self.monitors()
            .into_iter()
            .map(|monitor| {
                let output = self.outputs.iter().find(|o| o.name() == monitor.name);
                let current = output.and_then(|o| o.current_mode()).and_then(|mode| {
                    monitor.modes.iter().position(|m| {
                        m.width == mode.size.w
                            && m.height == mode.size.h
                            && m.refresh == mode.refresh
                    })
                });
                let position = output
                    .map(|o| {
                        let loc = o.current_location();
                        (loc.x, loc.y)
                    })
                    .unwrap_or((0, 0));
                HeadState {
                    enabled: output.is_some(),
                    current,
                    position,
                    monitor,
                }
            })
            .collect()
    }

    /// Tell every client that bound the protocol how the monitors look now. Old heads are
    /// retired and new ones announced, which every client already has to handle for hotplug.
    /// Does nothing when the monitors look the same as when clients were last told.
    pub fn output_management_changed(&mut self) {
        let heads = self.head_states();
        if heads == self.output_management.last_heads {
            return;
        }
        let display = self.display_handle.clone();
        let management = &mut self.output_management;
        management.last_heads = heads.clone();
        management.serial = management.serial.wrapping_add(1);
        management.managers.retain(|m| m.manager.is_alive());
        let serial = management.serial;
        for entry in &mut management.managers {
            for (head, modes) in entry.heads.drain(..) {
                for mode in modes {
                    mode.finished();
                }
                head.finished();
            }
            entry.heads = announce(&display, &entry.manager, &heads, serial);
        }
    }

    /// Whether a configuration could be applied as asked.
    fn check_output_config(&self, requests: &[HeadRequest]) -> bool {
        let monitors = self.monitors();
        let mut any_enabled = false;
        for monitor in &monitors {
            let request = requests.iter().find(|r| r.name == monitor.name);
            let enabled = match request {
                Some(request) => request.enabled,
                None => self.outputs.iter().any(|o| o.name() == monitor.name),
            };
            any_enabled |= enabled;
            if let Some(request) = request {
                if request.unsupported {
                    return false;
                }
                if let (true, Some(mode)) = (request.enabled, &request.mode) {
                    if !monitor.has_mode(mode) {
                        return false;
                    }
                }
            }
        }
        // Never leave the user with every monitor dark.
        any_enabled
            && requests
                .iter()
                .all(|r| monitors.iter().any(|m| m.name == r.name))
    }

    /// Apply a configuration. Returns whether the monitors ended up as asked.
    fn apply_output_config(&mut self, requests: &[HeadRequest]) -> bool {
        if !self.check_output_config(requests) {
            return false;
        }
        for request in requests {
            self.config.update_output(&request.name, |config| {
                config.off = !request.enabled;
                if request.enabled {
                    if let Some(mode) = request.mode {
                        config.mode = Some(mode);
                    }
                    if let Some(position) = request.position {
                        config.position = Some(position);
                    }
                }
            });
        }
        self.reconcile_outputs();
        requests
            .iter()
            .all(|r| self.outputs.iter().any(|o| o.name() == r.name) == r.enabled)
    }
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for JffState {
    // Sandboxed apps must not see this.
    fn can_view(client: smithay::reexports::wayland_server::Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        state: &mut JffState,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let manager = data_init.init(resource, ());
        let heads = announce(
            display,
            &manager,
            &state.head_states(),
            state.output_management.serial,
        );
        state
            .output_management
            .managers
            .push(ManagerEntry { manager, heads });
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    Mutex::new(ConfigState {
                        serial,
                        used: false,
                        requests: Vec::new(),
                    }),
                );
            }
            zwlr_output_manager_v1::Request::Stop => {
                state
                    .output_management
                    .managers
                    .retain(|m| m.manager != *resource);
                resource.finished();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputHeadV1, String> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrOutputHeadV1,
        _request: <ZwlrOutputHeadV1 as Resource>::Request,
        _data: &String,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
    }
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrOutputModeV1,
        _request: <ZwlrOutputModeV1 as Resource>::Request,
        _data: &ModeData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, Mutex<ConfigState>> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &Mutex<ConfigState>,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            zwlr_output_configuration_v1::Request::EnableHead { id, head } => {
                let name = head.data::<String>().cloned().unwrap_or_default();
                // The new object must be created whatever happens next.
                data_init.init(
                    id,
                    HeadConfigData {
                        config: resource.clone(),
                        name: name.clone(),
                    },
                );
                let mut config = data.lock().unwrap();
                if config.requests.iter().any(|r| r.name == name) {
                    drop(config);
                    resource.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        "head was already configured",
                    );
                    return;
                }
                config.requests.push(HeadRequest {
                    name,
                    enabled: true,
                    ..Default::default()
                });
            }
            zwlr_output_configuration_v1::Request::DisableHead { head } => {
                let name = head.data::<String>().cloned().unwrap_or_default();
                let mut config = data.lock().unwrap();
                if config.requests.iter().any(|r| r.name == name) {
                    drop(config);
                    resource.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        "head was already configured",
                    );
                    return;
                }
                config.requests.push(HeadRequest {
                    name,
                    enabled: false,
                    ..Default::default()
                });
            }
            zwlr_output_configuration_v1::Request::Apply
            | zwlr_output_configuration_v1::Request::Test => {
                let apply = matches!(request, zwlr_output_configuration_v1::Request::Apply);
                let (serial, requests) = {
                    let mut config = data.lock().unwrap();
                    if config.used {
                        drop(config);
                        resource.post_error(
                            zwlr_output_configuration_v1::Error::AlreadyUsed,
                            "configuration was already used",
                        );
                        return;
                    }
                    config.used = true;
                    (config.serial, config.requests.clone())
                };
                if serial != state.output_management.serial {
                    resource.cancelled();
                } else if !apply {
                    if state.check_output_config(&requests) {
                        resource.succeeded();
                    } else {
                        resource.failed();
                    }
                } else if state.apply_output_config(&requests) {
                    resource.succeeded();
                } else {
                    resource.failed();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, HeadConfigData> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &HeadConfigData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        let Some(parent) = data.config.data::<Mutex<ConfigState>>() else {
            return;
        };
        let mut config = parent.lock().unwrap();
        let Some(head) = config.requests.iter_mut().find(|r| r.name == data.name) else {
            return;
        };

        match request {
            zwlr_output_configuration_head_v1::Request::SetMode { mode } => {
                match mode.data::<ModeData>() {
                    // The protocol counts in mHz, the config in whole Hz.
                    Some(mode) => {
                        head.mode = Some(ModeRequest {
                            width: mode.width,
                            height: mode.height,
                            refresh: Some(mhz_to_hz(mode.refresh)),
                        })
                    }
                    None => head.unsupported = true,
                }
            }
            zwlr_output_configuration_head_v1::Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                head.mode = Some(ModeRequest {
                    width,
                    height,
                    // 0 means "any refresh rate".
                    refresh: (refresh > 0).then(|| mhz_to_hz(refresh)),
                });
            }
            zwlr_output_configuration_head_v1::Request::SetPosition { x, y } => {
                head.position = Some((x, y));
            }
            zwlr_output_configuration_head_v1::Request::SetTransform { transform } => {
                if !matches!(transform, WEnum::Value(Transform::Normal)) {
                    head.unsupported = true;
                }
            }
            zwlr_output_configuration_head_v1::Request::SetScale { scale } => {
                if (scale - 1.0).abs() > 1e-6 {
                    head.unsupported = true;
                }
            }
            _ => {}
        }
    }
}
