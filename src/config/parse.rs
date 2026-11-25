//! Parsers for each KDL node: turn a node into the config type it stands for, or an
//! error that says what is wrong with it.

use super::*;

/// A boolean setting: absent → `default`, bare name → on, otherwise
/// `true` / `false` (or `"true"` / `"false"`, `"yes"` / `"no"`, `"on"` / `"off"`).
pub(super) fn parse_flag(doc: &KdlDocument, name: &str, default: bool) -> anyhow::Result<bool> {
    match doc.get(name) {
        None => Ok(default),
        Some(node) => flag_value(node),
    }
}

pub(super) fn flag_value(node: &KdlNode) -> anyhow::Result<bool> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [] => Ok(true),
        [value] => value
            .as_bool()
            .or_else(|| match value.as_string() {
                Some("true" | "yes" | "on") => Some(true),
                Some("false" | "no" | "off") => Some(false),
                _ => None,
            })
            .with_context(|| format!("{name} takes true or false")),
        _ => bail!("{name} takes one value: true or false"),
    }
}

/// Positional arguments of `node`, excluding `key=value` properties.
pub(super) fn positional(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| entry.value())
        .collect()
}

/// One string argument, for settings like `layout "us"`.
pub(super) fn string_arg(node: &KdlNode) -> anyhow::Result<String> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_string()
            .map(str::to_owned)
            .with_context(|| format!("{name} takes a string")),
        _ => bail!("{name} takes one string"),
    }
}

/// One integer argument in `0..=max`.
pub(super) fn int_arg(node: &KdlNode, max: i32) -> anyhow::Result<i32> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_integer()
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (0..=max).contains(n))
            .with_context(|| format!("{name} takes a number from 0 to {max}")),
        _ => bail!("{name} takes one number from 0 to {max}"),
    }
}

/// The arguments of `layer-radius`: `N`, or `"NAMESPACE" N`, with N in `0..=MAX_RADIUS`.
pub(super) fn layer_radius_arg(node: &KdlNode) -> anyhow::Result<(Option<String>, i32)> {
    let name = node.name().value();
    let radius = |value: &KdlValue| {
        value
            .as_integer()
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (0..=MAX_RADIUS).contains(n))
            .with_context(|| format!("{name} takes a radius from 0 to {MAX_RADIUS}"))
    };
    match positional(node).as_slice() {
        [value] => Ok((None, radius(value)?)),
        [namespace, value] => {
            let namespace = namespace
                .as_string()
                .map(str::to_owned)
                .with_context(|| format!("{name} takes a namespace string, then a radius"))?;
            Ok((Some(namespace), radius(value)?))
        }
        _ => bail!(
            "{name} takes a radius, or a namespace and a radius: {name} \"JFShell-Bottom\" 16"
        ),
    }
}

/// One integer argument in `range`, a luminance in cd/m² like `sdr-white 203`.
pub(super) fn nits_arg(
    node: &KdlNode,
    range: &RangeInclusive<u32>,
    example: u32,
) -> anyhow::Result<u32> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_integer()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| range.contains(n))
            .with_context(|| {
                format!(
                    "{name} takes cd/m² from {} to {}: {name} {example}",
                    range.start(),
                    range.end()
                )
            }),
        _ => bail!(
            "{name} takes one value in cd/m² from {} to {}: {name} {example}",
            range.start(),
            range.end()
        ),
    }
}

/// How many single-letter changes it takes to turn `a` into `b`.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The known name that `name` looks like a typo of, if there is one.
pub fn closest<'a>(name: &str, options: &[&'a str]) -> Option<&'a str> {
    options
        .iter()
        .map(|option| (edit_distance(name, option), *option))
        .filter(|(distance, _)| *distance <= 2 && *distance < name.chars().count())
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, option)| option)
}

/// `period-ms`: a number of milliseconds, or `"none"` for 0 (standing still).
pub(super) fn period_arg(node: &KdlNode) -> anyhow::Result<i32> {
    if let [value] = positional(node).as_slice() {
        if value.as_string() == Some("none") {
            return Ok(0);
        }
    }
    int_arg(node, MAX_BORDER_PERIOD_MS)
}

/// A colour written as `"#rgb"`, `"#rgba"`, `"#rrggbb"` or `"#rrggbbaa"`, as straight RGBA.
pub fn parse_color(text: &str) -> anyhow::Result<[f32; 4]> {
    let hex = text
        .strip_prefix('#')
        .with_context(|| format!("colour {text:?} must start with #"))?;
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<_>>()
        .with_context(|| format!("colour {text:?} has a character that isn't a hex digit"))?;
    let [r, g, b, a] = match digits.as_slice() {
        [r, g, b] => [r * 17, g * 17, b * 17, 255],
        [r, g, b, a] => [r * 17, g * 17, b * 17, a * 17],
        [r1, r2, g1, g2, b1, b2] => [r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2, 255],
        [r1, r2, g1, g2, b1, b2, a1, a2] => {
            [r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2, a1 * 16 + a2]
        }
        _ => bail!("colour {text:?} must have 3, 4, 6 or 8 hex digits"),
    };
    Ok([r, g, b, a].map(|v| f32::from(v) / 255.0))
}

/// The `border { … }` block. Having the block at all turns borders on (2 px unless `width`
/// says otherwise, `width 0` turns them off again).
pub(super) fn parse_border(node: &KdlNode) -> anyhow::Result<Border> {
    let mut config = Border {
        width: DEFAULT_BORDER_WIDTH,
        ..Border::default()
    };
    let mut width_given = false;
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "width" => {
                config.width = int_arg(child, MAX_BORDER_WIDTH)?;
                width_given = true;
            }
            "look" => {
                config.look = match string_arg(child)?.as_str() {
                    "plain" => BorderLook::Plain,
                    "paper" => BorderLook::Paper,
                    other => bail!("look takes \"plain\" or \"paper\", not {other:?}"),
                }
            }
            "color" => config.color = parse_color(&string_arg(child)?)?,
            "focus-color" => config.focus_color = parse_color(&string_arg(child)?)?,
            "fade-ms" => config.fade_ms = int_arg(child, MAX_BORDER_FADE_MS)?,
            "focus-color-2" => config.focus_color_2 = Some(parse_color(&string_arg(child)?)?),
            "animate" => {
                config.animate = match string_arg(child)?.as_str() {
                    "none" => BorderAnimation::None,
                    "spin" => BorderAnimation::Spin,
                    "sweep" => BorderAnimation::Sweep,
                    "smoke" => BorderAnimation::Smoke,
                    "neon" => BorderAnimation::Neon,
                    "aurora" => BorderAnimation::Aurora,
                    other => bail!(
                        "animate takes \"none\", \"spin\", \"sweep\", \"smoke\", \"neon\" or \"aurora\", not {other:?}"
                    ),
                }
            }
            "period-ms" => config.period_ms = period_arg(child)?,
            other => bail!("unknown border setting {other:?}"),
        }
    }
    if config.look == BorderLook::Paper && !width_given {
        config.width = DEFAULT_PAPER_BORDER_WIDTH;
    }
    Ok(config)
}

/// The `keyboard { … }` block.
pub(super) fn parse_keyboard(node: &KdlNode) -> anyhow::Result<KeyboardConfig> {
    let mut config = KeyboardConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "xkb" => {
                for setting in child.children().map(KdlDocument::nodes).unwrap_or_default() {
                    let value = string_arg(setting).context("in xkb")?;
                    match setting.name().value() {
                        "rules" => config.rules = value,
                        "model" => config.model = value,
                        "layout" => config.layout = value,
                        "variant" => config.variant = value,
                        "options" => config.options = Some(value).filter(|v| !v.is_empty()),
                        other => bail!("unknown xkb setting {other:?}"),
                    }
                }
            }
            "repeat-delay" => config.repeat_delay = int_arg(child, MAX_REPEAT_DELAY)?,
            "repeat-rate" => config.repeat_rate = int_arg(child, MAX_REPEAT_RATE)?,
            "numlock" => config.numlock = flag_value(child)?,
            other => bail!("unknown keyboard setting {other:?}"),
        }
    }
    config.validate()?;
    Ok(config)
}

/// The `cursor { … }` block.
pub(super) fn parse_cursor(node: &KdlNode) -> anyhow::Result<CursorConfig> {
    let mut config = CursorConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "theme" => {
                let theme = string_arg(child)?;
                if theme.is_empty() {
                    bail!("theme takes a theme name");
                }
                config.theme = Some(theme);
            }
            "size" => {
                let size = int_arg(child, MAX_CURSOR_SIZE)?;
                if size == 0 {
                    bail!("size takes a number from 1 to {MAX_CURSOR_SIZE}");
                }
                config.size = Some(size);
            }
            "hide-when-typing" => config.hide_when_typing = flag_value(child)?,
            "hide-after-ms" => {
                config.hide_after_ms = int_arg(child, MAX_CURSOR_HIDE_AFTER_MS)? as u32;
            }
            "hide-after-idle" => {
                config.hide_after_idle = int_arg(child, MAX_CURSOR_HIDE_AFTER_IDLE)? as u32;
            }
            other => bail!("unknown cursor setting {other:?}"),
        }
    }
    Ok(config)
}

/// The `input { … }` block.
pub(super) fn parse_input(node: &KdlNode) -> anyhow::Result<InputConfig> {
    let mut config = InputConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "touchpad" => {
                config.touchpad = parse_device_settings(child).context("in touchpad")?;
            }
            "mouse" => config.mouse = parse_device_settings(child).context("in mouse")?,
            "device" => {
                let name = string_arg(child)?;
                let settings =
                    parse_device_settings(child).with_context(|| format!("in device {name:?}"))?;
                config.devices.push((name, settings));
            }
            "touchscreen" => {
                // `touchscreen { … }` for every touchscreen, `touchscreen "NAME" { … }` for one.
                let device = match positional(child).as_slice() {
                    [] => None,
                    [_] => Some(string_arg(child)?),
                    _ => bail!("touchscreen takes at most one device name"),
                };
                let mut output = None;
                for setting in child.children().map(KdlDocument::nodes).unwrap_or_default() {
                    match setting.name().value() {
                        "map-to-output" => output = Some(string_arg(setting)?),
                        other => bail!("unknown touchscreen setting {other:?}"),
                    }
                }
                if let Some(output) = output {
                    config.touchscreens.push(TouchscreenMap { device, output });
                }
            }
            other => bail!("unknown input setting {other:?}"),
        }
    }
    Ok(config)
}

/// The settings inside `touchpad`, `mouse` or `device`.
pub(super) fn parse_device_settings(node: &KdlNode) -> anyhow::Result<DeviceSettings> {
    let mut settings = DeviceSettings::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => settings.off = Some(flag_value(child)?),
            "disable-on-external-mouse" => {
                settings.disable_on_external_mouse = Some(flag_value(child)?);
            }
            "tap" => settings.tap = Some(flag_value(child)?),
            "tap-button-map" => {
                settings.tap_button_map = Some(match string_arg(child)?.as_str() {
                    "left-right-middle" => TapButtonMap::LeftRightMiddle,
                    "left-middle-right" => TapButtonMap::LeftMiddleRight,
                    other => bail!(
                        "tap-button-map takes \"left-right-middle\" or \"left-middle-right\", not {other:?}"
                    ),
                });
            }
            "drag" => settings.drag = Some(flag_value(child)?),
            "natural-scroll" => settings.natural_scroll = Some(flag_value(child)?),
            "accel-speed" => {
                let speed = float_arg(child)?;
                if !ACCEL_SPEED_RANGE.contains(&speed) {
                    bail!("accel-speed takes a number from -1 to 1");
                }
                settings.accel_speed = Some(speed);
            }
            "accel-profile" => {
                settings.accel_profile = Some(match string_arg(child)?.as_str() {
                    "flat" => AccelProfile::Flat,
                    "adaptive" => AccelProfile::Adaptive,
                    other => bail!("accel-profile takes \"flat\" or \"adaptive\", not {other:?}"),
                });
            }
            "scroll-method" => {
                settings.scroll_method = Some(match string_arg(child)?.as_str() {
                    "none" => ScrollMethod::None,
                    "two-finger" => ScrollMethod::TwoFinger,
                    "edge" => ScrollMethod::Edge,
                    "on-button-down" => ScrollMethod::OnButtonDown,
                    other => bail!(
                        "scroll-method takes \"none\", \"two-finger\", \"edge\" or \"on-button-down\", not {other:?}"
                    ),
                });
            }
            "scroll-button" => settings.scroll_button = Some(scroll_button_arg(child)?),
            "disable-while-typing" => settings.disable_while_typing = Some(flag_value(child)?),
            "click-method" => {
                settings.click_method = Some(match string_arg(child)?.as_str() {
                    "button-areas" => ClickMethod::ButtonAreas,
                    "clickfinger" => ClickMethod::Clickfinger,
                    other => {
                        bail!(
                            "click-method takes \"button-areas\" or \"clickfinger\", not {other:?}"
                        )
                    }
                });
            }
            "middle-emulation" => settings.middle_emulation = Some(flag_value(child)?),
            "left-handed" => settings.left_handed = Some(flag_value(child)?),
            other => bail!("unknown input setting {other:?}"),
        }
    }
    Ok(settings)
}

/// `scroll-button "middle"` or an evdev code like `scroll-button 274`.
pub(super) fn scroll_button_arg(node: &KdlNode) -> anyhow::Result<u32> {
    let hint = "scroll-button takes a button name (\"left\", \"right\", \"middle\", \"side\", \"extra\", \"forward\", \"back\") or an evdev code";
    match positional(node).as_slice() {
        [value] => value
            .as_string()
            .and_then(button_code)
            .or_else(|| {
                value
                    .as_integer()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n != 0)
            })
            .context(hint),
        _ => bail!(hint),
    }
}

/// One number argument, as a float. Whole numbers are fine (`offset 3`).
pub(super) fn float_arg(node: &KdlNode) -> anyhow::Result<f64> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_float()
            .or_else(|| value.as_integer().map(|n| n as f64))
            .with_context(|| format!("{name} takes a number")),
        _ => bail!("{name} takes one number"),
    }
}

/// The `blur { … }` block. Having the block at all turns blur on.
pub(super) fn parse_blur(node: &KdlNode) -> anyhow::Result<(Blur, BlurLayers, BlurWindows)> {
    let mut config = Blur {
        off: false,
        ..Blur::default()
    };
    let mut layers = BlurLayers::default();
    let mut windows = BlurWindows::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => config.off = flag_value(child)?,
            "all-windows" => config.all_windows = flag_value(child)?,
            "all-layers" => config.all_layers = flag_value(child)?,
            "behind-windows" => config.behind_windows = flag_value(child)?,
            // The namespace a panel reports in `jff msg` (`layers`), e.g. `ignore-layer "Nbar-Top"`.
            "ignore-layer" => layers.ignore.push(string_arg(child)?),
            // With any `only-layer`, only those namespaces are blurred, whole surface, on any
            // layer (the `bottom` layer too).
            "only-layer" => layers.only.push(string_arg(child)?),
            // Corner radius of the blur behind layer surfaces: `layer-radius 16` for all of
            // them, `layer-radius "JFShell-Bottom" 16` for one namespace (wins over the former).
            "layer-radius" => match layer_radius_arg(child)? {
                (None, radius) => layers.radius = radius,
                (Some(namespace), radius) => layers.radii.push((namespace, radius)),
            },
            // App ids (`jff msg` windows), e.g. `ignore-window "firefox"`: never blurred.
            "ignore-window" => windows.ignore.push(string_arg(child)?),
            // With any `only-window`, only those app ids are blurred.
            "only-window" => windows.only.push(string_arg(child)?),
            "passes" => {
                config.passes = u8::try_from(int_arg(child, 31)?)
                    .ok()
                    .filter(|n| *n >= 1)
                    .context("passes takes a number from 1 to 31")?;
            }
            "offset" => {
                config.offset = Some(float_arg(child)?)
                    .filter(|x| x.is_finite() && *x > 0.)
                    .context("offset takes a number above 0")?;
            }
            other => bail!("unknown blur setting {other:?}"),
        }
    }
    Ok((config, layers, windows))
}

/// `window-animations` as a flag, or as a block with `off`, `open-ms` and `close-ms`.
pub(super) fn parse_window_animations(node: &KdlNode) -> anyhow::Result<WindowAnimations> {
    let mut config = WindowAnimations::default();
    if !positional(node).is_empty() {
        config.on = flag_value(node)?;
    }
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => config.on = !flag_value(child)?,
            "open" => {
                config.open = match string_arg(child)?.as_str() {
                    "fade" => OpenStyle::Fade,
                    "zoom" => OpenStyle::Zoom,
                    "slide" => OpenStyle::Slide,
                    "pop" => OpenStyle::Pop,
                    "none" => OpenStyle::None,
                    other => bail!(
                    "open takes \"fade\", \"zoom\", \"slide\", \"pop\" or \"none\", not {other:?}"
                ),
                }
            }
            "open-ms" => config.open_ms = u32::try_from(int_arg(child, MAX_WINDOW_ANIM_MS)?)?,
            "close-ms" => config.close_ms = u32::try_from(int_arg(child, MAX_WINDOW_ANIM_MS)?)?,
            other => bail!("unknown window-animations setting {other:?}"),
        }
    }
    Ok(config)
}

pub(super) fn parse_osd(node: &KdlNode) -> anyhow::Result<OsdConfig> {
    let mut config = OsdConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => config.off = flag_value(child)?,
            "timeout-ms" => {
                let ms = int_arg(child, 60_000)?;
                if ms < 100 {
                    bail!("timeout-ms takes a number from 100 to 60000");
                }
                config.timeout_ms = u32::try_from(ms)?;
            }
            "font" => {
                config.font = Some(string_arg(child)?)
                    .filter(|f| !f.trim().is_empty())
                    .context("font takes a family name, like \"Inter\"")?;
            }
            other => bail!("unknown osd setting {other:?}"),
        }
    }
    Ok(config)
}

/// `close-kill { enable, seconds N }`. `seconds` may be fractional (`seconds 2.5`).
pub(super) fn parse_close_kill(node: &KdlNode) -> anyhow::Result<CloseKillConfig> {
    let mut config = CloseKillConfig::default();
    let Some(children) = node.children() else {
        return Ok(config);
    };
    for child in children.nodes() {
        match child.name().value() {
            "seconds" => {
                let secs = float_arg(child)?;
                if !(MIN_CLOSE_KILL_SECS..=MAX_CLOSE_KILL_SECS).contains(&secs) {
                    bail!(
                        "seconds takes a number from {MIN_CLOSE_KILL_SECS} to {MAX_CLOSE_KILL_SECS}"
                    );
                }
                config.timeout = Duration::from_secs_f64(secs);
            }
            "enable" => {}
            other => bail!("unknown close-kill setting {other:?}"),
        }
    }
    config.enable = parse_flag(children, "enable", config.enable)?;
    Ok(config)
}

/// `clipboard { off, history N, max-size N }`. `max-size` is in MiB.
pub(super) fn parse_clipboard(node: &KdlNode) -> anyhow::Result<ClipboardConfig> {
    use crate::primitives::clipboard::{MAX_HISTORY, MAX_MAX_SIZE_MIB};
    let mut config = ClipboardConfig::default();
    let Some(children) = node.children() else {
        return Ok(config);
    };
    for child in children.nodes() {
        match child.name().value() {
            "history" => {
                let n = int_arg(child, MAX_HISTORY as i32)?;
                if n < 1 {
                    bail!("history takes a number from 1 to {MAX_HISTORY}");
                }
                config.history = n as usize;
            }
            "max-size" => {
                let mib = int_arg(child, MAX_MAX_SIZE_MIB as i32)?;
                if mib < 1 {
                    bail!("max-size takes a number of MiB from 1 to {MAX_MAX_SIZE_MIB}");
                }
                config.max_size = mib as usize * 1024 * 1024;
            }
            "off" => {}
            other => bail!("unknown clipboard setting {other:?}"),
        }
    }
    config.off = parse_flag(children, "off", config.off)?;
    Ok(config)
}

/// `workspace-wall`, `workspace-wall #false`, or a block with `height N`, `thumbnails`,
/// `follow-window` and `off`.
/// `None` when it is off.
pub(super) fn parse_workspace_wall(node: &KdlNode) -> anyhow::Result<Option<WorkspaceWallConfig>> {
    let mut on = true;
    let mut height = DEFAULT_WALL_HEIGHT;
    let mut thumbnails = true;
    let mut follow_window = true;
    if !positional(node).is_empty() {
        on = flag_value(node)?;
    }
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "height" => {
                height = int_arg(child, MAX_WALL_HEIGHT)?;
                if height < MIN_WALL_HEIGHT {
                    bail!("height takes a number from {MIN_WALL_HEIGHT} to {MAX_WALL_HEIGHT}");
                }
            }
            "thumbnails" => thumbnails = flag_value(child)?,
            "follow-window" => follow_window = flag_value(child)?,
            "off" => on = !flag_value(child)?,
            other => bail!("unknown workspace-wall setting {other:?}"),
        }
    }
    Ok(on.then_some(WorkspaceWallConfig {
        height,
        thumbnails,
        follow_window,
    }))
}

/// A `window-rule` block: `match` lines, then actions.
pub(super) fn parse_window_rule(node: &KdlNode) -> anyhow::Result<WindowRule> {
    if !positional(node).is_empty() {
        bail!("window-rule takes a block, not arguments");
    }
    let mut rule = WindowRule::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "match" => parse_window_match(child, &mut rule.matches)?,
            "open-on-output" => {
                let name = string_arg(child)?;
                if name.is_empty() {
                    bail!("open-on-output takes a monitor name or \"pointer\"");
                }
                rule.output = Some(if name == "pointer" {
                    RuleOutput::Pointer
                } else {
                    RuleOutput::Named(name)
                });
            }
            "open-workspace" => {
                let max = i32::try_from(MAX_WORKSPACES).unwrap_or(i32::MAX);
                let number = int_arg(child, max)?;
                if number < 1 {
                    bail!("open-workspace takes a number from 1 to {MAX_WORKSPACES}");
                }
                rule.workspace = Some(usize::try_from(number - 1)?);
            }
            "open-maximized" => rule.maximized = Some(flag_value(child)?),
            "open-size" => {
                let sides = positional(child)
                    .iter()
                    .map(|value| {
                        value
                            .as_integer()
                            .and_then(|n| i32::try_from(n).ok())
                            .filter(|n| (1..=MAX_RULE_SIZE).contains(n))
                    })
                    .collect::<Option<Vec<i32>>>();
                match sides.as_deref() {
                    Some(&[width, height]) => rule.size = Some((width, height)),
                    _ => bail!("open-size takes width and height, each from 1 to {MAX_RULE_SIZE}"),
                }
            }
            "floating" => rule.floating = flag_value(child)?,
            "open-position" => {
                let sides = positional(child)
                    .iter()
                    .map(|value| {
                        value
                            .as_integer()
                            .and_then(|n| i32::try_from(n).ok())
                            .filter(|n| (0..=MAX_RULE_SIZE).contains(n))
                    })
                    .collect::<Option<Vec<i32>>>();
                match sides.as_deref() {
                    Some(&[x, y]) => rule.position = Some((x, y)),
                    _ => bail!(
                        "open-position takes x and y from the top left of the monitor, each from 0 to {MAX_RULE_SIZE}"
                    ),
                }
            }
            "open-centered" => rule.centered = flag_value(child)?,
            "open-in-container" => {
                let name = string_arg(child)?;
                if name.trim().is_empty() {
                    bail!("open-in-container takes the name of a container, not an empty string");
                }
                rule.container = Some(name);
            }
            "tag" => {
                let tag = string_arg(child)?;
                if tag.trim().is_empty() {
                    bail!("tag takes a name for the window, not an empty string");
                }
                rule.tag = Some(tag);
            }
            "opacity" => {
                let opacity = float_arg(child)?;
                if !(0.0..=1.0).contains(&opacity) {
                    bail!("opacity takes a number from 0 (invisible) to 1 (opaque)");
                }
                rule.opacity = Some((opacity * 1000.0).round() as u16);
            }
            "sdr-white" => rule.sdr_white = Some(nits_arg(child, &SDR_WHITE_RANGE, 300)?),
            "sdr" => rule.sdr = flag_value(child)?,
            "always-on-top" => rule.always_on_top = flag_value(child)?,
            "sticky" => rule.sticky = flag_value(child)?,
            "content-type" => {
                rule.content_type = Some(match string_arg(child)?.as_str() {
                    "game" => RuleContentType::Game,
                    "video" => RuleContentType::Video,
                    "photo" => RuleContentType::Photo,
                    "none" => RuleContentType::NoType,
                    _ => bail!("content-type takes \"game\", \"video\", \"photo\" or \"none\""),
                });
            }
            other => bail!("unknown window-rule setting {other:?}"),
        }
    }
    if rule.floating && rule.maximized == Some(true) {
        bail!("floating and open-maximized #true ask for opposite things");
    }
    if rule.floating && rule.container.is_some() {
        bail!("floating and open-in-container ask for opposite things");
    }
    if rule.position.is_some() && rule.centered {
        bail!("open-position and open-centered both say where the window goes");
    }
    if (rule.position.is_some() || rule.centered) && rule.maximized == Some(true) {
        bail!("open-position and open-centered ask for a floating window, open-maximized #true for the opposite");
    }
    if (rule.position.is_some() || rule.centered) && rule.container.is_some() {
        bail!("open-position and open-centered cannot be combined with open-in-container, the container places the window");
    }
    Ok(rule)
}

/// A `container-rule` block: `match` lines, then `name`.
pub(super) fn parse_container_rule(node: &KdlNode) -> anyhow::Result<ContainerRule> {
    if !node.entries().is_empty() {
        bail!("container-rule takes a block, not arguments");
    }
    let mut rule = ContainerRule::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "match" => {
                parse_window_match(child, &mut rule.matches)?;
                if let Some(other) = rule.matches.iter().find(|condition| {
                    matches!(condition, WindowMatch::Dialog(_) | WindowMatch::Restored(_))
                }) {
                    let key = match other {
                        WindowMatch::Dialog(_) => "dialog",
                        _ => "restored",
                    };
                    bail!("container-rule cannot match {key}: use app-id, title or tag");
                }
            }
            "name" => {
                let name = string_arg(child)?;
                if name.is_empty() {
                    bail!("name takes a name for the container, not an empty string");
                }
                rule.name = Some(name);
            }
            "layout" => {
                let layout = string_arg(child)?;
                if layout.trim().is_empty() {
                    bail!("layout takes the name of a saved layout, not an empty string");
                }
                rule.layout = Some(layout);
            }
            other => bail!("unknown container-rule setting {other:?}"),
        }
    }
    Ok(rule)
}

/// A `container-template "NAME" { run "prog" "arg" … app-id="ID" }` block.
pub(super) fn parse_container_template(node: &KdlNode) -> anyhow::Result<ContainerTemplate> {
    let name = match positional(node).as_slice() {
        [value] => value.as_string().map(str::to_owned),
        _ => None,
    }
    .filter(|name| !name.trim().is_empty())
    .context("container-template takes one name, like container-template \"dev\" { … }")?;
    if node.entries().iter().any(|entry| entry.name().is_some()) {
        bail!("container-template takes a name and a block, not properties");
    }
    let mut runs = Vec::new();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "run" => {
                let mut argv = Vec::new();
                let mut app_id = None;
                for entry in child.entries() {
                    let value = entry
                        .value()
                        .as_string()
                        .map(str::to_owned)
                        .context("run takes strings")?;
                    match entry.name().map(|key| key.value()) {
                        None => argv.push(value),
                        Some("app-id") => {
                            if value.trim().is_empty() {
                                bail!("app-id takes the app id of the window, not an empty string");
                            }
                            app_id = Some(value);
                        }
                        Some(other) => bail!("unknown run property {other:?}, only app-id"),
                    }
                }
                if argv.first().is_none_or(|program| program.trim().is_empty()) {
                    bail!("run needs a program, like run \"kitty\"");
                }
                if child.children().is_some() {
                    bail!("run takes no block");
                }
                // Most apps use their program's file name as their app id (kitty, foot, firefox).
                // The ones that do not say so with `app-id=`.
                let app_id = app_id.unwrap_or_else(|| {
                    std::path::Path::new(&argv[0])
                        .file_name()
                        .map_or_else(|| argv[0].clone(), |f| f.to_string_lossy().into_owned())
                });
                runs.push(TemplateRun { argv, app_id });
            }
            other => bail!("unknown container-template setting {other:?}, only run"),
        }
    }
    if runs.is_empty() {
        bail!("container-template {name:?} has no run, it does nothing");
    }
    Ok(ContainerTemplate { name, runs })
}

/// One `match` line: `key=value` pairs, all of which have to hold.
pub(super) fn parse_window_match(node: &KdlNode, out: &mut Vec<WindowMatch>) -> anyhow::Result<()> {
    if !positional(node).is_empty() {
        bail!("match takes name=value pairs, like match app-id=\"firefox\"");
    }
    let mut any = false;
    for entry in node.entries() {
        let Some(key) = entry.name().map(|key| key.value()) else {
            continue;
        };
        any = true;
        let value = entry.value();
        let text = || {
            value
                .as_string()
                .map(str::to_owned)
                .with_context(|| format!("match {key} takes a string"))
        };
        let flag = || {
            value
                .as_bool()
                .or_else(|| match value.as_string() {
                    Some("true" | "yes") => Some(true),
                    Some("false" | "no") => Some(false),
                    _ => None,
                })
                .with_context(|| format!("match {key} takes true or false"))
        };
        out.push(match key {
            "app-id" => WindowMatch::AppId(text()?),
            "title" => WindowMatch::Title(text()?),
            "tag" => WindowMatch::Tag(text()?),
            "dialog" => WindowMatch::Dialog(flag()?),
            "restored" => WindowMatch::Restored(flag()?),
            other => bail!("unknown match {other:?}: use app-id, title, tag, dialog or restored"),
        });
    }
    if !any {
        bail!("match needs a name=value pair, like match tag=\"browser\"");
    }
    Ok(())
}

/// The `switcher { … }` block.
pub(super) fn parse_switcher(node: &KdlNode) -> anyhow::Result<SwitcherConfig> {
    let mut config = SwitcherConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "thumbnail-height" => {
                let height = int_arg(child, MAX_THUMBNAIL_HEIGHT)?;
                if height < MIN_THUMBNAIL_HEIGHT {
                    bail!(
                        "thumbnail-height takes a number from {MIN_THUMBNAIL_HEIGHT} to {MAX_THUMBNAIL_HEIGHT}"
                    );
                }
                config.thumbnail_height = Some(height);
            }
            "slide-ms" => config.slide_ms = u32::try_from(int_arg(child, MAX_SWITCHER_SLIDE_MS)?)?,
            "group-containers" => config.group_containers = flag_value(child)?,
            other => bail!("unknown switcher setting {other:?}"),
        }
    }
    Ok(config)
}

/// The `overview { … }` block.
pub(super) fn parse_overview(node: &KdlNode) -> anyhow::Result<OverviewConfig> {
    let mut config = OverviewConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "selection-width" => config.selection_width = int_arg(child, MAX_BORDER_WIDTH)?,
            "selection-color" => config.selection_color = parse_color(&string_arg(child)?)?,
            "border" => config.border_width = int_arg(child, MAX_BORDER_WIDTH)?,
            "border-color" => config.border_color = Some(parse_color(&string_arg(child)?)?),
            "backdrop-color" => config.backdrop_color = Some(parse_color(&string_arg(child)?)?),
            "search-background-color" => {
                config.search_background_color = Some(parse_color(&string_arg(child)?)?)
            }
            "search-font-color" => {
                config.search_font_color = Some(parse_color(&string_arg(child)?)?)
            }
            "container-background-color" => {
                config.container_background_color = parse_color(&string_arg(child)?)?
            }
            "container-border-width" => {
                config.container_border_width = int_arg(child, MAX_BORDER_WIDTH)?
            }
            "container-border-color" => {
                config.container_border_color = parse_color(&string_arg(child)?)?
            }
            "container-font-color" => {
                config.container_font_color = parse_color(&string_arg(child)?)?
            }
            "container-radius" => config.container_radius = Some(int_arg(child, MAX_RADIUS)?),
            "auto-open" => config.auto_open = flag_value(child)?,
            "blur" => config.blur = flag_value(child)?,
            "animation-ms" => {
                config.animation_ms = u32::try_from(int_arg(child, MAX_OVERVIEW_ANIMATION_MS)?)?
            }
            other => bail!("unknown overview setting {other:?}"),
        }
    }
    Ok(config)
}

/// A workspace count: one number from 1 to [`MAX_WORKSPACES`].
pub(super) fn workspace_count(node: &KdlNode) -> anyhow::Result<usize> {
    positional(node)
        .first()
        .and_then(|value| value.as_integer())
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_WORKSPACES).contains(n))
        .with_context(|| {
            format!(
                "{} takes a number from 1 to {MAX_WORKSPACES}",
                node.name().value()
            )
        })
}

/// `workspaces N`, or a block with `count N` and `per-output`. Returns the count and whether
/// every monitor gets its own workspace.
pub(super) fn parse_workspaces(node: &KdlNode) -> anyhow::Result<(usize, bool)> {
    let mut count = DEFAULT_WORKSPACES;
    let mut per_output = false;
    let children = node.children().map(KdlDocument::nodes).unwrap_or_default();
    if positional(node).is_empty() && children.is_empty() {
        bail!("workspaces takes a number from 1 to {MAX_WORKSPACES}, or a block");
    }
    if !positional(node).is_empty() {
        count = workspace_count(node)?;
    }
    for child in children {
        match child.name().value() {
            "count" => count = workspace_count(child).context("in workspaces")?,
            "per-output" => per_output = flag_value(child).context("in workspaces")?,
            other => bail!("unknown workspaces setting {other:?}"),
        }
    }
    Ok((count, per_output))
}

/// The `hot-corner { … }` block, or `hot-corner N` for just the size. `None` when `size` is 0.
/// A block that doesn't say otherwise is on, with [`DEFAULT_HOT_CORNER_SIZE`].
pub(super) fn parse_hot_corner(node: &KdlNode) -> anyhow::Result<Option<HotCorner>> {
    let mut size = DEFAULT_HOT_CORNER_SIZE;
    let mut dwell_ms = DEFAULT_HOT_CORNER_DWELL_MS;
    let mut touch_size = DEFAULT_HOT_CORNER_TOUCH_SIZE;
    let mut monitors: Vec<String> = Vec::new();
    if !positional(node).is_empty() {
        size = int_arg(node, MAX_HOT_CORNER_SIZE)?;
    }
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "size" => size = int_arg(child, MAX_HOT_CORNER_SIZE)?,
            "dwell-ms" => dwell_ms = int_arg(child, MAX_HOT_CORNER_DWELL_MS)?,
            "touch-size" => touch_size = int_arg(child, MAX_HOT_CORNER_TOUCH_SIZE)?,
            "monitor" => monitors.push(string_arg(child)?),
            other => bail!("unknown hot-corner setting {other:?}"),
        }
    }
    if size == 0 {
        return Ok(None);
    }
    Ok(Some(HotCorner {
        size,
        dwell: Duration::from_millis(u64::try_from(dwell_ms)?),
        touch_size,
        monitors,
    }))
}

/// The `zoom { … }` block. A bare `zoom` leaves everything at its default.
pub(super) fn parse_zoom(node: &KdlNode) -> anyhow::Result<ZoomConfig> {
    let mut config = ZoomConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "step" => {
                let step = float_arg(child)?;
                if !ZOOM_STEP_RANGE.contains(&step) {
                    bail!(
                        "step takes a number from {} to {}",
                        ZOOM_STEP_RANGE.start(),
                        ZOOM_STEP_RANGE.end()
                    );
                }
                config.step = step;
            }
            "max" => {
                let max = float_arg(child)?;
                if !(zoom::MIN_LEVEL..=zoom::MAX_LEVEL).contains(&max) {
                    bail!(
                        "max takes a number from {} to {}",
                        zoom::MIN_LEVEL,
                        zoom::MAX_LEVEL
                    );
                }
                config.max = max;
            }
            "tracking" => {
                config.tracking = match string_arg(child)?.to_ascii_lowercase().as_str() {
                    "proportional" => Tracking::Proportional,
                    "centered" | "centred" => Tracking::Centered,
                    "push" => Tracking::Push,
                    "disabled" | "off" => Tracking::Disabled,
                    _ => bail!(
                        "tracking takes \"proportional\", \"centered\", \"push\" or \"disabled\""
                    ),
                }
            }
            "pointer" => {
                config.pointer = match string_arg(child)?.to_ascii_lowercase().as_str() {
                    "scale" => PointerStyle::Scale,
                    "keep" => PointerStyle::Keep,
                    "hide" => PointerStyle::Hide,
                    _ => bail!("pointer takes \"scale\", \"keep\" or \"hide\""),
                }
            }
            "animation-ms" => {
                let ms = int_arg(child, MAX_ZOOM_ANIMATION_MS)?;
                config.animation = Duration::from_millis(u64::try_from(ms)?);
            }
            "scroll-modifier" => {
                let word = string_arg(child)?;
                config.scroll_modifier =
                    if matches!(word.to_ascii_lowercase().as_str(), "none" | "off") {
                        None
                    } else {
                        Some(DragModifier::named(&word).with_context(|| {
                        "scroll-modifier takes \"Super\", \"Alt\", \"Ctrl\", \"Shift\" or \"none\""
                    })?)
                    };
            }
            other => bail!("unknown zoom setting {other:?}"),
        }
    }
    Ok(config)
}

/// The `screenshot { … }` block.
pub(super) fn parse_screenshot(node: &KdlNode) -> anyhow::Result<ScreenshotConfig> {
    let mut config = ScreenshotConfig::default();
    let Some(children) = node.children() else {
        return Ok(config);
    };
    for child in children.nodes() {
        match child.name().value() {
            "path" => config.path = Some(string_arg(child)?).filter(|p| !p.trim().is_empty()),
            "include-cursor" | "copy-to-clipboard" => {}
            other => bail!("unknown screenshot setting {other:?}"),
        }
    }
    config.include_cursor = parse_flag(children, "include-cursor", config.include_cursor)?;
    config.copy_to_clipboard = parse_flag(children, "copy-to-clipboard", config.copy_to_clipboard)?;
    Ok(config)
}

/// The `log { … }` block: `enable`, `path "FILE"`, `level "info"`.
pub(super) fn parse_log(node: &KdlNode) -> anyhow::Result<LogConfig> {
    let mut config = LogConfig::default();
    let Some(children) = node.children() else {
        return Ok(config);
    };
    for child in children.nodes() {
        match child.name().value() {
            "path" => config.path = Some(string_arg(child)?).filter(|p| !p.trim().is_empty()),
            "level" => {
                let text = string_arg(child)?;
                config.level = LogLevel::parse(&text).with_context(|| {
                    format!("level takes {}, not {text:?}", LogLevel::NAMES.join(", "))
                })?;
            }
            "enable" => {}
            other => bail!("unknown log setting {other:?}"),
        }
    }
    config.enable = parse_flag(children, "enable", config.enable)?;
    Ok(config)
}

pub(super) fn parse_output(node: &KdlNode) -> anyhow::Result<OutputConfig> {
    let name = positional(node)
        .first()
        .and_then(|value| value.as_string())
        .context("output needs a connector name, like \"DP-1\"")?
        .to_string();
    let mut config = OutputConfig {
        name,
        ..Default::default()
    };

    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        let args = positional(child);
        match child.name().value() {
            "off" => config.off = true,
            "content-type-hint" => {
                if !args.is_empty() {
                    bail!("content-type-hint takes no value");
                }
                config.content_type_hint = true;
            }
            "position" => {
                let coords = args
                    .iter()
                    .map(|value| value.as_integer().and_then(|i| i32::try_from(i).ok()))
                    .collect::<Option<Vec<_>>>();
                match coords.as_deref() {
                    Some(&[x, y]) => config.position = Some((x, y)),
                    _ => bail!("position takes two integers: position X Y"),
                }
            }
            "mode" => {
                let text = match args.as_slice() {
                    [value] => value
                        .as_string()
                        .context("mode takes a string like \"1920x1080@60\"")?,
                    _ => bail!("mode takes one string like \"1920x1080@60\""),
                };
                config.mode = Some(parse_mode(text)?);
            }
            "transform" => {
                config.transform = match args.as_slice() {
                    [value] => value
                        .as_string()
                        .and_then(OutputTransform::from_name)
                        .with_context(|| {
                            format!(
                                "transform takes {}: transform \"90\"",
                                OutputTransform::NAMES
                                    .map(|n| format!("\"{n}\""))
                                    .join(", ")
                            )
                        })?,
                    _ => bail!(
                        "transform takes one value, {}: transform \"90\"",
                        OutputTransform::NAMES
                            .map(|n| format!("\"{n}\""))
                            .join(", ")
                    ),
                };
            }
            "split" => {
                config.split = match args.as_slice() {
                    [value] => match value.as_string() {
                        Some("auto") => None,
                        Some("horizontal") => Some(Axis::Horizontal),
                        Some("vertical") => Some(Axis::Vertical),
                        _ => bail!("split takes \"auto\", \"horizontal\" or \"vertical\""),
                    },
                    _ => bail!("split takes one value: split \"vertical\""),
                };
            }
            "scale" => {
                if !config.set_scale(float_arg(child)?) {
                    bail!(
                        "scale takes a number from {} to {}: scale 1.5",
                        SCALE_RANGE.start(),
                        SCALE_RANGE.end()
                    );
                }
            }
            "adaptive-sync" => {
                config.vrr = match args.as_slice() {
                    [value] => match value.as_string() {
                        Some("off") => Vrr::Off,
                        Some("fullscreen") => Vrr::Fullscreen,
                        Some("always") => Vrr::Always,
                        _ => bail!("adaptive-sync takes \"off\", \"fullscreen\" or \"always\""),
                    },
                    _ => bail!("adaptive-sync takes one value: adaptive-sync \"fullscreen\""),
                };
            }
            "color-management" => config.color_management_off = !flag_value(child)?,
            "hdr" => config.hdr = flag_value(child)?,
            "sdr-white" => config.sdr_white = Some(nits_arg(child, &SDR_WHITE_RANGE, 203)?),
            "hdr-peak" => config.hdr_peak = Some(nits_arg(child, &HDR_PEAK_RANGE, 400)?),
            "hdr-contrast" => {
                let value = float_arg(child)? as f32;
                if !CONTRAST_RANGE.contains(&value) {
                    bail!(
                        "hdr-contrast takes a number from {} to {}: hdr-contrast 1.2",
                        CONTRAST_RANGE.start(),
                        CONTRAST_RANGE.end()
                    );
                }
                config.hdr_contrast = Some((value * 1000.0).round() as u32);
            }
            "sdr-transfer" => {
                config.sdr_transfer = Some(match args.as_slice() {
                    [value] => value
                        .as_string()
                        .and_then(SdrTransfer::from_name)
                        .with_context(|| {
                            format!(
                                "sdr-transfer takes {}: sdr-transfer \"gamma22\"",
                                SdrTransfer::NAMES.map(|n| format!("\"{n}\"")).join(" or ")
                            )
                        })?,
                    _ => bail!(
                        "sdr-transfer takes one value, {}: sdr-transfer \"gamma22\"",
                        SdrTransfer::NAMES.map(|n| format!("\"{n}\"")).join(" or ")
                    ),
                });
            }
            "hdr-black-level" => {
                let value = float_arg(child)? as f32;
                if !BLACK_LEVEL_RANGE.contains(&value) {
                    bail!(
                        "hdr-black-level takes cd/m² from {} to {}: hdr-black-level 0.2",
                        BLACK_LEVEL_RANGE.start(),
                        BLACK_LEVEL_RANGE.end()
                    );
                }
                config.hdr_black_level = Some((value * 1000.0).round() as i32);
            }
            "hdr-saturation" => {
                let value = float_arg(child)? as f32;
                if !SATURATION_RANGE.contains(&value) {
                    bail!(
                        "hdr-saturation takes a number from {} to {}: hdr-saturation 1.1",
                        SATURATION_RANGE.start(),
                        SATURATION_RANGE.end()
                    );
                }
                config.hdr_saturation = Some((value * 1000.0).round() as u32);
            }
            "hdr-white-balance" => {
                let hint = format!(
                    "hdr-white-balance takes the gain of red, green and blue, each from {} to {}: hdr-white-balance 1.0 0.95 0.9",
                    WHITE_BALANCE_RANGE.start(),
                    WHITE_BALANCE_RANGE.end()
                );
                let gains: Vec<f32> = positional(child)
                    .into_iter()
                    .map(|value| {
                        value
                            .as_float()
                            .or_else(|| value.as_integer().map(|n| n as f64))
                            .map(|n| n as f32)
                            .filter(|n| WHITE_BALANCE_RANGE.contains(n))
                    })
                    .collect::<Option<_>>()
                    .context(hint.clone())?;
                let [r, g, b] = gains.as_slice() else {
                    bail!(hint);
                };
                config.hdr_white_balance =
                    Some([r, g, b].map(|gain| (gain * 1000.0).round() as u32));
            }
            "hdr-content-metadata" => config.hdr_content_metadata = Some(flag_value(child)?),
            "hdr-gamut-map" => {
                config.hdr_gamut_map = Some(match args.as_slice() {
                    [value] => value
                        .as_string()
                        .and_then(GamutMap::from_name)
                        .with_context(|| {
                            format!(
                                "hdr-gamut-map takes {}: hdr-gamut-map \"clip\"",
                                GamutMap::NAMES.map(|n| format!("\"{n}\"")).join(" or ")
                            )
                        })?,
                    _ => bail!(
                        "hdr-gamut-map takes one value, {}: hdr-gamut-map \"clip\"",
                        GamutMap::NAMES.map(|n| format!("\"{n}\"")).join(" or ")
                    ),
                });
            }
            "hdr-min-luminance" => {
                let value = float_arg(child)? as f32;
                if !MIN_LUMINANCE_RANGE.contains(&value) {
                    bail!(
                        "hdr-min-luminance takes cd/m² from {} to {}: hdr-min-luminance 0.05",
                        MIN_LUMINANCE_RANGE.start(),
                        MIN_LUMINANCE_RANGE.end()
                    );
                }
                config.hdr_min_luminance = Some((value * 10_000.0).round() as u32);
            }
            "hdr-tone-map" => {
                config.hdr_tone_map = Some(match args.as_slice() {
                    [value] => value
                        .as_string()
                        .and_then(ToneMap::from_name)
                        .with_context(|| {
                            format!(
                                "hdr-tone-map takes {}: hdr-tone-map \"bt2390\"",
                                ToneMap::NAMES.map(|n| format!("\"{n}\"")).join(" or ")
                            )
                        })?,
                    _ => bail!(
                        "hdr-tone-map takes one value, {}: hdr-tone-map \"bt2390\"",
                        ToneMap::NAMES.map(|n| format!("\"{n}\"")).join(" or ")
                    ),
                });
            }
            "icc-profile" => {
                let path = match args.as_slice() {
                    [value] => value
                        .as_string()
                        .context("icc-profile takes a path in quotes")?,
                    _ => bail!("icc-profile takes one path: icc-profile \"/path/to/file.icc\""),
                };
                config.icc_profile = Some(expand_home(path));
            }
            other => bail!("unknown output setting {other:?}"),
        }
    }
    Ok(config)
}

/// `~/file` → `file` in the home directory.
pub(super) fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => std::path::PathBuf::from(path),
    }
}

/// `WIDTHxHEIGHT` or `WIDTHxHEIGHT@REFRESH`.
pub(super) fn parse_mode(text: &str) -> anyhow::Result<ModeRequest> {
    let (size, refresh) = match text.split_once('@') {
        Some((size, refresh)) => (size, Some(refresh)),
        None => (text, None),
    };
    let (width, height) = size.split_once('x').context("expected WIDTHxHEIGHT")?;
    Ok(ModeRequest {
        width: width.trim().parse()?,
        height: height.trim().parse()?,
        refresh: refresh
            .map(|hz| hz.trim().parse::<f64>().map(|hz| hz.round() as i32))
            .transpose()?,
    })
}

/// `Mod+Mod+Key`. The last component is the trigger (a keysym or a mouse
/// button), the rest are modifiers.
pub(super) fn parse_combo(combo: &str) -> anyhow::Result<(Mods, Trigger)> {
    let mut parts = combo.split('+').collect::<Vec<_>>();
    let key = parts
        .pop()
        .filter(|k| !k.is_empty())
        .context("missing key")?;

    let mut mods = Mods::default();
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "logo" | "mod4" => mods.logo = true,
            _ => bail!("unknown modifier {part:?}"),
        }
    }

    let trigger = match key.to_ascii_lowercase().as_str() {
        "mouseback" => Trigger::Button(MouseButton::Back),
        "mouseforward" => Trigger::Button(MouseButton::Forward),
        _ => {
            let sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
            if sym.raw() == 0 {
                bail!("unknown key {key:?}");
            }
            Trigger::Key(sym)
        }
    };
    Ok((mods, trigger))
}

/// `autostart "prog" "arg"` / `sh-autostart "line"` → a spawn action.
pub(super) fn parse_startup(node: &KdlNode) -> anyhow::Result<Action> {
    let args = positional(node)
        .into_iter()
        .map(|value| {
            value
                .as_string()
                .map(str::to_owned)
                .context("arguments must be strings")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    match node.name().value() {
        "autostart" if args.is_empty() => bail!("autostart needs a program"),
        "autostart" => Ok(Action::Spawn(args)),
        _ => match <[String; 1]>::try_from(args) {
            Ok([line]) => Ok(Action::SpawnShell(line)),
            Err(_) => bail!("sh-autostart takes exactly one command line"),
        },
    }
}

/// An action node, e.g. `spawn "foot" "-e" "htop"` or `workspace 3`.
pub(super) fn parse_action(node: &KdlNode) -> anyhow::Result<Action> {
    // The only actions that take a number, not strings. Workspaces count from 1 in the config.
    if let name @ ("workspace" | "move-to-workspace") = node.name().value() {
        let number = match positional(node).as_slice() {
            [value] => value
                .as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= 1),
            _ => None,
        }
        .with_context(|| format!("{name} takes one workspace number, like {name} 2"))?;
        return Ok(if name == "workspace" {
            Action::Workspace(Goto::To(number - 1))
        } else {
            Action::MoveToWorkspace(number - 1)
        });
    }

    let args = node
        .entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| {
            entry
                .value()
                .as_string()
                .map(str::to_owned)
                .context("arguments must be strings")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let name = node.name().value();
    let no_args = |action: Action| {
        if args.is_empty() {
            Ok(action)
        } else {
            Err(anyhow::anyhow!("{name} takes no arguments"))
        }
    };

    match name {
        "spawn" if args.is_empty() => bail!("spawn needs a program"),
        "spawn" => Ok(Action::Spawn(args)),
        "spawn-sh" => match <[String; 1]>::try_from(args) {
            Ok([command]) => Ok(Action::SpawnShell(command)),
            Err(_) => bail!("spawn-sh takes exactly one command line"),
        },
        "terminal" => no_args(Action::Terminal),
        "maximize" => no_args(Action::Maximize),
        "fullscreen" => no_args(Action::Fullscreen),
        "snap-left" => no_args(Action::Snap(Side::Left)),
        "snap-right" => no_args(Action::Snap(Side::Right)),
        "snap-top" => no_args(Action::Snap(Side::Top)),
        "snap-bottom" => no_args(Action::Snap(Side::Bottom)),
        "close" => no_args(Action::Close),
        "minimize" => no_args(Action::Minimize),
        "unminimize" => no_args(Action::Unminimize),
        "focus-urgent" => no_args(Action::FocusUrgent),
        "move-to-scratchpad" => no_args(Action::MoveToScratchpad),
        "scratchpad" => no_args(Action::Scratchpad),
        "overview" => no_args(Action::Overview),
        "switch-window" => no_args(Action::SwitchWindow { backwards: false }),
        "switch-window-back" => no_args(Action::SwitchWindow { backwards: true }),
        "workspace-next" => no_args(Action::Workspace(Goto::Next)),
        "workspace-prev" => no_args(Action::Workspace(Goto::Prev)),
        "reload-config" => no_args(Action::ReloadConfig),
        "screenshot-area" => no_args(Action::ScreenshotArea),
        "screenshot-fullscreen" => no_args(Action::ScreenshotFullscreen),
        "screenshot-window" => no_args(Action::ScreenshotWindow),
        "toggle-shortcuts-inhibit" => no_args(Action::ToggleShortcutsInhibit),
        "toggle-stats" => no_args(Action::ToggleStats),
        "zoom-in" => no_args(Action::ZoomIn),
        "zoom-out" => no_args(Action::ZoomOut),
        "zoom-reset" => no_args(Action::ZoomReset),
        "zoom-pan-left" => no_args(Action::ZoomPan(Pan::Left)),
        "zoom-pan-right" => no_args(Action::ZoomPan(Pan::Right)),
        "zoom-pan-up" => no_args(Action::ZoomPan(Pan::Up)),
        "zoom-pan-down" => no_args(Action::ZoomPan(Pan::Down)),
        "forward-key" => match <[String; 1]>::try_from(args) {
            Ok([app_id]) if !app_id.is_empty() => Ok(Action::ForwardKey(app_id)),
            _ => bail!("forward-key takes one app id, like forward-key \"com.obsproject.Studio\""),
        },
        "focus-container" => match <[String; 1]>::try_from(args) {
            Ok([name]) if !name.is_empty() => Ok(Action::FocusContainer(name)),
            _ => bail!(
                "focus-container takes one container name, like focus-container \"Terminals\""
            ),
        },
        "spawn-container" => match <[String; 1]>::try_from(args) {
            Ok([name]) if !name.trim().is_empty() => Ok(Action::SpawnContainer(name)),
            _ => bail!(
                "spawn-container takes one container-template name, like spawn-container \"dev\""
            ),
        },
        "container-next" => no_args(Action::ContainerStep { backwards: false }),
        "container-prev" => no_args(Action::ContainerStep { backwards: true }),
        "container-drag" => no_args(Action::ContainerDrag),
        "container-eject" => no_args(Action::ContainerEject),
        "save-container-layout" => match <[String; 1]>::try_from(args) {
            Ok([name]) if !name.trim().is_empty() => Ok(Action::SaveContainerLayout(name)),
            _ => bail!(
                "save-container-layout takes one layout name, like save-container-layout \"Dev\""
            ),
        },
        "apply-container-layout" => match <[String; 1]>::try_from(args) {
            Ok([name]) if !name.trim().is_empty() => Ok(Action::ApplyContainerLayout(name)),
            _ => bail!(
                "apply-container-layout takes one layout name, like apply-container-layout \"Dev\""
            ),
        },
        "focus-monitor" | "move-to-monitor" | "move-workspace-to-monitor" => {
            let target = match <[String; 1]>::try_from(args) {
                Ok([word]) => MonitorTarget::parse(&word),
                Err(_) => None,
            }
            .with_context(|| {
                format!(
                    "{name} takes one monitor ({}), like {name} \"right\"",
                    MonitorTarget::WORDS
                )
            })?;
            Ok(match name {
                "focus-monitor" => Action::FocusMonitor(target),
                "move-to-monitor" => Action::MoveToMonitor(target),
                _ => Action::MoveWorkspaceToMonitor(target),
            })
        }
        "suspend" => no_args(Action::Suspend),
        "power-off" => no_args(Action::PowerOff),
        "quit" => no_args(Action::Quit),
        other => bail!("unknown action {other:?}"),
    }
}
