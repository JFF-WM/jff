use std::io::Read;
use std::path::PathBuf;

use smithay::backend::renderer::ImportMemWl;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use crate::config::{Action, Config, ConfigWatch, CONFIG_WATCH_INTERVAL};
use crate::input::spawn_command;
use crate::state::JffState;

pub fn run() -> anyhow::Result<()> {
    // `jff msg ...` only talks to a running instance, it never starts one.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("msg") {
        std::process::exit(crate::ipc::client(&args[2..])?);
    }

    // `jff check-config [FILE]` looks at a config file and says what's wrong with it. It doesn't
    // start the compositor.
    if args.get(1).map(String::as_str) == Some("check-config") {
        std::process::exit(check_config(&args[2..]));
    }

    crate::logging::init();

    let mut event_loop: EventLoop<JffState> = EventLoop::try_new()?;
    let display: Display<JffState> = Display::new()?;

    // SIGTERM (what `wayland-logout` and a service manager send), SIGINT and SIGHUP end the
    // event loop, so what comes after it runs: the satellite is stopped, the session and the
    // display are closed in order. Without this the process dies on the spot and its clients
    // see the connection vanish (xwayland-satellite panics on it).
    let (signal_rx, signal_tx) = std::os::unix::net::UnixStream::pair()?;
    signal_rx.set_nonblocking(true)?;
    signal_tx.set_nonblocking(true)?;
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::low_level::pipe::register(signal, signal_tx.try_clone()?)?;
    }
    event_loop.handle().insert_source(
        Generic::new(signal_rx, Interest::READ, Mode::Level),
        |_, stream, state| {
            let mut buf = [0u8; 16];
            while matches!((&**stream).read(&mut buf), Ok(n) if n > 0) {}
            tracing::info!("signal received, shutting down");
            state.loop_signal.stop();
            Ok(PostAction::Continue)
        },
    )?;

    let mut startup_error = None;
    let config = Config::load().unwrap_or_else(|err| {
        tracing::warn!("invalid config, using defaults: {err:#}");
        startup_error = Some(format!("{err:#}"));
        Config::default()
    });
    // The log file is a config setting, so it can only start now. What was logged before this
    // (reading the file) went to stderr only.
    crate::logging::apply(config.log());

    // Remember the file as it is read now, so saving it later is noticed.
    let mut config_watch = ConfigWatch::new(Config::stamp());
    match Config::path() {
        Some(path) => tracing::info!("watching {} for changes", path.display()),
        None => tracing::info!("no config path, not watching for changes"),
    }

    let (backend, winit_loop, outputs) = super::init(&event_loop.handle(), &config)?;
    let mut state = JffState::new(
        display.handle(),
        event_loop.get_signal(),
        event_loop.handle(),
        backend,
        outputs,
        config,
    );

    // wl_shm offers the formats the renderer can upload (the likes of ABGR8888 and
    // XRGB2101010 besides the two every client has), so clients that draw in them need no
    // conversion first.
    let shm_formats: Vec<_> = state.backend.renderer().shm_formats().collect();
    state.shm_state.update_formats(shm_formats);

    // wp_drm_lease_device_v1 needs the display, which the backend was made without
    if let super::Backend::Tty(tty) = &mut state.backend {
        tty.init_leasing(&state.display_handle);
    }
    if let Some(why) = startup_error {
        state.show_error("Config error, using defaults", &why);
    }

    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_os_string();
    event_loop
        .handle()
        .insert_source(socket, |stream, _, state| state.insert_client(stream))?;

    event_loop.handle().insert_source(
        Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            // SAFETY: the display is never dropped while the loop is running.
            unsafe { display.get_mut().dispatch_clients(state).unwrap() };
            Ok(PostAction::Continue)
        },
    )?;

    // Apply the config when the file is saved: one stat of the file twice a second. A timer
    // rather than inotify, which would lose the file when an editor saves by replacing it.
    event_loop
        .handle()
        .insert_source(
            Timer::from_duration(CONFIG_WATCH_INTERVAL),
            move |_, _, state| {
                if config_watch.poll(Config::stamp()) {
                    tracing::info!("config file changed");
                    state.reload_config();
                }
                TimeoutAction::ToDuration(CONFIG_WATCH_INTERVAL)
            },
        )
        .map_err(|e| anyhow::anyhow!("failed to insert the config watcher: {}", e.error))?;

    // The insert error holds the winit loop, which isn't Send, so anyhow can't wrap it directly.
    if let Some(winit_loop) = winit_loop {
        event_loop
            .handle()
            .insert_source(winit_loop, |event, _, state| state.on_winit_event(event))
            .map_err(|e| anyhow::anyhow!("failed to insert winit event source: {}", e.error))?;
    }

    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    tracing::info!(
        "listening on WAYLAND_DISPLAY={}",
        socket_name.to_string_lossy()
    );

    let ipc_socket = match crate::ipc::init(&event_loop.handle(), &socket_name.to_string_lossy()) {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!("ipc disabled: {err:#}");
            None
        }
    };

    // xwayland-satellite is optional, when present it exports DISPLAY for spawned apps.
    let mut satellite = crate::feature::xwayland::spawn();

    // Has to run after the satellite so DISPLAY ends up in the exported environment.
    crate::feature::portals::setup(state.backend.is_tty());

    // Last on purpose: autostart programs should see WAYLAND_DISPLAY, DISPLAY and the portal vars.
    for action in state.config.autostart() {
        match action {
            Action::Spawn(args) => {
                if let Some((command, rest)) = args.split_first() {
                    tracing::info!("autostart: {command}");
                    spawn_command(command, rest);
                }
            }
            Action::SpawnShell(line) => {
                tracing::info!("autostart: sh -c {line:?}");
                spawn_command("sh", &["-c".to_string(), line.clone()]);
            }
            _ => {}
        }
    }

    // `jff <cmd>` launches a client right away.
    if let Some(cmd) = std::env::args().nth(1) {
        if let Err(err) = std::process::Command::new(&cmd).spawn() {
            tracing::warn!("could not spawn {cmd}: {err}");
        }
    }

    state.start_rendering();

    event_loop.run(None, &mut state, |state| {
        // Settle the state first (popups, focus, resize...), then publish it to clients.
        state.popups.cleanup();
        state.update_keyboard_focus();
        state.reap_globals();
        state.update_resize();
        state.update_idle_inhibited();
        state.service_session_lock();
        state.publish_events();
        state.publish_foreign_toplevels();
        state.publish_ext_foreign_toplevels();
        state.publish_ext_workspaces();
        state.service_image_copy();
        let _ = state.display_handle.flush_clients();
    })?;

    if let Some(path) = ipc_socket {
        let _ = std::fs::remove_file(path);
    }

    // Before the display goes away: the satellite is a client of it.
    if let Some(child) = satellite.as_mut() {
        crate::feature::xwayland::stop(child);
    }

    Ok(())
}

/// Runs `jff check-config`. Reads FILE (or the config jff would normally load) and says if it
/// works. Errors go to stderr, warnings to stdout. Exit code is 0 if the config loads (warnings
/// or no file at all are fine), 1 if it doesn't, 2 if the command line is wrong.
fn check_config(args: &[String]) -> i32 {
    let (path, explicit) = match args {
        [] => (Config::path(), false),
        [file] => (Some(PathBuf::from(file)), true),
        _ => {
            eprintln!("usage: jff check-config [FILE]");
            return 2;
        }
    };
    let Some(path) = path else {
        eprintln!("no config path: JFF_CONFIG, XDG_CONFIG_HOME and HOME are all unset");
        return 2;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !explicit => {
            println!(
                "{}: no file, the built-in defaults are used",
                path.display()
            );
            return 0;
        }
        Err(err) => {
            eprintln!("{}: error: {err}", path.display());
            return 1;
        }
    };
    match Config::check_text(&text) {
        Ok(warnings) if warnings.is_empty() => {
            println!("{}: ok", path.display());
            0
        }
        Ok(warnings) => {
            for warning in &warnings {
                println!("{}: warning: {warning}", path.display());
            }
            println!("{}: ok, with {} warning(s)", path.display(), warnings.len());
            0
        }
        Err(err) => {
            eprintln!("{}: error: {err:#}", path.display());
            1
        }
    }
}
