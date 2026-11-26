//! ext-transient-output-v1: a privileged client (wayvnc, remote desktop tools) asks for a
//! virtual monitor, to capture it with image-copy-capture. The monitor goes away when the
//! client destroys the transient output (or goes away itself).
//!
//! A transient output is an ordinary jff output with no scanout: windows are placed on it and
//! it is laid out beside the others, captures draw it offscreen. Without a vblank to wait for,
//! a timer stands in for one: it starts a frame cycle, sends frame callbacks and tells
//! screen capture that the output was drawn. It only runs while a transient output exists.
//!
//! The global is hidden from sandboxed clients: a client that can make monitors can see
//! everything on them.

use std::sync::Mutex;
use std::time::Duration;

use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::Transform;

use super::ext_transient_output_bindings::server::{
    ext_transient_output_manager_v1::{self, ExtTransientOutputManagerV1},
    ext_transient_output_v1::{self, ExtTransientOutputV1},
};
use crate::state::JffState;

/// How many transient outputs may exist at once. More are denied.
const MAX_TRANSIENT_OUTPUTS: usize = 16;
/// Size when the client doesn't say.
pub const DEFAULT_SIZE: (i32, i32) = (1920, 1080);
/// Largest side of a buffer accepted.
pub const MAX_SIDE: i32 = 16384;
/// Refresh rate of a transient output, in mHz.
const REFRESH_MHZ: i32 = 60_000;
/// Time between the frame cycles.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

pub struct TransientOutputs {
    _global: GlobalId,
    /// Number of the next output, for its name.
    next: u32,
    /// Outputs that exist (granted, not destroyed yet).
    outputs: Vec<Output>,
    /// The ones `jff msg output create` made: no client owns them, only `output remove` (or
    /// jff quitting) ends them.
    manual: Vec<Output>,
    /// The stand-in for the vblank is running.
    timer_armed: bool,
}

impl TransientOutputs {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, ExtTransientOutputManagerV1, ()>(1, ()),
            next: 0,
            outputs: Vec::new(),
            manual: Vec::new(),
            timer_armed: false,
        }
    }
}

/// What a transient output object knows.
#[derive(Default)]
pub struct TransientOutputData {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    // Double-buffered state, applied by `commit`.
    app_id: Option<String>,
    sink_tag: Option<String>,
    description: Option<String>,
    size: Option<(i32, i32)>,
    /// `None` until the first commit was granted, and after it is gone.
    output: Option<Output>,
    /// The first commit was answered (created or denied).
    answered: bool,
}

impl JffState {
    /// Run the frame cycles of transient outputs, as long as there are any.
    fn arm_transient_output_timer(&mut self) {
        if self.transient_outputs.timer_armed {
            return;
        }
        self.transient_outputs.timer_armed = true;
        let armed =
            self.loop_handle
                .insert_source(Timer::from_duration(FRAME_INTERVAL), |_, _, state| {
                    if state.transient_outputs.outputs.is_empty() {
                        state.transient_outputs.timer_armed = false;
                        return TimeoutAction::Drop;
                    }
                    for output in state.transient_outputs.outputs.clone() {
                        crate::app::pacing::advance_frame_sequence(&output);
                        state.send_frame_callbacks(&output);
                        state.image_copy.output_drawn(&output);
                        state.wlr_screencopy.output_drawn(&output);
                    }
                    TimeoutAction::ToDuration(FRAME_INTERVAL)
                });
        if armed.is_err() {
            self.transient_outputs.timer_armed = false;
        }
    }

    /// Make a transient output of `size`: an output like any other, without a monitor behind
    /// it. Shared by the protocol and `jff msg output create`. `model` becomes the output's
    /// description (smithay builds that from make and model, and it can't be changed
    /// afterwards).
    fn create_transient_output(
        &mut self,
        size: (i32, i32),
        model: String,
        serial: String,
    ) -> Result<Output, String> {
        if self.transient_outputs.outputs.len() >= MAX_TRANSIENT_OUTPUTS {
            return Err(format!(
                "there are {MAX_TRANSIENT_OUTPUTS} transient outputs already"
            ));
        }
        let number = self.transient_outputs.next;
        self.transient_outputs.next += 1;
        let output = Output::new(
            format!("TRANSIENT-{number}"),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "jff".into(),
                model,
                serial_number: serial,
            },
        );
        let mode = mode_of(size);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);

        self.add_output(output.clone());
        self.transient_outputs.outputs.push(output.clone());
        self.arm_transient_output_timer();
        Ok(output)
    }

    /// End a transient output, whoever made it.
    fn destroy_transient_output(&mut self, output: &Output) {
        self.transient_outputs.outputs.retain(|o| o != output);
        self.transient_outputs.manual.retain(|o| o != output);
        self.remove_output(output);
    }

    fn drop_transient_output(&mut self, data: &TransientOutputData) {
        let Some(output) = data.inner.lock().unwrap().output.take() else {
            return;
        };
        self.destroy_transient_output(&output);
    }

    /// `jff msg output create [WxH]`: a transient output that stays until `output remove`.
    pub fn create_manual_transient_output(
        &mut self,
        size: Option<(i32, i32)>,
    ) -> Result<Output, String> {
        let size = size.unwrap_or(DEFAULT_SIZE);
        if size.0 <= 0 || size.1 <= 0 || size.0 > MAX_SIDE || size.1 > MAX_SIDE {
            return Err(format!(
                "{}x{} is not a size, each side is 1 to {MAX_SIDE}",
                size.0, size.1
            ));
        }
        let output = self.create_transient_output(size, "transient".into(), "Unknown".into())?;
        self.transient_outputs.manual.push(output.clone());
        Ok(output)
    }

    /// `jff msg output remove NAME`: only outputs `output create` made, an output a client
    /// asked for goes away when that client lets go of it.
    pub fn remove_manual_transient_output(&mut self, name: &str) -> Result<(), String> {
        let Some(output) = self
            .transient_outputs
            .manual
            .iter()
            .find(|o| o.name() == name)
            .cloned()
        else {
            return Err(format!(
                "{name:?} is not an output made with `output create`"
            ));
        };
        self.destroy_transient_output(&output);
        Ok(())
    }
}

fn mode_of(size: (i32, i32)) -> Mode {
    Mode {
        size: size.into(),
        refresh: REFRESH_MHZ,
    }
}

impl GlobalDispatch<ExtTransientOutputManagerV1, ()> for JffState {
    fn can_view(client: Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtTransientOutputManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtTransientOutputManagerV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ExtTransientOutputManagerV1,
        request: ext_transient_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        if let ext_transient_output_manager_v1::Request::Create { transient_output } = request {
            data_init.init(transient_output, TransientOutputData::default());
        }
        // `destroy` is the only other request.
    }
}

impl Dispatch<ExtTransientOutputV1, TransientOutputData> for JffState {
    fn request(
        state: &mut JffState,
        client: &Client,
        resource: &ExtTransientOutputV1,
        request: ext_transient_output_v1::Request,
        data: &TransientOutputData,
        display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            ext_transient_output_v1::Request::SetAppId { app_id } => {
                data.inner.lock().unwrap().app_id = Some(app_id);
            }
            ext_transient_output_v1::Request::SetSinkTag { sink_tag } => {
                data.inner.lock().unwrap().sink_tag = Some(sink_tag);
            }
            ext_transient_output_v1::Request::SetDescription { description } => {
                data.inner.lock().unwrap().description = Some(description);
            }
            ext_transient_output_v1::Request::SetBufferSize { width, height } => {
                if width <= 0 || height <= 0 || width > MAX_SIDE || height > MAX_SIDE {
                    resource.post_error(
                        ext_transient_output_v1::Error::InvalidSize,
                        format!("invalid buffer size {width}x{height}"),
                    );
                    return;
                }
                data.inner.lock().unwrap().size = Some((width, height));
            }
            ext_transient_output_v1::Request::Commit => {
                let first = !data.inner.lock().unwrap().answered;
                if first {
                    commit_first(state, client, resource, data, display);
                } else {
                    commit_again(state, data);
                }
            }
            ext_transient_output_v1::Request::Destroy => {}
        }
    }

    // Also runs when the client goes away without destroying it.
    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        _resource: &ExtTransientOutputV1,
        data: &TransientOutputData,
    ) {
        state.drop_transient_output(data);
    }
}

/// The first commit: make the output, or say no.
fn commit_first(
    state: &mut JffState,
    client: &Client,
    resource: &ExtTransientOutputV1,
    data: &TransientOutputData,
    display: &DisplayHandle,
) {
    let (size, app_id, sink_tag, description) = {
        let mut inner = data.inner.lock().unwrap();
        inner.answered = true;
        (
            inner.size.unwrap_or(DEFAULT_SIZE),
            inner.app_id.clone(),
            inner.sink_tag.clone(),
            inner.description.clone(),
        )
    };
    if state.transient_outputs.outputs.len() >= MAX_TRANSIENT_OUTPUTS {
        resource.denied();
        return;
    }

    let number = state.transient_outputs.next;
    state.transient_outputs.next += 1;
    let name = format!("TRANSIENT-{number}");
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "jff".into(),
            // Smithay builds the output's description from make and model, and it can't be
            // changed afterwards: the client's description goes in here.
            model: description.or(app_id).unwrap_or_else(|| "transient".into()),
            serial_number: sink_tag.unwrap_or_else(|| "Unknown".into()),
        },
    );
    let mode = mode_of(size);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    state.add_output(output.clone());

    // The client needs the registry name of the new wl_output global.
    let global_name = state
        .output_globals
        .get(&name)
        .cloned()
        .and_then(|global| display.backend_handle().global_name(global, client.id()));
    let Some(global_name) = global_name else {
        state.remove_output(&output);
        resource.denied();
        return;
    };

    state.transient_outputs.outputs.push(output.clone());
    data.inner.lock().unwrap().output = Some(output);
    state.arm_transient_output_timer();
    resource.created(global_name);
}

/// A later commit: the size may have changed (app id, sink tag and description are fixed at
/// creation, the output's properties can't be changed).
fn commit_again(state: &mut JffState, data: &TransientOutputData) {
    let (output, size) = {
        let inner = data.inner.lock().unwrap();
        let Some(output) = inner.output.clone() else {
            return;
        };
        (output, inner.size)
    };
    if let Some(size) = size {
        let mode = mode_of(size);
        if output.current_mode() != Some(mode) {
            output.change_current_state(Some(mode), None, None, None);
            output.set_preferred(mode);
            state.relayout_outputs();
        }
    }
}
