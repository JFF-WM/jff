//! Backends: winit (nested window, for development) and tty (directly on the GPU).

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{WinitEventLoop, WinitGraphicsBackend};
use smithay::output::Output;
use smithay::reexports::calloop::LoopHandle;

use crate::config::Config;
use crate::state::JffState;

pub mod pacing;
pub mod run;
pub mod tty;
pub mod winit;

pub use run::run;
pub use winit::init_winit;

use self::tty::Tty;

pub type WinitBackend = WinitGraphicsBackend<GlesRenderer>;

pub const BACKGROUND: [f32; 4] = [0.08, 0.09, 0.12, 1.0];

pub enum Backend {
    Winit(WinitBackend),
    Tty(Tty),
}

impl Backend {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        match self {
            Backend::Winit(winit) => winit.renderer(),
            Backend::Tty(tty) => &mut tty.renderer,
        }
    }

    pub fn is_tty(&self) -> bool {
        matches!(self, Backend::Tty(_))
    }
}

/// winit if a Wayland/X11 session is around, tty otherwise. `JFF_BACKEND=winit|tty` overrides.
pub fn init(
    handle: &LoopHandle<'static, JffState>,
    config: &Config,
) -> anyhow::Result<(Backend, Option<WinitEventLoop>, Vec<Output>)> {
    let nested =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let use_winit = match std::env::var("JFF_BACKEND").as_deref() {
        Ok("winit") => true,
        Ok("tty") => false,
        _ => nested,
    };

    if use_winit {
        let (backend, winit_loop, output) = init_winit()?;
        Ok((backend, Some(winit_loop), vec![output]))
    } else {
        let (backend, outputs) = tty::init(handle, config)?;
        Ok((backend, None, outputs))
    }
}
