//! wlr-output-power-management-v1: lets wlopm and idle daemons switch a monitor on and off.
//! The hardware side is `JffState::set_output_power` in `app/tty`.

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::{
    zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
    zwlr_output_power_v1::{self, Mode, ZwlrOutputPowerV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use crate::state::JffState;

pub struct OutputPower {
    controls: Vec<(ZwlrOutputPowerV1, Output)>,
    _global: Option<GlobalId>,
}

impl OutputPower {
    /// Without `supported` (winit) there is no global, so clients see no support.
    pub fn new(display: &DisplayHandle, supported: bool) -> Self {
        Self {
            controls: Vec::new(),
            _global: supported
                .then(|| display.create_global::<JffState, ZwlrOutputPowerManagerV1, ()>(1, ())),
        }
    }
}

/// `None` for a control that got `failed` right away.
pub struct PowerData {
    output: Option<Output>,
}

fn to_mode(on: bool) -> Mode {
    if on {
        Mode::On
    } else {
        Mode::Off
    }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } = request else {
            return;
        };
        let output = Output::from_resource(&output);
        let taken = output.as_ref().is_some_and(|output| {
            let controls = &state.output_power.controls;
            controls
                .iter()
                .any(|(c, other)| c.is_alive() && other == output)
        });
        match output.filter(|_| !taken) {
            Some(output) => {
                let control = data_init.init(
                    id,
                    PowerData {
                        output: Some(output.clone()),
                    },
                );
                control.mode(to_mode(state.output_powered(&output)));
                state.output_power.controls.push((control, output));
            }
            None => data_init.init(id, PowerData { output: None }).failed(),
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, PowerData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        data: &PowerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        let zwlr_output_power_v1::Request::SetMode { mode } = request else {
            return;
        };
        let Some(output) = &data.output else {
            return;
        };
        let on = match mode {
            WEnum::Value(Mode::On) => true,
            WEnum::Value(Mode::Off) => false,
            _ => {
                resource.post_error(zwlr_output_power_v1::Error::InvalidMode, "unknown mode");
                return;
            }
        };
        if state.set_output_power(output, on) {
            resource.mode(to_mode(on));
        } else {
            resource.failed();
            state
                .output_power
                .controls
                .retain(|(control, _)| control != resource);
        }
    }

    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &ZwlrOutputPowerV1,
        _data: &PowerData,
    ) {
        state
            .output_power
            .controls
            .retain(|(control, _)| control != resource);
    }
}
