//! Window for trying out wp_alpha_modifier_v1.
//!
//! `alpha-test-client <percent> [child-percent]`
//!
//! Opens a 400x300 window: opaque red and blue halves (so a fade is easy to see against what is
//! behind it), and a strip along the bottom that is already 50% transparent in its own pixels
//! (the multiplier must stack on that). The multiplier of the window is `percent`.
//! With `child-percent` a green square subsurface is added with a multiplier of its own: it
//! must not follow the window's.

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface,
    wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::alpha_modifier::v1::client::wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1;
use wayland_protocols::wp::alpha_modifier::v1::client::wp_alpha_modifier_v1::WpAlphaModifierV1;
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const WIDTH: i32 = 400;
const HEIGHT: i32 = 300;
const CHILD: i32 = 100;

struct App {
    configured: bool,
    done: bool,
}

/// 0..=100 percent as the protocol's fixed-point factor (0xFFFFFFFF is fully opaque).
fn factor(percent: f64) -> u32 {
    (percent.clamp(0.0, 100.0) / 100.0 * f64::from(u32::MAX)).round() as u32
}

/// Premultiplied ARGB8888 pixels, little endian, from a colour function.
fn pixels(width: i32, height: i32, color: impl Fn(i32, i32) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, a] = color(x, y);
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    out
}

fn buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<App>,
    width: i32,
    height: i32,
    data: &[u8],
) -> wl_buffer::WlBuffer {
    let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is not set");
    let path = format!(
        "{dir}/alpha-test-client-{}-{width}x{height}",
        std::process::id()
    );
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("cannot create the shm file");
    // The open file keeps the data alive.
    fs::remove_file(&path).ok();
    file.write_all(data).expect("cannot fill the shm file");
    let pool = shm.create_pool(file.as_fd(), data.len() as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        width * 4,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    buffer
}

fn main() {
    let mut args = std::env::args().skip(1);
    let percent: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or_else(|| {
        eprintln!("usage: alpha-test-client <percent> [child-percent]");
        std::process::exit(2);
    });
    let child_percent: Option<f64> = args.next().and_then(|a| a.parse().ok());

    let conn = Connection::connect_to_env().expect("no Wayland compositor to connect to");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("cannot read globals");
    let qh = queue.handle();

    let compositor: wl_compositor::WlCompositor =
        globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=1, ()).expect("xdg_wm_base");
    let modifier: WpAlphaModifierV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("the compositor does not offer wp_alpha_modifier_v1");

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("alpha-test-client".into());
    surface.commit();

    let mut app = App {
        configured: false,
        done: false,
    };
    while !app.configured {
        queue.blocking_dispatch(&mut app).expect("dispatch");
    }

    // Parent: red | blue, and a strip along the bottom that is 50% transparent in its pixels.
    let data = pixels(WIDTH, HEIGHT, |x, y| {
        if y >= HEIGHT - 60 {
            [128, 128, 128, 128]
        } else if x < WIDTH / 2 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    });
    let parent_buffer = buffer(&shm, &qh, WIDTH, HEIGHT, &data);
    surface.attach(Some(&parent_buffer), 0, 0);
    let parent_modifier = modifier.get_surface(&surface, &qh, ());
    parent_modifier.set_multiplier(factor(percent));

    // Optional child with a multiplier of its own: only the window's own surface may be faded
    // by `percent`.
    let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
    if let Some(child_percent) = child_percent {
        let subcompositor: wl_subcompositor::WlSubcompositor =
            globals.bind(&qh, 1..=1, ()).expect("wl_subcompositor");
        let child = compositor.create_surface(&qh, ());
        let sub = subcompositor.get_subsurface(&child, &surface, &qh, ());
        sub.set_position(150, 100);
        let child_data = pixels(CHILD, CHILD, |_, _| [0, 255, 0, 255]);
        let child_buffer = buffer(&shm, &qh, CHILD, CHILD, &child_data);
        child.attach(Some(&child_buffer), 0, 0);
        let child_modifier = modifier.get_surface(&child, &qh, ());
        child_modifier.set_multiplier(factor(child_percent));
        // Subsurface is synchronized: the child takes effect with the parent's commit below.
        child.commit();
        keep.push(Box::new((child, sub, child_buffer, child_modifier)));
    }
    surface.commit();
    conn.flush().expect("flush");
    eprintln!(
        "window at {percent}%{}; close it to exit",
        child_percent.map_or(String::new(), |c| format!(", child at {c}%"))
    );

    while !app.done {
        queue.blocking_dispatch(&mut app).expect("dispatch");
    }
    drop(keep);
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            app.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            app.done = true;
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(App: ignore wl_subsurface::WlSubsurface);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore WpAlphaModifierV1);
delegate_noop!(App: ignore WpAlphaModifierSurfaceV1);
