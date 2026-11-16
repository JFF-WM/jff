//! `input { … }` applied to the libinput devices of the TTY backend.

use std::fmt::Debug;

use smithay::reexports::input as li;

use crate::config::{
    AccelProfile, ClickMethod, DeviceKind, DeviceSettings, ScrollMethod, TapButtonMap,
};
use crate::state::JffState;

impl JffState {
    /// A device appeared: remember it for reloads and set it up from the config.
    pub fn input_device_added(&mut self, mut device: li::Device) {
        // Not set up here, but its name is what `touchscreen \"NAME\"` in the config matches.
        if device.has_capability(li::DeviceCapability::Touch) {
            tracing::info!("input device {:?} is a touchscreen", device.name());
        }
        let Some(kind) = device_kind(&device) else {
            return;
        };
        tracing::info!("input device {:?} is a {kind:?}", device.name());
        self.apply_device_settings(&mut device, kind);
        self.input_devices.push(device);
    }

    /// A device went away.
    pub fn input_device_removed(&mut self, device: &li::Device) {
        self.input_devices
            .retain(|known| known.sysname() != device.sysname());
    }

    /// Set every known device up again, after a config reload.
    pub fn apply_input_config(&mut self) {
        let mut devices = std::mem::take(&mut self.input_devices);
        for device in &mut devices {
            if let Some(kind) = device_kind(device) {
                self.apply_device_settings(device, kind);
            }
        }
        self.input_devices = devices;
    }

    /// Put one device in the state the config asks for. What the config doesn't mention goes
    /// back to the device's own default, so a setting taken out of the file is undone.
    fn apply_device_settings(&self, device: &mut li::Device, kind: DeviceKind) {
        let settings = self.config.input().settings_for(&device.name(), kind);
        apply(device, &settings);
    }
}

/// A touchpad, a mouse (anything else that moves the pointer), or `None` for a keyboard, a
/// touchscreen, a pen.
fn device_kind(device: &li::Device) -> Option<DeviceKind> {
    if !device.has_capability(li::DeviceCapability::Pointer) {
        return None;
    }
    Some(if device.config_tap_finger_count() > 0 {
        DeviceKind::Touchpad
    } else {
        DeviceKind::Mouse
    })
}

/// Log a refused setting. A setting the config didn't ask for is refused on every device that
/// lacks the feature, which is nothing to report.
fn report<E: Debug>(device: &str, setting: &str, asked: bool, result: Result<(), E>) {
    if let Err(err) = result {
        if asked {
            tracing::warn!("input device {device:?}: {setting} not applied: {err:?}");
        } else {
            tracing::debug!("input device {device:?}: {setting} not applied: {err:?}");
        }
    }
}

fn apply(device: &mut li::Device, s: &DeviceSettings) {
    let name = device.name().into_owned();
    let name = name.as_str();

    let value = s.tap.unwrap_or_else(|| device.config_tap_default_enabled());
    report(
        name,
        "tap",
        s.tap.is_some(),
        device.config_tap_set_enabled(value),
    );

    if let Some(value) = s
        .tap_button_map
        .map(tap_button_map)
        .or_else(|| device.config_tap_default_button_map())
    {
        report(
            name,
            "tap-button-map",
            s.tap_button_map.is_some(),
            device.config_tap_set_button_map(value),
        );
    }

    let value = s
        .drag
        .unwrap_or_else(|| device.config_tap_default_drag_enabled());
    report(
        name,
        "drag",
        s.drag.is_some(),
        device.config_tap_set_drag_enabled(value),
    );

    let value = s
        .natural_scroll
        .unwrap_or_else(|| device.config_scroll_default_natural_scroll_enabled());
    report(
        name,
        "natural-scroll",
        s.natural_scroll.is_some(),
        device.config_scroll_set_natural_scroll_enabled(value),
    );

    let value = s
        .accel_speed
        .unwrap_or_else(|| device.config_accel_default_speed());
    report(
        name,
        "accel-speed",
        s.accel_speed.is_some(),
        device.config_accel_set_speed(value),
    );

    if let Some(value) = s
        .accel_profile
        .map(accel_profile)
        .or_else(|| device.config_accel_default_profile())
    {
        report(
            name,
            "accel-profile",
            s.accel_profile.is_some(),
            device.config_accel_set_profile(value),
        );
    }

    if let Some(value) = s
        .scroll_method
        .map(scroll_method)
        .or_else(|| device.config_scroll_default_method())
    {
        report(
            name,
            "scroll-method",
            s.scroll_method.is_some(),
            device.config_scroll_set_method(value),
        );
    }

    let value = s
        .scroll_button
        .unwrap_or_else(|| device.config_scroll_default_button());
    report(
        name,
        "scroll-button",
        s.scroll_button.is_some(),
        device.config_scroll_set_button(value),
    );

    let value = s
        .disable_while_typing
        .unwrap_or_else(|| device.config_dwt_default_enabled());
    report(
        name,
        "disable-while-typing",
        s.disable_while_typing.is_some(),
        device.config_dwt_set_enabled(value),
    );

    if let Some(value) = s
        .click_method
        .map(click_method)
        .or_else(|| device.config_click_default_method())
    {
        report(
            name,
            "click-method",
            s.click_method.is_some(),
            device.config_click_set_method(value),
        );
    }

    let value = s
        .middle_emulation
        .unwrap_or_else(|| device.config_middle_emulation_default_enabled());
    report(
        name,
        "middle-emulation",
        s.middle_emulation.is_some(),
        device.config_middle_emulation_set_enabled(value),
    );

    let value = s
        .left_handed
        .unwrap_or_else(|| device.config_left_handed_default());
    report(
        name,
        "left-handed",
        s.left_handed.is_some(),
        device.config_left_handed_set(value),
    );

    let asked = s.off.is_some() || s.disable_on_external_mouse.is_some();
    let mode = if s.off == Some(true) {
        li::SendEventsMode::DISABLED
    } else if s.disable_on_external_mouse == Some(true) {
        li::SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE
    } else {
        // Enabled, which is also every device's default.
        li::SendEventsMode::empty()
    };
    report(name, "off", asked, device.config_send_events_set_mode(mode));
}

fn tap_button_map(map: TapButtonMap) -> li::TapButtonMap {
    match map {
        TapButtonMap::LeftRightMiddle => li::TapButtonMap::LeftRightMiddle,
        TapButtonMap::LeftMiddleRight => li::TapButtonMap::LeftMiddleRight,
    }
}

fn accel_profile(profile: AccelProfile) -> li::AccelProfile {
    match profile {
        AccelProfile::Flat => li::AccelProfile::Flat,
        AccelProfile::Adaptive => li::AccelProfile::Adaptive,
    }
}

fn scroll_method(method: ScrollMethod) -> li::ScrollMethod {
    match method {
        ScrollMethod::None => li::ScrollMethod::NoScroll,
        ScrollMethod::TwoFinger => li::ScrollMethod::TwoFinger,
        ScrollMethod::Edge => li::ScrollMethod::Edge,
        ScrollMethod::OnButtonDown => li::ScrollMethod::OnButtonDown,
    }
}

fn click_method(method: ClickMethod) -> li::ClickMethod {
    match method {
        ClickMethod::ButtonAreas => li::ClickMethod::ButtonAreas,
        ClickMethod::Clickfinger => li::ClickMethod::Clickfinger,
    }
}
