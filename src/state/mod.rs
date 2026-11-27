//! Compositor state and the Wayland protocol handlers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Instant;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, PopupKeyboardGrab, PopupKind, PopupManager,
    PopupPointerGrab, PopupUngrabStrategy, Window,
};
use smithay::input::pointer::{CursorImageStatus, Focus, GrabStartData, PointerHandle};
use smithay::backend::input::{Keycode, TabletToolDescriptor};
use smithay::input::tablet::TabletSeatHandler;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::calloop::{Interest, LoopHandle, LoopSignal, RegistrationToken};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::{
    self, WmCapabilities,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, GlobalId,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{ClockSource, Logical, Monotonic, Point, Rectangle, Serial, Transform};
use smithay::wayland::alpha_modifier::AlphaModifierState;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::single_pixel_buffer::SinglePixelBufferState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::tablet_manager::TabletManagerState;
use smithay::wayland::compositor::{
    add_blocker, add_pre_commit_hook, get_parent, with_states, BufferAssignment,
    CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
};
use smithay::wayland::drm_syncobj::{
    supports_syncobj_eventfd, DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState,
};
use smithay::wayland::commit_timing::CommitTimingManagerState;
use smithay::wayland::background_effect::BackgroundEffectState;
use smithay::wayland::content_type::ContentTypeState;
use smithay::wayland::fifo::FifoManagerState;
use smithay::wayland::idle_inhibit::IdleInhibitManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::fractional_scale::{
    with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
};
use smithay::wayland::dmabuf::{
    get_dmabuf, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::pointer_constraints::{
    with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
};
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::ext_data_control::{
    DataControlHandler as ExtDataControlHandler, DataControlState as ExtDataControlState,
};
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::security_context::{
    SecurityContext, SecurityContextHandler, SecurityContextListenerSource, SecurityContextState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::dialog::{ToplevelDialogHint, XdgDialogHandler, XdgDialogState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_system_bell::{XdgSystemBellHandler, XdgSystemBellState};
use smithay::wayland::xdg_toplevel_icon::{XdgToplevelIconHandler, XdgToplevelIconManager};
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};

use crate::app::Backend;
use crate::config::{Config, KeyboardConfig, WindowFacts, WindowRule};
use crate::desk::{ContainerId, Desk, Side, WindowMode};
use crate::focus::{seat_grab_is_foreign, KeyboardFocus, PopupGrabState};
use crate::input::grabs::resize::ResizeGrab;
use crate::primitives::geom;
use crate::primitives::layout;
use crate::protocol::color_management::ColorManagement;
use crate::protocol::color_representation::ColorRepresentationManager;
use crate::protocol::ext_foreign_toplevel::ExtForeignToplevel;
use crate::protocol::ext_foreign_toplevel_pidfd::ExtForeignToplevelPidfd;
use crate::protocol::ext_transient_output::TransientOutputs;
use crate::protocol::fixes::Fixes;
use crate::protocol::foreign_toplevel::ForeignToplevel;
use crate::protocol::gamma_control::GammaControl;
use crate::protocol::gtk_shell::GtkShellManager;
use crate::protocol::image_copy::ImageCopy;
use crate::protocol::input_timestamps::InputTimestamps;
use crate::protocol::kde_appmenu::KdeAppmenuManager;
use crate::protocol::output_management::OutputManagement;
use crate::protocol::output_power::OutputPower;
use crate::protocol::pointer_warp::PointerWarp;
use crate::protocol::session_lock::SessionLock;
use crate::protocol::session_management::SessionManagement;
use crate::protocol::tearing_control::TearingControlState;
use crate::protocol::text_input::TextInput;
use crate::protocol::toplevel_drag::ToplevelDrags;
use crate::protocol::toplevel_tag::ToplevelTagManager;
use crate::protocol::transient_seat::TransientSeats;
use crate::protocol::virtual_pointer::VirtualPointerManager;
use crate::protocol::wlr_screencopy::WlrScreencopy;
use crate::protocol::workspace::ExtWorkspace;
use crate::protocol::xdg_dbus_annotation::DbusAnnotationManager;
use crate::protocol::xx_hotkey::Hotkeys;
use crate::protocol::xx_image_capture_color_management::XxImageCaptureColor;
use crate::protocol::xx_zones::Zones;
use crate::render::cursor::CursorManager;

mod handlers;
mod windows;
mod xdg_shell;

pub struct JffState {
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    /// Needed to register the sources that wake a commit up when its acquire fence signals.
    pub loop_handle: LoopHandle<'static, JffState>,
    /// wp_linux_drm_syncobj_v1 (explicit sync). `None` on winit and on GPUs/kernels without
    /// syncobj eventfd support: the global then just isn't advertised.
    pub drm_syncobj_state: Option<DrmSyncobjState>,
    pub start_time: Instant,

    pub backend: Backend,
    pub damage_tracker: OutputDamageTracker,
    /// What the nested (winit) HDR simulation keeps between frames, see `app/winit.rs`.
    pub winit_hdr: crate::app::winit::WinitHdr,
    pub outputs: Vec<Output>,
    /// What `refresh_window_outputs` last told the windows and panels, as a hash of everything
    /// it depends on (see `window_outputs_signature`), and whether a commit that may have changed
    /// the surfaces came since (see `note_commit_for_outputs`).
    /// It skips the whole pass while neither changed.
    pub window_outputs_sig: AtomicU64,
    pub window_outputs_dirty: AtomicBool,
    /// `window_list` while the publishers of one event-loop iteration share it: whether
    /// sharing is on, and the list once someone asked for it (see `share_window_list`).
    pub window_list_memo: std::sync::Mutex<(bool, Option<Vec<crate::ipc::events::WindowInfo>>)>,
    /// The `wl_output` global of every active output, by output name.
    pub output_globals: HashMap<String, GlobalId>,
    /// Globals of unplugged outputs and who still has to acknowledge their removal: destroyed
    /// once every client has (wl_fixes v2), or a while after they were disabled.
    pub retiring_globals: Vec<(GlobalId, crate::feature::outputs::Retirement<ClientId>)>,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    /// xdg_wm_dialog_v1: a window can say it is a dialog and whether it is modal.
    #[allow(dead_code)]
    pub xdg_dialog_state: XdgDialogState,
    /// Windows that said they are dialogs (modal or not). With or without a parent they are
    /// centred when they first show (see `fit_to_output_once`).
    pub dialog_toplevels: HashSet<WlSurface>,
    /// Dialogs that were centred a moment ago: when they were first centred, the size they were
    /// centred for and where they were put. Apps often show a small window first and grow it to
    /// its real size right after, so they are centred again while that goes on.
    pub settling_dialogs: HashMap<WlSurface, (Instant, (i32, i32), (i32, i32))>,
    /// With `tiling-containers`: the window that had the keyboard when a toplevel was created,
    /// for as long as the toplevel has not shown. The new window takes the keyboard itself at
    /// once, so when it first shows this is what tells which window it was opened from (see
    /// `join_container_on_map`). Only there when a window had it, and not when the overview was
    /// open or the session locked.
    pub focus_before_open: HashMap<WlSurface, Window>,
    /// With `tiling-containers`: the saved layout a container was laid out as by a
    /// `container-rule` `layout`. The container is saved under that name again when the user
    /// changes it, see `autosave_container_layout`.
    pub layout_autosave: HashMap<ContainerId, String>,
    /// xdg-decoration: lets `no-client-decorations` ask apps to leave out their titlebar.
    #[allow(dead_code)]
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    /// Primary selection (select text, paste with the middle button): clients do the work, jff
    /// only passes the offers on to the focused client.
    pub primary_selection_state: PrimarySelectionState,
    /// zwlr_data_control_v1: lets wl-copy / wl-paste (and so nvim's clipboard provider) reach
    /// the clipboard without opening a focus-stealing helper window.
    pub data_control_state: DataControlState,
    /// ext_data_control_v1: the standardized version of the same thing, newer clients prefer it.
    pub ext_data_control_state: ExtDataControlState,
    /// What jff keeps of the clipboard so it outlives the app that copied it, and the history
    /// of earlier copies (see `feature/clipboard.rs`).
    pub clipboard: crate::feature::clipboard::ClipboardKeeper,
    /// The picture the dragging client attached to the drag in progress (see `dnd.rs`).
    pub dnd_icon: Option<WlSurface>,
    /// The window of a tiling container the pointer carries right now, drawn small at the
    /// pointer until it is put down (see `feature/containers.rs`, `TileMoveGrab`).
    pub tile_carry: Option<crate::feature::containers::TileCarry>,
    pub osd: crate::feature::osd::Osd,
    /// `close-kill`: windows asked to close and the processes that are never killed (see
    /// `feature/close_watch.rs`).
    pub close_watch: crate::feature::close_watch::CloseWatch,
    /// `remember-container-layouts`: how containers were arranged, by the app ids of their
    /// windows (see `feature/layout_memory.rs`).
    pub layout_memory: crate::primitives::layout_memory::LayoutMemory<crate::desk::WindowId>,
    /// The stats overlay and its counters (`toggle-stats`, see `feature/stats.rs`).
    pub stats: crate::feature::stats::StatsOverlay,
    /// The screen magnifier (`zoom-in`, see `feature/zoom.rs`).
    pub zoom: crate::feature::zoom::ZoomState,
    /// Installed applications, for the overview search.
    pub app_index: crate::feature::app_index::AppIndex,
    /// `restore-apps`: the apps of the last session and the ones open now.
    pub app_restore: crate::feature::app_restore::AppRestore,
    /// `container-template`: the windows a `spawn-container` launch still waits for.
    pub template_claims: crate::feature::container_templates::TemplateClaims,
    /// xdg-foreign: lets a portal dialog say which window it belongs to.
    #[allow(dead_code)]
    pub xdg_foreign_state: XdgForeignState,
    /// xdg-activation: lets an app (or a portal) ask for one of its windows to be focused.
    pub xdg_activation_state: XdgActivationState,
    #[allow(dead_code)]
    pub xdg_system_bell_state: XdgSystemBellState,
    /// xdg-toplevel-icon: windows say their own icon. Read when the overview paints one (see
    /// `protocol/toplevel_icon.rs`), so the global needs no handler.
    #[allow(dead_code)]
    pub xdg_toplevel_icon_manager: XdgToplevelIconManager,
    #[allow(dead_code)]
    pub viewporter_state: ViewporterState,
    // Pointer lock/confine and raw relative motion: what games need for mouse look.
    pub pointer_constraints_state: PointerConstraintsState,
    /// wp_cursor_shape_v1: clients ask for a named cursor, which jff draws at its own size.
    pub cursor_shape_manager_state: CursorShapeManagerState,
    /// zwp_tablet_manager_v2: pens and tablets (pressure, tilt, eraser) for clients that ask.
    pub tablet_manager_state: TabletManagerState,
    /// The pointer devices libinput reports, to set them up again when the config reloads. Empty
    /// on the winit backend.
    pub input_devices: Vec<smithay::reexports::input::Device>,
    /// wp_security_context_v1: sockets for sandboxed apps.
    pub security_context_state: SecurityContextState,
    pub relative_pointer_manager_state: RelativePointerManagerState,
    /// zwp_pointer_gestures_v1: touchpad swipe, pinch and hold for the window under the pointer.
    /// Only has to stay alive: it owns the global.
    #[allow(dead_code)]
    pub pointer_gestures_state: PointerGesturesState,
    /// wp_presentation: the global needs no handler, feedback is answered in the backends (see
    /// `presentation.rs`).
    #[allow(dead_code)]
    pub presentation_state: PresentationState,
    /// wp_fifo_v1: lets a client queue frames FIFO-style (vsync) without guessing. The global
    /// needs no handler, barriers are released after each frame by
    /// `JffState::release_fifo_barriers` (see `presentation.rs`).
    #[allow(dead_code)]
    pub fifo_manager_state: FifoManagerState,
    /// wp_tearing_control_v1: games say whether a surface may be shown with tearing. The hint is
    /// recorded (see `crate::protocol::tearing_control::wants_tearing`) and the TTY backend acts
    /// on it, only with `allow-tearing` set and a fullscreen window that asked for it covering
    /// the monitor and being scanned out: that frame is queued as an async page flip. Every
    /// other frame is presented in sync with the display.
    #[allow(dead_code)]
    pub tearing_control_state: TearingControlState,
    /// ext-image-copy-capture-v1 / ext-image-capture-source-v1 (screen capture of monitors).
    pub image_copy: ImageCopy,
    /// wlr-screencopy-unstable-v1: grim, wf-recorder and friends (`protocol/wlr_screencopy.rs`).
    pub wlr_screencopy: WlrScreencopy,
    /// ext-foreign-toplevel-list-v1 (the read-only list of open windows).
    pub ext_foreign_toplevel: ExtForeignToplevel,
    /// ext-foreign-toplevel-pidfd-v1: a pidfd for each window of the list above
    /// (`protocol/ext_foreign_toplevel_pidfd.rs`).
    pub ext_foreign_toplevel_pidfd: ExtForeignToplevelPidfd,
    pub foreign_toplevel: ForeignToplevel,
    /// ext-workspace-v1 (the workspaces, for bars).
    pub ext_workspace: ExtWorkspace,
    /// ext-session-lock-v1 (screen lockers).
    pub session_lock: SessionLock,
    /// xdg-session-management-v1 (apps get their windows back where they were).
    pub session_management: SessionManagement,
    /// wp_single_pixel_buffer_v1: solid-colour surfaces without a real buffer. Only keeps the
    /// global alive, the renderer draws the buffers.
    #[allow(dead_code)]
    pub single_pixel_buffer_state: SinglePixelBufferState,
    /// wp_alpha_modifier_v1: a client fades a surface by setting its opacity. Only keeps the
    /// global alive, `WaylandSurfaceRenderElement` applies the multiplier when drawing.
    #[allow(dead_code)]
    pub alpha_modifier_state: AlphaModifierState,
    /// zwp_text_input_v3 / zwp_input_method_v2 (input methods and on-screen keyboards).
    pub text_input: TextInput,
    /// wp_fractional_scale_v1: tells each surface the scale of the monitor it's on. The
    /// preferred scale is kept current in `refresh_window_outputs`.
    #[allow(dead_code)]
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    /// wp_commit_timing_v1: clients ask for a commit to show up no earlier than a given time.
    /// Due commits are let through before each frame by `JffState::release_commit_timers`.
    #[allow(dead_code)]
    pub commit_timing_manager_state: CommitTimingManagerState,
    /// wp_content_type_v1: a surface says it shows a photo, video, or game. The global needs no
    /// handler, the TTY backend reads the value of a covering fullscreen window (see
    /// `content_type.rs`).
    #[allow(dead_code)]
    pub content_type_state: ContentTypeState,
    /// ext_background_effect_v1: a surface asks for a blurred background (see
    /// `protocol/background_effect.rs`). The global is always advertised, drawing is opt-in.
    #[allow(dead_code)]
    pub background_effect_state: BackgroundEffectState,
    /// zwp_keyboard_shortcuts_inhibit_v1: a client asks to get the keys jff keeps for its
    /// bindings (see `inhibit.rs`).
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    /// zwp_idle_inhibit_v1: a client asks to keep the screen awake while a surface is shown.
    #[allow(dead_code)]
    pub idle_inhibit_state: IdleInhibitManagerState,
    /// ext_idle_notify_v1: idle tools ask to be told after some time without input, held back
    /// while `idle_inhibited`.
    pub idle_notifier_state: IdleNotifierState<JffState>,
    /// The surfaces that asked for the screen to stay awake.
    pub idle_inhibitors: std::collections::HashSet<WlSurface>,
    /// A shown surface keeps the screen awake right now (what the notifier was last told).
    pub idle_inhibited: bool,
    pub dmabuf_state: DmabufState,
    #[allow(dead_code)]
    pub dmabuf_global: DmabufGlobal,
    pub seat_state: SeatState<JffState>,
    pub seat: Seat<JffState>,
    pub cursor: CursorManager,
    pub config: Config,
    /// State of the `wlr-output-management` protocol.
    pub output_management: OutputManagement,
    pub output_power: OutputPower,
    /// wlr-gamma-control: night-light tools (`protocol/gamma_control.rs`).
    pub gamma_control: GammaControl,
    /// xx-hotkey-v1: client-bound global hotkeys, opt-in (`protocol/xx_hotkey.rs`).
    pub hotkeys: Hotkeys,
    /// xx-zones-v1: client zones for placing a client's own windows (`protocol/xx_zones.rs`).
    pub zones: Zones,
    pub pointer_warp: PointerWarp,
    /// wl_fixes: lets clients free their `wl_registry` (`protocol/fixes.rs`).
    pub fixes: Fixes,
    /// zwp_input_timestamps_v1: nanosecond event times for clients that ask (`protocol/input_timestamps.rs`).
    pub input_timestamps: InputTimestamps,
    /// Hardware time of the libinput motion event being handled, so the timestamp can go to the
    /// client the pointer ends up on. Set only while that event is dispatched, `None` for motion
    /// that did not come from a device (virtual pointer, tablet, IPC).
    pub motion_hw_time: Option<smithay::backend::input::InputTime>,
    /// xdg-toplevel-drag: a drag-and-drop that carries a window (`protocol/toplevel_drag.rs`).
    pub toplevel_drags: ToplevelDrags,
    /// xdg-toplevel-tag: window tags for `window-rule` (`protocol/toplevel_tag.rs`).
    pub toplevel_tag_manager: ToplevelTagManager,
    /// xdg-dbus-annotation: DBus objects apps tie to their windows (`protocol/xdg_dbus_annotation.rs`).
    pub dbus_annotation: DbusAnnotationManager,
    /// KDE appmenu: the older protocol Qt and some browsers use for the same (`protocol/kde_appmenu.rs`).
    pub kde_appmenu: KdeAppmenuManager,
    /// gtk_shell1: GTK's private extension: menu addresses, modal, focus requests, title bar gestures (`protocol/gtk_shell.rs`).
    pub gtk_shell: GtkShellManager,
    /// wlr-virtual-pointer: clients that move the pointer and click (`protocol/virtual_pointer.rs`).
    pub virtual_pointer_manager: VirtualPointerManager,
    /// zwp-virtual-keyboard: clients that type for the user (wtype, wayvnc). Smithay does the
    /// work, jff only decides who may see the global.
    #[allow(dead_code)]
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    /// ext-transient-seat: clients that ask for a seat of their own to put virtual devices on
    /// (`protocol/transient_seat.rs`).
    pub transient_seats: TransientSeats,
    /// ext-transient-output: virtual monitors made on request
    /// (`protocol/ext_transient_output.rs`).
    pub transient_outputs: TransientOutputs,
    /// wp-color-representation: how a surface's buffer values are encoded, stored only so far
    /// (`protocol/color_representation.rs`).
    pub color_representation_manager: ColorRepresentationManager,
    /// wp-color-management: image descriptions of surfaces and outputs, and what each monitor
    /// shows (`protocol/color_management.rs`).
    pub color_management: ColorManagement,
    /// xx-image-capture-color-management-v1: the colors of a capture, and the description of a
    /// frame (`protocol/xx_image_capture_color_management.rs`).
    pub xx_capture_color: XxImageCaptureColor,
    /// State of the `wlr-layer-shell` protocol (panels, launchers, wallpapers).
    pub layer_shell_state: WlrLayerShellState,
    /// The id the next window gets (see `ipc.rs`).
    pub next_window_id: u64,
    /// Clients of `jff msg watch` that get an event on every change (see `ipc.rs`).
    pub subscribers: Vec<crate::ipc::Subscriber>,
    /// The id the next watcher gets.
    pub next_subscriber_id: u64,
    /// What the watchers were told last. Only kept while there is a watcher.
    pub last_snapshot: Option<crate::ipc::events::Snapshot>,
    /// Events that are not a change of the snapshot (button presses), sent with the next diff.
    pub pending_events: Vec<crate::ipc::events::Event>,
    /// Id of the window an interactive move grab is dragging, for the `drag` watch event.
    pub dragging_window: Option<u64>,

    pub desk: Desk<Window>,
    /// Tracks popups (menus, tooltips) so they're drawn and receive input with their parent.
    pub popups: PopupManager,
    /// The interactive resize in progress, if any.
    pub resize: Option<crate::input::grabs::resize::ActiveResize>,
    /// The window overview, while open (or animating closed).
    pub overview: Option<crate::feature::overview_mode::Overview>,
    /// Scroll over the overview not yet turned into a workspace step. Outlives the overview, which reopens after each step.
    pub overview_scroll: crate::primitives::scroll_steps::ScrollSteps,
    /// The touchpad swipe going on, and whether jff keeps it (see `primitives/swipe.rs`).
    pub swipe: crate::primitives::swipe::SwipeTracker,
    /// For `overview { auto-open }`: per monitor, the workspace it showed and whether that had no
    /// windows, as of the last look. The overview opens when this turns into "empty".
    pub overview_auto: HashMap<String, (usize, bool)>,
    /// Whether the pointer may start a hot corner dwell on its next entry into a corner. Cleared
    /// when the dwell fires (or the entry was blocked), set again once the pointer leaves.
    pub hot_corner_armed: bool,
    /// The dwell timer of the hot corner the pointer is resting in, if one is running.
    pub hot_corner_timer: Option<RegistrationToken>,
    /// Pictures of windows that were just closed, fading out (`window-animations`).
    pub closing: Vec<crate::feature::window_anim::Ghost>,
    /// The area screenshot selection, while open (see `screenshot.rs`).
    pub screenshot: Option<crate::feature::screenshot::ScreenshotUi>,
    /// Mouse buttons whose press ran a compositor binding: their release isn't forwarded.
    pub swallowed_buttons: std::collections::HashSet<u32>,
    /// The surface (and its origin) holding an active pointer lock, kept so mouse motion doesn't hit-test again while the cursor can't move.
    pub locked_pointer_under: Option<(WlSurface, Point<f64, Logical>)>,
    /// Keys whose press was kept from clients (a binding, the overview): their release is kept
    /// from clients too.
    pub suppressed_keys: std::collections::HashSet<Keycode>,
    /// Keys a `forward-key` binding handed to a window: the release goes to the same window.
    pub forwarded_keys: HashMap<Keycode, WlSurface>,
    /// The keymap of a virtual keyboard that is on the seat right now, `None` while the seat
    /// has the configured one (`protocol/virtual_keyboard.rs`).
    pub active_virtual_keymap: Option<smithay::input::keyboard::KeymapFileId>,
    /// Who the seat's keyboard was given to last (see `focus.rs`).
    pub keyboard_focus: KeyboardFocus,
    /// The window the user works in, as picked by a click, a new window, or a request. The
    /// keyboard follows it unless a launcher or the overview has it.
    pub focused_window: Option<Window>,
    /// The window `warp-mouse-to-focus` last dealt with, so one focus change warps once.
    pub warped_focus: Option<Window>,
    /// A panel the user clicked that takes keys when asked (`on_demand`).
    pub layer_on_demand_focus: Option<WlSurface>,
    /// Serials of the key presses, clicks and touches clients got, for xdg-activation tokens.
    pub input_serials: crate::input::serials::InputSerials,
    /// The menu that currently holds keyboard and pointer, if any.
    pub popup_grab: Option<PopupGrabState>,
    /// The fingers on a touchscreen (see `input/touch.rs`).
    pub fingers: crate::primitives::touch::Fingers<smithay::backend::input::TouchSlot, WlSurface>,
    /// The three-finger swipe on a touchscreen, when one is going on.
    pub touch_gesture: crate::primitives::touch::TouchGesture<smithay::backend::input::TouchSlot>,
    /// The swipe out of a monitor's top-left corner that opens the overview (touch hot corner).
    pub touch_corner: crate::primitives::touch::CornerSwipe<smithay::backend::input::TouchSlot>,
}

impl JffState {
    pub fn new(
        display_handle: DisplayHandle,
        loop_signal: LoopSignal,
        loop_handle: LoopHandle<'static, JffState>,
        mut backend: Backend,
        outputs: Vec<Output>,
        config: Config,
    ) -> Self {
        let compositor_state = CompositorState::new_v6::<Self>(&display_handle);
        let mut desk = Desk::default();
        desk.set_output_splits(config.output_splits());
        // Only advertise what jff really does: maximize, minimize and fullscreen. No window
        // menu, so clients don't show a button that would do nothing.
        let xdg_shell_state = XdgShellState::new_with_capabilities::<Self>(
            &display_handle,
            [
                WmCapabilities::Fullscreen,
                WmCapabilities::Maximize,
                WmCapabilities::Minimize,
            ],
        );
        let xdg_dialog_state = XdgDialogState::new::<Self>(&display_handle);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        // Clipboard and drag-and-drop. GTK3 refuses to create its seat (so no pointer or
        // keyboard input at all) when this global is missing.
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        // Clipboard managers can read every selection: not for sandboxed apps.
        let data_control_state = DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |client| !is_sandboxed(client),
        );
        let ext_data_control_state = ExtDataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |client| !is_sandboxed(client),
        );
        // Keeps the clipboard when its app exits (`clipboard { … }`).
        let clipboard = crate::feature::clipboard::ClipboardKeeper::new(&loop_handle);
        // wp_security_context_v1: Flatpak and friends ask for a socket of their own for one
        // app. Clients on such a socket are sandboxed (see `is_sandboxed`), and don't see
        // the global that makes more of them.
        let security_context_state =
            SecurityContextState::new::<Self, _>(&display_handle, |client| !is_sandboxed(client));

        // Portals: a file chooser opened for a window asks for that window as its parent (so
        // it's shown as its dialog, not as a window of its own), and asks to be activated.
        let xdg_foreign_state = XdgForeignState::new::<Self>(&display_handle);
        let xdg_activation_state = XdgActivationState::new::<Self>(&display_handle);
        let xdg_system_bell_state = XdgSystemBellState::new::<Self>(&display_handle);
        let mut xdg_toplevel_icon_manager = XdgToplevelIconManager::new::<Self>(&display_handle);
        for size in crate::protocol::toplevel_icon::ICON_SIZES {
            xdg_toplevel_icon_manager.add_icon_size(size);
        }

        // wp_viewporter: xwayland-satellite needs it to scale X11 windows.
        let viewporter_state = ViewporterState::new::<Self>(&display_handle);
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(&display_handle);
        // Without this global, GTK draws its own cursor at the size from its own settings
        // (GSettings), which can differ from jff's XCURSOR_SIZE and look bigger.
        let cursor_shape_manager_state = CursorShapeManagerState::new::<Self>(&display_handle);
        let tablet_manager_state = TabletManagerState::new::<Self>(&display_handle);
        let relative_pointer_manager_state =
            RelativePointerManagerState::new::<Self>(&display_handle);
        let pointer_gestures_state = PointerGesturesState::new::<Self>(&display_handle);

        // GPU buffer sharing. Vulkan (vkcube, DXVK/VKD3D under Proton), most GL games, and
        // xwayland-satellite's X11 windows all hand us dmabufs. Without zwp_linux_dmabuf,
        // clients can't negotiate a format ("can't find our preferred formats, falling back to
        // first exposed format") and many games never open a window.
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = {
            let renderer = backend.renderer();
            let formats: Vec<_> = renderer.dmabuf_formats().into_iter().collect();
            let render_node = EGLDevice::device_for_display(renderer.egl_context().display())
                .and_then(|device| device.try_get_render_node());
            let feedback = match render_node {
                Ok(Some(node)) => DmabufFeedbackBuilder::new(node.dev_id(), formats.clone())
                    .build()
                    .ok(),
                _ => None,
            };
            match feedback {
                Some(feedback) => dmabuf_state
                    .create_global_with_default_feedback::<Self>(&display_handle, &feedback),
                None => {
                    tracing::warn!("no render node for dmabuf feedback; using plain linux-dmabuf");
                    dmabuf_state.create_global::<Self>(&display_handle, formats)
                }
            }
        };

        // wp_linux_drm_syncobj_v1: explicit sync for Vulkan/Proton games and NVIDIA. Only on
        // TTY, and only when the kernel can signal syncobj points through an eventfd (that's
        // how a commit waits for its acquire fence without blocking the compositor).
        let drm_syncobj_state = match &backend {
            Backend::Tty(tty) => {
                let device_fd = tty.drm_device_fd().clone();
                if supports_syncobj_eventfd(&device_fd) {
                    Some(DrmSyncobjState::new::<Self>(&display_handle, device_fd))
                } else {
                    tracing::warn!("no syncobj eventfd support: explicit sync is not advertised");
                    None
                }
            }
            _ => None,
        };

        // wl_drm / EGL buffer sharing for older EGL clients (needs smithay's use_system_lib).
        if let Err(err) = backend.renderer().bind_wl_display(&display_handle) {
            tracing::warn!("could not bind EGL to the wayland display: {err:?}");
        }

        // Advertise the outputs to clients. A global stays alive until it's removed.
        let output_globals = outputs
            .iter()
            .map(|output| {
                (
                    output.name(),
                    crate::feature::outputs::advertise_output(&display_handle, output),
                )
            })
            .collect();

        // wp_presentation, with CLOCK_MONOTONIC as the presentation clock.
        let presentation_state =
            PresentationState::new::<Self>(&display_handle, Monotonic::ID as u32);

        let fractional_scale_manager_state =
            FractionalScaleManagerState::new::<Self>(&display_handle);

        let tearing_control_state = TearingControlState::new(&display_handle);
        let image_copy = ImageCopy::new(&display_handle);
        let wlr_screencopy = WlrScreencopy::new(&display_handle);
        let ext_foreign_toplevel = ExtForeignToplevel::new(&display_handle);
        let ext_foreign_toplevel_pidfd = ExtForeignToplevelPidfd::new(&display_handle);
        let foreign_toplevel = ForeignToplevel::new(&display_handle);
        let ext_workspace = ExtWorkspace::new(&display_handle);
        let session_lock = SessionLock::new(&display_handle);
        let session_management = SessionManagement::new(&display_handle);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&display_handle);
        let alpha_modifier_state = AlphaModifierState::new::<Self>(&display_handle);
        let text_input = TextInput::new(&display_handle);

        // wp_fifo_v1: barriers set by clients are cleared in `release_fifo_barriers`.
        let fifo_manager_state = FifoManagerState::new::<Self>(&display_handle);

        // wp_commit_timing_v1: timed commits are let through in `release_commit_timers`.
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&display_handle);

        // wp_content_type_v1: read for a covering fullscreen window in `render_crtc` (connector
        // content type hint).
        let content_type_state = ContentTypeState::new::<Self>(&display_handle);
        let background_effect_state = BackgroundEffectState::new::<Self>(&display_handle);

        // Inhibiting: keys for games, a screen that stays awake (see `inhibit.rs`).
        let keyboard_shortcuts_inhibit_state =
            KeyboardShortcutsInhibitState::new::<Self>(&display_handle);
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(&display_handle);
        let idle_notifier_state = IdleNotifierState::new(&display_handle, loop_handle.clone());

        let output_management = OutputManagement::new(&display_handle);
        let output_power = OutputPower::new(&display_handle, backend.is_tty());
        let gamma_control = GammaControl::new(&display_handle, backend.is_tty());
        let hotkeys = Hotkeys::new(&display_handle, config.global_hotkeys());
        let zones = Zones::new(&display_handle);
        let pointer_warp = PointerWarp::new(&display_handle);
        let fixes = Fixes::new(&display_handle);
        let input_timestamps = InputTimestamps::new(&display_handle);
        let toplevel_drags = ToplevelDrags::new(&display_handle);
        let toplevel_tag_manager = ToplevelTagManager::new(&display_handle);
        let dbus_annotation = DbusAnnotationManager::new(&display_handle);
        let kde_appmenu = KdeAppmenuManager::new(&display_handle);
        let gtk_shell = GtkShellManager::new(&display_handle);
        let virtual_pointer_manager = VirtualPointerManager::new(&display_handle);
        // A client that can type can do anything the user can: not for sandboxed apps.
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<Self, _>(&display_handle, |client| {
                !is_sandboxed(client)
            });
        let transient_seats = TransientSeats::new(&display_handle);
        let transient_outputs = TransientOutputs::new(&display_handle);
        let color_representation_manager = ColorRepresentationManager::new(&display_handle);
        let color_management =
            ColorManagement::new(&display_handle, !config.color_management().off);
        let xx_capture_color =
            XxImageCaptureColor::new(&display_handle, !config.color_management().off);

        // wlr-layer-shell: lets bars, launchers, wallpapers, and notification daemons place
        // themselves on a monitor. Not for sandboxed apps: a layer surface draws above every
        // window and can take the keyboard.
        let layer_shell_state =
            WlrLayerShellState::new_with_filter::<Self, _>(&display_handle, |client| {
                !is_sandboxed(client)
            });

        // A seat with a keyboard and pointer so clients see a normal wl_seat.
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "jff");
        let kb = config.keyboard();
        if let Err(err) = seat.add_keyboard(kb.xkb_config(), kb.repeat_delay, kb.repeat_rate) {
            tracing::warn!("invalid keyboard config, using the default keymap: {err:?}");
            seat.add_keyboard(Default::default(), kb.repeat_delay, kb.repeat_rate)
                .expect("failed to add keyboard to seat");
        }
        seat.add_pointer();
        // Touchscreens (wl_touch), see `input/touch.rs`.
        seat.add_touch();

        // Only winit uses this, and it always has exactly one output.
        let damage_tracker = match outputs.first() {
            Some(output) => OutputDamageTracker::from_output(output),
            None => OutputDamageTracker::new((0, 0), 1.0, Transform::Normal),
        };

        let mut state = Self {
            display_handle,
            loop_signal,
            loop_handle,
            drm_syncobj_state,
            start_time: Instant::now(),
            backend,
            damage_tracker,
            winit_hdr: Default::default(),
            outputs,
            window_outputs_sig: AtomicU64::new(0),
            // The first pass always runs.
            window_outputs_dirty: AtomicBool::new(true),
            window_list_memo: std::sync::Mutex::new((false, None)),
            output_globals,
            retiring_globals: Vec::new(),
            compositor_state,
            xdg_shell_state,
            xdg_dialog_state,
            dialog_toplevels: HashSet::new(),
            settling_dialogs: HashMap::new(),
            focus_before_open: HashMap::new(),
            layout_autosave: HashMap::new(),
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            ext_data_control_state,
            clipboard,
            dnd_icon: None,
            tile_carry: None,
            osd: crate::feature::osd::Osd::new(config.osd().font.clone()),
            close_watch: crate::feature::close_watch::CloseWatch::default(),
            layout_memory: crate::primitives::layout_memory::LayoutMemory::default(),
            stats: crate::feature::stats::StatsOverlay::new(),
            zoom: crate::feature::zoom::ZoomState::new(),
            app_index: crate::feature::app_index::AppIndex::new(),
            app_restore: crate::feature::app_restore::AppRestore::default(),
            template_claims: crate::feature::container_templates::TemplateClaims::default(),
            xdg_foreign_state,
            xdg_activation_state,
            xdg_system_bell_state,
            xdg_toplevel_icon_manager,
            viewporter_state,
            pointer_constraints_state,
            cursor_shape_manager_state,
            tablet_manager_state,
            input_devices: Vec::new(),
            security_context_state,
            relative_pointer_manager_state,
            pointer_gestures_state,
            presentation_state,
            fifo_manager_state,
            tearing_control_state,
            image_copy,
            wlr_screencopy,
            ext_foreign_toplevel,
            ext_foreign_toplevel_pidfd,
            foreign_toplevel,
            ext_workspace,
            session_lock,
            session_management,
            single_pixel_buffer_state,
            alpha_modifier_state,
            text_input,
            fractional_scale_manager_state,
            commit_timing_manager_state,
            content_type_state,
            background_effect_state,
            keyboard_shortcuts_inhibit_state,
            idle_inhibit_state,
            idle_notifier_state,
            idle_inhibitors: Default::default(),
            idle_inhibited: false,
            dmabuf_state,
            dmabuf_global,
            seat_state,
            seat,
            cursor: CursorManager::new(config.cursor()),
            config,
            output_management,
            output_power,
            gamma_control,
            hotkeys,
            zones,
            pointer_warp,
            fixes,
            input_timestamps,
            motion_hw_time: None,
            toplevel_drags,
            toplevel_tag_manager,
            dbus_annotation,
            kde_appmenu,
            gtk_shell,
            virtual_pointer_manager,
            virtual_keyboard_state,
            transient_seats,
            transient_outputs,
            color_representation_manager,
            color_management,
            xx_capture_color,
            layer_shell_state,
            next_window_id: 1,
            subscribers: Vec::new(),
            next_subscriber_id: 1,
            last_snapshot: None,
            pending_events: Vec::new(),
            dragging_window: None,
            desk,
            popups: PopupManager::default(),
            resize: None,
            overview: None,
            overview_scroll: Default::default(),
            swipe: Default::default(),
            overview_auto: HashMap::new(),
            hot_corner_armed: true,
            hot_corner_timer: None,
            closing: Vec::new(),
            screenshot: None,
            swallowed_buttons: Default::default(),
            locked_pointer_under: None,
            suppressed_keys: Default::default(),
            forwarded_keys: Default::default(),
            active_virtual_keymap: None,
            keyboard_focus: KeyboardFocus::Nobody,
            focused_window: None,
            warped_focus: None,
            layer_on_demand_focus: None,
            input_serials: Default::default(),
            popup_grab: None,
            fingers: Default::default(),
            touch_gesture: Default::default(),
            touch_corner: Default::default(),
        };
        state
            .desk
            .set_per_output(state.config.workspaces_per_output(), "");
        state.apply_numlock();
        state.relayout_outputs();
        state
    }

    pub fn insert_client(&mut self, stream: std::os::unix::net::UnixStream) {
        let single_output = self.single_output_for(&stream);
        let data = ClientState {
            // Taken now, from the socket, so the pid can't have been reused yet.
            pidfd: crate::protocol::ext_foreign_toplevel_pidfd::peer_pidfd(&stream),
            single_output,
            ..ClientState::default()
        };
        if let Err(err) = self.display_handle.insert_client(stream, Arc::new(data)) {
            tracing::warn!("error inserting client: {err}");
        }
    }
}

/// Per-client data attached to every Wayland connection.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Set when the client connected through a socket made by wp_security_context_v1, that is
    /// from inside a sandbox. Such clients don't see the privileged globals.
    pub security_context: Option<SecurityContext>,
    /// A pidfd of the process that connected (`SO_PEERPIDFD`, Linux 6.5+), for
    /// ext-foreign-toplevel-pidfd-v1. `None` on an older kernel.
    pub pidfd: Option<std::os::fd::OwnedFd>,
    /// The only monitor this client is shown, by name, fixed when it connects: a game with
    /// `JFF_SINGLE_OUTPUT` in its environment (or Wine, with `wine-single-output`) has no
    /// other monitor to open on. `None` for everybody else, they see all of them.
    pub single_output: Option<String>,
}

/// What a client's `JFF_SINGLE_OUTPUT` asks for.
enum SingleOutput {
    /// All monitors (unset, `0` or empty).
    All,
    /// The monitor in use when the client connects (`1`).
    Current,
    /// That monitor, by name (`DP-1`).
    Named(String),
}

/// The pid of the process at the other end of `stream` (`SO_PEERCRED`).
fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` outlive the call, and `len` is the size of `cred`.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid as u32)
}

/// What the process `pid` asks for with `JFF_SINGLE_OUTPUT` in its environment: `0` (or empty)
/// all monitors, `1` the one in use, anything else the monitor of that name. Without the
/// variable, a Wine process (the executable's name starts with `wine`) gets the monitor in use
/// when `wine_default` is set, and everything else sees all of them.
fn single_output_request(pid: u32, wine_default: bool) -> SingleOutput {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    for entry in environ.split(|byte| *byte == 0) {
        if let Some(value) = entry.strip_prefix(b"JFF_SINGLE_OUTPUT=") {
            return match value {
                b"" | b"0" => SingleOutput::All,
                b"1" => SingleOutput::Current,
                name => SingleOutput::Named(String::from_utf8_lossy(name).into_owned()),
            };
        }
    }
    let is_wine = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .is_some_and(|name| name.starts_with("wine"));
    if wine_default && is_wine {
        SingleOutput::Current
    } else {
        SingleOutput::All
    }
}

impl JffState {
    /// The one monitor a client connecting on `stream` is shown, if it asked for one (see
    /// [`ClientState::single_output`]). Always `None` with fewer than two monitors, and when
    /// the monitor it named doesn't exist (it sees all of them, and the log says so), so a
    /// client is never left without a monitor.
    fn single_output_for(&self, stream: &std::os::unix::net::UnixStream) -> Option<String> {
        if self.outputs.len() < 2 {
            return None;
        }
        let pid = peer_pid(stream)?;
        let name = match single_output_request(pid, self.config.wine_single_output()) {
            SingleOutput::All => return None,
            SingleOutput::Current => self.output_at_pointer()?.name(),
            SingleOutput::Named(name) => {
                if self.outputs.iter().all(|output| output.name() != name) {
                    tracing::warn!(
                        "pid {pid}: JFF_SINGLE_OUTPUT={name} is not a monitor, showing all of them"
                    );
                    return None;
                }
                name
            }
        };
        tracing::info!("pid {pid} is shown only the monitor {name}");
        Some(name)
    }
}

/// Did this client connect from inside a sandbox (a wp_security_context_v1 socket)?
/// Privileged globals (screen capture, session lock, clipboard managers, output control,
/// window lists, layer-shell, DRM leases) use this to hide themselves.
pub fn is_sandboxed(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|state| state.security_context.is_some())
}

/// Every client that is connected right now. jff makes all the connections itself, so it keeps
/// this list from `ClientData` instead of asking the backend to walk libwayland's client list
/// (`Handle::with_all_clients`), which loops forever in libwayland-server's
/// `wl_client_get_destroy_listener` when a monitor is unplugged.
static CONNECTED_CLIENTS: std::sync::LazyLock<std::sync::Mutex<HashSet<ClientId>>> =
    std::sync::LazyLock::new(Default::default);

/// The clients connected right now.
pub fn connected_clients() -> HashSet<ClientId> {
    CONNECTED_CLIENTS
        .lock()
        .map(|set| set.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        CONNECTED_CLIENTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(client_id);
    }

    fn disconnected(&self, client_id: ClientId, _reason: DisconnectReason) {
        CONNECTED_CLIENTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&client_id);
    }
}

/// Marks an activation token that came without an input serial. Such a token only says "this
/// window wants attention" (a terminal's bell, a finished download), it never moves the focus.
struct UrgentOnlyMarker;

// ---- xdg-decoration --------------------------------------------------------------------------

impl JffState {
    /// The decoration mode to answer apps with: server side (no titlebar, since jff draws none)
    /// when `no-client-decorations` is set, otherwise what apps did before: draw their own.
    fn decoration_mode(&self) -> DecorationMode {
        if self.config.no_client_decorations() {
            DecorationMode::ServerSide
        } else {
            DecorationMode::ClientSide
        }
    }

    fn send_decoration_mode(&self, toplevel: &ToplevelSurface) {
        let mode = self.decoration_mode();
        tracing::info!("xdg-decoration: telling a window to use {mode:?}");
        toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
        // Before the initial configure (sent on the first commit) the state just goes along.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    /// Apply the `keyboard` settings to the running seat after a config reload. `old` is what
    /// was in effect before, the keymap is only rebuilt when it changed.
    pub fn apply_keyboard_config(&mut self, old: &KeyboardConfig) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let new = self.config.keyboard().clone();
        if (new.repeat_delay, new.repeat_rate) != (old.repeat_delay, old.repeat_rate) {
            keyboard.change_repeat_info(new.repeat_rate, new.repeat_delay);
        }
        if new.keymap_differs(old) {
            if let Err(err) = keyboard.set_xkb_config(self, new.xkb_config()) {
                tracing::warn!("keyboard layout not applied: {err:?}");
            }
        }
        // A new keymap starts with every lock off, and the setting may have changed.
        if new.keymap_differs(old) || new.numlock != old.numlock {
            self.apply_numlock();
        }
    }

    /// Put Num Lock in the state the `keyboard { numlock }` setting asks for.
    pub fn apply_numlock(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let mut mods = keyboard.modifier_state();
        let want = self.config.keyboard().numlock;
        if mods.num_lock != want {
            mods.num_lock = want;
            let _ = keyboard.set_modifier_state(mods);
            keyboard.advertise_modifier_state(self);
        }
    }

    /// Apply `no-client-decorations` to windows that are already open, after a config reload.
    pub fn apply_decoration_mode(&mut self) {
        let toplevels: Vec<ToplevelSurface> = self
            .desk
            .all_top_down()
            .filter_map(|entry| entry.window.toplevel().cloned())
            .collect();
        for toplevel in &toplevels {
            // Only windows that use xdg-decoration have a mode to change.
            if toplevel.with_pending_state(|s| s.decoration_mode.is_some()) {
                self.send_decoration_mode(toplevel);
            }
        }
    }
}

/// Space kept free around a freshly mapped window that was too big for the output.
const WINDOW_MARGIN: i32 = 40;

/// What `apply_window_rule` leaves for `fit_to_output_once` to do.
enum RulePlacement {
    /// The rule maximized the window, sized it, or centred a dialog: nothing more to do.
    Done,
    /// The rule keeps the window floating: skip `maximize-new-windows`, but still fit it to the
    /// monitor and centre a dialog like any other.
    Floating,
    /// The rule says nothing about mode or size: carry on as without a rule.
    Untouched,
}
/// How long after a dialog first shows it is still centred again when its size changes.
const DIALOG_SETTLE: std::time::Duration = std::time::Duration::from_millis(2000);

fn geom_intersects(area: (i32, i32, i32, i32), output: &Output) -> bool {
    crate::primitives::layout::intersects(area, crate::feature::outputs::output_rect(output))
}

fn is_window_for(window: &Window, surface: &WlSurface) -> bool {
    window
        .toplevel()
        .map(|t| t.wl_surface() == surface)
        .unwrap_or(false)
}

// One macro wires every protocol global this state has handlers for.
smithay::delegate_dispatch2!(JffState);

pub fn configure_mode(
    toplevel: &ToplevelSurface,
    mode: WindowMode,
    keep_maximized: bool,
    size: Option<(i32, i32)>,
) {
    use xdg_toplevel::State as S;
    let maximized =
        mode == WindowMode::Maximized || (mode == WindowMode::Fullscreen && keep_maximized);
    let flags = [
        (S::Maximized, maximized),
        (S::Fullscreen, mode == WindowMode::Fullscreen),
        (S::TiledLeft, mode == WindowMode::Snapped(Side::Left)),
        (S::TiledRight, mode == WindowMode::Snapped(Side::Right)),
        (S::TiledTop, mode == WindowMode::Snapped(Side::Top)),
        (S::TiledBottom, mode == WindowMode::Snapped(Side::Bottom)),
    ];
    toplevel.with_pending_state(|state| {
        for (flag, on) in flags {
            if on {
                state.states.set(flag);
            } else {
                state.states.unset(flag);
            }
        }
        if let Some((w, h)) = size {
            state.size = Some((w, h).into());
        }
    });
    // Before the app's first commit the state only waits: the first configure is sent on that
    // commit (`send_initial_configure`), after a `restore_toplevel` has had its say. Sending
    // one now would end that, and make the window look mapped to its session.
    if toplevel.is_initial_configure_sent() {
        crate::protocol::gtk_shell::send_configure(toplevel);
        toplevel.send_pending_configure();
    }
}

/// Tell a member of a tiling container the rectangle the tree gave it: its size, and the tiled
/// states on all four edges, so it drops the shadow and the rounded corners it would draw
/// around a floating window. Not maximized, not fullscreen.
pub fn configure_tiled(toplevel: &ToplevelSurface, size: (i32, i32)) {
    configure_member(toplevel, size, false);
}

/// [`configure_tiled`] for a member that is maximized inside its container: it is told it is
/// maximized (`maximized`) as well, and the size is the container's whole area.
pub fn configure_member(toplevel: &ToplevelSurface, size: (i32, i32), maximized: bool) {
    use xdg_toplevel::State as S;
    toplevel.with_pending_state(|state| {
        if maximized {
            state.states.set(S::Maximized);
        } else {
            state.states.unset(S::Maximized);
        }
        state.states.unset(S::Fullscreen);
        for flag in [S::TiledLeft, S::TiledRight, S::TiledTop, S::TiledBottom] {
            state.states.set(flag);
        }
        state.size = Some(size.into());
    });
    crate::protocol::gtk_shell::send_configure(toplevel);
    toplevel.send_pending_configure();
}
