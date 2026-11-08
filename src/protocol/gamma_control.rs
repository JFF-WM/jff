//! wlr-gamma-control-unstable-v1: lets night-light tools (wlsunset, gammastep, redshift) set a
//! monitor's gamma curves. The hardware side is `JffState::set_client_gamma` in `app/tty`.
//!
//! One client at a time per monitor, a second one gets `failed`. While a client has control its
//! curves replace the ICC profile of the config, when it goes away the profile comes back (or the
//! table is cleared).

use std::fs::File;
use std::os::unix::fs::FileExt;

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::feature::icc::{GammaTable, Kind};
use crate::state::JffState;

pub struct GammaControl {
    /// The control that owns each monitor's curves.
    controls: Vec<(ZwlrGammaControlV1, Output)>,
    _global: Option<GlobalId>,
}

impl GammaControl {
    /// Without `supported` (winit) there is no global, so clients see no support.
    pub fn new(display: &DisplayHandle, supported: bool) -> Self {
        Self {
            controls: Vec::new(),
            _global: supported
                .then(|| display.create_global::<JffState, ZwlrGammaControlManagerV1, ()>(1, ())),
        }
    }
}

/// `output` is `None` for a control that got `failed` right away.
pub struct GammaData {
    output: Option<Output>,
    /// Entries per channel.
    size: u32,
}

impl GlobalDispatch<ZwlrGammaControlManagerV1, ()> for JffState {
    // Sandboxed apps must not see this.
    fn can_view(client: Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } = request else {
            return;
        };
        let output = Output::from_resource(&output);
        let taken = output.as_ref().is_some_and(|output| {
            let controls = &state.gamma_control.controls;
            controls
                .iter()
                .any(|(c, other)| c.is_alive() && other == output)
        });
        let size = output
            .as_ref()
            .filter(|_| !taken)
            .and_then(|output| state.output_gamma_size(output));
        match (output, size) {
            (Some(output), Some(size)) => {
                let control = data_init.init(
                    id,
                    GammaData {
                        output: Some(output.clone()),
                        size,
                    },
                );
                control.gamma_size(size);
                state.gamma_control.controls.push((control, output));
            }
            _ => data_init
                .init(
                    id,
                    GammaData {
                        output: None,
                        size: 0,
                    },
                )
                .failed(),
        }
    }
}

/// The three ramps in `file`, or `None` when it isn't exactly `3 * size` 16-bit entries.
fn read_ramps(file: &File, size: u32) -> Option<GammaTable> {
    let size = size as usize;
    let bytes = size * 3 * 2;
    if file.metadata().ok()?.len() != bytes as u64 {
        return None;
    }
    let mut raw = vec![0u8; bytes];
    file.read_exact_at(&mut raw, 0).ok()?;
    let words: Vec<u16> = raw
        .chunks_exact(2)
        .map(|b| u16::from_ne_bytes([b[0], b[1]]))
        .collect();
    Some(GammaTable {
        red: words[..size].to_vec(),
        green: words[size..2 * size].to_vec(),
        blue: words[2 * size..].to_vec(),
        kind: Kind::Client,
    })
}

impl Dispatch<ZwlrGammaControlV1, GammaData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        data: &GammaData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        let zwlr_gamma_control_v1::Request::SetGamma { fd } = request else {
            return;
        };
        let Some(output) = &data.output else {
            return;
        };
        let Some(table) = read_ramps(&File::from(fd), data.size) else {
            resource.post_error(
                zwlr_gamma_control_v1::Error::InvalidGamma,
                "the gamma table must be three ramps of gamma_size 16-bit entries",
            );
            return;
        };
        if !state.set_client_gamma(output, Some(table)) {
            resource.failed();
            state
                .gamma_control
                .controls
                .retain(|(control, _)| control != resource);
        }
    }

    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &ZwlrGammaControlV1,
        data: &GammaData,
    ) {
        let owned = state
            .gamma_control
            .controls
            .iter()
            .any(|(control, _)| control == resource);
        state
            .gamma_control
            .controls
            .retain(|(control, _)| control != resource);
        // Back to the ICC profile of the config, or no table.
        if let (true, Some(output)) = (owned, &data.output) {
            state.set_client_gamma(output, None);
        }
    }
}
