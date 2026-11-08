//! xdg-session-test: a small Wayland client that exercises xdg-session-management-v1.
//!
//! See README.md for the manual test procedure.

#![allow(unreachable_patterns, irrefutable_let_patterns)]

mod app;
mod draw;
mod proto;

use std::fs;
use std::io;
use std::ops::RangeInclusive;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use wayland_client::backend::WaylandError;
use wayland_client::globals::{registry_queue_init, BindError, GlobalList};
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_display, wl_output::WlOutput, wl_shm::{self, WlShm},
};
use wayland_client::{Connection, DispatchError, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_toplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use app::{App, OutputInfo, SessionStatus};
use proto::client::xdg_session_manager_v1::{self, Reason, XdgSessionManagerV1};
use proto::client::xdg_session_v1;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

const USAGE: &str = "\
Usage: xdg-session-test [OPTIONS]

Opens N toplevel windows, registers them with xdg_session_manager_v1 and, on the next
run, asks the compositor to restore them.

Options:
  -n, --windows N        number of windows (default 2)
      --state-file PATH  where the session id is kept
                         (default ~/.cache/xdg-session-test/session-id)
      --forget           on exit call session.remove and delete the state file
                         (default on exit: session.destroy, the compositor keeps the state)
  -v, --verbose          dump every protocol event
  -h, --help             show this help

Exit codes: 0 ok, 1 error (including protocol errors), 2 the compositor does not
advertise a required global (xdg_session_manager_v1 in particular).
";

struct Args {
    windows: usize,
    state_file: PathBuf,
    forget: bool,
    verbose: bool,
}

enum Parsed {
    Run(Args),
    Help,
}

fn default_state_file() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache/xdg-session-test/session-id")
}

fn parse_windows(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("--windows needs a number >= 1, got {value:?}")),
    }
}

fn parse_args() -> Result<Parsed, String> {
    let mut windows = 2;
    let mut state_file = None;
    let mut forget = false;
    let mut verbose = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--forget" => forget = true,
            "-v" | "--verbose" => verbose = true,
            "-n" | "--windows" => {
                let v = it.next().ok_or("--windows needs a number")?;
                windows = parse_windows(&v)?;
            }
            "--state-file" => {
                let v = it.next().ok_or("--state-file needs a path")?;
                state_file = Some(PathBuf::from(v));
            }
            s if s.starts_with("--windows=") => windows = parse_windows(&s["--windows=".len()..])?,
            s if s.starts_with("--state-file=") => {
                state_file = Some(PathBuf::from(&s["--state-file=".len()..]))
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Parsed::Run(Args {
        windows,
        state_file: state_file.unwrap_or_else(default_state_file),
        forget,
        verbose,
    }))
}

struct Failure {
    code: u8,
    msg: String,
}

impl From<String> for Failure {
    fn from(msg: String) -> Self {
        Failure { code: 1, msg }
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(1);
        }
    };

    // SAFETY: the handler only stores to an atomic.
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            eprintln!("{}", f.msg);
            ExitCode::from(f.code)
        }
    }
}

fn bind<I>(
    globals: &GlobalList,
    qh: &QueueHandle<App>,
    versions: RangeInclusive<u32>,
    interface: &str,
) -> Result<I, Failure>
where
    I: Proxy + 'static,
    App: Dispatch<I, ()> + 'static,
{
    globals
        .bind::<I, App, ()>(qh, versions, ())
        .map_err(|e| match e {
            BindError::NotPresent => Failure {
                code: 2,
                msg: format!("compositor does not advertise {interface}"),
            },
            other => Failure {
                code: 2,
                msg: format!("cannot bind {interface}: {other}"),
            },
        })
}

fn run(args: &Args) -> Result<(), Failure> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("cannot connect to the Wayland display: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)
        .map_err(|e| format!("cannot read the Wayland globals: {e}"))?;
    let qh = queue.handle();

    let compositor: WlCompositor = bind(&globals, &qh, 1..=6, "wl_compositor")?;
    let shm: WlShm = bind(&globals, &qh, 1..=1, "wl_shm")?;
    let wm_base: XdgWmBase = bind(&globals, &qh, 1..=6, "xdg_wm_base")?;
    let manager: XdgSessionManagerV1 = bind(&globals, &qh, 1..=1, "xdg_session_manager_v1")?;

    let mut app = App::new(args.verbose, compositor, shm, wm_base);

    // Outputs, so enter/leave can name the monitor.
    let output_globals: Vec<(u32, u32)> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|g| g.interface == "wl_output")
            .map(|g| (g.name, g.version))
            .collect()
    });
    for (name, version) in output_globals {
        let wl = globals
            .registry()
            .bind::<WlOutput, _, _>(name, version.min(4), &qh, ());
        app.outputs.push(OutputInfo::new(wl, name));
    }
    sync(&mut queue, &mut app)?;
    for info in &app.outputs {
        println!("output: {}", info.describe());
    }

    // Session id from the previous run, if any.
    let saved_id = read_state(&args.state_file);
    println!("state file: {}", args.state_file.display());
    match &saved_id {
        Some(id) => println!("loaded session id: {id}"),
        None => println!("no saved session id, starting a new session"),
    }

    let reason = if saved_id.is_some() {
        Reason::SessionRestore
    } else {
        Reason::Launch
    };
    println!("get_session(reason={reason:?}, session_id={saved_id:?})");
    app.session = Some(manager.get_session(reason, saved_id.clone(), &qh, ()));
    sync(&mut queue, &mut app)?;

    match &app.status {
        SessionStatus::Created(id) => {
            if let Some(old) = &saved_id {
                println!(
                    "note: the compositor had nothing saved for session id {old}, so this is a new session"
                );
            }
            if let Err(e) = write_state(&args.state_file, id) {
                eprintln!(
                    "warning: cannot write state file {}: {e}",
                    args.state_file.display()
                );
            }
        }
        SessionStatus::Restored => {}
        SessionStatus::Replaced => {}
        SessionStatus::Waiting => {
            return Err(Failure::from(
                "the compositor answered get_session with neither created nor restored".to_string(),
            ));
        }
    }

    // Windows. restore_toplevel only when an id was loaded, add_toplevel otherwise.
    let restore = saved_id.is_some();
    for i in 0..args.windows {
        app.new_window(&qh, i, restore);
    }

    loop {
        if STOP.load(Ordering::SeqCst) {
            println!("interrupted, closing");
            break;
        }
        if app.all_closed() {
            println!("all windows closed");
            break;
        }
        if let Some(msg) = app.fatal.take() {
            let _ = shutdown(&mut app, &mut queue, args.forget);
            return Err(Failure::from(msg));
        }
        pump(&conn, &mut queue, &mut app)?;
    }

    let restored = app.windows.iter().filter(|w| w.restored).count();
    println!(
        "summary: {restored}/{} windows restored by the compositor",
        app.windows.len()
    );

    shutdown(&mut app, &mut queue, args.forget)?;
    if args.forget {
        match fs::remove_file(&args.state_file) {
            Ok(()) => println!("state file deleted: {}", args.state_file.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => eprintln!(
                "warning: cannot delete state file {}: {e}",
                args.state_file.display()
            ),
        }
    }
    Ok(())
}

/// Close everything. Windows first (the compositor sees them go while the session is
/// still open), then the session: `destroy` keeps the saved state, `remove` (--forget)
/// drops it.
fn shutdown(app: &mut App, queue: &mut EventQueue<App>, forget: bool) -> Result<(), String> {
    if forget {
        if let Some(session) = app.session.take() {
            session.remove();
            println!("session.remove sent (--forget): the compositor drops the saved state");
        }
    }
    for window in app.windows.iter_mut() {
        window.destroy();
    }
    sync(queue, app)?;
    if let Some(session) = app.session.take() {
        session.destroy();
        println!("session.destroy sent: the compositor keeps the saved state");
        sync(queue, app)?;
    }
    Ok(())
}

fn sync(queue: &mut EventQueue<App>, app: &mut App) -> Result<(), String> {
    queue
        .roundtrip(app)
        .map(|_| ())
        .map_err(|e| explain(e, queue, app))
}

/// One turn of the event loop: dispatch what is queued, flush, wait up to 100 ms for the
/// socket (so Ctrl+C is noticed), dispatch what arrived.
fn pump(conn: &Connection, queue: &mut EventQueue<App>, app: &mut App) -> Result<(), String> {
    queue
        .dispatch_pending(app)
        .map_err(|e| explain(e, queue, app))?;

    if let Err(e) = conn.flush() {
        let would_block = matches!(&e, WaylandError::Io(err) if err.kind() == io::ErrorKind::WouldBlock);
        if !would_block {
            return Err(explain(DispatchError::Backend(e), queue, app));
        }
    }

    let backend = conn.backend();
    let mut pfd = libc::pollfd {
        fd: backend.poll_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
    if ready > 0 {
        queue
            .blocking_dispatch(app)
            .map_err(|e| explain(e, queue, app))?;
    }
    Ok(())
}

fn read_state(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_state(path: &Path, id: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{id}\n"))
}

// ---------------------------------------------------------------------------------------
// Error reporting
// ---------------------------------------------------------------------------------------

fn explain(err: DispatchError, queue: &mut EventQueue<App>, app: &mut App) -> String {
    match err {
        DispatchError::Backend(WaylandError::Protocol(p)) => {
            fmt_protocol(p.code, p.object_id, &p.object_interface, &p.message)
        }
        DispatchError::Backend(WaylandError::Io(e)) => {
            // The compositor may have posted a protocol error just before it closed the
            // socket. Try once to read it.
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::UnexpectedEof
            ) {
                if let Err(DispatchError::Backend(WaylandError::Protocol(p))) =
                    queue.blocking_dispatch(app)
                {
                    return fmt_protocol(p.code, p.object_id, &p.object_interface, &p.message);
                }
            }
            format!(
                "lost the connection to the compositor: {e}\n  \
                 (it may have closed the connection after a protocol error; \
                 run with WAYLAND_DEBUG=1 to see it)"
            )
        }
        other => format!("Wayland dispatch error: {other}"),
    }
}

fn fmt_protocol(code: u32, object_id: u32, interface: &str, message: &str) -> String {
    let name = error_name(interface, code);
    let hint = match name.as_deref() {
        Some("NameInUse") => {
            "\n  hint: two toplevels in this session use the same name; names must be unique"
        }
        Some("AlreadyMapped") => {
            "\n  hint: restore_toplevel must come before the first commit on the surface"
        }
        _ => "",
    };
    format!(
        "protocol error from the compositor: {interface}@{object_id} raised {} (code {code}): {message}{hint}",
        name.as_deref().unwrap_or("an unknown error"),
    )
}

fn error_name(interface: &str, code: u32) -> Option<String> {
    macro_rules! name {
        ($t:ty) => {
            <$t>::try_from(code).ok().map(|e| format!("{e:?}"))
        };
    }
    match interface {
        "wl_display" => name!(wl_display::Error),
        "wl_shm" => name!(wl_shm::Error),
        "xdg_wm_base" => name!(xdg_wm_base::Error),
        "xdg_surface" => name!(xdg_surface::Error),
        "xdg_toplevel" => name!(xdg_toplevel::Error),
        "xdg_session_manager_v1" => name!(xdg_session_manager_v1::Error),
        "xdg_session_v1" => name!(xdg_session_v1::Error),
        _ => None,
    }
}
