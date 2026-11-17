//! `xx-image-capture-color-management-v1`: what the colors of a capture are, and a way for a
//! client to say what it wants a frame in. It extends `ext-image-capture-source-v1` and
//! `ext-image-copy-capture-v1` (`protocol/image_copy.rs`) with the image descriptions of
//! `wp-color-management-v1` (`protocol/color_management.rs`, version 3 of it: the protocol hands
//! out `wp_image_description_reference_v1` objects).
//!
//! What jff says about a capture is in `primitives/capture_color.rs`: the 8-bit formats hold
//! plain sRGB, and the 10-bit formats of a monitor in HDR mode hold PQ in BT.2020. So:
//!
//! - `get_preferred` (and `preferred_changed`) say sRGB for a window and for a monitor that is
//!   not in HDR mode, and the description of the monitor for one that is, the same description
//!   (and so the same identity) that `wp_color_management_output_v1` gives for it. The
//!   preferred description follows the monitor going into and out of HDR mode
//!   ([`JffState::refresh_capture_colors`]).
//! - `set_frame_image_description` is kept for the frame, and checked when the frame is drawn
//!   (`image_copy.rs`): nothing converts a picture to another description, so a frame whose
//!   description is not what its buffer is drawn as fails (`unknown`) instead of being
//!   mislabeled. A frame with no description is what it always was.
//!
//! The global is hidden from sandboxed clients like the capture globals it extends, and it
//! follows `color-management { off }` like `wp_color_manager_v1`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::Frame;

use super::color_management::{init_reference, ready_description, Described};
use super::xx_image_capture_color_management_bindings::server::xx_image_capture_color_manager_v1::{
    self, XxImageCaptureColorManagerV1,
};
use super::xx_image_capture_color_management_bindings::server::xx_image_capture_source_colors_v1::{
    self, XxImageCaptureSourceColorsV1,
};
use crate::primitives::color::split_identity;
use crate::state::{is_sandboxed, JffState};

/// What a frame was given with `set_frame_image_description`, and whether it was captured.
struct FrameColor {
    object: ExtImageCopyCaptureFrameV1,
    /// How Smithay's `Frame` of this object is recognised, see [`frame_key`].
    key: String,
    described: Option<Arc<Described>>,
    /// `capture` was sent: the description can't be set any more.
    captured: bool,
}

/// The global and what jff keeps for the objects of the protocol.
pub struct XxImageCaptureColor {
    /// `None` while color management is off.
    global: Option<GlobalId>,
    frames: Vec<FrameColor>,
    /// The `xx_image_capture_source_colors_v1` objects, told when the preferred description changes.
    sources: Vec<XxImageCaptureSourceColorsV1>,
}

impl XxImageCaptureColor {
    /// `enabled` follows `color-management { off }`: the protocol is about image descriptions, a
    /// client that can't create any has no use for it.
    pub fn new(display: &DisplayHandle, enabled: bool) -> Self {
        Self {
            global: enabled.then(|| {
                display.create_global::<JffState, XxImageCaptureColorManagerV1, ()>(1, ())
            }),
            frames: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Create or remove the global after a config reload. Objects clients already hold stay valid.
    pub fn set_enabled(&mut self, display: &DisplayHandle, enabled: bool) {
        match (enabled, self.global.is_some()) {
            (true, false) => {
                self.global = Some(
                    display.create_global::<JffState, XxImageCaptureColorManagerV1, ()>(1, ()),
                );
            }
            (false, true) => {
                if let Some(global) = self.global.take() {
                    display.remove_global::<JffState>(global);
                }
            }
            _ => {}
        }
    }

    /// The description a client gave `frame`, `None` when it gave none.
    pub fn description_of(&self, frame: &Frame) -> Option<Arc<Described>> {
        let key = frame_key(frame)?;
        self.frames
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.described.clone())
    }

    /// `frame` was captured: from now on its description can't be set any more. (A frame that
    /// was never described has no entry, and a description set on it after the capture is
    /// accepted: the protocol forbids that, but the only harm is a description nobody reads.)
    pub fn frame_captured(&mut self, frame: &Frame) {
        self.frames.retain(|entry| entry.object.is_alive());
        let Some(key) = frame_key(frame) else {
            return;
        };
        if let Some(entry) = self.frames.iter_mut().find(|entry| entry.key == key) {
            entry.captured = true;
        }
    }
}

/// What tells the protocol object of a frame apart, as text: its `Debug` form.
fn object_key(object: &ExtImageCopyCaptureFrameV1) -> String {
    format!("{object:?}")
}

/// The [`object_key`] of the protocol object behind a frame Smithay hands to the compositor.
///
/// Smithay keeps that object in a private field of `FrameRef` and has no way to get at it, but
/// `FrameRef` derives `Debug` (`FrameRef { obj: <object>, inner: .. }`), which prints it. This is
/// the one place that depends on that. A `pub fn resource(&self) -> &ExtImageCopyCaptureFrameV1`
/// on Smithay's `FrameRef` would replace it.
fn frame_key(frame: &Frame) -> Option<String> {
    let text = format!("{:?}", &**frame);
    let rest = text.strip_prefix("FrameRef { obj: ")?;
    let (object, _) = rest.split_once(", inner:")?;
    Some(object.to_string())
}

/// One `xx_image_capture_source_colors_v1`.
pub struct SourceColors {
    /// `None` when the `ext_image_capture_source_v1` was not one of ours: the object is inert.
    source: Option<ImageCaptureSource>,
    /// The identity of the preferred description the client was last told of.
    told: AtomicU64,
}

impl JffState {
    /// Tell the clients that hold a source's colors that its preferred description changed: a
    /// monitor went into or out of HDR mode. Cheap, runs after every round of requests and
    /// every pass of the event loop.
    pub fn refresh_capture_colors(&mut self) {
        self.xx_capture_color.sources.retain(|object| object.is_alive());
        for object in &self.xx_capture_color.sources {
            let Some(data) = object.data::<SourceColors>() else {
                continue;
            };
            let Some(source) = data.source.as_ref() else {
                continue;
            };
            // Gone with its monitor or window: the object is inert, nothing more is said.
            let Some(described) = self.capture_source_preferred(source) else {
                continue;
            };
            if data.told.swap(described.identity, Ordering::SeqCst) != described.identity {
                let (hi, lo) = split_identity(described.identity);
                object.preferred_changed(hi, lo);
            }
        }
    }

    /// What a capture of `source` is, as far as an inert object is concerned: sRGB.
    fn inert_capture_preferred(&self) -> Arc<Described> {
        self.color_management.capture_preferred(None)
    }
}

impl GlobalDispatch<XxImageCaptureColorManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XxImageCaptureColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }

    // Like the capture globals it extends.
    fn can_view(client: Client, _global_data: &()) -> bool {
        !is_sandboxed(&client)
    }
}

impl Dispatch<XxImageCaptureColorManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XxImageCaptureColorManagerV1,
        request: xx_image_capture_color_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use xx_image_capture_color_manager_v1::{Error, Request};

        match request {
            Request::Destroy => {}
            Request::GetCaptureSourceColors { colors, source } => {
                let source = ImageCaptureSource::from_resource(&source);
                let told = source
                    .as_ref()
                    .and_then(|source| state.capture_source_preferred(source))
                    .unwrap_or_else(|| state.inert_capture_preferred())
                    .identity;
                let object = data_init.init(
                    colors,
                    SourceColors {
                        source,
                        told: AtomicU64::new(told),
                    },
                );
                state.xx_capture_color.sources.retain(|o| o.is_alive());
                state.xx_capture_color.sources.push(object);
            }
            Request::SetFrameImageDescription {
                frame,
                image_description,
            } => {
                let Some(described) = ready_description(&image_description) else {
                    resource.post_error(
                        Error::ImageDescriptionNotReady,
                        "the image description is not ready",
                    );
                    return;
                };
                let colors = &mut state.xx_capture_color;
                colors.frames.retain(|entry| entry.object.is_alive());
                let key = object_key(&frame);
                match colors.frames.iter_mut().find(|entry| entry.key == key) {
                    Some(entry) if entry.captured => resource.post_error(
                        Error::AlreadyCaptured,
                        "the frame has already been captured",
                    ),
                    Some(entry) => entry.described = Some(described),
                    None => colors.frames.push(FrameColor {
                        object: frame,
                        key,
                        described: Some(described),
                        captured: false,
                    }),
                }
            }
        }
    }
}

impl Dispatch<XxImageCaptureSourceColorsV1, SourceColors> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &XxImageCaptureSourceColorsV1,
        request: xx_image_capture_source_colors_v1::Request,
        data: &SourceColors,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use xx_image_capture_source_colors_v1::Request;

        match request {
            Request::Destroy => {}
            Request::GetPreferred { id } => {
                let described = data
                    .source
                    .as_ref()
                    .and_then(|source| state.capture_source_preferred(source))
                    .unwrap_or_else(|| state.inert_capture_preferred());
                // The client knows the current one now: `preferred_changed` is for what comes after.
                data.told.store(described.identity, Ordering::SeqCst);
                // The protocol says `get_information` works on it.
                init_reference(data_init, id, described, true);
            }
        }
    }
}
