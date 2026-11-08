//! wlr-virtual-pointer-unstable-v1: lets a client (wlrctl, wayvnc, test and automation tools)
//! move the pointer, press buttons and scroll as if it were a mouse.
//!
//! Motion and buttons go through the same code as a real mouse: `pointer_motion_relative`,
//! `pointer_motion_to` and `pointer_button` in `input/mod.rs`, so bindings, click-to-focus,
//! pointer constraints and the hot corner all see them. Scrolling is collected until `frame`,
//! as the protocol says, and then sent in one go.
//!
//! A client that can do this controls the whole desktop, so the global is hidden from sandboxed
//! clients (see `crate::state::is_sandboxed`).
//!
//! Not kept: the timestamps clients send. Events are stamped with the time they arrive. The
//! seat argument is ignored, jff has one seat.

use std::sync::Mutex;

use smithay::backend::input::{Axis, AxisSource, ButtonState, InputTime};
use smithay::input::pointer::AxisFrame;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::{
    zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
    zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_pointer;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Point};

use crate::feature::outputs::output_rect;
use crate::primitives::layout;
use crate::state::JffState;

pub struct VirtualPointerManager {
    _global: GlobalId,
}

impl VirtualPointerManager {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            // Version 2 added `create_virtual_pointer_with_output`.
            _global: display.create_global::<JffState, ZwlrVirtualPointerManagerV1, ()>(2, ()),
        }
    }
}

/// Scrolling gathered since the last `frame`. Index 0 is vertical, 1 horizontal.
#[derive(Default)]
struct PendingAxis {
    source: Option<AxisSource>,
    value: [Option<f64>; 2],
    v120: [Option<i32>; 2],
    stop: [bool; 2],
}

/// What one virtual pointer knows.
pub struct PointerData {
    /// `create_virtual_pointer_with_output`: absolute motion is scaled to this monitor.
    output: Option<Output>,
    pending: Mutex<PendingAxis>,
}

fn axis_of(axis: WEnum<wl_pointer::Axis>) -> Option<(Axis, usize)> {
    match axis {
        WEnum::Value(wl_pointer::Axis::VerticalScroll) => Some((Axis::Vertical, 0)),
        WEnum::Value(wl_pointer::Axis::HorizontalScroll) => Some((Axis::Horizontal, 1)),
        _ => None,
    }
}

impl GlobalDispatch<ZwlrVirtualPointerManagerV1, ()> for JffState {
    // Sandboxed apps must not be able to drive the pointer.
    fn can_view(client: Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrVirtualPointerManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrVirtualPointerManagerV1,
        request: zwlr_virtual_pointer_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointer { seat: _, id } => {
                data_init.init(
                    id,
                    PointerData {
                        output: None,
                        pending: Mutex::default(),
                    },
                );
            }
            zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointerWithOutput {
                seat: _,
                output,
                id,
            } => {
                data_init.init(
                    id,
                    PointerData {
                        output: output.as_ref().and_then(Output::from_resource),
                        pending: Mutex::default(),
                    },
                );
            }
            zwlr_virtual_pointer_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwlrVirtualPointerV1, PointerData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrVirtualPointerV1,
        request: zwlr_virtual_pointer_v1::Request,
        data: &PointerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use zwlr_virtual_pointer_v1::Request;

        // Idle tools (ext-idle-notify) count this as input, like a real mouse.
        if !matches!(request, Request::Destroy) {
            state.idle_notifier_state.notify_activity(&state.seat);
        }

        match request {
            Request::Motion { time: _, dx, dy } => {
                let delta: Point<f64, Logical> = (dx, dy).into();
                state.pointer_motion_relative(delta, delta, InputTime::now());
            }

            Request::MotionAbsolute {
                time: _,
                x,
                y,
                x_extent,
                y_extent,
            } => {
                if x_extent == 0 || y_extent == 0 {
                    return;
                }
                // The area x/y are measured in: the chosen monitor, else every monitor.
                let (ax, ay, aw, ah) = match &data.output {
                    Some(output) => output_rect(output),
                    None => layout::bounds(&state.output_rects()),
                };
                let pos: Point<f64, Logical> = (
                    ax as f64 + x as f64 / x_extent as f64 * aw as f64,
                    ay as f64 + y as f64 / y_extent as f64 * ah as f64,
                )
                    .into();
                state.pointer_motion_to(pos, InputTime::now());
            }

            Request::Button {
                time: _,
                button,
                state: button_state,
            } => {
                let button_state = match button_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => ButtonState::Pressed,
                    WEnum::Value(wl_pointer::ButtonState::Released) => ButtonState::Released,
                    _ => return,
                };
                state.pointer_button(button, button_state, InputTime::now());
            }

            Request::Axis {
                time: _,
                axis,
                value,
            } => {
                let Some((_, i)) = axis_of(axis) else {
                    resource
                        .post_error(zwlr_virtual_pointer_v1::Error::InvalidAxis, "unknown axis");
                    return;
                };
                data.pending.lock().unwrap().value[i] = Some(value);
            }

            Request::AxisSource { axis_source } => {
                let source = match axis_source {
                    WEnum::Value(wl_pointer::AxisSource::Wheel) => AxisSource::Wheel,
                    WEnum::Value(wl_pointer::AxisSource::Finger) => AxisSource::Finger,
                    WEnum::Value(wl_pointer::AxisSource::Continuous) => AxisSource::Continuous,
                    WEnum::Value(wl_pointer::AxisSource::WheelTilt) => AxisSource::WheelTilt,
                    _ => {
                        resource.post_error(
                            zwlr_virtual_pointer_v1::Error::InvalidAxisSource,
                            "unknown axis source",
                        );
                        return;
                    }
                };
                data.pending.lock().unwrap().source = Some(source);
            }

            Request::AxisStop { time: _, axis } => {
                let Some((_, i)) = axis_of(axis) else {
                    resource
                        .post_error(zwlr_virtual_pointer_v1::Error::InvalidAxis, "unknown axis");
                    return;
                };
                data.pending.lock().unwrap().stop[i] = true;
            }

            Request::AxisDiscrete {
                time: _,
                axis,
                value,
                discrete,
            } => {
                let Some((_, i)) = axis_of(axis) else {
                    resource
                        .post_error(zwlr_virtual_pointer_v1::Error::InvalidAxis, "unknown axis");
                    return;
                };
                let mut pending = data.pending.lock().unwrap();
                pending.value[i] = Some(value);
                // One wheel step is 120 in the high-resolution unit.
                pending.v120[i] = Some(discrete.saturating_mul(120));
            }

            Request::Frame => {
                let pending = std::mem::take(&mut *data.pending.lock().unwrap());
                let Some(pointer) = state.seat.get_pointer() else {
                    return;
                };
                let mut frame = AxisFrame::new(InputTime::now());
                if let Some(source) = pending.source {
                    frame = frame.source(source);
                }
                let mut any = pending.source.is_some();
                for (axis, i) in [(Axis::Vertical, 0), (Axis::Horizontal, 1)] {
                    if let Some(value) = pending.value[i] {
                        frame = frame.value(axis, value);
                        if let Some(v120) = pending.v120[i] {
                            frame = frame.v120(axis, v120);
                        }
                        any = true;
                    }
                    if pending.stop[i] {
                        frame = frame.stop(axis);
                        any = true;
                    }
                }
                if any {
                    pointer.axis(state, frame);
                }
                pointer.frame(state);
            }

            Request::Destroy => {}
            _ => {}
        }
    }
}
