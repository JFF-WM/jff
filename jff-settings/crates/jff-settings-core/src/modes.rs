//! The video modes a display offers, read from `wlr-randr`.
//!
//! jff answers wlr-output-management, so `wlr-randr` lists what every connected display can do.
//! A mode in the config is `"WIDTHxHEIGHT@HZ"` and jff rounds the refresh rate to a whole
//! number, so modes that only differ after the decimal point (59.94 and 60.00) are one entry.

use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: i64,
    pub height: i64,
    /// Whole Hz, rounded the way jff rounds the number in the config.
    pub refresh: i64,
    pub preferred: bool,
    pub current: bool,
}

impl Mode {
    /// What goes in the config: `1920x1080@165`.
    pub fn value(&self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.refresh)
    }

    /// What the list shows: `1920 × 1080 @ 165 Hz (current)`.
    pub fn label(&self) -> String {
        let mut label = format!("{} × {} @ {} Hz", self.width, self.height, self.refresh);
        match (self.preferred, self.current) {
            (true, true) => label.push_str(" (preferred, current)"),
            (true, false) => label.push_str(" (preferred)"),
            (false, true) => label.push_str(" (current)"),
            (false, false) => {}
        }
        label
    }
}

/// The modes of the connector `name`, biggest first. Empty when `wlr-randr` is missing, fails,
/// or doesn't know the connector, and the caller falls back to typing the mode.
pub fn for_output(name: &str) -> Vec<Mode> {
    let Ok(out) = Command::new("wlr-randr").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_wlr_randr(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .find(|(output, _)| output == name)
        .map(|(_, modes)| modes)
        .unwrap_or_default()
}

/// Parses the text `wlr-randr` prints: an unindented `NAME "description"` line per output,
/// then a `Modes:` section of `WIDTHxHEIGHT px, HZ Hz (preferred) (current)` lines.
pub fn parse_wlr_randr(text: &str) -> Vec<(String, Vec<Mode>)> {
    let mut outputs: Vec<(String, Vec<Mode>)> = Vec::new();
    let mut in_modes = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            let name = line.split_whitespace().next().unwrap_or_default();
            outputs.push((name.to_owned(), Vec::new()));
            in_modes = false;
            continue;
        }
        let trimmed = line.trim();
        if trimmed == "Modes:" {
            in_modes = true;
            continue;
        }
        if !in_modes {
            continue;
        }
        match (parse_mode_line(trimmed), outputs.last_mut()) {
            (Some(mode), Some((_, modes))) => add(modes, mode),
            // The next property (`Position:`, `Scale:`) ends the list.
            (None, _) => in_modes = false,
            _ => {}
        }
    }
    for (_, modes) in &mut outputs {
        modes.sort_by_key(|m| std::cmp::Reverse((m.width * m.height, m.width, m.refresh)));
    }
    outputs
}

/// Keeps the first of the modes that round to the same value, and any flag the others had.
fn add(modes: &mut Vec<Mode>, mode: Mode) {
    match modes.iter_mut().find(|m| m.value() == mode.value()) {
        Some(seen) => {
            seen.preferred |= mode.preferred;
            seen.current |= mode.current;
        }
        None => modes.push(mode),
    }
}

/// `1920x1080 px, 165.003006 Hz (preferred) (current)`.
fn parse_mode_line(line: &str) -> Option<Mode> {
    let (size, rest) = line.split_once(" px,")?;
    let (width, height) = size.split_once('x')?;
    let hz = rest.trim().split_whitespace().next()?;
    Some(Mode {
        width: width.trim().parse().ok()?,
        height: height.trim().parse().ok()?,
        refresh: hz.parse::<f64>().ok()?.round() as i64,
        preferred: rest.contains("(preferred)"),
        current: rest.contains("(current)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"DP-1 "DP-1"
  Physical size: 700x390 mm
  Enabled: yes
  Modes:
    1920x1080 px, 60.000000 Hz (preferred)
    1920x1080 px, 165.003006 Hz (current)
    1920x1080 px, 119.879997 Hz
    1920x1080 px, 119.982002 Hz
    1920x1080 px, 59.939999 Hz
    1280x1024 px, 75.025002 Hz
    640x480 px, 60.000000 Hz
  Position: 2560,0
  Transform: normal
  Scale: 1.000000
HDMI-A-2 "HDMI-A-2"
  Enabled: yes
  Modes:
    2560x1080 px, 59.978001 Hz (preferred)
    2560x1080 px, 74.990997 Hz (current)
  Position: 0,0
"#;

    fn values(modes: &[Mode]) -> Vec<String> {
        modes.iter().map(Mode::value).collect()
    }

    #[test]
    fn reads_every_output() {
        let outputs = parse_wlr_randr(SAMPLE);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].0, "DP-1");
        assert_eq!(outputs[1].0, "HDMI-A-2");
    }

    #[test]
    fn rounds_merges_and_sorts() {
        let outputs = parse_wlr_randr(SAMPLE);
        assert_eq!(
            values(&outputs[0].1),
            [
                "1920x1080@165",
                "1920x1080@120",
                "1920x1080@60",
                "1280x1024@75",
                "640x480@60"
            ]
        );
        assert_eq!(values(&outputs[1].1), ["2560x1080@75", "2560x1080@60"]);
    }

    #[test]
    fn keeps_flags_of_merged_modes() {
        let modes = &parse_wlr_randr(SAMPLE)[0].1;
        let sixty = modes.iter().find(|m| m.value() == "1920x1080@60").unwrap();
        assert!(sixty.preferred && !sixty.current);
        assert_eq!(sixty.label(), "1920 × 1080 @ 60 Hz (preferred)");
        assert_eq!(modes[0].label(), "1920 × 1080 @ 165 Hz (current)");
    }

    #[test]
    fn empty_or_unknown_text_has_no_modes() {
        assert!(parse_wlr_randr("").is_empty());
        assert!(parse_wlr_randr("garbage").iter().all(|(_, m)| m.is_empty()));
    }
}
