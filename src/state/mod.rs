//! Compositor state and the Wayland protocol handlers.

use std::collections::HashMap;
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
use smithay::reexports::calloop::{LoopHandle, LoopSignal};
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
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};

use crate::app::Backend;
use crate::config::{Config, KeyboardConfig};
use crate::desk::Desk;
use crate::focus::{KeyboardFocus, PopupGrabState};
use crate::input::grabs::r#move::MoveGrab;
use crate::input::grabs::resize::ResizeGrab;
use crate::primitives::geom;
use crate::protocol::ext_foreign_toplevel::ExtForeignToplevel;
use crate::protocol::foreign_toplevel::ForeignToplevel;
use crate::protocol::image_copy::ImageCopy;
use crate::protocol::output_management::OutputManagement;
use crate::protocol::output_power::OutputPower;
use crate::protocol::pointer_warp::PointerWarp;
use crate::protocol::session_lock::SessionLock;
use crate::protocol::tearing_control::TearingControlState;
use crate::protocol::text_input::TextInput;
use crate::protocol::workspace::ExtWorkspace;
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
    /// xdg-foreign: lets a portal dialog say which window it belongs to.
    #[allow(dead_code)]
    pub xdg_foreign_state: XdgForeignState,
    /// xdg-activation: lets an app (or a portal) ask for one of its windows to be focused.
    pub xdg_activation_state: XdgActivationState,
    #[allow(dead_code)]
    pub viewporter_state: ViewporterState,
    // Pointer lock/confine and raw relative motion: what games need for mouse look.
    pub pointer_constraints_state: PointerConstraintsState,
    /// wp_cursor_shape_v1: clients ask for a named cursor, which jff draws at its own size.
    pub cursor_shape_manager_state: CursorShapeManagerState,
    pub relative_pointer_manager_state: RelativePointerManagerState,
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
    pub pointer_warp: PointerWarp,
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

    pub desk: Desk<Window>,
    /// Tracks popups (menus, tooltips) so they're drawn and receive input with their parent.
    pub popups: PopupManager,
    /// The interactive resize in progress, if any.
    pub resize: Option<crate::input::grabs::resize::ActiveResize>,
    /// The window overview, while open (or animating closed).
    pub overview: Option<crate::feature::overview_mode::Overview>,
    /// Pictures of windows that were just closed, fading out (`window-animations`).
    pub closing: Vec<crate::feature::window_anim::Ghost>,
    /// The area screenshot selection, while open (see `screenshot.rs`).
    pub screenshot: Option<crate::feature::screenshot::ScreenshotUi>,
    /// Mouse buttons whose press ran a compositor binding: their release isn't forwarded.
    pub swallowed_buttons: std::collections::HashSet<u32>,
    /// Keys whose press was kept from clients (a binding, the overview): their release is kept
    /// from clients too.
    pub suppressed_keys: std::collections::HashSet<Keycode>,
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
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        // Clipboard and drag-and-drop. GTK3 refuses to create its seat (so no pointer or
        // keyboard input at all) when this global is missing.
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        let data_control_state = DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let ext_data_control_state = ExtDataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );

        // Portals: a file chooser opened for a window asks for that window as its parent (so
        // it's shown as its dialog, not as a window of its own), and asks to be activated.
        let xdg_foreign_state = XdgForeignState::new::<Self>(&display_handle);
        let xdg_activation_state = XdgActivationState::new::<Self>(&display_handle);

        // wp_viewporter: xwayland-satellite needs it to scale X11 windows.
        let viewporter_state = ViewporterState::new::<Self>(&display_handle);
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(&display_handle);
        // Without this global, GTK draws its own cursor at the size from its own settings
        // (GSettings), which can differ from jff's XCURSOR_SIZE and look bigger.
        let cursor_shape_manager_state = CursorShapeManagerState::new::<Self>(&display_handle);
        let relative_pointer_manager_state =
            RelativePointerManagerState::new::<Self>(&display_handle);

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
        let pointer_warp = PointerWarp::new(&display_handle);

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
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            ext_data_control_state,
            dnd_icon: None,
            xdg_foreign_state,
            xdg_activation_state,
            viewporter_state,
            pointer_constraints_state,
            cursor_shape_manager_state,
            relative_pointer_manager_state,
            presentation_state,
            fifo_manager_state,
            tearing_control_state,
            image_copy,
            ext_foreign_toplevel,
            foreign_toplevel,
            ext_workspace,
            session_lock,
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
            pointer_warp,
            layer_shell_state,
            next_view_id: 1,
            subscribers: Vec::new(),
            next_subscriber_id: 1,
            last_snapshot: None,
            desk: Desk::default(),
            popups: PopupManager::default(),
            resize: None,
            overview: None,
            closing: Vec::new(),
            screenshot: None,
            swallowed_buttons: Default::default(),
            suppressed_keys: Default::default(),
            keyboard_focus: KeyboardFocus::Nobody,
            focused_window: None,
            layer_on_demand_focus: None,
            popup_grab: None,
        };
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

        if let Some(entry) = self.desk.find_by(|w| is_window_for(w, &root)) {
            entry.window.on_commit();
            self.fit_to_output_once(&root);
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
        // Before the initial configure `new_toplevel` sends it, and this state goes along.
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

impl XdgShellHandler for JffState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Let the client pick its own size, but hint how much room there is (toolkits that
        // understand bounds, like GTK4 and Qt, pick a size that fits).
        let area = self.output_size();
        let (bw, bh) = geom::fit_size((i32::MAX, i32::MAX), area, WINDOW_MARGIN);
        surface.with_pending_state(|state| state.bounds = Some((bw, bh).into()));
        surface.send_configure();
        let window = Window::new_wayland_window(surface);
        self.assign_view_id(&window);
        let cascade = self.desk.add(window.clone());
        // New windows open on the monitor the pointer is on.
        let area = self.rect_at_pointer();
        let pos = (cascade.0 + area.0, cascade.1 + area.1);
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
        if self.desk.is_maximized(|w| is_window_for(w, wl_surface)) {
            return;
        }
        let Some(window) = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|e| e.window.clone())
        else {
            return;
        };
        let Some(initial_pos) = self.desk.pos_of(|w| *w == window) else {
            return;
        };

        let pointer = self.seat.get_pointer().unwrap();
        let grab = MoveGrab::new(start_data, window, initial_pos);
        // Focus::Clear: the client stops receiving pointer events for the duration of the drag.
        pointer.set_grab(self, grab, serial, Focus::Clear);
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
        if self.desk.is_maximized(|w| is_window_for(w, wl_surface)) {
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

    /// Size of the monitor the pointer is on.
    fn output_size(&self) -> (i32, i32) {
        let area = self.rect_at_pointer();
        (area.2, area.3)
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
        if self.desk.is_maximized(|w| is_window_for(w, root)) {
            return;
        }
        // `maximize-new-windows` (on by default). Done now rather than in `new_toplevel` so the
        // window's own size is known and remembered for when it's restored. Dialogs that belong
        // to another window keep the size they ask for.
        if self.config.maximize_new_windows() && toplevel.parent().is_none() {
            self.set_maximized(&toplevel, true);
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

    pub fn set_maximized(&mut self, surface: &ToplevelSurface, on: bool) {
        self.set_filled(surface, xdg_toplevel::State::Maximized, on);
    }

    /// Make a window fill the output (maximized or fullscreen), or put it back where it was.
    /// Always answers with a configure, as the protocol requires, even if nothing changed.
    pub fn set_filled(
        &mut self,
        surface: &ToplevelSurface,
        state_kind: xdg_toplevel::State,
        on: bool,
    ) {
        let wl_surface = surface.wl_surface();
        let is_this = |w: &Window| is_window_for(w, wl_surface);
        // Leaving fullscreen with the maximized state still set: the window was maximized
        // before it went fullscreen.
        let back_to_maximized = !on
            && state_kind == xdg_toplevel::State::Fullscreen
            && surface.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Maximized));

        let new_size = if on {
            let current = self
                .desk
                .find_by(is_this)
                .map(|e| e.window.geometry().size)
                .unwrap_or_default();
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let monitor = self.rect_for_window(pos, (current.w, current.h));
            // Maximized windows leave the room panels reserved, fullscreen ones cover it.
            let (out_x, out_y, out_w, out_h) = if state_kind == xdg_toplevel::State::Fullscreen {
                monitor
            } else {
                self.usable_rect(monitor)
            };
            // (An already maximized window keeps the geometry it will go back to.)
            if !self
                .desk
                .maximize_where(is_this, (out_x, out_y), (current.w, current.h))
            {
                self.desk.set_pos_where(is_this, (out_x, out_y));
            }
            Some((out_w, out_h).into())
        } else if back_to_maximized {
            // Fullscreen was entered from a maximized window (the state is still set, and the
            // remembered floating geometry is still in `restore`): go back to the maximized
            // window, not past it to the floating one.
            let current = self
                .desk
                .find_by(is_this)
                .map(|e| e.window.geometry().size)
                .unwrap_or_default();
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let monitor = self.rect_for_window(pos, (current.w, current.h));
            let (out_x, out_y, out_w, out_h) = self.usable_rect(monitor);
            self.desk.set_pos_where(is_this, (out_x, out_y));
            Some((out_w, out_h).into())
        } else {
            self.desk
                .unmaximize_where(is_this)
                .map(|(w, h)| (w, h).into())
        };

        tracing::info!("{state_kind:?}={on}, configuring size {new_size:?}");
        surface.with_pending_state(|state| {
            if on {
                state.states.set(state_kind);
            } else if back_to_maximized {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            } else {
                // Leaving either mode returns the window to its normal geometry.
                state.states.unset(xdg_toplevel::State::Maximized);
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            if let Some(size) = new_size {
                state.size = Some(size);
            }
        });
        surface.send_pending_configure();
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
