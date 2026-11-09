//! Compositor state and the Wayland protocol handlers.

use std::collections::{HashMap, HashSet};
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
use smithay::reexports::calloop::{LoopHandle, LoopSignal, RegistrationToken};
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
use smithay::wayland::selection::SelectionHandler;
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
use crate::desk::{Desk, Side, WindowMode};
use crate::focus::{KeyboardFocus, PopupGrabState};
use crate::input::grabs::resize::ResizeGrab;
use crate::primitives::geom;
use crate::primitives::layout;
use crate::protocol::color_representation::ColorRepresentationManager;
use crate::protocol::ext_foreign_toplevel::ExtForeignToplevel;
use crate::protocol::foreign_toplevel::ForeignToplevel;
use crate::protocol::gamma_control::GammaControl;
use crate::protocol::image_copy::ImageCopy;
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
use crate::protocol::workspace::ExtWorkspace;
use crate::protocol::xx_hotkey::Hotkeys;
use crate::render::cursor::CursorManager;

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
    pub outputs: Vec<Output>,
    /// The `wl_output` global of every active output, by output name.
    pub output_globals: HashMap<String, GlobalId>,
    /// Globals of unplugged outputs, destroyed a while after they were disabled.
    pub retiring_globals: Vec<(Instant, GlobalId)>,

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
    /// The picture the dragging client attached to the drag in progress (see `dnd.rs`).
    pub dnd_icon: Option<WlSurface>,
    pub osd: crate::feature::osd::Osd,
    /// Installed applications, for the overview search.
    pub app_index: crate::feature::app_index::AppIndex,
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
    /// wp_tearing_control_v1: games say whether a surface may be shown with tearing. jff only
    /// records the hint (see `crate::protocol::tearing_control::wants_tearing`) and still
    /// presents every frame in sync with the display.
    #[allow(dead_code)]
    pub tearing_control_state: TearingControlState,
    /// ext-image-copy-capture-v1 / ext-image-capture-source-v1 (screen capture of monitors).
    pub image_copy: ImageCopy,
    /// ext-foreign-toplevel-list-v1 (the read-only list of open windows).
    pub ext_foreign_toplevel: ExtForeignToplevel,
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
    pub pointer_warp: PointerWarp,
    /// xdg-toplevel-drag: a drag-and-drop that carries a window (`protocol/toplevel_drag.rs`).
    pub toplevel_drags: ToplevelDrags,
    /// xdg-toplevel-tag: window tags for `window-rule` (`protocol/toplevel_tag.rs`).
    pub toplevel_tag_manager: ToplevelTagManager,
    /// wlr-virtual-pointer: clients that move the pointer and click (`protocol/virtual_pointer.rs`).
    pub virtual_pointer_manager: VirtualPointerManager,
    /// zwp-virtual-keyboard: clients that type for the user (wtype, wayvnc). Smithay does the
    /// work, jff only decides who may see the global.
    #[allow(dead_code)]
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    /// ext-transient-seat: clients that ask for a seat of their own to put virtual devices on
    /// (`protocol/transient_seat.rs`).
    pub transient_seats: TransientSeats,
    /// wp-color-representation: how a surface's buffer values are encoded, stored only so far
    /// (`protocol/color_representation.rs`).
    pub color_representation_manager: ColorRepresentationManager,
    /// State of the `wlr-layer-shell` protocol (panels, launchers, wallpapers).
    pub layer_shell_state: WlrLayerShellState,
    /// The id the next window gets (see `ipc.rs`).
    pub next_view_id: u64,
    /// Clients of `jff msg watch` that get an event on every change (see `ipc.rs`).
    pub subscribers: Vec<crate::ipc::Subscriber>,
    /// The id the next watcher gets.
    pub next_subscriber_id: u64,
    /// What the watchers were told last. Only kept while there is a watcher.
    pub last_snapshot: Option<crate::ipc::events::Snapshot>,
    /// Events that are not a change of the snapshot (button presses), sent with the next diff.
    pub pending_events: Vec<crate::ipc::events::Event>,
    /// Id of the view an interactive move grab is dragging, for the `drag` watch event.
    pub dragging_view: Option<u64>,

    pub desk: Desk<Window>,
    /// Tracks popups (menus, tooltips) so they're drawn and receive input with their parent.
    pub popups: PopupManager,
    /// The interactive resize in progress, if any.
    pub resize: Option<crate::input::grabs::resize::ActiveResize>,
    /// The window overview, while open (or animating closed).
    pub overview: Option<crate::feature::overview_mode::Overview>,
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
    /// Keys whose press was kept from clients (a binding, the overview): their release is kept
    /// from clients too.
    pub suppressed_keys: std::collections::HashSet<Keycode>,
    /// Keys a `forward-key` binding handed to a window: the release goes to the same window.
    pub forwarded_keys: HashMap<Keycode, WlSurface>,
    /// The keymap of a virtual keyboard that is on the seat right now, `None` while the seat
    /// has the configured one (`protocol/virtual_keyboard.rs`).
    pub active_virtual_keymap: Option<Arc<str>>,
    /// Who the seat's keyboard was given to last (see `focus.rs`).
    pub keyboard_focus: KeyboardFocus,
    /// The window the user works in, as picked by a click, a new window, or a request. The
    /// keyboard follows it unless a launcher or the overview has it.
    pub focused_window: Option<Window>,
    /// A panel the user clicked that takes keys when asked (`on_demand`).
    pub layer_on_demand_focus: Option<WlSurface>,
    /// The menu that currently holds keyboard and pointer, if any.
    pub popup_grab: Option<PopupGrabState>,
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
        // Only advertise what jff really does: maximize and fullscreen. No minimize, no window
        // menu, so clients don't show buttons that would do nothing.
        let xdg_shell_state = XdgShellState::new_with_capabilities::<Self>(
            &display_handle,
            [WmCapabilities::Fullscreen, WmCapabilities::Maximize],
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
            .map(|output| (output.name(), output.create_global::<Self>(&display_handle)))
            .collect();

        // wp_presentation, with CLOCK_MONOTONIC as the presentation clock.
        let presentation_state =
            PresentationState::new::<Self>(&display_handle, Monotonic::ID as u32);

        let fractional_scale_manager_state =
            FractionalScaleManagerState::new::<Self>(&display_handle);

        let tearing_control_state = TearingControlState::new(&display_handle);
        let image_copy = ImageCopy::new(&display_handle);
        let ext_foreign_toplevel = ExtForeignToplevel::new(&display_handle);
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
        let pointer_warp = PointerWarp::new(&display_handle);
        let toplevel_drags = ToplevelDrags::new(&display_handle);
        let toplevel_tag_manager = ToplevelTagManager::new(&display_handle);
        let virtual_pointer_manager = VirtualPointerManager::new(&display_handle);
        // A client that can type can do anything the user can: not for sandboxed apps.
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<Self, _>(&display_handle, |client| {
                !is_sandboxed(client)
            });
        let transient_seats = TransientSeats::new(&display_handle);
        let color_representation_manager = ColorRepresentationManager::new(&display_handle);

        // wlr-layer-shell: lets bars, launchers, wallpapers, and notification daemons place
        // themselves on a monitor.
        let layer_shell_state = WlrLayerShellState::new::<Self>(&display_handle);

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
            outputs,
            output_globals,
            retiring_globals: Vec::new(),
            compositor_state,
            xdg_shell_state,
            xdg_dialog_state,
            dialog_toplevels: HashSet::new(),
            settling_dialogs: HashMap::new(),
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            ext_data_control_state,
            dnd_icon: None,
            osd: crate::feature::osd::Osd::new(config.osd().font.clone()),
            app_index: crate::feature::app_index::AppIndex::new(),
            xdg_foreign_state,
            xdg_activation_state,
            xdg_system_bell_state,
            xdg_toplevel_icon_manager,
            viewporter_state,
            pointer_constraints_state,
            cursor_shape_manager_state,
            tablet_manager_state,
            security_context_state,
            relative_pointer_manager_state,
            pointer_gestures_state,
            presentation_state,
            fifo_manager_state,
            tearing_control_state,
            image_copy,
            ext_foreign_toplevel,
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
            cursor: CursorManager::new(),
            config,
            output_management,
            output_power,
            gamma_control,
            hotkeys,
            pointer_warp,
            toplevel_drags,
            toplevel_tag_manager,
            virtual_pointer_manager,
            virtual_keyboard_state,
            transient_seats,
            color_representation_manager,
            layer_shell_state,
            next_view_id: 1,
            subscribers: Vec::new(),
            next_subscriber_id: 1,
            last_snapshot: None,
            pending_events: Vec::new(),
            dragging_view: None,
            desk: Desk::default(),
            popups: PopupManager::default(),
            resize: None,
            overview: None,
            hot_corner_armed: true,
            hot_corner_timer: None,
            closing: Vec::new(),
            screenshot: None,
            swallowed_buttons: Default::default(),
            suppressed_keys: Default::default(),
            forwarded_keys: Default::default(),
            active_virtual_keymap: None,
            keyboard_focus: KeyboardFocus::Nobody,
            focused_window: None,
            layer_on_demand_focus: None,
            popup_grab: None,
        };
        state
            .desk
            .set_per_output(state.config.workspaces_per_output(), "");
        state.apply_numlock();
        state.relayout_outputs();
        state
    }

    pub fn insert_client(&mut self, stream: std::os::unix::net::UnixStream) {
        if let Err(err) = self
            .display_handle
            .insert_client(stream, Arc::new(ClientState::default()))
        {
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
}

/// Did this client connect from inside a sandbox (a wp_security_context_v1 socket)?
/// Privileged globals (screen capture, session lock, clipboard managers, output control,
/// window lists) use this to hide themselves.
pub fn is_sandboxed(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|state| state.security_context.is_some())
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

// ---- wl_compositor / wl_shm / wl_buffer / wl_output -----------------------------------------

impl CompositorHandler for JffState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // Explicit sync: a commit with an acquire point is held back until the GPU finishes
        // the client's rendering, then the compositor state is told to go on.
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let mut acquire_point = None;
            let has_dmabuf = with_states(surface, |data| {
                acquire_point.clone_from(
                    &data
                        .cached_state
                        .get::<DrmSyncobjCachedState>()
                        .pending()
                        .acquire_point,
                );
                data.cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .is_some_and(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).is_ok(),
                        _ => false,
                    })
            });
            let (true, Some(acquire_point)) = (has_dmabuf, acquire_point) else {
                return;
            };
            let Ok((blocker, source)) = acquire_point.generate_blocker() else {
                return;
            };
            let Some(client) = surface.client() else {
                return;
            };
            let inserted = state.loop_handle.insert_source(source, move |_, _, data| {
                let dh = data.display_handle.clone();
                data.client_compositor_state(&client)
                    .blocker_cleared(data, &dh);
                Ok(())
            });
            if inserted.is_ok() {
                add_blocker(surface, blocker);
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Imports the attached buffer so the renderer can draw it.
        on_commit_buffer_handler::<Self>(surface);

        // Popups need their own commit bookkeeping before they can be drawn.
        self.popups.commit(surface);
        self.send_initial_popup_configure(surface);

        // Subsurface commits belong to the toplevel at the root of the tree.
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        // A window's first commit is the answer to nothing yet: now it gets its first configure.
        let first_commit = self
            .desk
            .find_by(|w| is_window_for(w, &root))
            .and_then(|entry| entry.window.toplevel().cloned())
            .filter(|toplevel| !toplevel.is_initial_configure_sent());
        if let Some(toplevel) = first_commit {
            self.send_initial_configure(&toplevel);
        }

        if let Some(entry) = self.desk.find_by(|w| is_window_for(w, &root)) {
            entry.window.on_commit();
            self.toplevel_drag_commit(&root);
            self.fit_to_output_once(&root);
            self.recenter_settling_dialog(&root);
        } else {
            // Panels, launchers, wallpapers.
            self.layer_commit(&root);
        }
    }
}

impl BufferHandler for JffState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl DrmSyncobjHandler for JffState {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
    }
}

impl DmabufHandler for JffState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Accept only if the renderer can really import it.
        if self.backend.renderer().import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<JffState>();
        } else {
            notifier.failed();
        }
    }
}

impl SecurityContextHandler for JffState {
    fn context_created(
        &mut self,
        source: SecurityContextListenerSource,
        security_context: SecurityContext,
    ) {
        // Every connection on the new socket is a sandboxed client.
        let inserted = self
            .loop_handle
            .insert_source(source, move |stream, _, state| {
                let data = ClientState {
                    security_context: Some(security_context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                    tracing::warn!("error inserting sandboxed client: {err}");
                }
            });
        if let Err(err) = inserted {
            tracing::warn!("error listening on a security context socket: {err}");
        }
    }
}

impl ShmHandler for JffState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for JffState {}

// ---- seat ------------------------------------------------------------------------------------

// wp_cursor_shape_v1 also serves tablet tools, so the state must handle their cursors too.
impl TabletSeatHandler for JffState {
    type ToolFocus = WlSurface;

    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        self.cursor.set_status(image);
    }
}

impl SeatHandler for JffState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    // The client under the pointer asked for a cursor (an icon name, its own surface, or none).
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.set_status(image);
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Clipboard offers follow keyboard focus.
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        // Primary selection follows keyboard focus too: the focused client is the one that gets
        // to paste it when the middle button is pressed.
        set_primary_focus(&self.display_handle, seat, client);
    }
}

// ---- clipboard / drag-and-drop ---------------------------------------------------------------

// Clients exchange clipboard contents directly, the compositor keeps none.
impl SelectionHandler for JffState {
    type SelectionUserData = ();
}

impl DataControlHandler for JffState {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

impl ExtDataControlHandler for JffState {
    fn data_control_state(&mut self) -> &mut ExtDataControlState {
        &mut self.ext_data_control_state
    }
}

impl PrimarySelectionHandler for JffState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

// wp_fractional_scale_v1: a client that just asked for the scale event gets the scale of the
// monitor its window is on right away, `refresh_window_outputs` keeps it current after.
impl FractionalScaleHandler for JffState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let scale = self.scale_of_surface(&root);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

impl DataDeviceHandler for JffState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

// Drag-and-drop between clients lives in `dnd.rs`.

// xdg-foreign: nothing to do, smithay tracks the exported and imported handles and sets the
// parent of the imported toplevel (which keeps `maximize-new-windows` off file dialogs).
impl XdgForeignHandler for JffState {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

/// Marks an activation token that came without an input serial. Such a token only says "this
/// window wants attention" (a terminal's bell, a finished download), it never moves the focus.
struct UrgentOnlyMarker;

// xdg-activation: focus a window when its app presents a token that came from real input.
impl XdgActivationHandler for JffState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        // No serial: the app didn't act on a key press or click, so the token can ask for
        // attention but never for the focus. Apps do this on a bell or a notification.
        let Some((serial, seat)) = data.serial else {
            data.user_data.insert_if_missing(|| UrgentOnlyMarker);
            return true;
        };
        let Some(seat) = Seat::<JffState>::from_resource(&seat) else {
            return false;
        };

        // The serial must be from the last time the seat's keyboard or pointer entered a
        // surface, or later: a client holding an old serial can't pull the focus to itself.
        // Both are checked, a panel without keyboard interactivity never has the keyboard.
        let keyboard_ok = seat
            .get_keyboard()
            .and_then(|k| k.last_enter())
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));
        let pointer_ok = seat
            .get_pointer()
            .and_then(|p| p.last_enter())
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));
        keyboard_ok || pointer_ok
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Stale tokens (older than a few seconds) are ignored so apps can't steal focus later.
        if token_data.timestamp.elapsed().as_secs() < 10 {
            let window = self
                .desk
                .find_by(|w| is_window_for(w, &surface))
                .map(|entry| entry.window.clone());
            if let Some(window) = window {
                if token_data.user_data.get::<UrgentOnlyMarker>().is_some() {
                    tracing::debug!("activation without an input serial, focus left alone");
                } else {
                    // The window may be on a workspace that isn't shown: go there, or the
                    // keyboard would have nowhere to follow.
                    self.show_workspace_of(&window);
                    self.focus_window(&window);
                }
            }
        }
        // A token is good for one request.
        self.xdg_activation_state.remove_token(&token);
    }
}

// xdg-toplevel-icon: nothing to do when an icon is set, the overview reads it when it paints.
impl XdgToplevelIconHandler for JffState {}

// xdg-system-bell: no sound, so it's logged and shown as a toast.
impl XdgSystemBellHandler for JffState {
    fn ring(&mut self, surface: Option<WlSurface>) {
        tracing::debug!(
            "system bell rung{}",
            if surface.is_some() {
                " for a surface"
            } else {
                ""
            }
        );
        self.show_bell(surface);
    }
}

// pointer-constraints: games lock the pointer (mouse look) or confine it to their window.
impl PointerConstraintsHandler for JffState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate right away when the pointer is already on that surface.
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        let active = with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        });
        if !active {
            return;
        }
        // `location` is relative to the surface, find where its window draws it.
        let origin = self.desk.all_top_down().find_map(|entry| {
            let toplevel = entry.window.toplevel()?;
            (toplevel.wl_surface() == surface)
                .then(|| Point::<i32, Logical>::from(entry.pos) - entry.window.geometry().loc)
        });
        if let Some(origin) = origin {
            pointer.set_location(origin.to_f64() + location);
        }
    }
}

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

impl XdgDecorationHandler for JffState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.send_decoration_mode(&toplevel);
    }

    // Apps may ask for a mode, the config decides.
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.send_decoration_mode(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.send_decoration_mode(&toplevel);
    }
}

// ---- xdg-shell -------------------------------------------------------------------------------

// xdg_wm_dialog_v1: a window says it is a dialog, modal or not. It is remembered until the
// window first shows, when dialogs are centred.
impl XdgDialogHandler for JffState {
    fn dialog_hint_changed(&mut self, toplevel: ToplevelSurface, hint: ToplevelDialogHint) {
        let surface = toplevel.wl_surface().clone();
        match hint {
            ToplevelDialogHint::Unknown => {
                self.dialog_toplevels.remove(&surface);
            }
            ToplevelDialogHint::Dialog | ToplevelDialogHint::Modal => {
                self.dialog_toplevels.insert(surface);
            }
        }
    }
}

impl XdgShellHandler for JffState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Let the client pick its own size, but hint how much room there is (toolkits that
        // understand bounds, like GTK4 and Qt, pick a size that fits).
        // Which monitor it opens on: the pointer's, unless a window fills that one already.
        let placement = self.placement_output_index();
        let placed_rect = placement
            .and_then(|i| self.output_rects().get(i).copied())
            .unwrap_or_else(|| self.rect_at_pointer());
        let area = (placed_rect.2, placed_rect.3);
        let (bw, bh) = geom::fit_size((i32::MAX, i32::MAX), area, WINDOW_MARGIN);
        surface.with_pending_state(|state| state.bounds = Some((bw, bh).into()));
        // The first configure waits for the app's first commit (`send_initial_configure`): an
        // app that asks for its window to be restored does so before that, and the restored
        // state has to be in the first configure.
        let window = Window::new_wayland_window(surface);
        self.assign_view_id(&window);
        // New windows open on the monitor the pointer is on, on the workspace it shows.
        let output = placement
            .and_then(|i| self.outputs.get(i))
            .map(|output| output.name())
            .unwrap_or_else(|| self.active_output_name());
        let cascade = self.desk.add_on(window.clone(), &output);
        let pos = (cascade.0 + placed_rect.0, cascade.1 + placed_rect.1);
        self.desk.set_pos_where(|w| *w == window, pos);
        tracing::info!("new toplevel at {pos:?}");
        // New windows take focus so you can type into them straight away. With the overview
        // open that can't happen (the overview owns the keyboard), so a helper window such as
        // wl-clipboard's would wait for focus forever and block the client that spawned it
        // (nvim). Close the overview in favour of the new window instead.
        if self.overview_active() {
            self.end_overview(Some(window.clone()));
        } else {
            self.focus_window(&window);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Save it as it was, while it is still on the desk.
        self.session_toplevel_gone(surface.wl_surface());
        // Take the last picture of the window while it can still be drawn.
        let closing = self
            .desk
            .find_by(|w| w.toplevel().map(|t| t == &surface).unwrap_or(false))
            .map(|e| (e.window.clone(), e.pos, e.workspace));
        if let Some((window, pos, workspace)) = closing {
            self.start_close_fade(&window, pos, workspace);
        }
        self.desk
            .remove_where(|w| w.toplevel().map(|t| t == &surface).unwrap_or(false));
        self.dialog_toplevels.remove(surface.wl_surface());
        self.settling_dialogs.remove(surface.wl_surface());
        self.toplevel_drag_gone(surface.wl_surface());
    }

    // Popups (menus, tooltips, and X11 override-redirect windows via xwayland-satellite).
    // Once tracked, Window::render_elements draws them and Window::surface_under hit-tests them.
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let popup = PopupKind::Xdg(surface.clone());
        self.unconstrain_popup(&popup);
        // A popup with its parent is placed by now: configure it right away. A panel's menu
        // gets its parent a moment later, `new_popup` of the layer shell configures it then.
        if find_popup_root_surface(&popup).is_ok() {
            let _ = surface.send_configure();
        }
        if let Err(err) = self.popups.track_popup(popup) {
            tracing::warn!("failed to track popup: {err:?}");
        }
    }

    // A client asked to be dragged (e.g. the user pressed on its titlebar).
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        let Some(start_data) = self.interactive_start(&surface, serial) else {
            return;
        };
        let wl_surface = surface.wl_surface();
        let Some(window) = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|e| e.window.clone())
        else {
            return;
        };
        let draggable = matches!(
            self.desk.mode_of(|w| *w == window),
            WindowMode::Floating | WindowMode::Snapped(_)
        );
        if draggable {
            self.start_move(&window, start_data, serial);
        }
    }

    // A client asked to be resized from one of its edges or corners.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let Some(start_data) = self.interactive_start(&surface, serial) else {
            return;
        };
        let wl_surface = surface.wl_surface();
        if !self.desk.is_floating(|w| is_window_for(w, wl_surface)) {
            return;
        }
        let Some((initial_pos, size)) = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            return;
        };

        let pointer = self.seat.get_pointer().unwrap();
        let grab = ResizeGrab::new(
            start_data,
            surface,
            crate::input::grabs::resize::edges_from(edges),
            initial_pos,
            (size.w, size.h),
        );
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested maximize");
        self.set_maximized(&surface, true);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested unmaximize");
        self.set_maximized(&surface, false);
    }

    // Games and video players ask for fullscreen: fill the whole output, no frame.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        tracing::info!("client requested fullscreen");
        self.set_filled(&surface, xdg_toplevel::State::Fullscreen, true);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested unfullscreen");
        self.set_filled(&surface, xdg_toplevel::State::Fullscreen, false);
    }

    // A popup asks to be "grabbed": it takes keyboard and pointer input until dismissed, which
    // is how menus close when you click outside them and how typing goes to an open menu.
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, serial: Serial) {
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        tracing::debug!(
            "popup grab requested: root={:?} window_root={} serial={serial:?}",
            root.id(),
            self.is_window_root(&root)
        );

        // A window's menu may only take the keyboard from that same window. Anything else (the
        // window isn't the one the user works in, a launcher or the overview has the keyboard)
        // would leave the grab and the keyboard focus disagreeing about who's in charge.
        let window_root = self.is_window_root(&root);
        if window_root {
            self.update_keyboard_focus();
            if self.keyboard_focus != KeyboardFocus::Window(root.clone()) {
                tracing::debug!("popup grab refused: its window does not have the keyboard");
                let _ = PopupManager::dismiss_popup(&root, &popup);
                return;
            }
        }

        let Ok(mut grab) = self
            .popups
            .grab_popup(root.clone(), popup, &self.seat, serial)
        else {
            return;
        };

        let keyboard = self.seat.get_keyboard();
        let pointer = self.seat.get_pointer();

        // Only take over when the request matches the grab already in place (the press that
        // opened the menu), a stale or unrelated request is refused.
        let kb_mismatch = keyboard.as_ref().is_some_and(|k| {
            k.is_grabbed()
                && !(k.has_grab(serial) || grab.previous_serial().is_none_or(|s| k.has_grab(s)))
        });
        let ptr_mismatch = pointer.as_ref().is_some_and(|p| {
            p.is_grabbed()
                && !(p.has_grab(serial) || grab.previous_serial().is_none_or(|s| p.has_grab(s)))
        });
        if kb_mismatch || ptr_mismatch {
            tracing::debug!(
                "popup grab refused: stale serial (keyboard mismatch={kb_mismatch}, pointer mismatch={ptr_mismatch})"
            );
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }

        if let Some(keyboard) = keyboard {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = pointer {
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        tracing::debug!(
            "popup grab taken: root={:?} window_root={window_root}",
            root.id()
        );
        self.popup_grab = Some(PopupGrabState {
            root,
            grab,
            serial,
            window_root,
        });
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&PopupKind::Xdg(surface.clone()));
        surface.send_repositioned(token);
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

impl JffState {
    /// Keep a popup on screen: let the client's positioner flip/slide it into the output.
    /// Clients that give an exact position and no adjustments are left as they asked.
    fn unconstrain_popup(&self, popup: &PopupKind) {
        let PopupKind::Xdg(xdg) = popup else {
            return;
        };
        let Ok(root) = find_popup_root_surface(popup) else {
            return;
        };
        let Some((win_pos, win_size)) = self
            .desk
            .find_by(|w| is_window_for(w, &root))
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            // Not a window's popup, maybe a menu of a panel.
            self.unconstrain_layer_popup(popup);
            return;
        };
        let (out_x, out_y, w, h) = self.rect_for_window(win_pos, (win_size.w, win_size.h));
        // The positioner works relative to the parent's window geometry, so express the output
        // rect in those terms: shift by the window position and the popup's offset in it.
        let offset = get_popup_toplevel_coords(popup);
        let target = Rectangle::<i32, Logical>::new(
            (out_x - win_pos.0 - offset.x, out_y - win_pos.1 - offset.y).into(),
            (w, h).into(),
        );
        xdg.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    /// Answer the first commit of a popup with its initial configure. By now the popup has its
    /// parent (a panel's menu gets it after creation) and was placed inside the monitor.
    fn send_initial_popup_configure(&mut self, surface: &WlSurface) {
        let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) else {
            return;
        };
        if popup.is_initial_configure_sent() {
            return;
        }
        if let Err(err) = popup.send_configure() {
            tracing::warn!("initial popup configure failed: {err:?}");
        }
    }

    /// Some apps open bigger than the screen (gedit asks for ~900x700). The first time a window
    /// shows real content, shrink it with a configure so its buttons and edges are reachable.
    fn fit_to_output_once(&mut self, root: &WlSurface) {
        let Some(entry) = self.desk.find_by(|w| is_window_for(w, root)) else {
            return;
        };
        let size = entry.window.geometry().size;
        let pos = entry.pos;
        let Some(toplevel) = entry.window.toplevel().cloned() else {
            return;
        };
        if size.w <= 0 || size.h <= 0 {
            return; // no buffer yet: wait for the first real commit
        }
        if !self.desk.mark_fitted_where(|w| is_window_for(w, root)) {
            return;
        }
        // A window a drag carries (xdg-toplevel-drag) is where the pointer puts it: not
        // maximized, centred or fitted like a window that opens on its own.
        if self.toplevel_drags.is_attached(root) {
            return;
        }
        if !self.desk.is_floating(|w| is_window_for(w, root)) {
            return;
        }
        // A dialog is a window that belongs to another one, or says it is a dialog. It keeps the
        // size it asks for (shrunk to fit) and is centred over its parent.
        // A window its app had put back (xdg-session-management) is where and how it was, not
        // maximized or centred like a new one. If it was maximized or the like, it is again.
        let restored = self.session_take_restored(root);
        let parent = toplevel.parent();
        let dialog = parent.is_some() || self.dialog_toplevels.contains(root);
        // `window-rule`: the first rule that fits. Matched here, not in `new_toplevel`: the app
        // id, the title, the tag and the dialog hint arrive after the toplevel is created, and
        // are all there by the first commit that has a buffer.
        let rule = self.matching_window_rule(root, dialog, restored.is_some());
        if let Some(saved) = restored.as_ref() {
            // A restored window keeps the monitor, size and mode its session saved. A rule only
            // chooses its workspace.
            if let Some(workspace) = rule.as_ref().and_then(|rule| rule.workspace) {
                self.send_window_to_workspace(root, workspace);
            }
            if saved.mode != WindowMode::Floating {
                self.session_finish_restore(&toplevel, saved);
                return;
            }
        }
        let mut ruled_floating = false;
        if let (None, Some(rule)) = (restored.as_ref(), rule.as_ref()) {
            match self.apply_window_rule(root, &toplevel, rule, parent.as_ref(), dialog) {
                RulePlacement::Done => return,
                RulePlacement::Floating => ruled_floating = true,
                RulePlacement::Untouched => {}
            }
        }
        // The rule may have moved the window to another monitor.
        let pos = self.desk.pos_of(|w| is_window_for(w, root)).unwrap_or(pos);
        // `maximize-new-windows` (on by default). Done now rather than in `new_toplevel` so the
        // window's own size is known and remembered for when it's restored. A window a rule
        // keeps floating stays that way.
        if self.config.maximize_new_windows() && !dialog && restored.is_none() && !ruled_floating {
            self.set_maximized(&toplevel, true);
            return;
        }
        if dialog && restored.is_none() {
            self.center_dialog(root, parent.as_ref(), (size.w, size.h), None);
            return;
        }
        // Only windows that overflow the output are touched, one that exactly fills it (a game
        // asking for its screen size) must be left alone.
        let (_, _, area_w, area_h) = self.rect_for_window(pos, (size.w, size.h));
        let area = (area_w, area_h);
        if !geom::overflows((size.w, size.h), area) {
            return;
        }
        let fitted = geom::fit_size((size.w, size.h), area, WINDOW_MARGIN);
        if fitted != (size.w, size.h) {
            tracing::info!(
                "window {}x{} too big for output, fitting to {fitted:?}",
                size.w,
                size.h
            );
            toplevel.with_pending_state(|state| state.size = Some((fitted.0, fitted.1).into()));
            toplevel.send_pending_configure();
        }
    }

    /// The `window-rule` a window gets, `None` when there is none. `dialog` and `restored` are
    /// what the window is when it first shows (see `fit_to_output_once`).
    fn matching_window_rule(
        &self,
        root: &WlSurface,
        dialog: bool,
        restored: bool,
    ) -> Option<WindowRule> {
        if self.config.window_rules().is_empty() {
            return None;
        }
        let (app_id, title) = crate::feature::osd::toplevel_names(root);
        let tag = crate::protocol::toplevel_tag::tag_of(root);
        let facts = WindowFacts {
            app_id: &app_id,
            title: &title,
            tag: tag.as_deref(),
            dialog,
            restored,
        };
        let rule = self.config.window_rule_for(&facts).cloned();
        if rule.is_some() {
            tracing::info!(
                "window rule applies: app-id={app_id:?} tag={tag:?} dialog={dialog} restored={restored}"
            );
        }
        rule
    }

    /// Send the window of `root` to workspace `workspace` (0-based, clamped).
    fn send_window_to_workspace(&mut self, root: &WlSurface, workspace: usize) {
        let window = self
            .desk
            .find_by(|w| is_window_for(w, root))
            .map(|entry| entry.window.clone());
        if let Some(window) = window {
            self.send_to_workspace(&window, workspace);
        }
    }

    /// Do what `rule` says about a window that shows for the first time and isn't restored:
    /// the monitor, then the workspace on it, then maximized or its size. What the rule leaves
    /// out stays as it is. Says what is left to do for the caller, see [`RulePlacement`].
    fn apply_window_rule(
        &mut self,
        root: &WlSurface,
        toplevel: &ToplevelSurface,
        rule: &WindowRule,
        parent: Option<&WlSurface>,
        dialog: bool,
    ) -> RulePlacement {
        let is_this = |w: &Window| is_window_for(w, root);
        let size = self
            .desk
            .find_by(is_this)
            .map(|entry| entry.window.geometry().size)
            .map_or((0, 0), |size| (size.w, size.h));

        // The monitor: the window moves there with the offset it had on the one it opened on.
        let mut monitor = None;
        if rule.output.is_some() {
            let rects = self.output_rects();
            let target = self.placement_output_index_for(Some(rule));
            if let Some((i, name)) = target.and_then(|i| Some((i, self.outputs.get(i)?.name()))) {
                let to = rects[i];
                let from = self
                    .desk
                    .find_by(is_this)
                    .and_then(|entry| self.outputs.iter().position(|o| o.name() == entry.output))
                    .map_or(to, |j| rects[j]);
                if let Some(pos) = self.desk.pos_of(is_this) {
                    let moved = (pos.0 - from.0 + to.0, pos.1 - from.1 + to.1);
                    self.desk
                        .set_pos_where(is_this, layout::fit_into(moved, size, to));
                }
                self.desk.set_output_where(is_this, &name);
                monitor = Some(to);
            }
        }

        // The workspace, on that monitor.
        if let Some(workspace) = rule.workspace {
            self.send_window_to_workspace(root, workspace);
        }

        // Maximized wins over a size.
        if rule.maximized == Some(true) {
            self.set_maximized(toplevel, true);
            return RulePlacement::Done;
        }

        // The size, kept inside the monitor it is on.
        let mut sized = None;
        if let Some((width, height)) = rule.size {
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let area = monitor.unwrap_or_else(|| self.rect_for_window(pos, size));
            let fitted = geom::fit_size((width, height), (area.2, area.3), WINDOW_MARGIN);
            toplevel.with_pending_state(|state| state.size = Some(fitted.into()));
            toplevel.send_pending_configure();
            if !dialog {
                // Still inside the monitor with the new size.
                self.desk
                    .set_pos_where(is_this, layout::fit_into(pos, fitted, area));
            }
            sized = Some(fitted);
        }

        // A dialog the rule gave a monitor or a size is centred there, over its parent when
        // that is on the same monitor.
        if dialog && (sized.is_some() || monitor.is_some()) {
            self.center_dialog(root, parent, sized.unwrap_or(size), monitor);
            return RulePlacement::Done;
        }
        if sized.is_some() {
            return RulePlacement::Done;
        }
        if rule.places_window() {
            return RulePlacement::Floating;
        }
        RulePlacement::Untouched
    }

    /// Put a dialog of `size` in the middle of the window it belongs to (the monitor the pointer
    /// is on when it has none), keeping it on that monitor. One too big for the monitor is
    /// shrunk to fit first.
    ///
    /// `monitor` puts it on that monitor (a window rule asked for one) instead of the parent's,
    /// or the pointer's when it has no parent.
    fn center_dialog(
        &mut self,
        root: &WlSurface,
        parent: Option<&WlSurface>,
        size: (i32, i32),
        monitor: Option<layout::Rect>,
    ) {
        let parent_rect = parent
            .and_then(|parent| self.desk.find_by(|w| is_window_for(w, parent)))
            .map(|entry| {
                let size = entry.window.geometry().size;
                (entry.pos.0, entry.pos.1, size.w, size.h)
            });
        let monitor = match (monitor, parent_rect) {
            (Some(monitor), _) => monitor,
            (None, Some((x, y, w, h))) => self.rect_for_window((x, y), (w, h)),
            (None, None) => self.rect_at_pointer(),
        };
        let fitted = geom::fit_size(size, (monitor.2, monitor.3), WINDOW_MARGIN);
        let pos = layout::dialog_pos(fitted, parent_rect, monitor, WINDOW_MARGIN);
        self.desk.set_pos_where(|w| is_window_for(w, root), pos);
        let since = self
            .settling_dialogs
            .get(root)
            .map_or_else(Instant::now, |settling| settling.0);
        self.settling_dialogs
            .insert(root.clone(), (since, size, pos));
        if fitted != size {
            if let Some(toplevel) = self
                .desk
                .find_by(|w| is_window_for(w, root))
                .and_then(|entry| entry.window.toplevel().cloned())
            {
                toplevel.with_pending_state(|state| state.size = Some(fitted.into()));
                toplevel.send_pending_configure();
            }
        }
    }

    /// A dialog that was centred a moment ago and has a new size now (the app showed a small
    /// window and grew it) is centred again, for its new size. Stops after a moment, or as soon
    /// as something else moved the window (the user dragging it).
    fn recenter_settling_dialog(&mut self, root: &WlSurface) {
        let Some(&(since, last, placed)) = self.settling_dialogs.get(root) else {
            return;
        };
        let Some(entry) = self.desk.find_by(|w| is_window_for(w, root)) else {
            self.settling_dialogs.remove(root);
            return;
        };
        if since.elapsed() > DIALOG_SETTLE || entry.pos != placed {
            self.settling_dialogs.remove(root);
            return;
        }
        let size = entry.window.geometry().size;
        let size = (size.w, size.h);
        if size == last || size.0 <= 0 || size.1 <= 0 {
            return;
        }
        let parent = entry
            .window
            .toplevel()
            .and_then(|toplevel| toplevel.parent());
        self.center_dialog(root, parent.as_ref(), size, None);
    }

    pub fn set_maximized(&mut self, surface: &ToplevelSurface, on: bool) {
        self.set_filled(surface, xdg_toplevel::State::Maximized, on);
    }

    /// Maximize or fullscreen a window, or float it again. Always answers with a configure,
    /// as the protocol requires, even if nothing changed.
    pub fn set_filled(
        &mut self,
        surface: &ToplevelSurface,
        state_kind: xdg_toplevel::State,
        on: bool,
    ) {
        let wl_surface = surface.wl_surface();
        let is_this = |w: &Window| is_window_for(w, wl_surface);
        let target = match (state_kind == xdg_toplevel::State::Fullscreen, on) {
            (true, true) => WindowMode::Fullscreen,
            (true, false) => self.desk.mode_after_fullscreen(is_this),
            (false, true) => WindowMode::Maximized,
            (false, false) => WindowMode::Floating,
        };
        self.set_mode(surface, target);
    }

    pub fn set_mode(&mut self, surface: &ToplevelSurface, target: WindowMode) {
        let wl_surface = surface.wl_surface();
        let is_this = |w: &Window| is_window_for(w, wl_surface);
        let size = self
            .desk
            .find_by(is_this)
            .map(|e| e.window.geometry().size)
            .unwrap_or_default();
        let new_size = if target == WindowMode::Floating {
            self.desk.float_where(is_this)
        } else {
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let monitor = self.rect_for_window(pos, (size.w, size.h));
            let (x, y, w, h) = match target {
                WindowMode::Fullscreen => monitor,
                WindowMode::Snapped(side) => geom::snapped_rect(
                    self.panel_free_rect(monitor),
                    self.config.maximize_inset(),
                    self.config.snap_gap(),
                    side,
                ),
                _ => self.usable_rect(monitor),
            };
            self.desk
                .set_mode_where(is_this, target, (x, y), (size.w, size.h));
            Some((w, h))
        };
        let keep_maximized = self
            .desk
            .find_by(is_this)
            .is_some_and(|e| e.before_fullscreen == WindowMode::Maximized);
        tracing::info!("{target:?}, configuring size {new_size:?}");
        configure_mode(surface, target, keep_maximized, new_size);
    }

    pub fn snap_focused(&mut self, side: Side) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned())
        else {
            return;
        };
        self.set_mode(&toplevel, WindowMode::Snapped(side));
    }

    /// Toggle maximize on the window that has keyboard focus.
    pub fn toggle_maximize_focused(&mut self) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned())
        else {
            return;
        };
        let maximized = self.desk.is_maximized(|w| is_window_for(w, &focus));
        self.set_maximized(&toplevel, !maximized);
    }

    /// Toggle fullscreen on the window that has keyboard focus.
    pub fn toggle_fullscreen_focused(&mut self) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned())
        else {
            return;
        };
        let fullscreen =
            toplevel.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen));
        self.set_filled(&toplevel, xdg_toplevel::State::Fullscreen, !fullscreen);
    }

    /// Ask the window that has keyboard focus to close itself.
    pub fn close_focused(&mut self) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        if let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel())
        {
            toplevel.send_close();
        }
    }

    /// Validate an interactive move/resize request. It must match the button press the client
    /// is reacting to (same serial), and that press must have landed on the client's own
    /// surface.
    fn interactive_start(
        &self,
        surface: &ToplevelSurface,
        serial: Serial,
    ) -> Option<GrabStartData<JffState>> {
        let wl_surface = surface.wl_surface();
        let pointer = self.seat.get_pointer()?;
        pointer
            .with_grab(|grab_serial, grab| {
                let data = grab.start_data();
                let same_client = data
                    .focus
                    .as_ref()
                    .map(|(focus, _)| focus.id().same_client_as(&wl_surface.id()))
                    .unwrap_or(false);
                (grab_serial == serial && same_client).then(|| data.clone())
            })
            .flatten()
    }
}

impl JffState {
    /// The scale of the monitor `root` (a window or a layer surface) is on. First monitor
    /// stands in when it's on none, and 1.0 when there is no monitor at all.
    fn scale_of_surface(&self, root: &WlSurface) -> f64 {
        let output = self
            .desk
            .find_by(|w| is_window_for(w, root))
            .and_then(|entry| {
                let size = entry.window.geometry().size;
                let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
                self.outputs.iter().find(|o| geom_intersects(area, o))
            })
            .or_else(|| {
                self.outputs.iter().find(|o| {
                    smithay::desktop::layer_map_for_output(o)
                        .layer_for_surface(root, smithay::desktop::WindowSurfaceType::TOPLEVEL)
                        .is_some()
                })
            })
            .or_else(|| self.outputs.first());
        output.map_or(1.0, |o| o.current_scale().fractional_scale())
    }
}

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
    toplevel.send_pending_configure();
}
