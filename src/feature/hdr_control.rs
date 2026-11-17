//! HDR on a monitor, changed while jff runs: `jff msg hdr on|off|toggle`, `jff msg sdr-white`, `jff msg hdr-set` and `jff msg hdr-reset`.
//!
//! The change is an edit of the monitor's config entry, like what output management does, so it
//! lasts until `reload-config` (or `hdr-reset`) and the monitor is lit again when HDR needs a new swapchain.

use crate::app::Backend;
use crate::config::{Config, OutputConfig};
use crate::primitives::gamut::GamutMap;
use crate::primitives::hdr_output::{
    HdrChange, SdrTransfer, ToneMap, BLACK_LEVEL_RANGE, CONTRAST_RANGE, DEFAULT_SDR_WHITE,
    HDR_PEAK_RANGE, SATURATION_RANGE, WHITE_BALANCE_RANGE,
};
use crate::state::JffState;

/// A setting `jff msg hdr-set` changes, in the units the config keeps (thousandths for the fractions). `None` puts the default back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrSetting {
    /// `hdr-peak`, cd/m².
    Peak(Option<u32>),
    /// `hdr-tone-map`.
    ToneMap(Option<ToneMap>),
    /// `hdr-contrast`, thousandths.
    Contrast(Option<u32>),
    /// `hdr-black-level`, thousandths of cd/m².
    BlackLevel(Option<i32>),
    /// `hdr-saturation`, thousandths.
    Saturation(Option<u32>),
    /// `hdr-white-balance`, gain of red, green and blue in thousandths.
    WhiteBalance(Option<[u32; 3]>),
    /// `hdr-gamut-map`.
    GamutMap(Option<GamutMap>),
}

impl HdrSetting {
    /// The keys `hdr-set` takes, each also with the `hdr-` of the config in front.
    pub const KEYS: &'static str =
        "peak, tone-map, contrast, black-level, saturation, white-balance, gamut-map";

    /// The setting `key` set to `values` (the words after the key). `default` puts the default back.
    pub fn parse(key: &str, values: &[&str]) -> Result<Self, String> {
        let name = key.strip_prefix("hdr-").unwrap_or(key);
        let default = matches!(values, ["default"]);
        let one = |what: &str| match values {
            [value] => Ok(*value),
            _ => Err(format!("{key} takes {what}")),
        };
        let number = |value: &str, range: &std::ops::RangeInclusive<f32>| {
            value
                .parse::<f32>()
                .ok()
                .filter(|n| range.contains(n))
                .map(|n| (n * 1000.0).round() as i64)
                .ok_or_else(|| {
                    format!(
                        "{value:?} is not a number from {} to {}",
                        range.start(),
                        range.end()
                    )
                })
        };
        match name {
            "peak" => {
                if default {
                    return Ok(Self::Peak(None));
                }
                let value = one("cd/m², or default")?;
                value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| HDR_PEAK_RANGE.contains(n))
                    .map(|n| Self::Peak(Some(n)))
                    .ok_or_else(|| {
                        format!(
                            "{value:?} is not a luminance from {} to {} cd/m²",
                            HDR_PEAK_RANGE.start(),
                            HDR_PEAK_RANGE.end()
                        )
                    })
            }
            "tone-map" => {
                if default {
                    return Ok(Self::ToneMap(None));
                }
                let value = one("a curve or default")?;
                ToneMap::from_name(value)
                    .map(|t| Self::ToneMap(Some(t)))
                    .ok_or_else(|| format!("{value:?} is not {}", ToneMap::NAMES.join(" or ")))
            }
            "gamut-map" => {
                if default {
                    return Ok(Self::GamutMap(None));
                }
                let value = one("a mapping or default")?;
                GamutMap::from_name(value)
                    .map(|g| Self::GamutMap(Some(g)))
                    .ok_or_else(|| format!("{value:?} is not {}", GamutMap::NAMES.join(" or ")))
            }
            "contrast" if default => Ok(Self::Contrast(None)),
            "contrast" => Ok(Self::Contrast(Some(
                number(one("a number or default")?, &CONTRAST_RANGE)? as u32,
            ))),
            "saturation" if default => Ok(Self::Saturation(None)),
            "saturation" => Ok(Self::Saturation(Some(
                number(one("a number or default")?, &SATURATION_RANGE)? as u32,
            ))),
            "black-level" if default => Ok(Self::BlackLevel(None)),
            "black-level" => Ok(Self::BlackLevel(Some(
                number(one("cd/m² or default")?, &BLACK_LEVEL_RANGE)? as i32,
            ))),
            "white-balance" if default => Ok(Self::WhiteBalance(None)),
            "white-balance" => match values {
                [r, g, b] => Ok(Self::WhiteBalance(Some([
                    number(r, &WHITE_BALANCE_RANGE)? as u32,
                    number(g, &WHITE_BALANCE_RANGE)? as u32,
                    number(b, &WHITE_BALANCE_RANGE)? as u32,
                ]))),
                _ => Err(
                    "white-balance takes the gain of red, green and blue (1.0 0.95 0.9), or default"
                        .to_string(),
                ),
            },
            _ => Err(format!("{key:?} is not a setting, settings: {}", Self::KEYS)),
        }
    }

    /// Put the setting in the monitor's config entry.
    pub fn apply(self, config: &mut OutputConfig) {
        match self {
            Self::Peak(v) => config.hdr_peak = v,
            Self::ToneMap(v) => config.hdr_tone_map = v,
            Self::Contrast(v) => config.hdr_contrast = v,
            Self::BlackLevel(v) => config.hdr_black_level = v,
            Self::Saturation(v) => config.hdr_saturation = v,
            Self::WhiteBalance(v) => config.hdr_white_balance = v,
            Self::GamutMap(v) => config.hdr_gamut_map = v,
        }
    }
}

/// Give `config` the HDR settings `jff msg` can change from `file`, the monitor's entry in the config file (the defaults when it has none). Nothing else of the entry is touched.
pub fn restore_hdr_settings(config: &mut OutputConfig, file: Option<&OutputConfig>) {
    let none = OutputConfig::default();
    let file = file.unwrap_or(&none);
    config.hdr = file.hdr;
    config.sdr_white = file.sdr_white;
    config.hdr_peak = file.hdr_peak;
    config.hdr_tone_map = file.hdr_tone_map;
    config.hdr_contrast = file.hdr_contrast;
    config.hdr_black_level = file.hdr_black_level;
    config.hdr_saturation = file.hdr_saturation;
    config.hdr_white_balance = file.hdr_white_balance;
    config.hdr_gamut_map = file.hdr_gamut_map;
}

/// What a monitor is set to after a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HdrReport {
    pub output: String,
    /// HDR is asked for.
    pub requested: bool,
    /// The monitor is really signalled as HDR (it stays SDR when the panel or the link cannot).
    pub active: bool,
    /// SDR white, in cd/m².
    pub sdr_white: u32,
}

/// `value` with up to `digits` decimals and at least one: `1.20` is `1.2`, `1` is `1.0`.
fn trimmed(value: f64, digits: usize) -> String {
    let text = format!("{value:.digits$}");
    let text = text.trim_end_matches('0');
    if text.ends_with('.') {
        format!("{text}0")
    } else {
        text.to_string()
    }
}

/// A thousandths value as the config writes it: `1200` is `1.2`, `1000` is `1.0`, `-500` is `-0.5`.
fn milli(value: i64) -> String {
    trimmed(value as f64 / 1000.0, 3)
}

/// Every HDR setting of the monitor's entry as a JSON object, `null` for a setting at its default (the EDID's for `peak` and `min_luminance`, on for `content_metadata`).
pub fn hdr_settings_json(config: &OutputConfig) -> String {
    let number = |v: Option<u32>| v.map_or_else(|| "null".to_string(), |v| v.to_string());
    let fraction = |v: Option<i64>| v.map_or_else(|| "null".to_string(), milli);
    let name = |v: Option<&str>| v.map_or_else(|| "null".to_string(), |v| format!("\"{v}\""));
    let balance = config.hdr_white_balance.map_or_else(
        || "null".to_string(),
        |gains| {
            let gains: Vec<String> = gains.iter().map(|g| milli(i64::from(*g))).collect();
            format!("[{}]", gains.join(","))
        },
    );
    format!(
        "{{\"hdr\":{},\"sdr_white\":{},\"sdr_transfer\":{},\"peak\":{},\"min_luminance\":{},\"tone_map\":{},\"contrast\":{},\"black_level\":{},\"saturation\":{},\"white_balance\":{},\"gamut_map\":{},\"content_metadata\":{}}}",
        config.hdr,
        number(config.sdr_white),
        name(config.sdr_transfer.map(SdrTransfer::name)),
        number(config.hdr_peak),
        config.hdr_min_luminance.map_or_else(
            || "null".to_string(),
            |units| trimmed(f64::from(units) / 10_000.0, 4)
        ),
        name(config.hdr_tone_map.map(ToneMap::name)),
        fraction(config.hdr_contrast.map(i64::from)),
        fraction(config.hdr_black_level.map(i64::from)),
        fraction(config.hdr_saturation.map(i64::from)),
        balance,
        name(config.hdr_gamut_map.map(GamutMap::name)),
        config
            .hdr_content_metadata
            .map_or_else(|| "null".to_string(), |on| on.to_string()),
    )
}

/// The HDR settings of `running` that differ from `file`, the monitor's entry in the config file (the defaults when it has none). Each is the setting's name as `hdr-get` reports it and the line that gives the config file that value, or a comment saying to remove the line when the setting is at its default now.
pub fn hdr_changes(
    running: &OutputConfig,
    file: Option<&OutputConfig>,
) -> Vec<(&'static str, String)> {
    let none = OutputConfig::default();
    let file = file.unwrap_or(&none);
    let mut changes = Vec::new();
    // `value` is the line for the setting when it is set, `remove` the config key to drop when it is not.
    let mut note = |name: &'static str, differs: bool, line: Option<String>, remove: &str| {
        if differs {
            changes.push((name, line.unwrap_or_else(|| format!("// remove {remove}"))));
        }
    };
    note(
        "hdr",
        running.hdr != file.hdr,
        running.hdr.then(|| "hdr".to_string()),
        "hdr",
    );
    note(
        "sdr_white",
        running.sdr_white != file.sdr_white,
        running.sdr_white.map(|v| format!("sdr-white {v}")),
        "sdr-white",
    );
    note(
        "peak",
        running.hdr_peak != file.hdr_peak,
        running.hdr_peak.map(|v| format!("hdr-peak {v}")),
        "hdr-peak",
    );
    note(
        "tone_map",
        running.hdr_tone_map != file.hdr_tone_map,
        running
            .hdr_tone_map
            .map(|v| format!("hdr-tone-map \"{}\"", v.name())),
        "hdr-tone-map",
    );
    note(
        "contrast",
        running.hdr_contrast != file.hdr_contrast,
        running
            .hdr_contrast
            .map(|v| format!("hdr-contrast {}", milli(i64::from(v)))),
        "hdr-contrast",
    );
    note(
        "black_level",
        running.hdr_black_level != file.hdr_black_level,
        running
            .hdr_black_level
            .map(|v| format!("hdr-black-level {}", milli(i64::from(v)))),
        "hdr-black-level",
    );
    note(
        "saturation",
        running.hdr_saturation != file.hdr_saturation,
        running
            .hdr_saturation
            .map(|v| format!("hdr-saturation {}", milli(i64::from(v)))),
        "hdr-saturation",
    );
    note(
        "white_balance",
        running.hdr_white_balance != file.hdr_white_balance,
        running.hdr_white_balance.map(|g| {
            format!(
                "hdr-white-balance {} {} {}",
                milli(i64::from(g[0])),
                milli(i64::from(g[1])),
                milli(i64::from(g[2]))
            )
        }),
        "hdr-white-balance",
    );
    note(
        "gamut_map",
        running.hdr_gamut_map != file.hdr_gamut_map,
        running
            .hdr_gamut_map
            .map(|v| format!("hdr-gamut-map \"{}\"", v.name())),
        "hdr-gamut-map",
    );
    changes
}

/// The reply of `hdr-get` for `output`: the settings in use, and what changed from `file` (`None` when the config file could not be read), as the names and the lines to paste into the monitor's `output` block.
pub fn hdr_get_json(
    output: &str,
    running: &OutputConfig,
    file: Option<Option<&OutputConfig>>,
) -> String {
    let (changed, kdl) = match file {
        None => ("null".to_string(), "null".to_string()),
        Some(file) => {
            let changes = hdr_changes(running, file);
            let names: Vec<String> = changes.iter().map(|(n, _)| format!("\"{n}\"")).collect();
            let lines: Vec<&str> = changes.iter().map(|(_, l)| l.as_str()).collect();
            (
                format!("[{}]", names.join(",")),
                crate::primitives::json::json_str(&lines.join("\n")),
            )
        }
    };
    format!(
        "{{\"ok\":true,\"output\":{},\"settings\":{},\"changed\":{changed},\"kdl\":{kdl}}}\n",
        crate::primitives::json::json_str(output),
        hdr_settings_json(running),
    )
}

impl JffState {
    /// The `hdr-get` reply for `output`: the HDR settings in use and the ones that were changed while jff runs. `Err` is a sentence for the user.
    pub fn hdr_get(&self, output: &str) -> Result<String, String> {
        if !self.outputs.iter().any(|o| o.name() == output) {
            return Err(format!("there is no monitor {output:?}"));
        }
        let running = self.config.output(output).cloned().unwrap_or_default();
        let file = Config::load();
        let file = match &file {
            Ok(file) => Some(file.output(output)),
            Err(err) => {
                tracing::warn!("hdr-get: cannot read the config to compare with: {err:#}");
                None
            }
        };
        Ok(hdr_get_json(output, &running, file))
    }

    /// Change HDR on `output`, the monitor in use for `None`. `Err` is a sentence for the user.
    pub fn change_hdr(
        &mut self,
        output: Option<&str>,
        change: HdrChange,
    ) -> Result<HdrReport, String> {
        let sets_white = matches!(change, HdrChange::SdrWhite(_) | HdrChange::SdrWhiteBy(_));
        self.edit_hdr(output, |config| {
            let white = config.sdr_white.unwrap_or(DEFAULT_SDR_WHITE);
            let (requested, white) = change.apply(config.hdr, white);
            config.hdr = requested;
            if sets_white {
                config.sdr_white = Some(white);
            }
        })
    }

    /// Change one HDR setting of `output` (`hdr-set`). Lasts until `reload-config` or `hdr-reset`.
    pub fn set_hdr_setting(
        &mut self,
        output: &str,
        setting: HdrSetting,
    ) -> Result<HdrReport, String> {
        self.edit_hdr(Some(output), |config| setting.apply(config))
    }

    /// Put the HDR settings of `output` back to what the config file says (`hdr-reset`), the rest of the running config stays. The file is read for it, but nothing else of it is applied.
    pub fn reset_hdr(&mut self, output: &str) -> Result<HdrReport, String> {
        let file = Config::load().map_err(|err| format!("cannot read the config: {err:#}"))?;
        let file = file.output(output).cloned();
        self.edit_hdr(Some(output), |config| {
            restore_hdr_settings(config, file.as_ref())
        })
    }

    /// Edit the config entry of `output`, the monitor in use for `None`, and light the monitor again as the entry says. `Err` is a sentence for the user.
    fn edit_hdr(
        &mut self,
        output: Option<&str>,
        edit: impl FnOnce(&mut OutputConfig),
    ) -> Result<HdrReport, String> {
        let name = output.map_or_else(|| self.active_output_name(), str::to_owned);
        if !self.outputs.iter().any(|o| o.name() == name) {
            return Err(format!("there is no monitor {name:?}"));
        }
        if !matches!(self.backend, Backend::Tty(_)) {
            return Err("HDR can only be changed on a monitor jff drives directly".to_string());
        }
        self.config.update_output(&name, edit);
        self.reconcile_outputs();
        self.queue_redraw_all();
        let current = self.config.output(&name);
        let requested = current.is_some_and(|c| c.hdr);
        let sdr_white = current
            .and_then(|c| c.sdr_white)
            .unwrap_or(DEFAULT_SDR_WHITE);
        let active = match &self.backend {
            Backend::Tty(tty) => tty.hdr_descriptions().iter().any(|(n, _)| *n == name),
            _ => false,
        };
        Ok(HdrReport {
            output: name,
            requested,
            active,
            sdr_white,
        })
    }
}
