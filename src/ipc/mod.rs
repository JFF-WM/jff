//! Control socket: scripts and other programs ask jff about its views (windows) and tell it
//! what to do. `jff msg <command>` is the CLI.
//!
//! The socket lives at `$XDG_RUNTIME_DIR/jff.<wayland display>.sock` and is exported to
//! everything jff starts as `$JFF_SOCKET`. A connection carries one request (one line of text)
//! and gets one line of JSON back, then the connection closes. The exception is `watch`: it
//! keeps the connection open and streams events.
//!
//! ```text
//! views                      every view
//! focused                    the view that has the keyboard
//! view-info [ID]             one view (the focused one without an ID)
//! close [ID]                 ask a view to close
//! maximize [ID]              maximize a view
//! unmaximize [ID]            put it back
//! toggle-maximize [ID]       maximize, or put back when it already is
//! fullscreen [ID]            make a view fullscreen
//! unfullscreen [ID]          leave fullscreen
//! focus ID                   raise a view and give it the keyboard (switches workspace if needed)
//! workspaces                 the shown workspace and how many there are
//! workspace N|next|prev      show a workspace (N counts from 1)
//! move-to-workspace N [ID]   send a view (the focused one without an ID) to workspace N
//! move-cursor X Y            put the pointer at X, Y (pixels in the shared space, like events)
//! click left|right|middle    press and release that button where the pointer is
//! toggle-overview            open the window overview, or close it when it is open
//! status                     views and workspace state in one reply (for panels)
//! screenshot-area            freeze the monitor under the pointer and pick a rectangle to save
//! screenshot-fullscreen      save a picture of the monitor under the pointer
//! screenshot-window          save a picture of the window the user works in
//! spawn-sh LINE              run LINE through `sh -c` (for panels and scripts)
//! spawn PROG [ARG...]        run PROG (whitespace-split, no quoting)
//! watch [GROUP...]           keep the connection open and stream events (see below)
//! ```
//!
//! `spawn` starts programs in jff's environment: `WAYLAND_DISPLAY`, `DISPLAY` (from
//! xwayland-satellite if it's running), `JFF_SOCKET`, `XCURSOR_THEME`, etc. The child is
//! detached and not waited for, the reply only says it started. Intended for a panel or script
//! running as the same user, which the socket's `0600` permissions already restrict it to.
//!
//! Replies look like `{"ok":true,"view":{...}}`, `{"ok":true,"views":[...]}`, or
//! `{"ok":false,"error":"..."}`. Workspace commands reply
//! `{"ok":true,"workspace":2,"count":9}` (the shown workspace, from 1). `status` replies
//! `{"ok":true,"views":[...],"workspace":2,"count":9}`. A view is
//! `{"id":1,"app_id":"foot","title":"~","x":40,"y":40,"width":800,"height":600,
//!   "maximized":false,"fullscreen":false,"focused":true,"output":"DP-1","workspace":1}`.
//! `id` is assigned when the window appears and never reused while jff runs.
//!
//! Watching: `watch [GROUP...]`
//!
//! `jff msg watch` is for panels. The connection stays open, and jff writes one JSON object per
//! line, an event, every time something a panel cares about changes. Nothing is sent when
//! nothing changed. Every line is UTF-8, ends with `\n`, and has an `"event"` string. Clients
//! ignore events and members they don't know, more may be added. The client must keep its side
//! open: closing it (EOF) ends the watch, and anything sent after the `watch` line is ignored.
//!
//! The first line is always a `snapshot` of the whole state, so a client never has to call
//! `status` too. Cut up for readability:
//!
//! ```text
//! {"event":"snapshot",
//!  "views":[VIEW,...],                     every view, topmost first (a view is described above)
//!  "workspace":2,"count":9,                the shown workspace (from 1) and how many there are
//!  "focused":3,                            id of the view that has the keyboard, or null
//!  "outputs":[{"name":"DP-1","x":0,"y":0,"width":1920,"height":1080,"scanout":false},...],
//!  "overview":false,"screenshot":false,    overview / screenshot selection is open
//!  "locked":false,                         the session is locked
//!  "popups":[POPUP,...],                   mapped menus and popovers
//!  "layers":[LAYER,...]}                   mapped panels, bars, launchers, wallpapers
//! ```
//!
//! Following events are written flat, `{"event":"title","id":5,"title":"..."}`, in an order a
//! client can apply: monitors added or changed, views opened, changes on views already there,
//! the shown workspace, focus, views closed (after the `focus` that moved away from them),
//! monitors removed, overview, screenshot selection, lock. Panels open before views and close
//! after them, menus open after their view or panel and close before it.
//!
//! ```text
//! event            members                                   group
//! view-opened      all members of a view                     views
//! view-closed      id                                        views
//! title            id, title (string or null)                views
//! app-id           id, app_id (string or null)               views
//! geometry         id, x, y, width, height                   views
//! drag             id, on                                    views
//! maximized        id, on                                    views
//! fullscreen       id, on                                    views
//! view-workspace   id, workspace                             views
//! focus            id, previous (view ids, or null)          focus
//! workspace        workspace, count                          workspaces
//! output-added     name, x, y, width, height, scanout        outputs
//! output-changed   name, x, y, width, height, scanout        outputs
//! output-removed   name                                      outputs
//! overview         open                                      modes
//! screenshot       open (the area selection)                 modes
//! lock             on                                        modes
//! popup-opened     id, view, layer, x, y, width, height      popups
//! popup-closed     id                                        popups
//! layer-opened     id, namespace, layer, output, x, y,       layers
//!                  width, height
//! layer-closed     id                                        layers
//! click            button, x, y, target, + see below         clicks
//! ```
//!
//! A popup (menu, popover) belongs to a `view` or to a `layer` (the id of a panel), the other
//! one is null. Its `x`, `y` is its own top-left in the shared space, so a panel can tell where
//! the menu it opened ended up. `layer` is `background`, `bottom`, `top` or `overlay`. Popup and
//! layer ids are numbers of their own, never reused while jff runs.
//!
//! `click` is a mouse button press (`button` is the Linux code, 272 left, 273 right, 274 middle,
//! `x`, `y` the pointer in the shared space). `target` is `view` (with `id`, `app_id`, `title`),
//! `layer` (with `id`, `namespace`, `layer`, `output`, and `popup`, true when the press landed
//! on a menu of that panel) or null for the empty desktop. `namespace` is what
//! `blur { ignore-layer "…" }` matches. A popup also carries `layer_namespace`, the namespace
//! of the panel it belongs to.
//!
//! `scanout` is true while the monitor's primary plane shows a client's buffer directly (direct
//! scanout, no copy), false while it shows a frame jff composited. It changes with an
//! `output-changed` event, so `jff msg watch outputs` shows direct scanout starting and stopping.
//!
//! `drag` says the user started (`on` true) or stopped (`on` false) moving a window by its
//! titlebar, or carrying its thumbnail in the overview (after the pointer moved far enough to
//! be a drag, not a click). In the overview the window itself doesn't move until it's dropped
//! on another monitor, so there may be no `geometry` between the two. Otherwise the `geometry`
//! events in between are the movement, so a panel can tell a drag from a window that moved by
//! itself. It comes before the first `geometry` of the drag and
//! after the last one. A press and release inside one event-loop iteration makes no event.
//!
//! `geometry` is one move or resize event, however many of the four numbers changed, a view
//! gets at most one per event-loop iteration, so a drag is coalesced. `focus` follows the
//! keyboard, so it's `null` while a panel or launcher has the keyboard. The `focused` flag
//! inside a view object is only a value at the time of that object: use the `focus` events to
//! follow it.
//!
//! `watch` takes the groups to report (`views`, `focus`, `workspaces`, `outputs`, `modes` for
//! overview, screenshot selection and lock, `popups`, `layers`). `watch views workspaces` sends
//! only those events and a snapshot with only matching members (`views`, `workspace` and
//! `count`, `focused`, `outputs`, `overview`, `screenshot` and `locked`, `popups`, `layers`).
//! No groups means all of them. An unknown group gets the usual
//! `{"ok":false,...}` reply and the connection closes.
//!
//! jff never waits for a watcher: one that doesn't read fast enough (256 KiB of unsent events)
//! is disconnected, and can just connect again to get a fresh snapshot.

pub mod events;

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context};
use smithay::backend::input::{ButtonState, InputTime};
use smithay::desktop::{layer_map_for_output, PopupManager, Window};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::Layer;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::app::Backend;
use crate::desk::Goto;
use crate::feature::outputs::output_rect;
use crate::input::{spawn_command, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use crate::ipc::events::{
    ClickInfo, ClickTarget, Event, Group, GroupSet, LayerInfo, OutputInfo, PopupInfo, Snapshot,
    View,
};
use crate::primitives::json::json_str;
use crate::protocol::layer_shell::{is_mapped, Under};
use crate::state::JffState;

/// Requests longer than this are dropped.
const MAX_REQUEST: usize = 4096;

/// Bytes of events a slow watcher can have waiting before it's dropped.
pub const MAX_PENDING: usize = 256 * 1024;

/// A watcher may send nothing but EOF. Past this, it's dropped (also keeps one client from
/// keeping the event loop busy reading what's ignored anyway).
const MAX_WATCH_INPUT: usize = 64 * 1024;

/// The number clients use to name a view. Attached to the window when it appears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewId(pub u64);

/// Same for popups and layer surfaces, given the first time one is listed.
struct SurfaceId(u64);

static NEXT_SURFACE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn surface_id(surface: &WlSurface) -> u64 {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(|| {
            SurfaceId(NEXT_SURFACE_ID.fetch_add(1, Ordering::Relaxed))
        });
        states.data_map.get::<SurfaceId>().map_or(0, |id| id.0)
    })
}

// ---- requests ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maximize {
    On,
    Off,
    Toggle,
}

/// The buttons `click` can press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Views,
    Status,
    Focused,
    Info(Option<u64>),
    Close(Option<u64>),
    Maximize(Option<u64>, Maximize),
    Fullscreen(Option<u64>, bool),
    Focus(u64),
    Workspaces,
    Workspace(Goto),
    /// Workspace number, counted from 1, as typed.
    MoveToWorkspace(Option<u64>, usize),
    /// `spawn-sh LINE`: the command line, verbatim.
    SpawnShell(String),
    /// `spawn PROG ARG...`: split on whitespace, no shell, no quoting.
    Spawn(Vec<String>),
    /// `screenshot-area`: start the rectangle selection (reply comes when it has started).
    ScreenshotArea,
    /// `screenshot-fullscreen`: monitor under the pointer.
    ScreenshotFullscreen,
    /// `screenshot-window`: the window the user works in.
    ScreenshotWindow,
    /// `move-cursor X Y`: put the pointer there (clamped to the monitors).
    MoveCursor(i32, i32),
    /// `toggle-overview`: open the overview, or close it when it is open.
    ToggleOverview,
    /// `click left|right|middle`: a press and release at the pointer.
    Click(ClickButton),
    /// `watch [GROUP...]`: keep the connection and stream events. No groups means all of them.
    /// Only the accept loop can carry this out (see `serve`).
    Watch(Vec<Group>),
}

pub const COMMANDS: &str =
    "views, status, focused, view-info, close, maximize, unmaximize, toggle-maximize, \
     fullscreen, unfullscreen, focus, workspaces, workspace, move-to-workspace, spawn, spawn-sh, \
     screenshot-area, screenshot-fullscreen, screenshot-window, move-cursor, click, toggle-overview, watch";

fn parse_workspace_number(word: &str) -> Result<usize, String> {
    word.parse::<usize>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("{word:?} is not a workspace number (they count from 1)"))
}

pub fn parse_request(line: &str) -> Result<Request, String> {
    let line = line.trim();
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else {
        return Err(format!("empty request, commands: {COMMANDS}"));
    };

    // `spawn` commands take the rest of the line verbatim: `spawn-sh` is split by the shell,
    // not here. Strip the command word and exactly one separator.
    match command {
        "spawn-sh" => {
            let rest = line[command.len()..].strip_prefix(' ').unwrap_or("").trim();
            if rest.is_empty() {
                return Err("spawn-sh needs a command line".to_string());
            }
            return Ok(Request::SpawnShell(rest.to_string()));
        }
        "spawn" => {
            let rest: Vec<String> = line[command.len()..]
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            if rest.is_empty() {
                return Err("spawn needs a program".to_string());
            }
            return Ok(Request::Spawn(rest));
        }
        "watch" => {
            let groups = words
                .map(|word| {
                    Group::parse(word).ok_or_else(|| {
                        format!("unknown watch group {word:?}, groups: {}", Group::NAMES)
                    })
                })
                .collect::<Result<Vec<Group>, String>>()?;
            return Ok(Request::Watch(groups));
        }
        _ => {}
    }

    let rest: Vec<&str> = words.clone().collect();
    match (command, rest.as_slice()) {
        ("workspaces", []) => return Ok(Request::Workspaces),
        ("workspaces", _) => return Err("workspaces takes no arguments".to_string()),
        ("toggle-overview", []) => return Ok(Request::ToggleOverview),
        ("toggle-overview", _) => return Err("toggle-overview takes no arguments".to_string()),
        ("status", []) => return Ok(Request::Status),
        ("status", _) => return Err("status takes no arguments".to_string()),
        ("screenshot-area", []) => return Ok(Request::ScreenshotArea),
        ("screenshot-fullscreen", []) => return Ok(Request::ScreenshotFullscreen),
        ("screenshot-window", []) => return Ok(Request::ScreenshotWindow),
        ("screenshot-area" | "screenshot-fullscreen" | "screenshot-window", _) => {
            return Err(format!("{command} takes no arguments"));
        }
        ("move-cursor", [x, y]) => {
            let coord = |word: &str| {
                word.parse::<i32>()
                    .map_err(|_| format!("{word:?} is not a whole number of pixels"))
            };
            return Ok(Request::MoveCursor(coord(x)?, coord(y)?));
        }
        ("move-cursor", _) => return Err("move-cursor needs X and Y".to_string()),
        ("click", ["left"]) => return Ok(Request::Click(ClickButton::Left)),
        ("click", ["right"]) => return Ok(Request::Click(ClickButton::Right)),
        ("click", ["middle"]) => return Ok(Request::Click(ClickButton::Middle)),
        ("click", _) => return Err("click needs left, right or middle".to_string()),
        ("workspace", ["next"]) => return Ok(Request::Workspace(Goto::Next)),
        ("workspace", ["prev"]) => return Ok(Request::Workspace(Goto::Prev)),
        ("workspace", [n]) => {
            return Ok(Request::Workspace(Goto::To(parse_workspace_number(n)? - 1)));
        }
        ("workspace", _) => return Err("workspace needs a number, next or prev".to_string()),
        ("move-to-workspace", [n]) => {
            return Ok(Request::MoveToWorkspace(None, parse_workspace_number(n)?));
        }
        ("move-to-workspace", [n, id]) => {
            let id = id
                .parse::<u64>()
                .map_err(|_| format!("{id:?} is not a view id"))?;
            return Ok(Request::MoveToWorkspace(
                Some(id),
                parse_workspace_number(n)?,
            ));
        }
        ("move-to-workspace", _) => {
            return Err(
                "move-to-workspace needs a workspace number and optionally a view id".to_string(),
            );
        }
        _ => {}
    }
    let id = match words.next() {
        None => None,
        Some(word) => Some(
            word.parse::<u64>()
                .map_err(|_| format!("{word:?} is not a view id"))?,
        ),
    };
    if words.next().is_some() {
        return Err(format!("too many arguments to {command}"));
    }
    let no_id = |request| match id {
        None => Ok(request),
        Some(_) => Err(format!("{command} takes no view id")),
    };

    match command {
        "views" => no_id(Request::Views),
        "focused" => no_id(Request::Focused),
        "view-info" => Ok(Request::Info(id)),
        "close" => Ok(Request::Close(id)),
        "maximize" => Ok(Request::Maximize(id, Maximize::On)),
        "unmaximize" => Ok(Request::Maximize(id, Maximize::Off)),
        "toggle-maximize" => Ok(Request::Maximize(id, Maximize::Toggle)),
        "fullscreen" => Ok(Request::Fullscreen(id, true)),
        "unfullscreen" => Ok(Request::Fullscreen(id, false)),
        "focus" => id
            .map(Request::Focus)
            .ok_or_else(|| "focus needs a view id".to_string()),
        other => Err(format!("unknown command {other:?}, commands: {COMMANDS}")),
    }
}

fn error_reply(message: &str) -> String {
    format!("{{\"ok\":false,\"error\":{}}}\n", json_str(message))
}

// ---- running requests -------------------------------------------------------------------------

impl JffState {
    /// Give a new window its id.
    pub fn assign_view_id(&mut self, window: &Window) {
        let id = self.next_view_id;
        self.next_view_id += 1;
        window.user_data().insert_if_missing(|| ViewId(id));
    }

    /// Carry out one request, return the reply line.
    fn ipc_reply(&mut self, request: Request) -> String {
        match self.run_request(request) {
            Ok(reply) => reply,
            Err(message) => error_reply(&message),
        }
    }

    fn run_request(&mut self, request: Request) -> Result<String, String> {
        match request {
            Request::Views => {
                let views = self.views_json();
                Ok(format!("{{\"ok\":true,\"views\":[{}]}}\n", views.join(",")))
            }
            Request::Status => {
                let views = self.views_json();
                Ok(format!(
                    "{{\"ok\":true,\"views\":[{}],\"workspace\":{},\"count\":{}}}\n",
                    views.join(","),
                    self.current_workspace() + 1,
                    self.workspace_count(),
                ))
            }
            Request::Focused => Ok(match self.focused_window() {
                Some(window) => format!("{{\"ok\":true,\"view\":{}}}\n", self.view_json(&window)),
                None => "{\"ok\":true,\"view\":null}\n".to_string(),
            }),
            Request::Info(id) => {
                let window = self.target_window(id)?;
                Ok(format!(
                    "{{\"ok\":true,\"view\":{}}}\n",
                    self.view_json(&window)
                ))
            }
            Request::Close(id) => {
                let window = self.target_window(id)?;
                let toplevel = window.toplevel().ok_or("the view cannot be closed")?;
                toplevel.send_close();
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::Maximize(id, how) => {
                let window = self.target_window(id)?;
                let toplevel = window
                    .toplevel()
                    .cloned()
                    .ok_or("the view cannot be maximized")?;
                let now = self.desk.is_maximized(|w| *w == window);
                let want = match how {
                    Maximize::On => true,
                    Maximize::Off => false,
                    Maximize::Toggle => !now,
                };
                if want != now {
                    self.set_maximized(&toplevel, want);
                }
                Ok(format!(
                    "{{\"ok\":true,\"view\":{}}}\n",
                    self.view_json(&window)
                ))
            }
            Request::Fullscreen(id, want) => {
                let window = self.target_window(id)?;
                let toplevel = window
                    .toplevel()
                    .cloned()
                    .ok_or("the view cannot go fullscreen")?;
                let now = toplevel.with_pending_state(|state| {
                    state.states.contains(xdg_toplevel::State::Fullscreen)
                });
                if want != now {
                    self.set_filled(&toplevel, xdg_toplevel::State::Fullscreen, want);
                }
                Ok(format!(
                    "{{\"ok\":true,\"view\":{}}}\n",
                    self.view_json(&window)
                ))
            }
            Request::Focus(id) => {
                let window = self.target_window(Some(id))?;
                self.show_workspace_of(&window);
                // With the overview open, pick the window right away and close the overview
                // (a plain focus would be taken back on the next frame).
                if self.overview_active() {
                    self.end_overview(Some(window.clone()));
                } else {
                    self.focus_window(&window);
                }
                Ok(format!(
                    "{{\"ok\":true,\"view\":{}}}\n",
                    self.view_json(&window)
                ))
            }
            Request::Workspaces => Ok(self.workspace_reply()),
            Request::Workspace(goto) => {
                let count = self.workspace_count();
                if let Goto::To(n) = goto {
                    if n >= count {
                        return Err(format!("there are only {count} workspaces"));
                    }
                }
                self.goto_workspace(goto);
                Ok(self.workspace_reply())
            }
            Request::MoveToWorkspace(id, number) => {
                let count = self.workspace_count();
                if number > count {
                    return Err(format!("there are only {count} workspaces"));
                }
                let window = self.target_window(id)?;
                self.send_to_workspace(&window, number - 1);
                Ok(format!(
                    "{{\"ok\":true,\"view\":{}}}\n",
                    self.view_json(&window)
                ))
            }
            Request::SpawnShell(line) => {
                spawn_command("sh", &["-c".to_string(), line]);
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::Spawn(argv) => {
                let Some((command, args)) = argv.split_first() else {
                    return Err("spawn needs a program".to_string());
                };
                spawn_command(command, args);
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::ScreenshotArea => self
                .start_screenshot_area()
                .map(|()| "{\"ok\":true}\n".to_string()),
            Request::ScreenshotFullscreen => self
                .screenshot_fullscreen()
                .map(|()| "{\"ok\":true}\n".to_string()),
            Request::ScreenshotWindow => self
                .screenshot_window()
                .map(|()| "{\"ok\":true}\n".to_string()),
            Request::ToggleOverview => {
                // Same as the key binding. It can decline (a move or resize is going on), so the
                // reply says what state it ended in.
                self.toggle_overview();
                Ok(format!(
                    "{{\"ok\":true,\"open\":{}}}\n",
                    self.overview_active()
                ))
            }
            Request::MoveCursor(x, y) => {
                let (x, y) = self.move_cursor((f64::from(x), f64::from(y)));
                Ok(format!("{{\"ok\":true,\"x\":{x},\"y\":{y}}}\n"))
            }
            Request::Click(button) => {
                let (code, name) = match button {
                    ClickButton::Left => (BTN_LEFT, "left"),
                    ClickButton::Right => (BTN_RIGHT, "right"),
                    ClickButton::Middle => (BTN_MIDDLE, "middle"),
                };
                // The same path a real mouse takes, so focus, panels, grabs and `click`
                // events for watchers all behave as for a physical click.
                self.pointer_button(code, ButtonState::Pressed, InputTime::now());
                self.pointer_button(code, ButtonState::Released, InputTime::now());
                let at = self
                    .seat
                    .get_pointer()
                    .map(|p| p.current_location())
                    .unwrap_or_default();
                Ok(format!(
                    "{{\"ok\":true,\"button\":\"{name}\",\"x\":{},\"y\":{}}}\n",
                    at.x.round() as i32,
                    at.y.round() as i32
                ))
            }
            // The connection has to stay open, which a reply can't express. `serve` handles
            // `watch` before it gets here.
            Request::Watch(_) => {
                Err("watch keeps the connection open, it has no single reply".to_string())
            }
        }
    }

    /// Every visible view, top-to-bottom stacking order. `views`, `status` and the `watch`
    /// snapshot all come from here, so they can't disagree.
    pub(crate) fn view_list(&self) -> Vec<View> {
        let focused = self.focused_window();
        self.desk
            .all_top_down()
            .filter(|entry| {
                let size = entry.window.geometry().size;
                size.w > 0 && size.h > 0
            })
            .map(|entry| self.view_data(&entry.window, focused.as_ref()))
            .collect()
    }

    /// Every visible view, as JSON objects, top-to-bottom stacking order.
    fn views_json(&self) -> Vec<String> {
        self.view_list().iter().map(View::to_json).collect()
    }

    fn workspace_reply(&self) -> String {
        format!(
            "{{\"ok\":true,\"workspace\":{},\"count\":{}}}\n",
            self.current_workspace() + 1,
            self.workspace_count()
        )
    }

    /// The window a request is about: the one with `id`, else the focused one.
    fn target_window(&self, id: Option<u64>) -> Result<Window, String> {
        match id {
            Some(id) => self
                .desk
                .find_by(|w| w.user_data().get::<ViewId>() == Some(&ViewId(id)))
                .map(|entry| entry.window.clone())
                .ok_or_else(|| format!("no view with id {id}")),
            None => self
                .focused_window()
                .ok_or_else(|| "no focused view".to_string()),
        }
    }

    /// The window that has the keyboard, if the keyboard is on a window (it can also be on a
    /// panel or launcher).
    pub fn focused_window(&self) -> Option<Window> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        self.desk
            .find_by(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == focus))
            .map(|entry| entry.window.clone())
    }

    /// A view as a JSON object.
    fn view_json(&self, window: &Window) -> String {
        self.view_data(window, self.focused_window().as_ref())
            .to_json()
    }

    /// What's known about a view. `focused` is the window that has the keyboard (asked once by
    /// callers that describe many views).
    fn view_data(&self, window: &Window, focused: Option<&Window>) -> View {
        let id = window.user_data().get::<ViewId>().map_or(0, |id| id.0);
        let pos = self.desk.pos_of(|w| w == window).unwrap_or_default();
        let size = window.geometry().size;
        let maximized = self.desk.is_maximized(|w| w == window);
        let focused = focused.is_some_and(|w| w == window);
        let workspace = self.desk.workspace_of(|w| w == window).unwrap_or(0) + 1;

        let (mut app_id, mut title, mut fullscreen) = (None, None, false);
        if let Some(toplevel) = window.toplevel() {
            (app_id, title) = with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|data| {
                        let data = data.lock().unwrap();
                        (data.app_id.clone(), data.title.clone())
                    })
                    .unwrap_or_default()
            });
            fullscreen = toplevel
                .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Fullscreen));
        }

        let monitor = self.rect_for_window(pos, (size.w, size.h));
        let output = self
            .outputs
            .iter()
            .find(|o| output_rect(o) == monitor)
            .map(|o| o.name());

        View {
            id,
            app_id,
            title,
            x: pos.0,
            y: pos.1,
            width: size.w,
            height: size.h,
            maximized,
            fullscreen,
            focused,
            output,
            workspace,
        }
    }

    /// Everything `watch` reports, right now. The focus is the keyboard's (`focused_window()`),
    /// not the `focused_window` field, which only says what the user picked last.
    pub fn take_snapshot(&self) -> Snapshot {
        let views = self.view_list();
        // A window that has the keyboard but isn't in `views` yet (no size) isn't announced
        // either, so a `focus` event never names a view the client hasn't been told about.
        let focused = views.iter().find(|v| v.focused).map(|v| v.id);
        let outputs = self
            .outputs
            .iter()
            .map(|output| {
                let (x, y, width, height) = output_rect(output);
                OutputInfo {
                    name: output.name(),
                    x,
                    y,
                    width,
                    height,
                    scanout: match &self.backend {
                        Backend::Tty(tty) => tty.scanning_out(output),
                        _ => false,
                    },
                }
            })
            .collect();
        Snapshot {
            views,
            workspace: self.current_workspace() + 1,
            count: self.workspace_count(),
            focused,
            outputs,
            overview: self.overview_active(),
            screenshot: self.screenshot.is_some(),
            locked: self.session_locked(),
            popups: self.popup_list(),
            layers: self.layer_list(),
            dragging: self.dragging_view.or_else(|| self.overview_dragged_view()),
        }
    }

    /// Every mapped menu, of windows and of panels. Position is the menu's own top-left in the
    /// shared space.
    fn popup_list(&self) -> Vec<PopupInfo> {
        let mut out = Vec::new();
        let mut add =
            |root: &WlSurface, origin: (i32, i32), view, layer, layer_namespace: Option<String>| {
                for (popup, offset) in PopupManager::popups_for_surface(root) {
                    if !is_mapped(popup.wl_surface()) {
                        continue;
                    }
                    let size = popup.geometry().size;
                    out.push(PopupInfo {
                        id: surface_id(popup.wl_surface()),
                        view,
                        layer,
                        layer_namespace: layer_namespace.clone(),
                        x: origin.0 + offset.x,
                        y: origin.1 + offset.y,
                        width: size.w,
                        height: size.h,
                    });
                }
            };
        for entry in self.desk.all_top_down() {
            let Some(toplevel) = entry.window.toplevel() else {
                continue;
            };
            let view = entry.window.user_data().get::<ViewId>().map(|id| id.0);
            add(toplevel.wl_surface(), entry.pos, view, None, None);
        }
        for output in &self.outputs {
            let (ox, oy, _, _) = output_rect(output);
            let map = layer_map_for_output(output);
            for surface in map.layers() {
                let Some(geo) = map.layer_geometry(surface) else {
                    continue;
                };
                let layer = Some(surface_id(surface.wl_surface()));
                let origin = (ox + geo.loc.x, oy + geo.loc.y);
                add(
                    surface.wl_surface(),
                    origin,
                    None,
                    layer,
                    Some(surface.namespace().to_string()),
                );
            }
        }
        out.sort_by_key(|p| p.id);
        out
    }

    /// Every mapped panel, bar, launcher and wallpaper.
    fn layer_list(&self) -> Vec<LayerInfo> {
        let mut out = Vec::new();
        for output in &self.outputs {
            let (ox, oy, _, _) = output_rect(output);
            let map = layer_map_for_output(output);
            for surface in map.layers() {
                let Some(geo) = map.layer_geometry(surface) else {
                    continue;
                };
                if !is_mapped(surface.wl_surface()) {
                    continue;
                }
                out.push(LayerInfo {
                    id: surface_id(surface.wl_surface()),
                    namespace: surface.namespace().to_string(),
                    layer: match surface.layer() {
                        Layer::Background => "background",
                        Layer::Bottom => "bottom",
                        Layer::Top => "top",
                        Layer::Overlay => "overlay",
                    }
                    .to_string(),
                    output: output.name(),
                    x: ox + geo.loc.x,
                    y: oy + geo.loc.y,
                    width: geo.size.w,
                    height: geo.size.h,
                });
            }
        }
        out.sort_by_key(|l| l.id);
        out
    }
}

// ---- watchers ---------------------------------------------------------------------------------

/// Bytes waiting to be written to a watcher: what its socket didn't take yet.
pub struct Outbox {
    pub pending: VecDeque<u8>,
    limit: usize,
}

impl Outbox {
    pub fn new(limit: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            limit,
        }
    }

    /// Queue `bytes`. Returns false (and queues nothing) when that would go over the limit.
    pub fn enqueue(&mut self, bytes: &[u8]) -> bool {
        if self.pending.len().saturating_add(bytes.len()) > self.limit {
            return false;
        }
        self.pending.extend(bytes);
        true
    }

    /// Write as much as `to` takes. A full socket (`WouldBlock`) isn't an error, the rest stays
    /// queued for the next try. Any other error means the watcher is gone.
    pub fn flush<W: Write>(&mut self, to: &mut W) -> io::Result<()> {
        while !self.pending.is_empty() {
            match to.write(self.pending.make_contiguous()) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.pending.drain(..n);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// A client that sent `watch` and gets events (see `JffState::publish_events`).
pub struct Subscriber {
    pub id: u64,
    pub stream: UnixStream,
    pub groups: GroupSet,
    pub out: Outbox,
}

impl Subscriber {
    /// Queue `bytes` and write what the socket takes. False means drop this watcher.
    pub fn send(&mut self, bytes: &[u8]) -> bool {
        if !self.out.enqueue(bytes) {
            tracing::warn!(
                "ipc watcher {} does not read its events (over {MAX_PENDING} bytes waiting), \
                 dropping it",
                self.id
            );
            return false;
        }
        self.flush()
    }

    /// Write what's queued. False means drop this watcher.
    pub fn flush(&mut self) -> bool {
        let mut conn: &UnixStream = &self.stream;
        match self.out.flush(&mut conn) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!("ipc watcher {} is gone: {err}", self.id);
                false
            }
        }
    }

    /// The socket became readable or writable: look for EOF, write what's queued. Whatever the
    /// client sends is read and thrown away. False means drop this watcher.
    pub fn service(&mut self) -> bool {
        let mut conn: &UnixStream = &self.stream;
        let mut chunk = [0u8; 512];
        let mut total = 0usize;
        loop {
            match conn.read(&mut chunk) {
                // EOF: the client went away, or closed its sending side (which ends a watch).
                Ok(0) => return false,
                Ok(n) => {
                    total += n;
                    if total > MAX_WATCH_INPUT {
                        tracing::warn!("ipc watcher {} keeps sending, dropping it", self.id);
                        return false;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        self.flush()
    }
}

impl Drop for Subscriber {
    /// Hang up for real: the event loop holds another handle to the same socket (to be woken
    /// by it), so just dropping this one wouldn't make the client see EOF.
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl JffState {
    /// Tell watchers what changed since the last call. Runs once per event-loop iteration and
    /// does nothing but one `is_empty()` check while nobody watches.
    ///
    /// The previous snapshot is only kept while somebody watches (the first watcher's snapshot
    /// is the starting point), so the first diff is against what that watcher was told. Writes
    /// never block: whatever a socket doesn't take stays in the watcher's `Outbox` and goes out
    /// when the event loop says the socket is writable again.
    pub fn publish_events(&mut self) {
        if self.subscribers.is_empty() {
            self.pending_events.clear();
            return;
        }
        let new = self.take_snapshot();
        let mut events: Vec<Event> = match self.last_snapshot.take() {
            Some(old) => events::diff(&old, &new),
            None => Vec::new(),
        };
        // Clicks happened before whatever changed because of them.
        if !self.pending_events.is_empty() {
            let mut clicks = std::mem::take(&mut self.pending_events);
            clicks.append(&mut events);
            events = clicks;
        }
        self.last_snapshot = Some(new);
        if events.is_empty() {
            return;
        }

        // Written once, copied to every watcher that wants the group.
        let lines: Vec<(Group, String)> = events
            .iter()
            .map(|event| (event.group(), format!("{}\n", event.to_json())))
            .collect();
        self.subscribers.retain_mut(|watcher| {
            let mut bytes = Vec::new();
            for (group, line) in &lines {
                if watcher.groups.contains(*group) {
                    bytes.extend_from_slice(line.as_bytes());
                }
            }
            bytes.is_empty() || watcher.send(&bytes)
        });
        self.subscribers_changed();
    }

    /// Queue a `click` event for watchers: the button, where the pointer was, and what was
    /// under it (a window, a panel's namespace, a panel's menu, or nothing). Costs one
    /// `is_empty()` check while nobody watches.
    pub fn record_click(&mut self, button: u32, pos: (f64, f64), under: Option<&Under>) {
        if self.subscribers.is_empty() {
            return;
        }
        let target = match under {
            Some(Under::Window(window, _, _)) => ClickTarget::View {
                id: window.user_data().get::<ViewId>().map(|id| id.0),
                app_id: window.toplevel().and_then(|t| {
                    with_states(t.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|d| d.lock().unwrap().app_id.clone())
                    })
                }),
                title: window.toplevel().and_then(|t| {
                    with_states(t.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|d| d.lock().unwrap().title.clone())
                    })
                }),
            },
            Some(Under::Layer(layer, surface, _)) => {
                let output = self
                    .outputs
                    .iter()
                    .find(|o| {
                        layer_map_for_output(o)
                            .layers()
                            .any(|l| l.wl_surface() == layer.wl_surface())
                    })
                    .map(|o| o.name());
                ClickTarget::Layer {
                    id: surface_id(layer.wl_surface()),
                    namespace: layer.namespace().to_string(),
                    layer: match layer.layer() {
                        Layer::Background => "background",
                        Layer::Bottom => "bottom",
                        Layer::Top => "top",
                        Layer::Overlay => "overlay",
                    }
                    .to_string(),
                    output,
                    // The press landed on a menu of the panel, not the panel itself.
                    popup: surface != layer.wl_surface(),
                }
            }
            None => ClickTarget::None,
        };
        self.pending_events.push(Event::Click(ClickInfo {
            button,
            x: pos.0.round() as i32,
            y: pos.1.round() as i32,
            target,
        }));
    }

    /// Forget the snapshot when the last watcher is gone.
    fn subscribers_changed(&mut self) {
        if self.subscribers.is_empty() {
            self.last_snapshot = None;
        }
    }

    /// Add a watcher and write its snapshot. Returns its id, or `None` when it couldn't be
    /// written to.
    fn subscribe(&mut self, stream: UnixStream, groups: GroupSet) -> Option<u64> {
        // Bring the other watchers up to date first. Then `last_snapshot` is the state right
        // now, the new watcher starts from it, and the next diff holds only what comes later:
        // nobody gets an event twice and nobody misses one.
        self.publish_events();
        let snapshot = match &self.last_snapshot {
            Some(snapshot) => snapshot.clone(),
            None => {
                let snapshot = self.take_snapshot();
                self.last_snapshot = Some(snapshot.clone());
                snapshot
            }
        };

        let id = self.next_subscriber_id;
        self.next_subscriber_id += 1;
        let mut watcher = Subscriber {
            id,
            stream,
            groups,
            out: Outbox::new(MAX_PENDING),
        };
        let line = format!("{}\n", snapshot.to_json(groups));
        if !watcher.send(line.as_bytes()) {
            drop(watcher);
            self.subscribers_changed();
            return None;
        }
        self.subscribers.push(watcher);
        tracing::debug!("ipc watcher {id} added, {} in all", self.subscribers.len());
        Some(id)
    }

    /// The socket of watcher `id` is readable or writable. Returns false when the watcher is
    /// gone (or was already), so the caller stops watching its socket.
    fn subscriber_ready(&mut self, id: u64) -> bool {
        let Some(index) = self.subscribers.iter().position(|w| w.id == id) else {
            return false;
        };
        if self.subscribers[index].service() {
            return true;
        }
        self.subscribers.remove(index);
        self.subscribers_changed();
        false
    }
}

// ---- the socket (compositor side) -------------------------------------------------------------

/// Where the socket for the Wayland display `display` goes.
fn socket_path(display: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("jff.{display}.sock"))
}

/// Start listening and export `JFF_SOCKET`. Returns the socket's path, to delete on exit.
pub fn init(handle: &LoopHandle<'static, JffState>, display: &str) -> anyhow::Result<PathBuf> {
    let path = socket_path(display);
    // A socket file left behind by a crashed jff.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("cannot listen on {}", path.display()))?;
    listener.set_nonblocking(true)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    let clients = handle.clone();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, _state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => serve(&clients, stream),
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(err) => {
                            tracing::warn!("ipc accept failed: {err}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("failed to watch the ipc socket: {}", e.error))?;

    std::env::set_var("JFF_SOCKET", &path);
    tracing::info!("ipc on JFF_SOCKET={}", path.display());
    Ok(path)
}

/// Read one request from `stream` as it arrives, answer it, hang up. Except `watch`, which
/// answers with a snapshot and keeps the stream (see `start_watch`).
fn serve(handle: &LoopHandle<'static, JffState>, stream: UnixStream) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    // The closure needs the loop handle too (to register a watcher's socket).
    let loop_handle = handle.clone();
    let mut request: Vec<u8> = Vec::new();
    let result = handle.insert_source(
        Generic::new(stream, Interest::READ, Mode::Level),
        move |_, stream, state| {
            let mut conn: &UnixStream = &**stream;
            let mut chunk = [0u8; 512];
            loop {
                let finished = match conn.read(&mut chunk) {
                    // The client is done sending: whatever came is the request.
                    Ok(0) => true,
                    Ok(n) => {
                        request.extend_from_slice(&chunk[..n]);
                        if request.len() > MAX_REQUEST {
                            return Ok(PostAction::Remove);
                        }
                        request.contains(&b'\n')
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(PostAction::Continue);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return Ok(PostAction::Remove),
                };
                if finished {
                    let text = String::from_utf8_lossy(&request);
                    let line = text.lines().next().unwrap_or("");
                    let reply = match parse_request(line) {
                        Ok(Request::Watch(groups)) => {
                            start_watch(&loop_handle, state, conn, &groups);
                            // Its own source took over, this one has nothing left to do.
                            return Ok(PostAction::Remove);
                        }
                        Ok(parsed) => {
                            let reply = state.ipc_reply(parsed);
                            // a command may have moved, focused or changed anything
                            state.queue_redraw_all();
                            reply
                        }
                        Err(message) => error_reply(&message),
                    };
                    // Replies are small: if the client can't take one, drop it.
                    let _ = conn.write_all(reply.as_bytes());
                    return Ok(PostAction::Remove);
                }
            }
        },
    );
    if let Err(err) = result {
        tracing::warn!("ipc client not served: {}", err.error);
    }
}

/// Make `conn` a watcher: write its snapshot, then keep it for `JffState::publish_events`.
///
/// The event loop gets its own handle to the socket, edge-triggered for reading and writing.
/// Reading tells us at once when the client hangs up (the watcher is forgotten without waiting
/// for the next event). Writing tells us when a socket that was full has room again, so events
/// that didn't fit go out even if jff is otherwise idle. Retrying on every loop iteration
/// instead would cost work while nothing changes, and would leave the tail of a burst unsent
/// until something else woke the loop.
fn start_watch(
    handle: &LoopHandle<'static, JffState>,
    state: &mut JffState,
    conn: &UnixStream,
    groups: &[Group],
) {
    let (Ok(kept), Ok(polled)) = (conn.try_clone(), conn.try_clone()) else {
        tracing::warn!("ipc watcher not served: cannot duplicate the socket");
        return;
    };
    let Some(id) = state.subscribe(kept, GroupSet::from_groups(groups)) else {
        return;
    };
    let result = handle.insert_source(
        Generic::new(polled, Interest::BOTH, Mode::Edge),
        move |_, _, state| {
            Ok(if state.subscriber_ready(id) {
                PostAction::Continue
            } else {
                // Gone, or dropped for being too slow: this handle closes with the source.
                PostAction::Remove
            })
        },
    );
    if let Err(err) = result {
        tracing::warn!("ipc watcher not served: {}", err.error);
        // Not watching its socket means nobody notices it going away: drop it now.
        if let Some(index) = state.subscribers.iter().position(|w| w.id == id) {
            state.subscribers.remove(index);
            state.subscribers_changed();
        }
    }
}

// ---- the command line client (`jff msg ...`) --------------------------------------------------

/// The socket of a running jff: `$JFF_SOCKET`, else the first live `jff.*.sock` in the runtime
/// directory.
fn find_socket() -> anyhow::Result<UnixStream> {
    if let Some(path) = std::env::var_os("JFF_SOCKET") {
        return UnixStream::connect(&path)
            .with_context(|| format!("cannot connect to {}", Path::new(&path).display()));
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("jff.") && name.ends_with(".sock") {
            if let Ok(stream) = UnixStream::connect(entry.path()) {
                return Ok(stream);
            }
        }
    }
    bail!("no running jff found (JFF_SOCKET is not set)")
}

/// `jff msg <command...>`: send the command, print the reply. Returns the exit code.
pub fn client(args: &[String]) -> anyhow::Result<i32> {
    if args.is_empty() {
        eprintln!(
            "usage: jff msg <command> [view id]\n       jff msg watch [{}]\ncommands: {COMMANDS}",
            Group::NAMES.replace(", ", "|")
        );
        return Ok(2);
    }
    if args[0] == "watch" {
        return watch_client(args);
    }
    let mut stream = find_socket()?;
    stream.write_all(args.join(" ").as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(Shutdown::Write)?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    print!("{reply}");
    Ok(if reply.starts_with("{\"ok\":true") {
        0
    } else {
        1
    })
}

/// `jff msg watch [group...]`: print every line jff sends as it arrives. Returns the exit code:
/// 0 when the connection closed or stdout did (`| head -1`), 1 for an error reply.
fn watch_client(args: &[String]) -> anyhow::Result<i32> {
    let mut stream = find_socket()?;
    stream.write_all(args.join(" ").as_bytes())?;
    stream.write_all(b"\n")?;
    // Unlike other commands, the sending side stays open: jff takes EOF as the end of the watch.

    let mut events = BufReader::new(stream);
    let mut stdout = io::stdout().lock();
    let mut line = Vec::new();
    let mut first = true;
    loop {
        line.clear();
        match events.read_until(b'\n', &mut line) {
            Ok(0) => {
                if first {
                    eprintln!("jff closed the connection without an answer");
                    return Ok(1);
                }
                return Ok(0);
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        }
        // `write_all` and `flush` instead of `print!`: a closed stdout is an error to handle,
        // not a panic.
        let written = stdout.write_all(&line).and_then(|()| stdout.flush());
        match written {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
            Err(err) => return Err(err.into()),
        }
        if first && line.starts_with(b"{\"ok\":false") {
            return Ok(1);
        }
        first = false;
    }
}
