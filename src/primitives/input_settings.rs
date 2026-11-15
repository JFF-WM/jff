//! Pointer device settings (`input { … }`) as plain values, and how they combine.

/// What kind of pointer device a setting block is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// A touchpad.
    Touchpad,
    /// A mouse, trackball, pointing stick or anything else that moves the pointer.
    Mouse,
}

/// Which finger count taps as which button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapButtonMap {
    /// One finger left, two right, three middle.
    LeftRightMiddle,
    /// One finger left, two middle, three right.
    LeftMiddleRight,
}

/// How the pointer speed follows the hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelProfile {
    /// The same distance at any speed.
    Flat,
    /// Faster hand, longer way.
    Adaptive,
}

/// How a device scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollMethod {
    /// No scrolling from this device.
    None,
    /// Two fingers on a touchpad.
    TwoFinger,
    /// One finger along the edge of a touchpad.
    Edge,
    /// Moving the device while a button is held, see `scroll-button`.
    OnButtonDown,
}

/// How a touchpad produces a click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickMethod {
    /// Pressing in the bottom left or right area.
    ButtonAreas,
    /// One, two or three fingers pressing.
    Clickfinger,
}

/// Settings for one device. `None` leaves a setting at what the device has by default.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeviceSettings {
    /// The device sends nothing at all.
    pub off: Option<bool>,
    /// The device sends nothing while an external mouse is plugged in.
    pub disable_on_external_mouse: Option<bool>,
    /// Tapping clicks.
    pub tap: Option<bool>,
    /// Which finger count taps as which button.
    pub tap_button_map: Option<TapButtonMap>,
    /// Tap, then touch and move to drag.
    pub drag: Option<bool>,
    /// Content follows the fingers.
    pub natural_scroll: Option<bool>,
    /// Pointer speed, -1.0 (slowest) to 1.0 (fastest).
    pub accel_speed: Option<f64>,
    /// How the speed follows the hand.
    pub accel_profile: Option<AccelProfile>,
    /// How the device scrolls.
    pub scroll_method: Option<ScrollMethod>,
    /// Button held for `on-button-down` scrolling, an evdev code.
    pub scroll_button: Option<u32>,
    /// A touchpad ignores touches while typing.
    pub disable_while_typing: Option<bool>,
    /// How a touchpad produces a click.
    pub click_method: Option<ClickMethod>,
    /// Both buttons together are a middle click.
    pub middle_emulation: Option<bool>,
    /// Left and right buttons swapped.
    pub left_handed: Option<bool>,
}

/// The pointer speed range libinput accepts.
pub const ACCEL_SPEED_RANGE: std::ops::RangeInclusive<f64> = -1.0..=1.0;

impl DeviceSettings {
    /// What jff has always done for a device of this kind, before any `input` block: taps click
    /// on a touchpad, everything else is the device's default.
    pub fn builtin(kind: DeviceKind) -> Self {
        Self {
            tap: (kind == DeviceKind::Touchpad).then_some(true),
            ..Self::default()
        }
    }

    /// These settings with every one `over` sets replaced by its value.
    #[must_use]
    pub fn overridden_by(self, over: &Self) -> Self {
        Self {
            off: over.off.or(self.off),
            disable_on_external_mouse: over
                .disable_on_external_mouse
                .or(self.disable_on_external_mouse),
            tap: over.tap.or(self.tap),
            tap_button_map: over.tap_button_map.or(self.tap_button_map),
            drag: over.drag.or(self.drag),
            natural_scroll: over.natural_scroll.or(self.natural_scroll),
            accel_speed: over.accel_speed.or(self.accel_speed),
            accel_profile: over.accel_profile.or(self.accel_profile),
            scroll_method: over.scroll_method.or(self.scroll_method),
            scroll_button: over.scroll_button.or(self.scroll_button),
            disable_while_typing: over.disable_while_typing.or(self.disable_while_typing),
            click_method: over.click_method.or(self.click_method),
            middle_emulation: over.middle_emulation.or(self.middle_emulation),
            left_handed: over.left_handed.or(self.left_handed),
        }
    }
}

/// The evdev code of a mouse button by name (`"middle"`), `None` for a name jff doesn't know.
pub fn button_code(name: &str) -> Option<u32> {
    Some(match name {
        "left" => 0x110,
        "right" => 0x111,
        "middle" => 0x112,
        "side" => 0x113,
        "extra" => 0x114,
        "forward" => 0x115,
        "back" => 0x116,
        _ => return None,
    })
}
