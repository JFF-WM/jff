//! Read-only getters over a parsed [`Config`]: the one place the rest of jff asks
//! what a setting is, so a setting can move without its callers knowing.

use super::*;

impl Config {
    /// What [`Config::parse_lenient`] left out of the file, one line each.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Corner radius in pixels of windows that aren't fullscreen (0 by default: square).
    pub fn radius(&self) -> i32 {
        self.radius
    }

    /// The border block (`width` 0 when there is none).
    pub fn border(&self) -> Border {
        self.border
    }

    /// Pixels kept free on each side of a maximized window: `gaps`, but at least the border
    /// width, which is drawn outside the window and would otherwise be cut off by the screen.
    pub fn maximize_inset(&self) -> i32 {
        self.gaps.max(self.border.width)
    }

    /// Pixels between two snapped windows: `gaps`, but at least twice the border width, since
    /// each window's border is drawn outside it and the two rings must not touch.
    pub fn snap_gap(&self) -> i32 {
        self.gaps.max(2 * self.border.width)
    }

    /// Corner radius in pixels the blur behind popups (menus, popovers) is shaped with: the
    /// `popup-radius` setting, `radius` when it isn't set. GTK popovers are rounded by the
    /// client, which the compositor can't see, so this says how much.
    pub fn popup_radius(&self) -> i32 {
        self.popup_radius.unwrap_or(self.radius)
    }

    /// Pixels kept free on each side of a maximized window (0 by default). Fullscreen windows
    /// ignore it.
    pub fn gaps(&self) -> i32 {
        self.gaps
    }

    /// `autostart` + `sh-autostart`, in file order. Only [`Action::Spawn`] and
    /// [`Action::SpawnShell`] appear here.
    pub fn autostart(&self) -> &[Action] {
        &self.autostart
    }

    /// The blur block.
    pub fn blur(&self) -> Blur {
        self.blur
    }

    /// The layer-surface filter of the blur block (namespaces as in `layers` of `jff msg`):
    /// `ignore-layer` never gets blur, neither its own nor behind its menus, whatever it asks
    /// for and whatever `all-layers` says. `only-layer` gets whole-surface blur on any layer.
    pub fn blur_layers(&self) -> &BlurLayers {
        &self.blur_layers
    }

    /// The window filter of the blur block.
    pub fn blur_windows(&self) -> &BlurWindows {
        &self.blur_windows
    }

    /// `blur` for the window with this app id: `None` when the window's blur (and its popups') is filtered out.
    pub fn blur_for_window(&self, blur: Option<Blur>, app_id: &str) -> Option<Blur> {
        self.blur_windows.apply(blur, app_id)
    }

    pub fn osd(&self) -> &OsdConfig {
        &self.osd
    }

    pub fn close_kill(&self) -> &CloseKillConfig {
        &self.close_kill
    }

    pub fn clipboard(&self) -> &ClipboardConfig {
        &self.clipboard
    }

    pub fn switcher(&self) -> &SwitcherConfig {
        &self.switcher
    }

    pub fn overview(&self) -> &OverviewConfig {
        &self.overview
    }

    /// The workspace wall of the overview's drag, `None` when it is off (the default).
    pub fn workspace_wall(&self) -> Option<WorkspaceWallConfig> {
        self.workspace_wall
    }

    /// The hot corner, `None` when it is off (no block, `hot-corner 0`, or `size 0`).
    pub fn hot_corner(&self) -> Option<HotCorner> {
        self.hot_corner.clone()
    }

    /// The `zoom { … }` settings of the screen magnifier.
    pub fn zoom(&self) -> ZoomConfig {
        self.zoom
    }

    /// Whether `color-representation { yuv }` is on.
    pub fn color_representation_yuv(&self) -> bool {
        self.color_representation_yuv
    }

    /// The `color-management { … }` block (everything off or unset by default).
    pub fn color_management(&self) -> ColorManagementConfig {
        self.color_management
    }

    /// The monitors that are signalled as HDR (`hdr` on a monitor that is not `off`).
    pub fn hdr_output_names(&self) -> Vec<&str> {
        self.outputs
            .iter()
            .filter(|output| output.hdr && !output.off)
            .map(|output| output.name.as_str())
            .collect()
    }

    /// What to tell the user when `color-management { off }` stops jff from offering wp-color-management while a monitor has `hdr`: the monitor is signalled as HDR, but no client can see that, so a game finds nothing to ask for HDR with.
    pub fn hdr_hidden_warning(&self) -> Option<String> {
        let names = self.hdr_output_names();
        (self.color_management.off && !names.is_empty()).then(|| {
            format!(
                "HDR is on for {}, but color management is off, so apps can't use it",
                names.join(", ")
            )
        })
    }

    /// The `window-rule` blocks, in file order.
    pub fn window_rules(&self) -> &[WindowRule] {
        &self.window_rules
    }

    /// The rule a window gets: the first one whose `match` lines all hold. A rule that only
    /// gives a `tag` is passed over, see [`Config::user_tag_for`].
    pub fn window_rule_for(&self, window: &WindowFacts<'_>) -> Option<&WindowRule> {
        self.window_rules
            .iter()
            .find(|rule| !rule.only_tags() && rule.matches(window))
    }

    /// The tag the user's rules give a window: that of the first rule with a `tag` whose `match`
    /// lines all hold (`window` has the tag the app set, if any). It does not depend on which
    /// rule the window gets for its placement, so a rule that places OBS and one that tags its
    /// docks can both apply. `None` when no rule tags it.
    pub fn user_tag_for(&self, window: &WindowFacts<'_>) -> Option<&str> {
        self.window_rules
            .iter()
            .filter(|rule| rule.matches(window))
            .find_map(|rule| rule.tag.as_deref())
    }

    /// The opacity the user's rules give a window, 0 to 1: that of the first rule with an
    /// `opacity` whose `match` lines all hold. Like [`Config::user_tag_for`] it does not depend
    /// on which rule the window gets for its placement. `None` when no rule gives one.
    pub fn opacity_for(&self, window: &WindowFacts<'_>) -> Option<f32> {
        self.window_rules
            .iter()
            .filter(|rule| rule.matches(window))
            .find_map(|rule| rule.opacity)
            .map(|thousandths| f32::from(thousandths) / 1000.0)
    }

    /// How the user's rules say a window's colors are read: that of the first rule with `sdr` or
    /// `sdr-white` whose `match` lines all hold, which does not depend on which rule the window
    /// gets for its placement. `None` when no rule says anything.
    pub fn sdr_for(&self, window: &WindowFacts<'_>) -> Option<WindowSdr> {
        self.window_rules
            .iter()
            .filter(|rule| rule.matches(window))
            .find(|rule| rule.sets_sdr())
            .map(WindowRule::window_sdr)
    }

    /// What the user's rules say a window shows: that of the first rule with a `content-type`
    /// whose `match` lines all hold, which does not depend on which rule the window gets for
    /// its placement. `None` when no rule says anything, and the client's own word counts.
    pub fn content_type_for(&self, window: &WindowFacts<'_>) -> Option<RuleContentType> {
        self.window_rules
            .iter()
            .filter(|rule| rule.matches(window))
            .find_map(|rule| rule.content_type)
    }

    /// Whether the user's rules keep a window above the others: some rule with `always-on-top` whose `match` lines all hold, whichever rule the window gets for its placement.
    pub fn always_on_top_for(&self, window: &WindowFacts<'_>) -> bool {
        self.window_rules
            .iter()
            .any(|rule| rule.always_on_top && rule.matches(window))
    }

    /// Whether the user's rules show a window on every workspace: some rule with `sticky` whose `match` lines all hold, whichever rule the window gets for its placement.
    pub fn sticky_for(&self, window: &WindowFacts<'_>) -> bool {
        self.window_rules
            .iter()
            .any(|rule| rule.sticky && rule.matches(window))
    }

    /// Whether windows can be grouped in tiling containers (`tiling-containers`, off by default).
    pub fn tiling_containers(&self) -> bool {
        self.tiling_containers
    }

    /// Whether a container's arrangement is remembered and given back to the same windows
    /// tiled together again (`remember-container-layouts`, off by default).
    pub fn remember_container_layouts(&self) -> bool {
        self.remember_container_layouts
    }

    /// Whether windows behind the focused container are left out of the frame
    /// (`hide-behind-containers`, on by default).
    pub fn hide_behind_containers(&self) -> bool {
        self.hide_behind_containers
    }

    /// The `container-rule` blocks, in file order.
    pub fn container_rules(&self) -> &[ContainerRule] {
        &self.container_rules
    }

    /// The `container-template` blocks, in file order.
    pub fn container_templates(&self) -> &[ContainerTemplate] {
        &self.container_templates
    }

    /// The `container-template` called `name`. When two have the same name, the first.
    pub fn container_template(&self, name: &str) -> Option<&ContainerTemplate> {
        self.container_templates.iter().find(|t| t.name == name)
    }

    /// The name a container gets for `window`, the first one it fits: the first rule whose
    /// `match` lines all hold and that has a `name`. `None` when no rule gives one. A rule with
    /// no name does nothing, so it does not hide a later one.
    pub fn container_name_for(&self, window: &WindowFacts<'_>) -> Option<&str> {
        self.container_rules
            .iter()
            .filter(|rule| rule.matches(window))
            .find_map(|rule| rule.name.as_deref())
    }

    /// The saved layout to apply when `window` joins a container: that of the first rule that fits and has a `layout`.
    pub fn container_layout_for(&self, window: &WindowFacts<'_>) -> Option<&str> {
        self.container_rules
            .iter()
            .filter(|rule| rule.matches(window))
            .find_map(|rule| rule.layout.as_deref())
    }

    /// The screenshot block.
    pub fn log(&self) -> &LogConfig {
        &self.log
    }

    pub fn screenshot(&self) -> &ScreenshotConfig {
        &self.screenshot
    }

    /// Keyboard layout and key repeat.
    pub fn keyboard(&self) -> &KeyboardConfig {
        &self.keyboard
    }

    /// Pointer device settings.
    pub fn input(&self) -> &InputConfig {
        &self.input
    }

    /// Cursor theme, size and hiding.
    pub fn cursor(&self) -> &CursorConfig {
        &self.cursor
    }

    /// Whether a fullscreen window may be shown with tearing when it asks for it.
    pub fn allow_tearing(&self) -> bool {
        self.allow_tearing
    }

    /// What the PC's power button does.
    pub fn power_key(&self) -> PowerKey {
        self.power_key
    }

    /// Whether clients may bind their own global hotkeys (xx-hotkey-v1).
    pub fn global_hotkeys(&self) -> bool {
        self.global_hotkeys
    }

    /// The GPU that draws everything (`render-device`), `None` for the primary one. Read when jff starts.
    pub fn render_device(&self) -> Option<&str> {
        self.render_device.as_deref()
    }

    /// Whether the apps of the last session are started again, and the open ones remembered
    /// (`restore-apps`, off by default).
    pub fn restore_apps(&self) -> bool {
        self.restore_apps
    }

    /// Whether new windows start maximized.
    pub fn maximize_new_windows(&self) -> bool {
        self.maximize_new_windows
    }

    /// Whether a new window takes the keyboard (`focus-new-windows`, on by default).
    pub fn focus_new_windows(&self) -> bool {
        self.focus_new_windows
    }

    /// Whether the pointer follows the keyboard focus (`warp-mouse-to-focus`, off by default).
    pub fn warp_mouse_to_focus(&self) -> bool {
        self.warp_mouse_to_focus
    }

    /// Whether the window under the pointer gets the keyboard (`focus-follows-mouse`, off by
    /// default).
    pub fn focus_follows_mouse(&self) -> bool {
        self.focus_follows_mouse
    }

    /// Whether a pointer pushed against a monitor edge with no monitor in front of it jumps to
    /// the neighbouring monitor at the same relative position (`edge-warp`, off by default).
    pub fn edge_warp(&self) -> bool {
        self.edge_warp
    }

    /// Whether windows fade in and out (`window-animations`, on by default).
    pub fn window_animations(&self) -> bool {
        self.window_animations.on
    }

    /// How new windows appear.
    pub fn window_open_style(&self) -> OpenStyle {
        self.window_animations.open
    }

    /// How long the open and close fades take.
    pub fn window_animation_times(&self) -> (Duration, Duration) {
        let a = self.window_animations;
        (
            Duration::from_millis(a.open_ms.into()),
            Duration::from_millis(a.close_ms.into()),
        )
    }

    /// Whether apps are asked to drop their own titlebar and borders.
    pub fn no_client_decorations(&self) -> bool {
        self.no_client_decorations
    }

    /// Whether the drag modifier (Alt by default) is held.
    pub fn drag_modifier_held(&self, mods: &ModifiersState) -> bool {
        match self.drag_modifier {
            DragModifier::Alt => mods.alt,
            DragModifier::Super => mods.logo,
            DragModifier::Ctrl => mods.ctrl,
            DragModifier::Shift => mods.shift,
        }
    }

    /// How many workspaces there are.
    pub fn workspaces(&self) -> usize {
        self.workspaces
    }

    /// Whether every monitor shows its own workspace (`workspaces { per-output }`), instead of
    /// all of them showing the same one. Off by default.
    pub fn workspaces_per_output(&self) -> bool {
        self.workspaces_per_output
    }

    /// The `split` every monitor with one asks for, by connector name.
    pub fn output_splits(&self) -> HashMap<String, Axis> {
        self.outputs
            .iter()
            .filter_map(|o| o.split.map(|axis| (o.name.clone(), axis)))
            .collect()
    }

    /// What the config says about `name`, if anything.
    pub fn output(&self, name: &str) -> Option<&OutputConfig> {
        self.outputs.iter().find(|o| o.name == name)
    }

    /// Edit (or add) the entry for `name`. Used by `wlr-output-management`,
    /// lasts until `reload-config`.
    pub fn update_output(&mut self, name: &str, change: impl FnOnce(&mut OutputConfig)) {
        let index = match self.outputs.iter().position(|o| o.name == name) {
            Some(index) => index,
            None => {
                self.outputs.push(OutputConfig {
                    name: name.to_string(),
                    ..Default::default()
                });
                self.outputs.len() - 1
            }
        };
        change(&mut self.outputs[index]);
    }

    /// The action bound to `sym` with exactly `mods`, if any.
    pub fn action_for(&self, mods: &ModifiersState, sym: Keysym) -> Option<&Action> {
        let mods = Mods::from_state(mods);
        self.binds
            .iter()
            .find(|b| b.mods == mods && b.trigger == Trigger::Key(sym))
            .map(|b| &b.action)
    }

    /// The action bound to the bindable mouse button with evdev code `code`,
    /// with exactly `mods`, if any.
    pub fn button_action_for(&self, mods: &ModifiersState, code: u32) -> Option<&Action> {
        let button = MouseButton::from_code(code)?;
        let mods = Mods::from_state(mods);
        self.binds
            .iter()
            .find(|b| b.mods == mods && b.trigger == Trigger::Button(button))
            .map(|b| &b.action)
    }
}
