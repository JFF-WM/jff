//! Client state and the event handlers for every object we bind or create.

use std::fmt::Debug;

use wayland_client::{
    globals::GlobalListContents,
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_compositor::{self, WlCompositor},
        wl_output::{self, WlOutput},
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::{self, WlShmPool},
        wl_surface::{self, WlSurface},
    },
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use crate::draw::{self, Frame, PALETTE};
use crate::proto::client::{
    xdg_session_manager_v1::{self, XdgSessionManagerV1},
    xdg_session_v1::{self, XdgSessionV1},
    xdg_toplevel_session_v1::{self, XdgToplevelSessionV1},
};

/// Size used when the compositor says "pick your own" (0x0) and we have none yet.
const DEFAULT_SIZE: (i32, i32) = (480, 320);

/// What the compositor said about the session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    /// No answer to get_session yet.
    Waiting,
    Created(String),
    Restored,
    Replaced,
}

pub struct OutputInfo {
    pub wl: WlOutput,
    pub global: u32,
    pub name: Option<String>,
    pub description: Option<String>,
    pub make: String,
    pub model: String,
    pub pos: (i32, i32),
    /// width, height, refresh in mHz
    pub mode: Option<(i32, i32, i32)>,
    pub scale: i32,
}

impl OutputInfo {
    pub fn new(wl: WlOutput, global: u32) -> Self {
        Self {
            wl,
            global,
            name: None,
            description: None,
            make: String::new(),
            model: String::new(),
            pos: (0, 0),
            mode: None,
            scale: 1,
        }
    }

    pub fn describe(&self) -> String {
        let label = match &self.name {
            Some(name) => name.clone(),
            None if !self.make.is_empty() || !self.model.is_empty() => {
                format!("{} {}", self.make, self.model).trim().to_string()
            }
            None => format!("wl_output#{}", self.global),
        };
        let mode = self
            .mode
            .map(|(w, h, r)| format!(" mode={w}x{h}@{:.0}Hz", r as f64 / 1000.0))
            .unwrap_or_default();
        format!(
            "{label} pos=({},{}){mode} scale={}",
            self.pos.0, self.pos.1, self.scale
        )
    }
}

pub struct Window {
    pub name: String,
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    pub tl_session: Option<XdgToplevelSessionV1>,
    pub color: u32,
    pub size: (i32, i32),
    pub states: Vec<String>,
    pending: Option<(i32, i32)>,
    pub restore_requested: bool,
    pub restored: bool,
    first_configure_seen: bool,
    pub closed: bool,
}

impl Window {
    /// Destroy the protocol objects, children first. Does not touch the session itself.
    pub fn destroy(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(tl_session) = self.tl_session.take() {
            tl_session.destroy();
        }
        self.toplevel.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
    }
}

pub struct App {
    pub verbose: bool,
    pub compositor: WlCompositor,
    pub shm: WlShm,
    pub wm_base: XdgWmBase,
    pub session: Option<XdgSessionV1>,
    pub status: SessionStatus,
    pub outputs: Vec<OutputInfo>,
    pub windows: Vec<Window>,
    /// Set by handlers that hit an error they cannot return; the main loop reports it.
    pub fatal: Option<String>,
}

impl App {
    pub fn new(
        verbose: bool,
        compositor: WlCompositor,
        shm: WlShm,
        wm_base: XdgWmBase,
    ) -> Self {
        Self {
            verbose,
            compositor,
            shm,
            wm_base,
            session: None,
            status: SessionStatus::Waiting,
            outputs: Vec::new(),
            windows: Vec::new(),
            fatal: None,
        }
    }

    fn vlog(&self, what: &str, ev: &dyn Debug) {
        if self.verbose {
            println!("[event] {what}: {ev:?}");
        }
    }

    fn vlog_w(&self, idx: usize, iface: &str, ev: &dyn Debug) {
        if self.verbose {
            println!("[event] {} {iface}: {ev:?}", self.windows[idx].name);
        }
    }

    pub fn describe_output(&self, output: &WlOutput) -> String {
        match self.outputs.iter().find(|i| &i.wl == output) {
            Some(info) => info.describe(),
            None => format!("unknown output {:?}", output.id()),
        }
    }

    /// Create window `index`: surface, xdg_surface, toplevel, title, app_id, then the
    /// session call, and only then the initial empty commit.
    pub fn new_window(&mut self, qh: &QueueHandle<App>, index: usize, restore: bool) {
        let name = format!("window-{index}");
        let surface = self.compositor.create_surface(qh, index);
        let xdg_surface = self.wm_base.get_xdg_surface(&surface, qh, index);
        let toplevel = xdg_surface.get_toplevel(qh, index);
        toplevel.set_title(format!("session test {name}"));
        toplevel.set_app_id("xdg-session-test".to_string());

        // Must happen before the first commit, or restore_toplevel is an already_mapped error.
        let usable = self.status != SessionStatus::Replaced;
        let tl_session = match (&self.session, usable) {
            (Some(session), true) if restore => {
                println!("{name}: restore_toplevel");
                Some(session.restore_toplevel(&toplevel, name.clone(), qh, index))
            }
            (Some(session), true) => {
                println!("{name}: add_toplevel");
                Some(session.add_toplevel(&toplevel, name.clone(), qh, index))
            }
            _ => None,
        };

        surface.commit();

        self.windows.push(Window {
            name,
            surface,
            xdg_surface,
            toplevel,
            tl_session,
            color: PALETTE[index % PALETTE.len()],
            size: (0, 0),
            states: Vec::new(),
            pending: None,
            restore_requested: restore && usable,
            restored: false,
            first_configure_seen: false,
            closed: false,
        });
    }

    fn on_toplevel_configure(&mut self, idx: usize, width: i32, height: i32, raw: &[u8]) {
        let states = decode_states(raw);
        let w = &mut self.windows[idx];
        w.pending = Some((width, height));
        w.states = states.clone();
        let label = if w.first_configure_seen {
            "configure"
        } else {
            "first configure"
        };
        println!(
            "{}: {label} {width}x{height} states=[{}]",
            w.name,
            states.join(", ")
        );
        if !w.first_configure_seen {
            w.first_configure_seen = true;
            if w.restored && (width <= 0 || height <= 0) {
                println!(
                    "{}: WARNING restored by the compositor but the first configure has size 0x0",
                    w.name
                );
            }
            if w.restore_requested && !w.restored {
                println!(
                    "{}: note: restore was requested but no restored event arrived \
                     (nothing saved under this name, or the compositor does not restore)",
                    w.name
                );
            }
        }
    }

    fn on_surface_configure(&mut self, idx: usize, serial: u32, qh: &QueueHandle<App>) {
        let w = &mut self.windows[idx];
        if w.closed {
            return;
        }
        w.xdg_surface.ack_configure(serial);
        let size = match w.pending.take() {
            Some((pw, ph)) if pw > 0 && ph > 0 => (pw, ph),
            _ if w.size.0 > 0 && w.size.1 > 0 => w.size,
            _ => DEFAULT_SIZE,
        };
        w.size = size;
        self.redraw(idx, qh);
    }

    fn redraw(&mut self, idx: usize, qh: &QueueHandle<App>) {
        let shm = self.shm.clone();
        let w = &self.windows[idx];
        let status = if w.restored {
            "restored by compositor"
        } else {
            "not restored"
        };
        let states = w.states.join(" ");
        let frame = Frame {
            name: &w.name,
            color: w.color,
            width: w.size.0,
            height: w.size.1,
            status,
            states: &states,
        };
        let result = draw::render(&shm, qh, &frame);
        match result {
            Ok(buffer) => {
                let w = &self.windows[idx];
                w.surface.attach(Some(&buffer), 0, 0);
                w.surface
                    .damage_buffer(0, 0, w.size.0.max(1), w.size.1.max(1));
                w.surface.commit();
            }
            Err(e) => {
                self.fatal = Some(format!("{}: cannot draw: {e}", self.windows[idx].name));
            }
        }
    }

    fn on_close(&mut self, idx: usize) {
        println!("{}: close requested", self.windows[idx].name);
        self.windows[idx].destroy();
    }

    pub fn all_closed(&self) -> bool {
        !self.windows.is_empty() && self.windows.iter().all(|w| w.closed)
    }
}

fn decode_states(raw: &[u8]) -> Vec<String> {
    raw.chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .map(|v| match xdg_toplevel::State::try_from(v) {
            Ok(state) => format!("{state:?}").to_lowercase(),
            Err(_) => format!("unknown({v})"),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Dispatch implementations
// ---------------------------------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        state.vlog("wl_registry", &event);
        // Monitors plugged in while we run.
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_output" {
                let wl = registry.bind::<WlOutput, _, _>(name, version.min(4), qh, ());
                state.outputs.push(OutputInfo::new(wl, name));
            }
        }
    }
}

impl Dispatch<WlCompositor, ()> for App {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlShm,
        event: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog("wl_shm", &event);
    }
}

impl Dispatch<WlShmPool, ()> for App {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for App {
    fn event(
        state: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog("wl_buffer", &event);
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

impl Dispatch<WlOutput, ()> for App {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog("wl_output", &event);
        let Some(info) = state.outputs.iter_mut().find(|i| &i.wl == output) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, make, model, ..
            } => {
                info.pos = (x, y);
                info.make = make;
                info.model = model;
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } => {
                if let WEnum::Value(flags) = flags {
                    if flags.contains(wl_output::Mode::Current) {
                        info.mode = Some((width, height, refresh));
                    }
                }
            }
            wl_output::Event::Scale { factor } => info.scale = factor,
            wl_output::Event::Name { name } => info.name = Some(name),
            wl_output::Event::Description { description } => info.description = Some(description),
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, usize> for App {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog_w(*idx, "wl_surface", &event);
        match event {
            wl_surface::Event::Enter { output } => {
                let desc = state.describe_output(&output);
                println!("{}: entered output {desc}", state.windows[*idx].name);
            }
            wl_surface::Event::Leave { output } => {
                let desc = state.describe_output(&output);
                println!("{}: left output {desc}", state.windows[*idx].name);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        state: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog("xdg_wm_base", &event);
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, usize> for App {
    fn event(
        state: &mut Self,
        _: &XdgSurface,
        event: xdg_surface::Event,
        idx: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        state.vlog_w(*idx, "xdg_surface", &event);
        if let xdg_surface::Event::Configure { serial } = event {
            state.on_surface_configure(*idx, serial, qh);
        }
    }
}

impl Dispatch<XdgToplevel, usize> for App {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog_w(*idx, "xdg_toplevel", &event);
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => state.on_toplevel_configure(*idx, width, height, &states),
            xdg_toplevel::Event::Close => state.on_close(*idx),
            _ => {}
        }
    }
}

impl Dispatch<XdgSessionManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &XdgSessionManagerV1,
        _: xdg_session_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgSessionV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &XdgSessionV1,
        event: xdg_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog("xdg_session_v1", &event);
        match event {
            xdg_session_v1::Event::Created { session_id } => {
                println!("session created: {session_id}");
                state.status = SessionStatus::Created(session_id);
            }
            xdg_session_v1::Event::Restored => {
                println!("session restored");
                state.status = SessionStatus::Restored;
            }
            xdg_session_v1::Event::Replaced => {
                println!("session replaced: another client took it over, not using it any more");
                state.status = SessionStatus::Replaced;
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevelSessionV1, usize> for App {
    fn event(
        state: &mut Self,
        _: &XdgToplevelSessionV1,
        event: xdg_toplevel_session_v1::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.vlog_w(*idx, "xdg_toplevel_session_v1", &event);
        match event {
            xdg_toplevel_session_v1::Event::Restored => {
                println!("{}: restored by compositor", state.windows[*idx].name);
                state.windows[*idx].restored = true;
            }
            _ => {}
        }
    }
}
