//! HDR on a monitor, changed while jff runs: `jff msg hdr on|off|toggle` and `jff msg sdr-white`.
//!
//! The change is an edit of the monitor's config entry, like what output management does, so it
//! lasts until `reload-config` and the monitor is lit again when HDR needs a new swapchain.

use crate::app::Backend;
use crate::primitives::hdr_output::{HdrChange, DEFAULT_SDR_WHITE};
use crate::state::JffState;

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

impl JffState {
    /// Change HDR on `output`, the monitor in use for `None`. `Err` is a sentence for the user.
    pub fn change_hdr(
        &mut self,
        output: Option<&str>,
        change: HdrChange,
    ) -> Result<HdrReport, String> {
        let name = output.map_or_else(|| self.active_output_name(), str::to_owned);
        if !self.outputs.iter().any(|o| o.name() == name) {
            return Err(format!("there is no monitor {name:?}"));
        }
        if !matches!(self.backend, Backend::Tty(_)) {
            return Err("HDR can only be changed on a monitor jff drives directly".to_string());
        }
        let current = self.config.output(&name);
        let enabled = current.is_some_and(|c| c.hdr);
        let sdr_white = current
            .and_then(|c| c.sdr_white)
            .unwrap_or(DEFAULT_SDR_WHITE);
        let (requested, sdr_white) = change.apply(enabled, sdr_white);
        let sets_white = matches!(change, HdrChange::SdrWhite(_) | HdrChange::SdrWhiteBy(_));
        self.config.update_output(&name, |config| {
            config.hdr = requested;
            if sets_white {
                config.sdr_white = Some(sdr_white);
            }
        });
        self.reconcile_outputs();
        self.queue_redraw_all();
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
