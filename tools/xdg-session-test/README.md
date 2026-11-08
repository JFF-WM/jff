# xdg-session-test

A small Wayland client that exercises `xdg-session-management-v1`, to check whether a
compositor saves window positions across client restarts and restores them. It opens N
toplevel windows, registers them in a session, and on the next run asks the compositor
to restore them. It does not depend on any particular compositor.

## Before you build: use the real protocol XML

`protocols/xdg-session-management-v1.xml` in this crate is a **reconstruction** from the
interface description. Opcodes follow the order of requests and events in the XML, so
this file must match the compositor's copy exactly. Overwrite it with the one from the
jff tree:

    cp /path/to/jff/protocols/xdg-session-management-v1.xml protocols/

The client expects these names (change `src/app.rs` if your XML differs):
`xdg_session_manager_v1.get_session(reason enum, session_id)`,
`xdg_session_v1` events `created/restored/replaced` and requests
`destroy/remove/add_toplevel/restore_toplevel/remove_toplevel`,
`xdg_toplevel_session_v1` event `restored` and request `destroy`.

## Build

    cargo build --release

Needs a Rust toolchain and network access for crates.io (wayland-client 0.31,
wayland-protocols 0.32, wayland-scanner 0.31, memmap2, tempfile, libc). No system
libraries are needed (pure-Rust wayland backend).

## Run

    cargo run --release -- [OPTIONS]

    -n, --windows N        number of windows (default 2)
        --state-file PATH  session id file (default ~/.cache/xdg-session-test/session-id)
        --forget           on exit call session.remove and delete the state file
    -v, --verbose          dump every protocol event
    -h, --help

Run it inside the session of the compositor you are testing (`WAYLAND_DISPLAY` set).
`WAYLAND_DEBUG=1` additionally shows the raw wire traffic.

Windows are named `window-0`, `window-1`, ... (stable across runs), titled
`session test window-<i>`, with app_id `xdg-session-test`, each a different solid colour
showing its name, size, restore status and xdg_toplevel states. Closing a window with its
close button closes only that window; the client exits when all are closed or on
Ctrl+C. On a normal exit it destroys the windows and then calls `session.destroy`, so
the compositor keeps the state. With `--forget` it calls `session.remove` instead and
deletes the state file.

Exit codes: `0` ok, `1` error (including compositor protocol errors such as
`name_in_use` or `already_mapped`, printed with the interface, error name and message),
`2` a required global is missing (for the session manager the client prints
`compositor does not advertise xdg_session_manager_v1`).

## What it prints

    session created: <id>                  first run, id saved to the state file
    session restored                       later runs, the id was known
    window-0: restore_toplevel             (add_toplevel on the first run)
    window-0: restored by compositor       xdg_toplevel_session_v1.restored
    window-0: first configure 900x600 states=[maximized, activated]
    window-0: configure ...                every later configure
    window-0: entered output DP-1 pos=(1920,0) mode=2560x1440@60Hz scale=1
    window-0: left output ...
    summary: 2/2 windows restored by the compositor

If a restore was requested but no `restored` event comes, the client says so. If a
restored window gets a 0x0 first configure it prints a warning. A compositor may apply
maximize/fullscreen after the first configure, which is why every configure is printed,
not just the first.

## Manual test procedure

1. Run the client. Both windows open, a new session is created, the id is saved.
2. Move `window-0` to one monitor and `window-1` to another. Resize one of them.
   Maximize the other. Close the client (close buttons or Ctrl+C).
3. Run it again. Expect `session restored` and `window-<i>: restored by compositor` for
   both windows, each opening on the monitor, with the size and in the mode it had before.
4. Run with `--forget`, close it, then run again. Expect a new session
   (`session created`) and no restore.

Also worth trying: `--windows 3` after a run with 2 (the third name is unknown, so
`restore_toplevel` behaves like `add_toplevel` and no `restored` event is sent for it);
two instances sharing one state file (the second takes the session over and the first
prints `session replaced`).

## Notes for testing jff

jff keeps its store in `$XDG_STATE_HOME/jff-wm/sessions` (default
`~/.local/state/jff-wm/sessions`) and logs `xdg-session:` lines through tracing, which
shows what it saved and restored. jff only saves a window once it has been shown, and
replies to an unknown session id with `created` carrying the same id.
