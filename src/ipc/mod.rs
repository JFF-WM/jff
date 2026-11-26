//! Control socket: scripts and other programs ask jff about its windows (windows) and tell it
//! what to do. `jff msg <command>` is the CLI.
//!
//! The socket lives at `$XDG_RUNTIME_DIR/jff.<wayland display>.sock` and is exported to
//! everything jff starts as `$JFF_SOCKET`. A connection carries one request (one line of text)
//! and gets one line of JSON back, then the connection closes. The exception is `watch`: it
//! keeps the connection open and streams events.
//!
//! ```text
//! windows                      every window
//! focused                    the window that has the keyboard
//! window-info [ID]             one window (the focused one without an ID)
//! close [ID]                 ask a window to close
//! maximize [ID]              maximize a window
//! unmaximize [ID]            put it back
//! toggle-maximize [ID]       maximize, or put back when it already is
//! minimize [ID]              put a window out of sight (the focused one without an ID)
//! unminimize [ID]            bring a minimized window back (the one minimized last without an ID)
//! focus-urgent               focus the window that asked for attention last, on its workspace (a minimized one comes back), as the `focus-urgent` bind does, again for the one before
//! move-to-scratchpad [ID]    put a window (the focused one without an ID) into the scratchpad, out of sight
//! scratchpad [ID]            bring an away scratchpad window to the monitor in use, or put a shown one away (without an ID: as the `scratchpad` bind does)
//! fullscreen [ID]            make a window fullscreen
//! unfullscreen [ID]          leave fullscreen
//! focus ID                   raise a window and give it the keyboard (switches workspace if needed)
//! containers                 the tiling containers on the current workspace
//! layout-save NAME [ID]      save the layout of a window's container as NAME (without ID: the focused window's, else the only container's)
//! layout-apply NAME [ID]     lay that container out the way the saved layout NAME says
//! layout-list                the saved container layouts
//! layout-delete NAME         forget a saved layout
//! tag [NAME [ID]]            give a window (the focused one without an ID) a tag of yours, or show its tag without arguments
//! untag [ID]                 take the tag you gave a window away again
//! workspaces                 the shown workspace and how many there are
//! workspace N|next|prev      show a workspace (N counts from 1)
//! move-to-workspace N [ID]   send a window (the focused one without an ID) to workspace N
//! focus-monitor TARGET       put the pointer and the keyboard on another monitor
//! move-to-monitor TARGET [ID]  send a window (the focused one without an ID) to another monitor, the keyboard and the pointer follow
//! move-workspace-to-monitor TARGET  trade the workspace the pointer's monitor shows with the one TARGET shows (needs `workspaces { per-output }`)
//! move-cursor X Y            put the pointer at X, Y (pixels in the shared space, like events)
//! click left|right|middle    press and release that button where the pointer is
//! toggle-overview            open the window overview, or close it when it is open
//! stats                      what happened to the focused window on its way to the screen, as
//!                            JSON (client, shown and display frame rates, what the frames cost
//!                            jff against the refresh interval, sync, commit-to-screen latency
//!                            and drift from the predicted vblank, the buffer's format and
//!                            modifier, whether the client uses fifo, commit timing or explicit
//!                            sync, VRR, HDR,
//!                            direct scanout and why not). Starts counting for 10 seconds after
//!                            each request without drawing anything, so the first reply can have
//!                            `"stats":null`
//! toggle-stats               show or hide the stats overlay (the `toggle-stats` action)
//! status                     windows and workspace state in one reply (for panels)
//! outputs                    every monitor: area, scanout, adaptive sync, colors and HDR
//! hotkeys                    the global hotkeys clients bound (xx-hotkey-v1): who, which trigger, whether it is held
//! keyboard-layouts           the layouts of the keyboard's keymap and the one in use
//! switch-layout next|prev|N|CODE  make another layout the one in use (N counts from 1, CODE is `us`, `br`, ...)
//! hdr on|off|toggle [MONITOR]    turn HDR on a monitor on or off
//! sdr-white [+|-]NITS [MONITOR]  SDR white on an HDR monitor, in cd/m²
//! hdr-set MONITOR KEY VALUE...   change one HDR setting of a monitor (see below)
//! hdr-get MONITOR            HDR settings in use, and the ones changed while jff runs
//! hdr-reset MONITOR          HDR settings of a monitor back to the config file's
//! screenshot-area            freeze the monitor under the pointer and pick a rectangle to save
//! screenshot-fullscreen      save a picture of the monitor under the pointer
//! screenshot-window          save a picture of the window the user works in
//! window-thumbnail ID [MAXW MAXH]  PNG of a window that fits MAXW x MAXH (default 320 x 200)
//!                            at $XDG_RUNTIME_DIR/jff-wm/thumbs/thumb-ID.png, for panel previews,
//!                            reply {"ok":true,"path":...,"width":...,"height":...}
//! clipboard-history          the copies jff keeps of the clipboard, newest first (see `clipboard { history N }`)
//! clipboard-select ID        make one of them the clipboard again, it moves to the front
//! clipboard-clear            forget every copy (and empty the clipboard when jff is the one holding it)
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
//! `jff msg` prints a reply as it came, one line, when its output is a pipe or a file, so
//! `jff msg windows | jq` and scripts get one JSON object per line. On a terminal it indents the
//! JSON for reading, and so does each event of `watch` (with an empty line after it).
//! `jff msg --pretty COMMAND` indents in any case, `jff msg --compact COMMAND` never does, the
//! flag comes before the command. The socket itself always speaks one line.
//!
//! Replies look like `{"ok":true,"window":{...}}`, `{"ok":true,"windows":[...]}`, or
//! `{"ok":false,"error":"..."}`. Workspace commands reply
//! `{"ok":true,"workspace":2,"count":9}` (the shown workspace, from 1). `status` replies
//! `{"ok":true,"windows":[...],"workspace":2,"count":9}`. `TARGET` of the three monitor commands is `left`,
//! `right`, `up`, `down` (the monitor that lies that way from the one in use, from the window's
//! for `move-to-monitor`), `next`, `prev` (left to right, then top to bottom, wrapping) or the
//! name of a monitor as `outputs` lists it. `focus-monitor` replies `{"ok":true,"output":"DP-2"}`,
//! `move-to-monitor` `{"ok":true,"output":"DP-2","window":{...}}` (the window is on the workspace that
//! monitor shows), `move-workspace-to-monitor`
//! `{"ok":true,"output":"DP-2","workspace":2,"count":9}` with the workspace now shown there. A target
//! that is not there (the edge of the desktop, a name that is not connected) is an error reply.
//! A window is
//! `{"id":1,"app_id":"foot","title":"~","x":40,"y":40,"width":800,"height":600,
//!   "maximized":false,"fullscreen":false,"focused":true,"urgent":false,"output":"DP-1",
//!   "workspace":1,"pid":4242,"container":null,"tag":null,"icon":null}`.
//! `id` is assigned when the window appears and never reused while jff runs. `urgent` is true
//! while the window asked for attention (an xdg-activation token without input, a terminal's
//! bell or a finished download) and hasn't had the keyboard since, the `urgent` event tells when it
//! changes. `pid` is the
//! process id of the client that owns the window (null when unknown), an X11 window reports
//! xwayland-satellite's pid. `container` is `{"id":1,"name":"Terminals"}` for a window that is a
//! member of a tiling container (`name` is null for a container without one), null for any
//! other window. `tag` is the tag that `window-rule { match tag=".." }` sees: the one given with the
//! `tag` command or a rule's `tag` line, else the one the app set itself with xdg-toplevel-tag, null
//! when there is none, the `tag` event tells when it changes. It is in `windows`, `focused`, `window-info`, `status` and the `watch` snapshot.
//!
//! `scratchpad` is true for a window that belongs to the scratchpad, whether it is shown or away,
//! and `hidden` for a window that is out of sight on purpose: minimized, or away in the scratchpad.
//! So `scratchpad && hidden` is an away scratchpad window, `scratchpad && !hidden` one that is
//! shown, and `hidden && !scratchpad` a minimized one (`minimized` is that last case, and still
//! has its own event). The `scratchpad` and `hidden` events tell when they change. They are in the
//! same places as the rest of a window. `move-to-scratchpad` puts a window away into the
//! scratchpad, `scratchpad ID` brings that window to the monitor in use when it is away and puts
//! it away when it is shown, `scratchpad` alone does what the bind does (the one away longest
//! comes, or the focused scratchpad window goes). Both reply `{"ok":true,"window":{...}}`, except
//! `scratchpad` alone, `{"ok":true}`. A window that is not in the scratchpad, an empty scratchpad
//! and a window that cannot be hidden (it is hidden already, or a fullscreen member of a
//! container) are error replies. Sending a window to another workspace takes it out of the
//! scratchpad.
//!
//! `icon` of a window is what the app set with xdg-toplevel-icon, `{"name":"org.example.App",
//! "path":"/usr/share/icons/.../app.png"}`, null when it set none. `name` is the theme name it gave
//! (null for pictures only) and `path` a file a bar can load: the theme's own file when the name can
//! be found, else a PNG of the largest picture the app set, which jff wrote to
//! `$XDG_RUNTIME_DIR/jff-wm/icons` (null when it cannot be written). The `icon` event tells when it
//! changes. It is in the same places as the rest of a window.
//!
//! `containers` replies `{"ok":true,"workspace":2,"containers":[CONTAINER,...]}`, the
//! containers on the shown workspace, oldest first. A container is
//! `{"id":1,"name":null,"workspace":2,"output":"DP-1","x":0,"y":0,"width":1920,"height":1080,
//!   "windows":[3,4],"focused":4,"maximized":null}`: `windows` are the window ids of its windows in tree
//! order, `focused` the one that has the focus inside it, `maximized` the one that fills it
//! (null when none does), and `x`, `y`, `width` and `height` the rectangle it is laid out in
//! (all null before it has been laid out).
//!
//! `layout-save` replies `{"ok":true,"name":"Dev","layout":"h0.7(_,@browser)","slots":[null,"browser"]}`, `layout-apply` replies `{"ok":true,"name":"Dev","slots":2,"empty":0,"extra":0}`, `layout-list` replies `{"ok":true,"layouts":[{"name":..,"layout":..,"slots":[..]}]}`, a tagged window goes to the slot with its tag, layouts are in `$XDG_STATE_HOME/jff-wm/container-layouts`.
//!
//! `clipboard-history` replies `{"ok":true,"enabled":true,"limit":10,"entries":[{"id":3,"time":1767225600,"size":42,"mimes":["text/plain,charset=utf-8"],"preview":"the text, on one line","current":true}]}`.
//! `id` is what `clipboard-select` takes, `time` is unix seconds, `size` bytes over all the mime types, `preview` the start of the text (`[image/png 12.3 KiB]` for anything else), `current` is true for the newest copy.
//! The history lives in memory only. A copy that a password manager marks as a secret is never in it.
//! `clipboard-select` and `clipboard-clear` reply `{"ok":true}`, `clipboard-select` with an id that is not there is an error reply.
//!
//! `outputs` replies `{"ok":true,"outputs":[OUTPUT,...]}`, each OUTPUT what `watch` describes
//! below (`name`, `x`, `y`, `width`, `height`, `workspace`, `scanout`, `adaptive_sync`, `color`, `hdr`), as it is
//! now, plus `focused`: true on the monitor the user is working on (the one the pointer is on), false on the others, and only here, not in `watch` or the snapshot. A script that wants one answer gets it here, a panel that wants to follow the changes
//! uses `watch outputs`.
//!
//! `hotkeys` replies `{"ok":true,"enabled":true,"hotkeys":[HOTKEY,...]}`, `enabled` being whether
//! `global-hotkeys` is on (off: the list is empty), the HOTKEYs in the order they were bound, which
//! is the order that decides a clash (first come, first served). Each is
//! `{"app_id":"org.mumble.Mumble","description":"Push to talk","trigger":"Ctrl+F13","kind":"key",
//!   "code":65482,"modifiers":["ctrl"],"tap":false,"seat":null,"held":false}`: `app_id` is what the
//! client claims (null when it gave none, a client can say anything), `trigger` the key or button as
//! a binding writes it (`Super+Shift+b`, `BTN_SIDE`), `kind` `key` or `button`, `code` the keysym
//! number or the button code, `modifiers` the ones that must be held (lowercase), `tap` a lone
//! modifier that fires on release, `seat` the seat name it is limited to (null: all), and `held`
//! whether its key or button is down now (a tap never is).
//!
//! `keyboard-layouts` replies `{"ok":true,"active":2,"layouts":[LAYOUT,...]}` with the layouts of the
//! keymap on the seat in keymap order and `active`, the `index` of the one in use (null when the seat has no
//! keyboard). A LAYOUT is `{"index":2,"name":"Portuguese (Brazil)","code":"br","variant":"abnt2"}`:
//! `index` counts from 1, `name` is what xkb calls the layout, `code` and `variant` the entries of
//! the config's `keyboard { layout }` and `{ variant }` it was built from (`variant` is null for the
//! default one). Both are null when jff can't tell which entry belongs to a layout: a client
//! (a virtual keyboard) set its own keymap, or the config lists fewer entries than the keymap has
//! layouts. `switch-layout` makes another layout the one in use, as the xkb group switch keys
//! do, and replies like `keyboard-layouts` does, after the switch. `next` and `prev` wrap around,
//! `N` is an `index`, `CODE` a `code` (any case). A layout that is not there is an error reply.
//! The `keyboard-layout` event tells a bar when the layout in use changes, by any means.
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
//!  "windows":[VIEW,...],                     every window, topmost first (a window is described above)
//!  "workspace":2,"count":9,                the shown workspace (from 1) and how many there are
//!  "focused":3,                            id of the window that has the keyboard, or null
//!  "outputs":[{"name":"DP-1","x":0,"y":0,"width":1920,"height":1080,"scanout":false,
//!              "adaptive_sync":false,"color":COLOR,"hdr":HDR},...],
//!  "overview":false,"overview_output":null,   overview is open, and on which monitor (null while closed)
//!  "screenshot":false,                   screenshot selection is open
//!  "locked":false,                         the session is locked
//!  "popups":[POPUP,...],                   mapped menus and popovers
//!  "layers":[LAYER,...],                   mapped panels, bars, launchers, wallpapers
//!  "dbus_annotations":[ANNOTATION,...],    DBus objects apps tied to a window or to themselves (xdg-dbus-annotation, KDE appmenu, gtk_shell1)
//!  "keyboard":{"active":1,"layouts":[LAYOUT,...]}}    the keyboard layouts and the one in use (see `keyboard-layouts`)
//! ```
//!
//! Following events are written flat, `{"event":"title","id":5,"title":"..."}`, in an order a
//! client can apply: monitors added or changed, windows opened, changes on windows already there,
//! the shown workspace, focus, windows closed (after the `focus` that moved away from them),
//! monitors removed, overview, screenshot selection, lock, keyboard layout. Panels open before windows and close
//! after them, menus open after their window or panel and close before it.
//!
//! ```text
//! event            members                                   group
//! window-opened      all members of a window                     windows
//! window-closed      id                                        windows
//! title            id, title (string or null)                windows
//! app-id           id, app_id (string or null)               windows
//! tag              id, tag (string or null)                  windows
//! icon             id, icon (object or null)                 windows
//! geometry         id, x, y, width, height                   windows
//! drag             id, on                                    windows
//! maximized        id, on                                    windows
//! fullscreen       id, on                                    windows
//! window-workspace   id, workspace                             windows
//! window-container   id, container (see `container` above)     windows
//! scratchpad       id, on                                    windows
//! hidden           id, on                                    windows
//! focus            id, previous (window ids, or null)          focus
//! workspace        workspace, count                          workspaces
//! output-added     name, x, y, width, height, workspace, scanout, adaptive_sync, color, hdr   outputs
//! output-changed   name, x, y, width, height, workspace, scanout, adaptive_sync, color, hdr   outputs
//! output-removed   name                                      outputs
//! overview         open, output (the monitor it opened or closed on)   modes
//! screenshot       open (the area selection)                 modes
//! lock             on                                        modes
//! popup-opened     id, window, layer, x, y, width, height      popups
//! popup-closed     id                                        popups
//! layer-opened     id, namespace, layer, output, x, y,       layers
//!                  width, height
//! layer-closed     id                                        layers
//! dbus-annotation  id, window, pid, interface, service_name,   dbus-annotation
//!                  object_path
//! dbus-annotation-removed  id                                  dbus-annotation
//! click            button, x, y, target, + see below         clicks
//! keyboard-layout  active, layouts (see `keyboard-layouts`)  keyboard
//! ```
//!
//! A popup (menu, popover) belongs to a `window` or to a `layer` (the id of a panel), the other
//! one is null. Its `x`, `y` is its own top-left in the shared space, so a panel can tell where
//! the menu it opened ended up. `layer` is `background`, `bottom`, `top` or `overlay`. Popup and
//! layer ids are numbers of their own, never reused while jff runs.
//!
//! `click` is a mouse button press (`button` is the Linux code, 272 left, 273 right, 274 middle,
//! `x`, `y` the pointer in the shared space). `target` is `window` (with `id`, `app_id`, `title`),
//! `layer` (with `id`, `namespace`, `layer`, `output`, and `popup`, true when the press landed
//! on a menu of that panel) or null for the empty desktop. `namespace` is what
//! `blur { ignore-layer "…" }` matches. A popup also carries `layer_namespace`, the namespace
//! of the panel it belongs to.
//!
//! `workspace` is the workspace the monitor shows now, counting from 1. With `workspaces { per-output }`
//! every monitor has its own, so a bar on each monitor can follow `watch outputs` and show the right
//! one (the top-level `workspace` of the snapshot and of the `workspace` event is only that of the
//! monitor the pointer is on). Without `per-output` all monitors report the shared workspace, and
//! switching it is an `output-changed` event for each of them as well as a `workspace` event.
//!
//! `scanout` is true while the monitor's primary plane shows a client's buffer directly (direct
//! scanout, no copy), false while it shows a frame jff composited. It changes with an
//! `output-changed` event, so `jff msg watch outputs` shows direct scanout starting and stopping.
//!
//! `adaptive_sync` is true while the monitor runs with variable refresh rate, false while it
//! has a fixed refresh. With `adaptive-sync "fullscreen"` it turns on and off as a fullscreen
//! window covers the monitor, and each change is an `output-changed` event too.
//!
//! `color` is how jff takes the monitor's colors to be, and what `wp_color_management_v1`
//! clients are told about it. It changes with an `output-changed` event, for instance when
//! `reload-config` gives the monitor another `icc-profile`:
//!
//! ```text
//! "color":{"profile":"/home/me/.local/share/color/icc/DP-1.icc",   the config's icc-profile, or null
//!          "primaries":"display-p3",       srgb, display-p3, dci-p3, bt2020, adobe-rgb, pal, ntsc,
//!                                          or null for primaries that have no such name
//!          "transfer":"srgb",              srgb, gamma22, gamma28, or power:2.4
//!          "white":"d65",                  d65, d50, dci, or xy:0.3127,0.3290
//!          "gamut":"wide",                 wide, srgb, or unknown
//!          "icc_readable":true}
//! ```
//!
//! `primaries`, `transfer` and `white` come from the profile, and are sRGB's when the monitor has
//! none or jff can't read it, since that is what clients are told then. Primaries and white match
//! a name within a small tolerance (0.004 and 0.003 in xy): a profile's numbers are rounded and
//! adapted to D50 and back, so they are never exactly a standard's. `gamut` is `wide` when the
//! gamut is clearly (5%) larger than sRGB's, `srgb` otherwise, and `unknown` when jff has no
//! profile data for the panel to tell: no `icc-profile`, or one that can't be read. So a monitor
//! that is `srgb` here is one a profile says is sRGB, not just one nobody described.
//! `icc_readable` is true when the profile was found and read. It is false without a profile
//! too, `profile` being null tells that apart from a profile that is missing or broken.
//!
//! `hdr` is what the monitor is asked to do and what it really does, so a bar or a script can
//! show it without guessing. A change of any member is an `output-changed` event, `jff msg hdr`
//! and `jff msg sdr-white` included:
//!
//! ```text
//! "hdr":{"requested":true,         `hdr` is set (config, or `jff msg hdr on`)
//!        "active":true,            the monitor is really drawn and signalled as HDR
//!        "reason":null,            why it is requested and not active, a sentence (the EDID has
//!                                  no HDR static metadata block, the connector's max bpc is too
//!                                  low, the renderer has no 16-bit float target, ...), else null
//!        "eotf":"PQ",              PQ or HLG, null while not active
//!        "peak":1000,              cd/m² the frame is mapped to and the monitor is told
//!        "peak_source":"edid",     edid, hdr-peak, or assumed when neither gave a peak
//!        "sdr_white":203,          cd/m², always the configured one, also while HDR is off
//!        "bits":10,                bits per color the picture is carried and dithered at
//!        "night_light":false,      a night light tint is applied
//!        "content_light":{"max_cll":800,"max_fall":400},
//!                                  light levels of the window on the primary plane, null when
//!                                  none is there or it states none, either member may be null
//!        "scanout":{"active":false,"reason":"the surface is not PQ in BT.2020"}}
//! ```
//!
//! `scanout.active` is the monitor's `scanout`. `scanout.reason` says why an HDR monitor does not
//! scan a window out directly (no fullscreen window, something drawn over it, the buffer is not a
//! dmabuf, ...), and is null while it does and on a monitor that is not active. On a monitor
//! without HDR, and on winit (which simulates it), `eotf`, `peak`, `peak_source` and `bits` are
//! null or the simulation's, there is no night light, plane or `content_light`.
//!
//! `hdr`, `sdr-white`, `hdr-set` and `hdr-reset` change a monitor's config entry while jff runs,
//! on a monitor jff drives directly, and reply `{"ok":true,"output":"DP-1","hdr":true,
//! "active":true,"sdr_white":203}` (what is asked for, what the monitor does, SDR white). The
//! change lasts until `reload-config`, and the `outputs` and `watch` `hdr` object follows it.
//! `hdr-set MONITOR KEY VALUE` takes `peak` (cd/m², 100 to 10000), `tone-map` (`knee` or
//! `bt2390`), `contrast` (0.5 to 2.0), `black-level` (cd/m², -5.0 to 5.0), `saturation` (0.0 to
//! 2.0), `white-balance` (the gain of red, green and blue, each 0.5 to 1.5: `1.0 0.95 0.9`) and
//! `gamut-map` (`clip` or `compress`), each also as in the config with `hdr-` in front. The value
//! `default` puts the default back, a value out of range is refused. `hdr-reset MONITOR` gives a
//! monitor the `hdr`, `sdr-white` and these settings the config file says (the defaults when the
//! file has no entry for it). It reads the file for that and applies nothing else of it, so
//! other runtime changes and the rest of the config stay as they are.
//!
//! `hdr-get MONITOR` replies with the settings in use and what differs from the config file:
//! `{"ok":true,"output":"DP-1","settings":{"hdr":true,"sdr_white":250,"sdr_transfer":null,
//! "peak":1000,"min_luminance":null,"tone_map":"bt2390","contrast":1.2,"black_level":null,
//! "saturation":null,"white_balance":[1.0,0.95,0.9],"gamut_map":null,"content_metadata":null},
//! "changed":["sdr_white","contrast"],"kdl":"sdr-white 250\nhdr-contrast 1.2"}`. `settings` has
//! every HDR setting of the monitor, also those `hdr-set` does not change (`sdr_transfer`,
//! `min_luminance` in cd/m², `content_metadata`). `null` in it is the default (the EDID's for
//! `peak` and `min_luminance`, on for `content_metadata`). `changed` names the settings that differ from the file, and `kdl` holds the lines for
//! the monitor's `output` block that give the file those values, one per change. A setting that
//! is at its default now but has a value in the file comes as `// remove hdr-contrast`. When the
//! file cannot be read, `changed` and `kdl` are null and `settings` is still there. It is a
//! request, not part of `outputs` and `watch`.
//!
//! `drag` says the user started (`on` true) or stopped (`on` false) moving a window by its
//! titlebar, or carrying its thumbnail in the overview (after the pointer moved far enough to
//! be a drag, not a click). In the overview the window itself doesn't move until it's dropped
//! on another monitor, so there may be no `geometry` between the two. Otherwise the `geometry`
//! events in between are the movement, so a panel can tell a drag from a window that moved by
//! itself. It comes before the first `geometry` of the drag and
//! after the last one. A press and release inside one event-loop iteration makes no event.
//!
//! `geometry` is one move or resize event, however many of the four numbers changed, a window
//! gets at most one per event-loop iteration, so a drag is coalesced. `focus` follows the
//! keyboard, so it's `null` while a panel or launcher has the keyboard. The `focused` flag
//! inside a window object is only a value at the time of that object: use the `focus` events to
//! follow it.
//!
//! `watch` takes the groups to report (`windows`, `focus`, `workspaces`, `outputs`, `modes` for
//! overview, screenshot selection and lock, `popups`, `layers`, `clicks`, `dbus-annotation`,
//! `keyboard`).
//! `watch windows workspaces` sends
//! only those events and a snapshot with only matching members (`windows`, `workspace` and
//! `count`, `focused`, `outputs`, `overview` and `overview_output`, `screenshot` and `locked`, `popups`, `layers`,
//! `dbus_annotations`, `keyboard`).
//! No groups means all of them. An unknown group gets the usual
//! `{"ok":false,...}` reply and the connection closes.
//!
//! ANNOTATION (xdg-dbus-annotation-v1, `watch dbus-annotation`) is
//! `{"id":1,"window":3,"pid":4242,"interface":"com.canonical.dbusmenu",
//! "service_name":":1.42","object_path":"/MenuBar"}`. `window` is null for an annotation the app
//! made for itself, `service_name` and `object_path` are null until the app has sent an address.
//! A `dbus-annotation` event comes when an annotation appears and whenever its address changes,
//! `dbus-annotation-removed` when the app drops it, its window closes or the app exits.
//!
//! GTK windows (gtk_shell1) show up the same way, one annotation per address they gave, under the
//! interfaces `org.gtk.Menus:menubar`, `org.gtk.Menus:app-menu`, `org.gtk.Actions:window` and
//! `org.gtk.Actions:application`.
//!
//! jff never waits for a watcher: one that doesn't read fast enough (256 KiB of unsent events)
//! is disconnected, and can just connect again to get a fresh snapshot.

pub mod events;

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context};
use smithay::backend::input::{ButtonState, InputTime};
use smithay::desktop::{layer_map_for_output, PopupManager, Window};
use smithay::input::keyboard::Layout;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::Layer;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::app::Backend;
use crate::desk::Goto;
use crate::feature::outputs::output_rect;
use crate::input::{spawn_command, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use crate::ipc::events::{
    ClickInfo, ClickTarget, ContainerInfo, ContainerRef, DbusAnnotationInfo, Event, Group,
    GroupSet, KeyboardInfo, LayerInfo, OutputColor, OutputInfo, PopupInfo, Snapshot, WindowInfo,
};
use crate::primitives::json::{json_opt, json_str, pretty};
use crate::primitives::layout::MonitorTarget;
use crate::protocol::layer_shell::{is_mapped, Under};
use crate::protocol::xx_hotkey::HotkeyInfo;
use crate::state::JffState;

/// Requests longer than this are dropped.
const MAX_REQUEST: usize = 4096;

/// Bytes of events a slow watcher can have waiting before it's dropped.
pub const MAX_PENDING: usize = 256 * 1024;

/// A watcher may send nothing but EOF. Past this, it's dropped (also keeps one client from
/// keeping the event loop busy reading what's ignored anyway).
const MAX_WATCH_INPUT: usize = 64 * 1024;

/// The number clients use to name a window. Attached to the window when it appears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowId(pub u64);

/// Whether a window asked for attention (an activation token without input, see
/// `XdgActivationHandler`) and hasn't had the keyboard since. Attached to the window. Zero is
/// "not urgent", anything else is the place of its last request in line, a higher number is
/// a later one (`focus-urgent` takes the highest first).
#[derive(Debug, Default)]
struct Urgent(AtomicU64);

/// Whether `window` asked for attention and hasn't had the keyboard since.
pub fn window_is_urgent(window: &Window) -> bool {
    window
        .user_data()
        .get::<Urgent>()
        .is_some_and(|u| u.0.load(std::sync::atomic::Ordering::Relaxed) != 0)
}

/// The next place in line for an urgent window.
static NEXT_URGENT: AtomicU64 = AtomicU64::new(1);

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

/// What `always-on-top`, `not-always-on-top` and `toggle-always-on-top` ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnTop {
    On,
    Off,
    Toggle,
}

/// What `sticky`, `unsticky` and `toggle-sticky` ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stick {
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

/// What `tag` and `untag` ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagOp {
    /// `tag`: say what the tag of the window is.
    Show,
    /// `tag NAME`: give it this one.
    Set(String),
    /// `untag`: take the user's tag away.
    Clear,
}

/// What `switch-layout` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutTarget {
    /// `next`: the layout after the one in use, the first one after the last.
    Next,
    /// `prev`: the layout before the one in use, the last one before the first.
    Prev,
    /// A layout number, counted from 1 as `keyboard-layouts` lists them.
    Number(usize),
    /// The `code` of a layout (`us`), matched without regard to case.
    Code(String),
}

impl LayoutTarget {
    /// Read the word after `switch-layout`: `next`, `prev`, a number from 1, or a layout code.
    pub fn parse(word: &str) -> Result<LayoutTarget, String> {
        match word {
            "next" => Ok(LayoutTarget::Next),
            "prev" => Ok(LayoutTarget::Prev),
            _ if word.bytes().all(|byte| byte.is_ascii_digit()) => word
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .map(LayoutTarget::Number)
                .ok_or_else(|| format!("{word:?} is not a layout number (they count from 1)")),
            _ => Ok(LayoutTarget::Code(word.to_string())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Windows,
    Status,
    /// `outputs`: every monitor, as `watch` describes them.
    Outputs,
    /// `hotkeys`: the global hotkeys clients have bound (xx-hotkey-v1).
    Hotkeys,
    /// `keyboard-layouts`: the layouts of the keymap and the one in use.
    KeyboardLayouts,
    /// `switch-layout next|prev|N|CODE`: make another layout the one in use.
    SwitchLayout(LayoutTarget),
    Focused,
    Info(Option<u64>),
    Close(Option<u64>),
    Maximize(Option<u64>, Maximize),
    /// `minimize` (true) and `unminimize` (false): put a window out of sight, or bring one back.
    Minimize(Option<u64>, bool),
    /// `move-to-scratchpad [ID]`: put a window into the scratchpad, out of sight.
    MoveToScratchpad(Option<u64>),
    /// `scratchpad [ID]`: show an away scratchpad window, or put a shown one away.
    Scratchpad(Option<u64>),
    Fullscreen(Option<u64>, bool),
    /// `always-on-top`, `not-always-on-top` and `toggle-always-on-top`: keep a window above the others, or not.
    AlwaysOnTop(Option<u64>, OnTop),
    /// `sticky`, `unsticky` and `toggle-sticky`: show a window on every workspace of its monitor, or not.
    Sticky(Option<u64>, Stick),
    Focus(u64),
    /// `focus-urgent`: focus the window that asked for attention last.
    FocusUrgent,
    Workspaces,
    /// `containers`: the tiling containers on the shown workspace.
    Containers,
    /// `tag [NAME [ID]]` and `untag [ID]`: what to do with the user's tag of a window.
    Tag(Option<u64>, TagOp),
    /// `layout-save NAME [ID]`: keep the layout of the window's container.
    LayoutSave(String, Option<u64>),
    /// `layout-apply NAME [ID]`: lay the window's container out as the saved layout says.
    LayoutApply(String, Option<u64>),
    /// `layout-list`: the saved layouts.
    LayoutList,
    /// `layout-delete NAME`: forget a saved layout.
    LayoutDelete(String),
    /// `clipboard-history`: the copies kept of the clipboard, newest first.
    ClipboardHistory,
    /// `clipboard-select ID`: make a kept copy the clipboard again.
    ClipboardSelect(u64),
    /// `clipboard-clear`: forget every kept copy.
    ClipboardClear,
    Workspace(Goto),
    /// Workspace number, counted from 1, as typed.
    MoveToWorkspace(Option<u64>, usize),
    /// `focus-monitor TARGET`: the pointer and the keyboard go to that monitor.
    FocusMonitor(MonitorTarget),
    /// `move-to-monitor TARGET [ID]`: send a window to that monitor.
    MoveToMonitor(Option<u64>, MonitorTarget),
    /// `move-workspace-to-monitor TARGET`: trade workspaces with that monitor.
    MoveWorkspaceToMonitor(MonitorTarget),
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
    /// `window-thumbnail ID [MAXW MAXH]`: a small picture of a window, in a file.
    WindowThumbnail(u64, Option<(u32, u32)>),
    /// `move-cursor X Y`: put the pointer there (clamped to the monitors).
    MoveCursor(i32, i32),
    /// `toggle-overview`: open the overview, or close it when it is open.
    ToggleOverview,
    /// `hdr on|off|toggle [MONITOR]` and `sdr-white [+|-]NITS [MONITOR]`: change HDR on a monitor until the config is reloaded.
    Hdr(crate::primitives::hdr_output::HdrChange, Option<String>),
    /// `hdr-set MONITOR KEY VALUE...`: change one HDR setting of a monitor until the config is reloaded or `hdr-reset`.
    HdrSet(String, crate::feature::hdr_control::HdrSetting),
    /// `hdr-get MONITOR`: the HDR settings of a monitor, and which of them were changed while jff runs.
    HdrGet(String),
    /// `hdr-reset MONITOR`: the HDR settings of a monitor back to the config file's, without reloading the rest.
    HdrReset(String),
    /// `stats`: the numbers of the stats overlay for the focused window, as JSON. Starts
    /// counting if nothing is (for a few seconds after the last request, nothing is drawn).
    Stats,
    /// `toggle-stats`: show or hide the stats overlay.
    ToggleStats,
    /// `click left|right|middle`: a press and release at the pointer.
    Click(ClickButton),
    /// `watch [GROUP...]`: keep the connection and stream events. No groups means all of them.
    /// Only the accept loop can carry this out (see `serve`).
    Watch(Vec<Group>),
}

pub const COMMANDS: &str =
    "windows, status, outputs, focused, window-info, close, maximize, unmaximize, toggle-maximize, \
     minimize, unminimize, move-to-scratchpad, scratchpad, fullscreen, unfullscreen, always-on-top, not-always-on-top, toggle-always-on-top, sticky, unsticky, toggle-sticky, focus, focus-urgent, containers, layout-save, layout-apply, layout-list, layout-delete, tag, untag, workspaces, workspace, move-to-workspace, focus-monitor, move-to-monitor, move-workspace-to-monitor, spawn, spawn-sh, \
     screenshot-area, screenshot-fullscreen, screenshot-window, window-thumbnail, move-cursor, click, toggle-overview, clipboard-history, clipboard-select, clipboard-clear, hdr, sdr-white, hdr-set, hdr-reset, hdr-get, stats, toggle-stats, hotkeys, keyboard-layouts, switch-layout, watch";

fn parse_workspace_number(word: &str) -> Result<usize, String> {
    word.parse::<usize>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("{word:?} is not a workspace number (they count from 1)"))
}

/// A monitor target of `focus-monitor`, `move-to-monitor` or `move-workspace-to-monitor`.
fn parse_monitor(word: &str) -> Result<MonitorTarget, String> {
    MonitorTarget::parse(word)
        .ok_or_else(|| format!("{word:?} is not a monitor, use {}", MonitorTarget::WORDS))
}

/// The slot tags of `template` as a JSON array, `null` for a free slot.
fn layout_slots_json(template: &crate::primitives::layout_template::Template) -> String {
    let slots: Vec<String> = template.slots().into_iter().map(json_opt).collect();
    format!("[{}]", slots.join(","))
}

/// The reply of `keyboard-layouts` and `switch-layout`.
fn keyboard_reply(keyboard: &KeyboardInfo) -> String {
    format!("{{\"ok\":true,{}}}\n", keyboard.members())
}

/// The reply of the commands that change HDR: what the monitor is set to now.
fn hdr_report_json(report: &crate::feature::hdr_control::HdrReport) -> String {
    format!(
        "{{\"ok\":true,\"output\":{},\"hdr\":{},\"active\":{},\"sdr_white\":{}}}\n",
        json_str(&report.output),
        report.requested,
        report.active,
        report.sdr_white,
    )
}

/// `layout-save` or `layout-apply` (`command`) for the layout `name` and the window `id`.
fn layout_request(command: &str, name: &str, id: Option<u64>) -> Request {
    if command == "layout-save" {
        Request::LayoutSave(name.to_string(), id)
    } else {
        Request::LayoutApply(name.to_string(), id)
    }
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
        ("hdr", [action]) | ("hdr", [action, _]) => {
            use crate::primitives::hdr_output::HdrChange;
            let change = match *action {
                "on" => HdrChange::On,
                "off" => HdrChange::Off,
                "toggle" => HdrChange::Toggle,
                other => return Err(format!("{other:?} is not on, off or toggle")),
            };
            return Ok(Request::Hdr(change, rest.get(1).map(|s| s.to_string())));
        }
        ("hdr", _) => {
            return Err("hdr needs on, off or toggle and optionally a monitor".to_string())
        }
        ("sdr-white", [value]) | ("sdr-white", [value, _]) => {
            use crate::primitives::hdr_output::HdrChange;
            let not_nits = || format!("{value:?} is not a luminance in cd/m², use 203, +20 or -20");
            let change = if value.starts_with(['+', '-']) {
                HdrChange::SdrWhiteBy(value.parse().map_err(|_| not_nits())?)
            } else {
                HdrChange::SdrWhite(value.parse().map_err(|_| not_nits())?)
            };
            return Ok(Request::Hdr(change, rest.get(1).map(|s| s.to_string())));
        }
        ("sdr-white", _) => {
            return Err(
                "sdr-white needs cd/m² (203, +20, -20) and optionally a monitor".to_string(),
            );
        }
        ("hdr-set", [monitor, key, values @ ..]) if !values.is_empty() => {
            let setting = crate::feature::hdr_control::HdrSetting::parse(key, values)?;
            return Ok(Request::HdrSet(monitor.to_string(), setting));
        }
        ("hdr-set", _) => {
            return Err(format!(
                "hdr-set needs a monitor, a setting and a value (hdr-set DP-1 contrast 1.2), settings: {}",
                crate::feature::hdr_control::HdrSetting::KEYS
            ))
        }
        ("hdr-get", [monitor]) => return Ok(Request::HdrGet(monitor.to_string())),
        ("hdr-get", _) => return Err("hdr-get needs a monitor".to_string()),
        ("hdr-reset", [monitor]) => return Ok(Request::HdrReset(monitor.to_string())),
        ("hdr-reset", _) => return Err("hdr-reset needs a monitor".to_string()),
        ("toggle-overview", []) => return Ok(Request::ToggleOverview),
        ("toggle-overview", _) => return Err("toggle-overview takes no arguments".to_string()),
        ("stats", []) => return Ok(Request::Stats),
        ("stats", _) => return Err("stats takes no arguments".to_string()),
        ("toggle-stats", []) => return Ok(Request::ToggleStats),
        ("toggle-stats", _) => return Err("toggle-stats takes no arguments".to_string()),
        ("status", []) => return Ok(Request::Status),
        ("status", _) => return Err("status takes no arguments".to_string()),
        ("outputs", []) => return Ok(Request::Outputs),
        ("outputs", _) => return Err("outputs takes no arguments".to_string()),
        ("hotkeys", []) => return Ok(Request::Hotkeys),
        ("hotkeys", _) => return Err("hotkeys takes no arguments".to_string()),
        ("keyboard-layouts", []) => return Ok(Request::KeyboardLayouts),
        ("keyboard-layouts", _) => return Err("keyboard-layouts takes no arguments".to_string()),
        ("switch-layout", [word]) => {
            return Ok(Request::SwitchLayout(LayoutTarget::parse(word)?));
        }
        ("switch-layout", _) => {
            return Err(
                "switch-layout needs next, prev, a layout number or a layout code".to_string(),
            );
        }
        ("window-thumbnail", [id]) => {
            let id = id
                .parse::<u64>()
                .map_err(|_| format!("{id:?} is not a window id"))?;
            return Ok(Request::WindowThumbnail(id, None));
        }
        ("window-thumbnail", [id, w, h]) => {
            let id = id
                .parse::<u64>()
                .map_err(|_| format!("{id:?} is not a window id"))?;
            let side = |word: &str| match word.parse::<u32>() {
                Ok(n) if (1..=4096).contains(&n) => Ok(n),
                _ => Err(format!("{word:?} is not a size from 1 to 4096 pixels")),
            };
            return Ok(Request::WindowThumbnail(id, Some((side(w)?, side(h)?))));
        }
        ("window-thumbnail", _) => {
            return Err("window-thumbnail needs ID and optionally MAXW MAXH".to_string());
        }
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
        ("tag", []) => return Ok(Request::Tag(None, TagOp::Show)),
        ("tag", [name]) => return Ok(Request::Tag(None, TagOp::Set((*name).to_string()))),
        ("tag", [name, id]) => {
            let id = id
                .parse::<u64>()
                .map_err(|_| format!("{id:?} is not a window id"))?;
            return Ok(Request::Tag(Some(id), TagOp::Set((*name).to_string())));
        }
        ("tag", _) => return Err("tag takes a name and optionally a window id".to_string()),
        ("layout-save" | "layout-apply", [name]) => {
            return Ok(layout_request(command, name, None));
        }
        ("layout-save" | "layout-apply", [name, id]) => {
            let id = id
                .parse::<u64>()
                .map_err(|_| format!("{id:?} is not a window id"))?;
            return Ok(layout_request(command, name, Some(id)));
        }
        ("layout-save" | "layout-apply", _) => {
            return Err(format!(
                "{command} takes a layout name and optionally a window id"
            ));
        }
        ("layout-list", []) => return Ok(Request::LayoutList),
        ("layout-list", _) => return Err("layout-list takes no arguments".to_string()),
        ("layout-delete", [name]) => return Ok(Request::LayoutDelete((*name).to_string())),
        ("layout-delete", _) => return Err("layout-delete takes a layout name".to_string()),
        ("clipboard-history", []) => return Ok(Request::ClipboardHistory),
        ("clipboard-history", _) => {
            return Err("clipboard-history takes no arguments".to_string());
        }
        ("clipboard-select", [id]) => {
            return id.parse::<u64>().map(Request::ClipboardSelect).map_err(|_| {
                format!("{id:?} is not a clipboard entry id (see clipboard-history)")
            });
        }
        ("clipboard-select", _) => {
            return Err("clipboard-select takes an entry id".to_string());
        }
        ("clipboard-clear", []) => return Ok(Request::ClipboardClear),
        ("clipboard-clear", _) => return Err("clipboard-clear takes no arguments".to_string()),
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
                .map_err(|_| format!("{id:?} is not a window id"))?;
            return Ok(Request::MoveToWorkspace(
                Some(id),
                parse_workspace_number(n)?,
            ));
        }
        ("move-to-workspace", _) => {
            return Err(
                "move-to-workspace needs a workspace number and optionally a window id".to_string(),
            );
        }
        ("focus-monitor", [word]) => return Ok(Request::FocusMonitor(parse_monitor(word)?)),
        ("focus-monitor", _) => {
            return Err(format!(
                "focus-monitor needs one monitor: {}",
                MonitorTarget::WORDS
            ));
        }
        ("move-to-monitor", [word]) => {
            return Ok(Request::MoveToMonitor(None, parse_monitor(word)?));
        }
        ("move-to-monitor", [word, id]) => {
            let id = id
                .parse::<u64>()
                .map_err(|_| format!("{id:?} is not a window id"))?;
            return Ok(Request::MoveToMonitor(Some(id), parse_monitor(word)?));
        }
        ("move-to-monitor", _) => {
            return Err(format!(
                "move-to-monitor needs a monitor ({}) and optionally a window id",
                MonitorTarget::WORDS
            ));
        }
        ("move-workspace-to-monitor", [word]) => {
            return Ok(Request::MoveWorkspaceToMonitor(parse_monitor(word)?));
        }
        ("move-workspace-to-monitor", _) => {
            return Err(format!(
                "move-workspace-to-monitor needs one monitor: {}",
                MonitorTarget::WORDS
            ));
        }
        _ => {}
    }
    let id = match words.next() {
        None => None,
        Some(word) => Some(
            word.parse::<u64>()
                .map_err(|_| format!("{word:?} is not a window id"))?,
        ),
    };
    if words.next().is_some() {
        return Err(format!("too many arguments to {command}"));
    }
    let no_id = |request| match id {
        None => Ok(request),
        Some(_) => Err(format!("{command} takes no window id")),
    };

    match command {
        "windows" => no_id(Request::Windows),
        "focused" => no_id(Request::Focused),
        "containers" => no_id(Request::Containers),
        "window-info" => Ok(Request::Info(id)),
        "untag" => Ok(Request::Tag(id, TagOp::Clear)),
        "close" => Ok(Request::Close(id)),
        "maximize" => Ok(Request::Maximize(id, Maximize::On)),
        "unmaximize" => Ok(Request::Maximize(id, Maximize::Off)),
        "toggle-maximize" => Ok(Request::Maximize(id, Maximize::Toggle)),
        "minimize" => Ok(Request::Minimize(id, true)),
        "unminimize" => Ok(Request::Minimize(id, false)),
        "move-to-scratchpad" => Ok(Request::MoveToScratchpad(id)),
        "scratchpad" => Ok(Request::Scratchpad(id)),
        "fullscreen" => Ok(Request::Fullscreen(id, true)),
        "unfullscreen" => Ok(Request::Fullscreen(id, false)),
        "always-on-top" => Ok(Request::AlwaysOnTop(id, OnTop::On)),
        "not-always-on-top" => Ok(Request::AlwaysOnTop(id, OnTop::Off)),
        "toggle-always-on-top" => Ok(Request::AlwaysOnTop(id, OnTop::Toggle)),
        "sticky" => Ok(Request::Sticky(id, Stick::On)),
        "unsticky" => Ok(Request::Sticky(id, Stick::Off)),
        "toggle-sticky" => Ok(Request::Sticky(id, Stick::Toggle)),
        "focus-urgent" => no_id(Request::FocusUrgent),
        "focus" => id
            .map(Request::Focus)
            .ok_or_else(|| "focus needs a window id".to_string()),
        other => Err(format!("unknown command {other:?}, commands: {COMMANDS}")),
    }
}

fn error_reply(message: &str) -> String {
    format!("{{\"ok\":false,\"error\":{}}}\n", json_str(message))
}

// ---- running requests -------------------------------------------------------------------------

impl JffState {
    /// Give a new window its id.
    pub fn assign_window_id(&mut self, window: &Window) {
        let id = self.next_window_id;
        self.next_window_id += 1;
        window.user_data().insert_if_missing(|| WindowId(id));
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
            Request::Windows => {
                let windows = self.windows_json();
                Ok(format!(
                    "{{\"ok\":true,\"windows\":[{}]}}\n",
                    windows.join(",")
                ))
            }
            Request::Status => {
                let windows = self.windows_json();
                Ok(format!(
                    "{{\"ok\":true,\"windows\":[{}],\"workspace\":{},\"count\":{}}}\n",
                    windows.join(","),
                    self.current_workspace() + 1,
                    self.workspace_count(),
                ))
            }
            Request::Outputs => {
                let active = self.active_output_name();
                let outputs: Vec<String> = self
                    .output_list()
                    .iter()
                    .map(|info| info.to_json_focused(info.name == active))
                    .collect();
                Ok(format!(
                    "{{\"ok\":true,\"outputs\":[{}]}}\n",
                    outputs.join(",")
                ))
            }
            Request::Hotkeys => {
                let hotkeys: Vec<String> =
                    self.hotkey_list().iter().map(HotkeyInfo::to_json).collect();
                Ok(format!(
                    "{{\"ok\":true,\"enabled\":{},\"hotkeys\":[{}]}}\n",
                    self.hotkeys_enabled(),
                    hotkeys.join(",")
                ))
            }
            Request::KeyboardLayouts => Ok(keyboard_reply(&self.keyboard_info())),
            Request::SwitchLayout(target) => {
                let keyboard = self.switch_keyboard_layout(&target)?;
                Ok(keyboard_reply(&keyboard))
            }
            Request::Focused => Ok(match self.focused_window() {
                Some(window) => {
                    format!("{{\"ok\":true,\"window\":{}}}\n", self.window_json(&window))
                }
                None => "{\"ok\":true,\"window\":null}\n".to_string(),
            }),
            Request::Containers => Ok(format!(
                "{{\"ok\":true,\"workspace\":{},\"containers\":[{}]}}\n",
                self.current_workspace() + 1,
                self.container_list()
                    .iter()
                    .map(ContainerInfo::to_json)
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            Request::LayoutSave(name, id) => {
                let window = self.layout_target(id)?;
                let template = self.save_container_layout(&name, &window)?;
                Ok(format!(
                    "{{\"ok\":true,\"name\":{},\"layout\":{},\"slots\":{}}}\n",
                    json_str(&name),
                    json_str(&template.to_text()),
                    layout_slots_json(&template),
                ))
            }
            Request::LayoutApply(name, id) => {
                let window = self.layout_target(id)?;
                let applied = self.apply_container_layout(&name, &window)?;
                Ok(format!(
                    "{{\"ok\":true,\"name\":{},\"slots\":{},\"empty\":{},\"extra\":{}}}\n",
                    json_str(&name),
                    applied.slots,
                    applied.empty,
                    applied.extra,
                ))
            }
            Request::LayoutList => {
                let layouts: Vec<String> = crate::desk::layout_store::load_layouts()
                    .iter()
                    .map(|(name, template)| {
                        format!(
                            "{{\"name\":{},\"layout\":{},\"slots\":{}}}",
                            json_str(name),
                            json_str(&template.to_text()),
                            layout_slots_json(template),
                        )
                    })
                    .collect();
                Ok(format!(
                    "{{\"ok\":true,\"layouts\":[{}]}}\n",
                    layouts.join(",")
                ))
            }
            Request::LayoutDelete(name) => {
                self.delete_container_layout(&name)?;
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::ClipboardHistory => Ok(self.clipboard_history_json()),
            Request::ClipboardSelect(id) => {
                self.clipboard_select(id)?;
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::ClipboardClear => {
                self.clipboard_clear();
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::Info(id) => {
                let window = self.target_window(id)?;
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::Tag(id, op) => {
                let window = self.target_window(id)?;
                let surface = window
                    .toplevel()
                    .map(|toplevel| toplevel.wl_surface().clone())
                    .ok_or("the window cannot be tagged")?;
                match op {
                    TagOp::Show => {}
                    TagOp::Set(name) => {
                        crate::protocol::toplevel_tag::set_user_tag(&surface, Some(name));
                    }
                    TagOp::Clear => crate::protocol::toplevel_tag::set_user_tag(&surface, None),
                }
                let window = window.user_data().get::<WindowId>().map_or(0, |id| id.0);
                // `tag` is what rules see, `app_tag` is what the app set itself, if it did.
                Ok(format!(
                    "{{\"ok\":true,\"id\":{window},\"tag\":{},\"app_tag\":{}}}\n",
                    json_opt(crate::protocol::toplevel_tag::tag_of(&surface).as_deref()),
                    json_opt(crate::protocol::toplevel_tag::app_tag_of(&surface).as_deref()),
                ))
            }
            Request::Close(id) => {
                let window = self.target_window(id)?;
                let toplevel = window
                    .toplevel()
                    .cloned()
                    .ok_or("the window cannot be closed")?;
                self.request_close(&toplevel);
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::Maximize(id, how) => {
                let window = self.target_window(id)?;
                let toplevel = window
                    .toplevel()
                    .cloned()
                    .ok_or("the window cannot be maximized")?;
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
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::Minimize(id, true) => {
                let window = self.target_window(id)?;
                self.minimize_window(&window);
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::Minimize(Some(id), false) => {
                let window = self.target_window(Some(id))?;
                self.unminimize_window(&window);
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            // Without an ID: the window minimized last, as the `unminimize` bind does.
            Request::Minimize(None, false) => {
                let last = self
                    .desk
                    .newest_hidden(false)
                    .map(|entry| entry.window.clone())
                    .ok_or("there is no minimized window")?;
                self.unminimize_window(&last);
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&last)
                ))
            }
            Request::MoveToScratchpad(id) => {
                let window = self.target_window(id)?;
                if !self.move_to_scratchpad_window(&window) {
                    return Err(
                        "the window cannot go into the scratchpad (it is hidden already, or a fullscreen member of a container)"
                            .to_string(),
                    );
                }
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            // Without an ID: what the `scratchpad` bind does.
            Request::Scratchpad(None) => {
                if !self.scratchpad_has_windows() {
                    return Err("the scratchpad is empty".to_string());
                }
                self.toggle_scratchpad();
                Ok("{\"ok\":true}\n".to_string())
            }
            Request::Scratchpad(Some(id)) => {
                let window = self.target_window(Some(id))?;
                if !self.toggle_scratchpad_window(&window) {
                    return Err(format!("window {id} is not in the scratchpad"));
                }
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::Fullscreen(id, want) => {
                let window = self.target_window(id)?;
                let toplevel = window
                    .toplevel()
                    .cloned()
                    .ok_or("the window cannot go fullscreen")?;
                let now = toplevel.with_pending_state(|state| {
                    state.states.contains(xdg_toplevel::State::Fullscreen)
                });
                if want != now {
                    self.set_filled(&toplevel, xdg_toplevel::State::Fullscreen, want);
                }
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::AlwaysOnTop(id, how) => {
                let window = self.target_window(id)?;
                let now = self.desk.is_above(|w| *w == window);
                let want = match how {
                    OnTop::On => true,
                    OnTop::Off => false,
                    OnTop::Toggle => !now,
                };
                if want != now {
                    self.set_always_on_top(&window, want);
                }
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::Sticky(id, how) => {
                let window = self.target_window(id)?;
                let now = self.desk.is_sticky(|w| *w == window);
                let want = match how {
                    Stick::On => true,
                    Stick::Off => false,
                    Stick::Toggle => !now,
                };
                if want != now {
                    if self.desk.container_of(|w| *w == window).is_some() {
                        return Err(
                            "the window is in a container, which is on one workspace: take it out first"
                                .to_string(),
                        );
                    }
                    self.set_sticky(&window, want);
                }
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::FocusUrgent => {
                let window = self.focus_urgent().ok_or("no window is urgent")?;
                Ok(format!(
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
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
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
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
                    "{{\"ok\":true,\"window\":{}}}\n",
                    self.window_json(&window)
                ))
            }
            Request::FocusMonitor(target) => {
                let output = self.focus_monitor(&target)?;
                Ok(format!(
                    "{{\"ok\":true,\"output\":{}}}\n",
                    json_str(&output)
                ))
            }
            Request::MoveToMonitor(id, target) => {
                let window = self.target_window(id)?;
                let output = self.move_to_monitor(&window, &target)?;
                Ok(format!(
                    "{{\"ok\":true,\"output\":{},\"window\":{}}}\n",
                    json_str(&output),
                    self.window_json(&window)
                ))
            }
            Request::MoveWorkspaceToMonitor(target) => {
                let output = self.move_workspace_to_monitor(&target)?;
                Ok(format!(
                    "{{\"ok\":true,\"output\":{},\"workspace\":{},\"count\":{}}}\n",
                    json_str(&output),
                    self.current_workspace() + 1,
                    self.workspace_count()
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
            Request::WindowThumbnail(id, max) => {
                let window = self.target_window(Some(id))?;
                let (max_w, max_h) = max.unwrap_or((320, 200));
                let picture = self.capture_window_thumbnail(&window, max_w, max_h)?;
                let dir = crate::feature::screenshot::save::thumbnail_dir()
                    .ok_or("there is no $XDG_RUNTIME_DIR to put the picture in")?;
                let (width, height) = (picture.width, picture.height);
                let path = crate::feature::screenshot::save::write_thumbnail(
                    &dir,
                    id,
                    picture.rgba,
                    width,
                    height,
                )?;
                Ok(format!(
                    "{{\"ok\":true,\"path\":{},\"width\":{width},\"height\":{height}}}\n",
                    json_str(&path.to_string_lossy())
                ))
            }
            Request::ToggleOverview => {
                // Same as the key binding. It can decline (a move or resize is going on), so the
                // reply says what state it ended in.
                self.toggle_overview();
                Ok(format!(
                    "{{\"ok\":true,\"open\":{},\"output\":{}}}\n",
                    self.overview_active(),
                    self.overview_output_name()
                        .map_or("null".to_owned(), |name| json_str(&name))
                ))
            }
            Request::Hdr(change, output) => {
                let report = self.change_hdr(output.as_deref(), change)?;
                Ok(hdr_report_json(&report))
            }
            Request::HdrSet(output, setting) => {
                let report = self.set_hdr_setting(&output, setting)?;
                Ok(hdr_report_json(&report))
            }
            Request::HdrGet(output) => self.hdr_get(&output),
            Request::HdrReset(output) => {
                let report = self.reset_hdr(&output)?;
                Ok(hdr_report_json(&report))
            }
            Request::Stats => {
                // Counts for a few seconds after this request, drawing nothing. The first
                // answer can be empty: there is nothing measured yet.
                self.stats.request_collect(std::time::Instant::now());
                self.arm_stats_timer();
                self.queue_redraw_all();
                Ok(self.stats.json())
            }
            Request::ToggleStats => {
                self.toggle_stats();
                Ok(format!(
                    "{{\"ok\":true,\"shown\":{}}}\n",
                    self.stats.drawn()
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

    /// Every visible window, top-to-bottom stacking order. `windows`, `status` and the `watch`
    /// snapshot all come from here, so they can't disagree.
    pub(crate) fn window_list(&self) -> Vec<WindowInfo> {
        {
            let memo = self
                .window_list_memo
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let (true, Some(list)) = &*memo {
                return list.clone();
            }
        }
        let list = self.build_window_list();
        let mut memo = self
            .window_list_memo
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if memo.0 {
            memo.1 = Some(list.clone());
        }
        list
    }

    /// From now until [`Self::stop_sharing_window_list`], `window_list` is built once and
    /// handed out again: the publishers of one event-loop iteration all want the same list, and
    /// nothing they do changes a window.
    pub fn share_window_list(&self) {
        *self
            .window_list_memo
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (true, None);
    }

    /// Back to building `window_list` on every call.
    pub fn stop_sharing_window_list(&self) {
        *self
            .window_list_memo
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (false, None);
    }

    fn build_window_list(&self) -> Vec<WindowInfo> {
        let focused = self.focused_window();
        self.desk
            .all_top_down()
            .filter(|entry| {
                let size = entry.window.geometry().size;
                size.w > 0 && size.h > 0
            })
            .map(|entry| self.window_data(&entry.window, focused.as_ref()))
            .collect()
    }

    /// The tiling containers on the shown workspace, oldest first, as `containers` lists them.
    fn container_list(&self) -> Vec<ContainerInfo> {
        let window_id = |window: &Window| window.user_data().get::<WindowId>().map(|id| id.0);
        self.desk
            .containers_on(self.current_workspace())
            .into_iter()
            .map(|container| {
                let of = |id| {
                    self.desk
                        .find_by_id(id)
                        .and_then(|entry| window_id(&entry.window))
                };
                ContainerInfo {
                    id: container.id().get(),
                    name: container.name().map(str::to_owned),
                    workspace: container.workspace() + 1,
                    output: Some(container.output().to_owned()).filter(|name| !name.is_empty()),
                    area: container.area(),
                    windows: self
                        .desk
                        .members(container.id())
                        .into_iter()
                        .filter_map(|entry| window_id(&entry.window))
                        .collect(),
                    focused: of(container.focused()),
                    maximized: container.zoomed().and_then(of),
                }
            })
            .collect()
    }

    /// The container `window` is a member of, as a window says it.
    fn container_ref(&self, window: &Window) -> Option<ContainerRef> {
        let container = self
            .desk
            .container_of(|w| w == window)
            .and_then(|id| self.desk.container(id))?;
        Some(ContainerRef {
            id: container.id().get(),
            name: container.name().map(str::to_owned),
        })
    }

    /// Every visible window, as JSON objects, top-to-bottom stacking order.
    fn windows_json(&self) -> Vec<String> {
        self.window_list().iter().map(WindowInfo::to_json).collect()
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
                .find_by(|w| w.user_data().get::<WindowId>() == Some(&WindowId(id)))
                .map(|entry| entry.window.clone())
                .ok_or_else(|| format!("no window with id {id}")),
            None => self
                .focused_window()
                .ok_or_else(|| "no focused window".to_string()),
        }
    }

    /// The window a layout request is about: the window `id`, else the focused one when it is in a container, else the focused window of the only container on the shown workspace.
    fn layout_target(&self, id: Option<u64>) -> Result<Window, String> {
        if id.is_some() {
            return self.target_window(id);
        }
        let focused = self.focused_window();
        if focused
            .as_ref()
            .is_some_and(|w| self.desk.container_of(|c| c == w).is_some())
        {
            return self.target_window(None);
        }
        let containers = self.desk.containers_on(self.current_workspace());
        match containers.as_slice() {
            [only] => self
                .desk
                .find_by_id(only.focused())
                .map(|entry| entry.window.clone())
                .ok_or_else(|| "the container has no windows".to_string()),
            [] => Err("there is no tiling container on this workspace".to_string()),
            _ => Err("the focused window is not in a container and there are several: pass the id of a window of the one you mean".to_string()),
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

    /// A window as a JSON object.
    fn window_json(&self, window: &Window) -> String {
        self.window_data(window, self.focused_window().as_ref())
            .to_json()
    }

    /// What's known about a window. `focused` is the window that has the keyboard (asked once by
    /// callers that describe many windows).
    fn window_data(&self, window: &Window, focused: Option<&Window>) -> WindowInfo {
        let id = window.user_data().get::<WindowId>().map_or(0, |id| id.0);
        let pos = self.desk.pos_of(|w| w == window).unwrap_or_default();
        let size = window.geometry().size;
        let maximized = self.desk.is_maximized(|w| w == window);
        let minimized = self.desk.is_minimized(|w| w == window);
        let focused = focused.is_some_and(|w| w == window);
        let workspace = self.desk.workspace_of(|w| w == window).unwrap_or(0) + 1;

        let (mut app_id, mut title, mut fullscreen, mut tag, mut icon) =
            (None, None, false, None, None);
        if let Some(toplevel) = window.toplevel() {
            tag = crate::protocol::toplevel_tag::tag_of(toplevel.wl_surface());
            icon = crate::protocol::toplevel_icon::ipc_icon(toplevel.wl_surface());
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

        WindowInfo {
            id,
            app_id,
            title,
            x: pos.0,
            y: pos.1,
            width: size.w,
            height: size.h,
            maximized,
            minimized,
            fullscreen,
            focused,
            urgent: self.is_urgent(window),
            always_on_top: self.desk.is_above(|w| w == window),
            sticky: self.desk.is_sticky(|w| w == window),
            scratchpad: self.desk.is_scratch(|w| w == window),
            hidden: self.desk.is_hidden(|w| w == window),
            output,
            workspace,
            pid: self.window_pid(window),
            container: self.container_ref(window),
            tag,
            icon,
        }
    }

    /// Whether `window` asked for attention and hasn't been focused since.
    pub fn is_urgent(&self, window: &Window) -> bool {
        window_is_urgent(window)
    }

    /// [`JffState::set_urgent`] on for `window`, and an open overview draws again to show the
    /// badge. Returns whether it changed.
    pub fn mark_urgent(&mut self, window: &Window) -> bool {
        let changed = self.set_urgent(window, true);
        if changed && self.overview_active() {
            self.queue_redraw_all();
        }
        changed
    }

    /// The urgent window that asked last, on any workspace, hidden ones included.
    pub fn newest_urgent(&self) -> Option<Window> {
        use std::sync::atomic::Ordering::Relaxed;
        self.desk
            .all_top_down()
            .filter_map(|entry| {
                let since = entry.window.user_data().get::<Urgent>()?.0.load(Relaxed);
                (since != 0).then_some((since, &entry.window))
            })
            .max_by_key(|(since, _)| *since)
            .map(|(_, window)| window.clone())
    }

    /// Mark `window` as wanting attention (`true`) or clear it. Returns whether it changed. A
    /// window that asks again moves to the front of the line for `focus-urgent`.
    pub fn set_urgent(&self, window: &Window, on: bool) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if !on && window.user_data().get::<Urgent>().is_none() {
            return false;
        }
        window.user_data().insert_if_missing(Urgent::default);
        let flag = window.user_data().get::<Urgent>();
        flag.is_some_and(|u| {
            let next = if on {
                NEXT_URGENT.fetch_add(1, Relaxed)
            } else {
                0
            };
            (u.0.swap(next, Relaxed) != 0) != on
        })
    }

    /// Process id of the client that owns `window`, if the socket can tell.
    fn window_pid(&self, window: &Window) -> Option<u32> {
        // The pid of a client does not change, and asking the socket is a system call.
        if let Some(known) = window.user_data().get::<KnownPid>() {
            return Some(known.0);
        }
        let surface = window.toplevel()?.wl_surface();
        let client = surface.client()?;
        let creds = client.get_credentials(&self.display_handle).ok()?;
        let pid = u32::try_from(creds.pid).ok()?;
        window.user_data().insert_if_missing(|| KnownPid(pid));
        Some(pid)
    }

    /// Every monitor, as `watch` and `outputs` describe them.
    fn output_list(&self) -> Vec<OutputInfo> {
        self.outputs
            .iter()
            .map(|output| {
                let (x, y, width, height) = output_rect(output);
                OutputInfo {
                    name: output.name(),
                    x,
                    y,
                    width,
                    height,
                    workspace: self.desk.current_on(&output.name()) + 1,
                    scanout: match &self.backend {
                        Backend::Tty(tty) => tty.scanning_out(output),
                        _ => false,
                    },
                    adaptive_sync: match &self.backend {
                        Backend::Tty(tty) => tty.adaptive_sync(output),
                        _ => false,
                    },
                    color: self.output_color(&output.name()),
                    hdr: match &self.backend {
                        Backend::Tty(tty) => tty.output_hdr(output, &self.config),
                        Backend::Winit(_) => self.winit_output_hdr(output),
                    },
                }
            })
            .collect()
    }

    /// The colors of the monitor `name`: its ICC profile (the config's) and what jff took from
    /// it. Without a profile, or with one that can't be read, that is sRGB.
    fn output_color(&self, name: &str) -> OutputColor {
        let profile = self
            .config
            .output(name)
            .and_then(|config| config.icc_profile.as_deref());
        let readable = profile.is_some() && self.color_management.monitor_has_profile(name);
        OutputColor::new(
            profile.map(|path| path.to_string_lossy().into_owned()),
            &self.color_management.monitor_description(name),
            readable,
        )
    }

    /// Everything `watch` reports, right now. The focus is the keyboard's (`focused_window()`),
    /// not the `focused_window` field, which only says what the user picked last.
    pub fn take_snapshot(&mut self) -> Snapshot {
        let keyboard = self.keyboard_info();
        let windows = self.window_list();
        // A window that has the keyboard but isn't in `windows` yet (no size) isn't announced
        // either, so a `focus` event never names a window the client hasn't been told about.
        let focused = windows.iter().find(|v| v.focused).map(|v| v.id);
        let outputs = self.output_list();
        Snapshot {
            windows,
            workspace: self.current_workspace() + 1,
            count: self.workspace_count(),
            focused,
            outputs,
            overview: self.overview_active(),
            overview_output: self.overview_output_name(),
            screenshot: self.screenshot.is_some(),
            locked: self.session_locked(),
            popups: self.popup_list(),
            layers: self.layer_list(),
            dbus_annotations: self.dbus_annotation_list(),
            keyboard,
            dragging: self
                .dragging_window
                .or_else(|| self.overview_dragged_view()),
        }
    }

    /// The layouts of the seat's keymap and the one in use. `code` and `variant` come from the
    /// config, so they're left out while a client's keymap is on the seat.
    pub(crate) fn keyboard_info(&mut self) -> KeyboardInfo {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return KeyboardInfo::default();
        };
        let (names, active) = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            let names: Vec<String> = xkb
                .layouts()
                .map(|layout| xkb.layout_name(layout).to_owned())
                .collect();
            (names, xkb.active_layout().0 as usize)
        });
        let config = self.config.keyboard();
        let (layout, variant) = match self.active_virtual_keymap {
            Some(_) => ("", ""),
            None => (config.layout.as_str(), config.variant.as_str()),
        };
        KeyboardInfo::new(names, active, layout, variant)
    }

    /// Make the layout `target` names the one in use, and say what the keyboard looks like
    /// after. The focused client hears about it through the modifiers event.
    fn switch_keyboard_layout(&mut self, target: &LayoutTarget) -> Result<KeyboardInfo, String> {
        let keyboard = self.seat.get_keyboard().ok_or("there is no keyboard")?;
        let info = self.keyboard_info();
        let count = info.layouts.len();
        if count == 0 {
            return Err("the keyboard has no layouts".to_string());
        }
        let current = info.active.map_or(0, |index| index - 1);
        let next = match target {
            LayoutTarget::Next => (current + 1) % count,
            LayoutTarget::Prev => (current + count - 1) % count,
            LayoutTarget::Number(n) if (1..=count).contains(n) => n - 1,
            LayoutTarget::Number(n) => {
                return Err(format!("there is no layout {n}, there are {count}"));
            }
            LayoutTarget::Code(code) => info
                .layouts
                .iter()
                .position(|layout| {
                    layout
                        .code
                        .as_deref()
                        .is_some_and(|known| known.eq_ignore_ascii_case(code))
                })
                .ok_or_else(|| {
                    let codes: Vec<&str> = info
                        .layouts
                        .iter()
                        .filter_map(|layout| layout.code.as_deref())
                        .collect();
                    format!(
                        "{code:?} is not a layout of the keyboard, codes: {}",
                        codes.join(", ")
                    )
                })?,
        };
        keyboard.with_xkb_state(self, |mut context| context.set_layout(Layout(next as u32)));
        Ok(self.keyboard_info())
    }

    /// Every DBus annotation of a window, then those of apps that annotated themselves.
    fn dbus_annotation_list(&self) -> Vec<DbusAnnotationInfo> {
        use crate::protocol::xdg_dbus_annotation::{surface_annotations, DbusAnnotation};

        let info = |annotation: DbusAnnotation, window: Option<u64>, pid: Option<u32>| {
            let (service_name, object_path) = match annotation.address {
                Some(address) => (Some(address.service_name), Some(address.object_path)),
                None => (None, None),
            };
            DbusAnnotationInfo {
                id: annotation.id,
                window,
                pid,
                interface: annotation.interface,
                service_name,
                object_path,
            }
        };
        let mut out = Vec::new();
        for entry in self.desk.all_top_down() {
            let Some(toplevel) = entry.window.toplevel() else {
                continue;
            };
            let window = entry.window.user_data().get::<WindowId>().map(|id| id.0);
            let pid = self.window_pid(&entry.window);
            for annotation in surface_annotations(toplevel.wl_surface()) {
                out.push(info(annotation, window, pid));
            }
        }
        for (client, annotations) in self.dbus_annotation.all_client_annotations() {
            // `get_client` takes an object id, not a client id, so ask the backend directly.
            let pid = self
                .display_handle
                .backend_handle()
                .get_client_credentials(client)
                .ok()
                .and_then(|creds| u32::try_from(creds.pid).ok());
            for annotation in annotations {
                out.push(info(annotation, None, pid));
            }
        }
        out
    }

    /// Every mapped menu, of windows and of panels. Position is the menu's own top-left in the
    /// shared space.
    fn popup_list(&self) -> Vec<PopupInfo> {
        let mut out = Vec::new();
        let mut add = |root: &WlSurface,
                       origin: (i32, i32),
                       window,
                       layer,
                       layer_namespace: Option<String>| {
            for (popup, offset) in PopupManager::popups_for_surface(root) {
                if !is_mapped(popup.wl_surface()) {
                    continue;
                }
                let size = popup.geometry().size;
                out.push(PopupInfo {
                    id: surface_id(popup.wl_surface()),
                    window,
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
            let window = entry.window.user_data().get::<WindowId>().map(|id| id.0);
            add(toplevel.wl_surface(), entry.pos, window, None, None);
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

/// The process id of a window's client, kept on the window once the socket has told it.
struct KnownPid(u32);

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
            Some(Under::Window(window, _, _)) => ClickTarget::Window {
                id: window.user_data().get::<WindowId>().map(|id| id.0),
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

/// The leading `--pretty` / `-p` and `--compact` / `-c` flags of `jff msg`, and the rest. The
/// flag is `Some(true)` for pretty, `Some(false)` for compact, `None` when none was given (a
/// terminal gets pretty, anything else compact). Only leading words count: after the command
/// they are its arguments (`spawn-sh ls -c`). The last flag wins.
pub fn split_style(args: &[String]) -> (Option<bool>, &[String]) {
    let mut style = None;
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        match first.as_str() {
            "--pretty" | "-p" => style = Some(true),
            "--compact" | "-c" => style = Some(false),
            _ => break,
        }
        rest = tail;
    }
    (style, rest)
}

/// What `jff msg` prints for the reply or event `text`: as it is, or each line of JSON in it
/// indented. Text that is not a JSON object stays as it was.
fn render(text: &str, pretty_print: bool) -> String {
    if !pretty_print {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() * 2);
    for line in text.lines() {
        if line.starts_with('{') {
            out.push_str(&pretty(line));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// The commands for the help text, in groups: the command with its arguments, and what it does.
const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Windows",
        &[
            ("windows", "list every window"),
            ("focused", "the focused window"),
            ("window-info [ID]", "details of a window"),
            ("focus ID", "focus a window"),
            (
                "focus-urgent",
                "focus the window that asked for attention last",
            ),
            ("close [ID]", "close a window"),
            ("maximize [ID]", "maximize a window"),
            ("unmaximize [ID]", "restore a maximized window"),
            ("toggle-maximize [ID]", "toggle maximize"),
            ("minimize [ID]", "put a window out of sight"),
            ("unminimize [ID]", "bring a minimized window back"),
            (
                "move-to-scratchpad [ID]",
                "put a window into the scratchpad",
            ),
            ("scratchpad [ID]", "show or hide a scratchpad window"),
            ("fullscreen [ID]", "make a window fullscreen"),
            ("unfullscreen [ID]", "leave fullscreen"),
            ("always-on-top [ID]", "keep a window above the others"),
            ("not-always-on-top [ID]", "stop keeping a window above"),
            ("toggle-always-on-top [ID]", "toggle always on top"),
            ("sticky [ID]", "show a window on every workspace"),
            ("unsticky [ID]", "keep a window on its workspace only"),
            ("toggle-sticky [ID]", "toggle sticky"),
            ("tag [NAME] [ID]", "tag a window, or show its tag"),
            ("untag [ID]", "remove the tag of a window"),
        ],
    ),
    (
        "Workspaces and monitors",
        &[
            ("workspaces", "list the workspaces"),
            ("workspace N|next|prev", "go to a workspace"),
            ("move-to-workspace N [ID]", "send a window to workspace N"),
            ("outputs", "list the monitors"),
            ("focus-monitor MONITOR", "focus a monitor"),
            ("move-to-monitor MONITOR [ID]", "send a window to a monitor"),
            (
                "move-workspace-to-monitor MONITOR",
                "send the workspace to a monitor",
            ),
        ],
    ),
    (
        "Containers and layouts",
        &[
            ("containers", "list the tiling containers"),
            ("layout-save NAME [ID]", "save the layout of a container"),
            ("layout-apply NAME [ID]", "apply a saved layout"),
            ("layout-list", "list the saved layouts"),
            ("layout-delete NAME", "delete a saved layout"),
        ],
    ),
    (
        "Clipboard",
        &[
            ("clipboard-history", "list the copies jff kept"),
            ("clipboard-select ID", "make a kept copy the clipboard"),
            ("clipboard-clear", "forget every kept copy"),
        ],
    ),
    (
        "Screenshots",
        &[
            ("screenshot-area", "pick an area and capture it"),
            ("screenshot-fullscreen", "capture the screen"),
            ("screenshot-window", "capture the focused window"),
            (
                "window-thumbnail ID [MAXW MAXH]",
                "small PNG of a window, for previews",
            ),
        ],
    ),
    (
        "Other",
        &[
            ("status", "the state of the compositor"),
            ("hotkeys", "the global hotkeys clients have bound"),
            ("spawn PROGRAM [ARGS]", "run a program"),
            ("spawn-sh COMMAND LINE", "run a command line in the shell"),
            ("move-cursor X Y", "move the pointer"),
            ("click left|right|middle", "click a button"),
            ("toggle-overview", "open or close the overview"),
            (
                "hdr on|off|toggle [MONITOR]",
                "turn HDR on a monitor on or off",
            ),
            (
                "sdr-white [+|-]NITS [MONITOR]",
                "set SDR white on an HDR monitor",
            ),
            (
                "hdr-set MONITOR KEY VALUE",
                "change an HDR setting of a monitor",
            ),
            (
                "hdr-reset MONITOR",
                "HDR settings back to the config file's",
            ),
            ("stats", "frame statistics"),
            ("toggle-stats", "show or hide the statistics"),
        ],
    ),
];

/// The help text of `jff msg`.
fn usage() -> String {
    let width = HELP_GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter())
        .map(|(command, _)| command.len())
        .max()
        .unwrap_or(0);
    let mut out = String::from(
        "Usage: jff msg [--pretty|--compact] <command> [args]\n       \
         jff msg [--pretty|--compact] watch [GROUP...]\n\n\
         ID is a window id (see `windows`), the focused window when left out.\n",
    );
    for (title, commands) in HELP_GROUPS {
        out.push_str(&format!("\n{title}:\n"));
        for (command, what) in *commands {
            out.push_str(&format!("  {command:<width$}  {what}\n"));
        }
    }
    out.push_str(&format!(
        "\nwatch [GROUP...]: print events as they happen. Groups: {}\n\
         MONITOR: {}\n",
        Group::NAMES,
        MonitorTarget::WORDS
    ));
    out
}

/// `jff msg <command...>`: send the command, print the reply. Returns the exit code.
pub fn client(args: &[String]) -> anyhow::Result<i32> {
    let (style, args) = split_style(args);
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        print!("{}", usage());
        return Ok(0);
    }
    if args.is_empty() {
        eprint!("{}", usage());
        return Ok(2);
    }
    let pretty_print = style.unwrap_or_else(|| io::stdout().is_terminal());
    if args[0] == "watch" {
        return watch_client(args, pretty_print);
    }
    let mut stream = find_socket()?;
    stream.write_all(args.join(" ").as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(Shutdown::Write)?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    print!("{}", render(&reply, pretty_print));
    Ok(if reply.starts_with("{\"ok\":true") {
        0
    } else {
        1
    })
}

/// `jff msg watch [group...]`: print every line jff sends as it arrives. Returns the exit code:
/// 0 when the connection closed or stdout did (`| head -1`), 1 for an error reply.
fn watch_client(args: &[String], pretty_print: bool) -> anyhow::Result<i32> {
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
        let written = if pretty_print {
            // An empty line after each event, since an event is now many lines.
            let text = render(&String::from_utf8_lossy(&line), true);
            stdout
                .write_all(text.as_bytes())
                .and_then(|()| stdout.write_all(b"\n"))
        } else {
            stdout.write_all(&line)
        };
        let written = written.and_then(|()| stdout.flush());
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
