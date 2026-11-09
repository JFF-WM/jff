//! xx-zones-v1: a client groups its own toplevels in a zone and places them inside it.
//!
//! jff policy, on top of what the protocol requires:
//! - The zone is jff's: it picks the monitor and where the zone sits on it, the client only
//!   ever sees coordinates inside it. A zone is as big as the part of its monitor that panels
//!   leave free, and starts at the top-left corner of that.
//! - Windows keep their position in the shared desktop space on the desk, so drawing, hit-testing,
//!   snap, maximize, fullscreen (and direct scanout), workspaces and the session see zone
//!   members as the windows they are. The zone is only an origin and a list of members.
//! - Alt+drag on a floating member moves the zone: its floating members keep their offsets.
//!   Dragging a member by its own titlebar moves that member alone.
//! - Maximized, fullscreen and snapped members are not moved with the zone. They stay in the zone
//!   (what they float back to moves with it). A request to place one of them fails.
//! - A member dragged onto another monitor leaves the zone (`item_left`).
//! - Members share the workspace of their zone: sending one to a workspace sends the others.
//! - A window that its session put back keeps its place (the protocol allows that). The first
//!   such window decides where the zone is, so the offsets its client chose stay as they were.
//! - A zone member is not maximized or centred when it first shows, the client placed it.
//!   `window-rule` still matches it, except `open-on-output`, the zone has a monitor already.
//! - Nothing here runs without zones: every entry point from the rest of jff starts with a
//!   check that returns at once while no client has made one.

use std::collections::HashSet;
use std::sync::Mutex;

use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::xx_zones_bindings::server::xx_zone_item_v1::{self, XxZoneItemV1};
use super::xx_zones_bindings::server::xx_zone_manager_v1::{self, XxZoneManagerV1};
use super::xx_zones_bindings::server::xx_zone_v1::{self, XxZoneV1};
use crate::desk::{Pos, WindowMode};
use crate::feature::outputs::output_rect;
use crate::ipc::ViewId;
use crate::primitives::geom::Size;
use crate::primitives::layout::{self, Rect};
use crate::primitives::zone as geometry;
use crate::state::JffState;

/// What an item was asked, applied on the next commit of its surface.
enum Op {
    Add(XxZoneV1),
    Remove(XxZoneV1),
    Position(i32, i32),
}

/// Data of an `xx_zone_item_v1`.
pub struct ItemData {
    /// `None` when the toplevel was gone already: the item is inert.
    surface: Option<WlSurface>,
    ops: Mutex<Vec<Op>>,
}

/// Data of an `xx_zone_v1`. Id 0 is an invalid zone.
pub struct ZoneData {
    id: u64,
}

struct Zone {
    id: u64,
    handle: String,
    /// The monitor it is on, by name.
    output: String,
    /// Where it is in the shared desktop space.
    origin: Pos,
    /// Where it is on its monitor, to put it back there when the monitor moves.
    offset: Pos,
    size: Size,
    /// The objects of clients that hold this zone.
    refs: Vec<XxZoneV1>,
    /// A window its session restored told where the zone is.
    anchored: bool,
}

struct Item {
    resource: XxZoneItemV1,
    surface: WlSurface,
    /// The zone it is in, and the object of the client that put it there (events go to it).
    zone: Option<(u64, XxZoneV1)>,
    /// What the client was last told.
    last: Option<Pos>,
    /// What the client asked for last, in zone coordinates.
    wanted: Option<(i32, i32)>,
}

/// The zones and their items.
pub struct Zones {
    _global: GlobalId,
    next_id: u64,
    zones: Vec<Zone>,
    items: Vec<Item>,
    /// Surfaces of items with requests waiting for their commit.
    pending: HashSet<WlSurface>,
    /// The zone an Alt+drag is moving.
    dragging: Option<u64>,
}

impl Zones {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, XxZoneManagerV1, ()>(1, ()),
            next_id: 1,
            zones: Vec::new(),
            items: Vec::new(),
            pending: HashSet::new(),
            dragging: None,
        }
    }

    /// No client has a zone or an item.
    pub fn is_idle(&self) -> bool {
        self.zones.is_empty() && self.items.is_empty()
    }

    /// Some item has requests that wait for a commit.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether the toplevel at `surface` is in a zone.
    pub fn is_member(&self, surface: &WlSurface) -> bool {
        self.items
            .iter()
            .any(|item| item.zone.is_some() && item.surface == *surface)
    }

    /// The monitor of the zone `window` is in, so that its members stay together.
    pub fn output_of(&self, window: &Window) -> Option<String> {
        if self.is_idle() {
            return None;
        }
        let surface = window.toplevel()?.wl_surface();
        let (id, _) = self.items[self.item_of_surface(surface)?].zone.as_ref()?;
        Some(self.zones[self.zone_index(*id)?].output.clone())
    }

    fn zone_index(&self, id: u64) -> Option<usize> {
        self.zones.iter().position(|zone| zone.id == id)
    }

    fn item_index(&self, resource: &XxZoneItemV1) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.resource == *resource)
    }

    fn item_of_surface(&self, surface: &WlSurface) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.zone.is_some() && item.surface == *surface)
    }
}

/// A zone that an Alt+drag carries: what it takes to move all of it by the pointer's travel.
pub struct ZoneDrag {
    zone: u64,
    origin: Pos,
    /// The other floating members and where they were when the drag began.
    mates: Vec<(Window, Pos)>,
}

/// A handle that no other zone has and that is not guessable: it lets whoever has it join.
fn new_handle(id: u64) -> String {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    let random = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    let noise = if random {
        u64::from_le_bytes(bytes)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
    };
    format!("jff-zone-{id}-{noise:016x}")
}

impl GlobalDispatch<XxZoneManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XxZoneManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XxZoneManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &XxZoneManagerV1,
        request: xx_zone_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use xx_zone_manager_v1::Request;
        match request {
            Request::GetZoneItem { id, toplevel } => state.zones_new_item(id, &toplevel, data_init),
            Request::GetZone { id, output } => {
                let name = output
                    .as_ref()
                    .and_then(Output::from_resource)
                    .map(|output| output.name());
                state.zones_new_zone(id, name, data_init);
            }
            Request::GetZoneFromHandle { id, handle } => state.zones_join(id, &handle, data_init),
            // `destroy` leaves what the manager made valid.
            _ => {}
        }
    }
}

impl Dispatch<XxZoneV1, ZoneData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XxZoneV1,
        request: xx_zone_v1::Request,
        data: &ZoneData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use xx_zone_v1::Request;
        match request {
            Request::AddItem { item } => {
                if data.id == 0 {
                    resource.post_error(xx_zone_v1::Error::Invalid, "the zone is invalid");
                    return;
                }
                state.zones_queue(&item, Op::Add(resource.clone()));
            }
            Request::RemoveItem { item } => state.zones_queue(&item, Op::Remove(resource.clone())),
            // `destroy` is handled in `destroyed`.
            _ => {}
        }
    }

    // Also runs when the client goes away without destroying it.
    fn destroyed(state: &mut JffState, _client: ClientId, resource: &XxZoneV1, data: &ZoneData) {
        state.zones_zone_released(resource, data.id);
    }
}

impl Dispatch<XxZoneItemV1, ItemData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XxZoneItemV1,
        request: xx_zone_item_v1::Request,
        _data: &ItemData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use xx_zone_item_v1::Request;
        match request {
            Request::SetPosition { x, y } => state.zones_queue(resource, Op::Position(x, y)),
            // `destroy`: tell the zone while the item can still be named.
            Request::Destroy => state.zones_item_released(resource),
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &XxZoneItemV1,
        _data: &ItemData,
    ) {
        state.zones_item_released(resource);
    }
}

impl JffState {
    // ---- requests ----

    fn zones_new_item(
        &mut self,
        id: New<XxZoneItemV1>,
        toplevel: &XdgToplevel,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let surface = self
            .xdg_shell_state
            .get_toplevel(toplevel)
            .map(|toplevel| toplevel.wl_surface().clone());
        let resource = data_init.init(
            id,
            ItemData {
                surface: surface.clone(),
                ops: Mutex::new(Vec::new()),
            },
        );
        match surface {
            Some(surface) => self.zones.items.push(Item {
                resource,
                surface,
                zone: None,
                last: None,
                wanted: None,
            }),
            None => resource.closed(),
        }
    }

    /// A zone on `output` (by name), or where jff chooses without one. Invalid when there is no
    /// monitor.
    fn zones_new_zone(
        &mut self,
        id: New<XxZoneV1>,
        output: Option<String>,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let name = output
            .filter(|name| self.outputs.iter().any(|o| o.name() == *name))
            .or_else(|| {
                self.placement_output_index()
                    .and_then(|i| self.outputs.get(i))
                    .map(|output| output.name())
            });
        let placement = name
            .as_deref()
            .and_then(|name| self.zone_placement(name).map(|p| (name.to_owned(), p)));
        let Some((name, (origin, size, offset))) = placement else {
            let resource = data_init.init(id, ZoneData { id: 0 });
            resource.size(-1, -1);
            resource.handle(String::new());
            resource.done();
            return;
        };
        let zone_id = self.zones.next_id;
        self.zones.next_id += 1;
        let resource = data_init.init(id, ZoneData { id: zone_id });
        let handle = new_handle(zone_id);
        tracing::info!("zone {zone_id} on {name}: origin {origin:?} size {size:?}");
        resource.size(size.0, size.1);
        resource.handle(handle.clone());
        resource.done();
        self.zones.zones.push(Zone {
            id: zone_id,
            handle,
            output: name,
            origin,
            offset,
            size,
            refs: vec![resource],
            anchored: false,
        });
    }

    fn zones_join(
        &mut self,
        id: New<XxZoneV1>,
        handle: &str,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let Some(i) = self
            .zones
            .zones
            .iter()
            .position(|zone| zone.handle == handle)
        else {
            // Unknown: a new zone, with a handle of its own.
            return self.zones_new_zone(id, None, data_init);
        };
        let zone = &mut self.zones.zones[i];
        let resource = data_init.init(id, ZoneData { id: zone.id });
        resource.size(zone.size.0, zone.size.1);
        resource.handle(zone.handle.clone());
        resource.done();
        zone.refs.push(resource);
    }

    /// Hold a request until the item's surface commits. Requests on an inert item are ignored.
    fn zones_queue(&mut self, item: &XxZoneItemV1, op: Op) {
        if self.zones.item_index(item).is_none() {
            return;
        }
        let Some(data) = item.data::<ItemData>() else {
            return;
        };
        let Some(surface) = data.surface.clone() else {
            return;
        };
        data.ops.lock().unwrap().push(op);
        self.zones.pending.insert(surface);
    }

    /// An item object went away: it leaves its zone.
    fn zones_item_released(&mut self, item: &XxZoneItemV1) {
        let Some(i) = self.zones.item_index(item) else {
            return;
        };
        let gone = self.zones.items.remove(i);
        if let Some((_, zone)) = gone.zone {
            if zone.is_alive() {
                zone.item_left(&gone.resource);
            }
        }
        if !self
            .zones
            .items
            .iter()
            .any(|other| other.surface == gone.surface)
        {
            self.zones.pending.remove(&gone.surface);
        }
    }

    /// A zone object went away. The zone lasts while a client holds it or has an item in it.
    fn zones_zone_released(&mut self, resource: &XxZoneV1, id: u64) {
        let Some(i) = self.zones.zone_index(id) else {
            return;
        };
        self.zones.zones[i].refs.retain(|held| held != resource);
        self.zones_drop_if_unused(id);
    }

    fn zones_drop_if_unused(&mut self, id: u64) {
        let used = self
            .zones
            .items
            .iter()
            .any(|item| item.zone.as_ref().is_some_and(|(zone, _)| *zone == id));
        if let Some(i) = self.zones.zone_index(id) {
            let zone = &mut self.zones.zones[i];
            zone.refs.retain(|held| held.is_alive());
            if zone.refs.is_empty() && !used {
                self.zones.zones.remove(i);
            }
        }
    }

    // ---- commit ----

    /// The root surface `surface` of a toplevel committed: apply what its items were asked.
    pub fn zones_commit(&mut self, surface: &WlSurface) {
        if !self.zones.pending.remove(surface) {
            return;
        }
        let items: Vec<XxZoneItemV1> = self
            .zones
            .items
            .iter()
            .filter(|item| item.surface == *surface)
            .map(|item| item.resource.clone())
            .collect();
        for item in items {
            let Some(data) = item.data::<ItemData>() else {
                continue;
            };
            let ops = std::mem::take(&mut *data.ops.lock().unwrap());
            for op in ops {
                match op {
                    Op::Add(zone) => self.zones_apply_add(&item, &zone),
                    Op::Remove(zone) => self.zones_apply_remove(&item, &zone),
                    Op::Position(x, y) => self.zones_apply_position(&item, (x, y), true),
                }
            }
        }
    }

    fn zones_apply_add(&mut self, item: &XxZoneItemV1, zone: &XxZoneV1) {
        let Some(id) = zone.data::<ZoneData>().map(|data| data.id) else {
            return;
        };
        let (Some(zi), Some(ii)) = (self.zones.zone_index(id), self.zones.item_index(item)) else {
            return;
        };
        let surface = self.zones.items[ii].surface.clone();
        let Some(window) = self.zone_window(&surface) else {
            return;
        };
        let output = self.zones.zones[zi].output.clone();
        if !self.outputs.iter().any(|o| o.name() == output) {
            zone.item_blocked(item);
            return;
        }
        // Another zone first.
        if let Some((old, old_zone)) = self.zones.items[ii].zone.take() {
            if old != id && old_zone.is_alive() {
                old_zone.item_left(item);
            }
            self.zones_drop_if_unused(old);
        }
        // Leaving the old zone may have removed it, which moves the indices.
        let Some(zi) = self.zones.zone_index(id) else {
            return;
        };
        let (origin, size) = {
            let zone = &self.zones.zones[zi];
            (zone.origin, zone.size)
        };

        // The monitor and workspace of the zone.
        let mate_workspace = self
            .zone_members(id)
            .into_iter()
            .filter(|mate| *mate != window)
            .find_map(|mate| self.desk.workspace_of(|w| *w == mate));
        self.desk.set_output_where(|w| *w == window, &output);
        if let Some(workspace) = mate_workspace {
            if self.desk.workspace_of(|w| *w == window) != Some(workspace) {
                self.send_to_workspace(&window, workspace);
            }
        }
        // Inside the zone.
        if let Some(entry) = self.desk.find_by(|w| *w == window) {
            if entry.mode == WindowMode::Floating {
                let geo = entry.window.geometry().size;
                let at = layout::fit_into(
                    entry.pos,
                    (geo.w, geo.h),
                    (origin.0, origin.1, size.0, size.1),
                );
                if at != entry.pos {
                    self.desk.set_pos_where(|w| *w == window, at);
                }
            }
        }

        let pos = self.desk.pos_of(|w| *w == window).unwrap_or(origin);
        let rel = geometry::to_zone(origin, pos);
        self.zones.items[ii].zone = Some((id, zone.clone()));
        self.zones.items[ii].last = Some(rel);
        zone.item_entered(item);
        item.frame_extents(0, 0, 0, 0);
        item.position(rel.0, rel.1);
    }

    fn zones_apply_remove(&mut self, item: &XxZoneItemV1, zone: &XxZoneV1) {
        let Some(id) = zone.data::<ZoneData>().map(|data| data.id) else {
            return;
        };
        if let Some(ii) = self.zones.item_index(item) {
            if self.zones.items[ii]
                .zone
                .as_ref()
                .is_some_and(|(held, _)| *held == id)
            {
                self.zones.items[ii].zone = None;
                self.zones.items[ii].last = None;
            }
        }
        // Even when it never was in it.
        zone.item_left(item);
        self.zones_drop_if_unused(id);
    }

    /// Place the item at `request` (zone coordinates), as far as policy allows. `notify` tells
    /// the client when that is not at all.
    fn zones_apply_position(&mut self, item: &XxZoneItemV1, request: (i32, i32), notify: bool) {
        let fail = |item: &XxZoneItemV1| {
            if notify {
                item.position_failed();
            }
        };
        let Some(ii) = self.zones.item_index(item) else {
            return;
        };
        self.zones.items[ii].wanted = Some(request);
        let surface = self.zones.items[ii].surface.clone();
        let Some((id, _)) = self.zones.items[ii].zone.clone() else {
            return fail(item);
        };
        let (Some(zi), Some(window)) = (self.zones.zone_index(id), self.zone_window(&surface))
        else {
            return fail(item);
        };
        let Some((pos, geo, mode)) = self
            .desk
            .find_by(|w| *w == window)
            .map(|entry| (entry.pos, entry.window.geometry().size, entry.mode))
        else {
            return fail(item);
        };
        if mode != WindowMode::Floating || self.zone_member_interacting(&window, &surface) {
            return fail(item);
        }
        // A restored window stays where its session had it, and the zone is where its offsets say.
        if self.session_is_restored(&surface) {
            let zone = &mut self.zones.zones[zi];
            if !zone.anchored {
                zone.anchored = true;
                zone.origin = (pos.0 - request.0, pos.1 - request.1);
                tracing::info!(
                    "zone {id} anchored by a restored window at {:?}",
                    zone.origin
                );
            }
            let rel = geometry::to_zone(zone.origin, pos);
            self.zones.items[ii].last = Some(rel);
            item.position(rel.0, rel.1);
            return;
        }
        let (origin, size) = {
            let zone = &self.zones.zones[zi];
            (zone.origin, zone.size)
        };
        let rel = geometry::sanitize(request, (geo.w, geo.h), size);
        self.desk
            .set_pos_where(|w| *w == window, geometry::to_desktop(origin, rel));
        self.zones.items[ii].last = Some(rel);
        item.position(rel.0, rel.1);
    }

    /// The user is dragging or resizing this window, or pressing a button on it.
    fn zone_member_interacting(&self, window: &Window, surface: &WlSurface) -> bool {
        let view = window.user_data().get::<ViewId>().map(|id| id.0);
        if view.is_some() && self.dragging_view == view {
            return true;
        }
        if self
            .resize
            .as_ref()
            .is_some_and(|resize| resize.is_for(surface))
        {
            return true;
        }
        self.seat
            .get_pointer()
            .and_then(|pointer| {
                pointer.with_grab(|_, grab| {
                    grab.start_data()
                        .focus
                        .as_ref()
                        .is_some_and(|(focus, _)| focus.id().same_client_as(&surface.id()))
                })
            })
            .unwrap_or(false)
    }

    // ---- lookups ----

    fn zone_window(&self, surface: &WlSurface) -> Option<Window> {
        self.desk
            .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .map(|entry| entry.window.clone())
    }

    /// The windows in zone `id`.
    fn zone_members(&self, id: u64) -> Vec<Window> {
        self.zones
            .items
            .iter()
            .filter(|item| item.zone.as_ref().is_some_and(|(zone, _)| *zone == id))
            .filter_map(|item| self.zone_window(&item.surface))
            .collect()
    }

    fn zone_of_window(&self, window: &Window) -> Option<u64> {
        let surface = window.toplevel()?.wl_surface().clone();
        let i = self.zones.item_of_surface(&surface)?;
        self.zones.items[i].zone.as_ref().map(|(id, _)| *id)
    }

    /// Origin, size and offset of a zone on the monitor `name`: the part of it panels left free.
    fn zone_placement(&self, name: &str) -> Option<(Pos, Size, Pos)> {
        let output = self.outputs.iter().find(|o| o.name() == name)?;
        let rect = output_rect(output);
        let area = self.panel_free_rect(rect);
        let origin = geometry::default_origin(area);
        Some((origin, (area.2, area.3), geometry::offset_in(rect, origin)))
    }

    fn zone_output_rect(&self, id: u64) -> Option<Rect> {
        let name = &self.zones.zones[self.zones.zone_index(id)?].output;
        self.outputs
            .iter()
            .find(|o| o.name() == *name)
            .map(output_rect)
    }

    // ---- what the rest of jff asks ----

    /// Whether the toplevel at `root` is in a zone.
    pub fn zones_is_member(&self, root: &WlSurface) -> bool {
        !self.zones.is_idle() && self.zones.is_member(root)
    }

    /// A zone member showed its first buffer: its size is known now, place it again with that.
    pub fn zones_member_fitted(&mut self, root: &WlSurface) {
        let Some(ii) = self.zones.item_of_surface(root) else {
            return;
        };
        if let Some(wanted) = self.zones.items[ii].wanted {
            let item = self.zones.items[ii].resource.clone();
            self.zones_apply_position(&item, wanted, false);
        }
    }

    /// The other windows of the zone `window` is in, that live where it does.
    pub fn zones_mates(&self, window: &Window) -> Vec<Window> {
        if self.zones.is_idle() {
            return Vec::new();
        }
        let Some(id) = self.zone_of_window(window) else {
            return Vec::new();
        };
        let home = self.desk.home_of(|w| w == window);
        self.zone_members(id)
            .into_iter()
            .filter(|mate| mate != window && self.desk.home_of(|w| w == mate) == home)
            .collect()
    }

    /// A toplevel is gone: its item says so and leaves its zone.
    pub fn zones_toplevel_gone(&mut self, surface: &WlSurface) {
        if self.zones.is_idle() {
            return;
        }
        self.zones.pending.remove(surface);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.zones.items)
            .into_iter()
            .partition(|item| item.surface == *surface);
        self.zones.items = kept;
        for item in gone {
            let zone = item.zone.as_ref().map(|(id, _)| *id);
            if let Some((_, zone)) = &item.zone {
                if zone.is_alive() {
                    zone.item_left(&item.resource);
                }
            }
            item.resource.closed();
            if let Some(id) = zone {
                self.zones_drop_if_unused(id);
            }
        }
    }

    /// Once a frame: tell clients what changed, take members that left their monitor out of the
    /// zone, forget zones nobody uses.
    pub fn zones_sync(&mut self) {
        if self.zones.is_idle() {
            return;
        }
        let monitors: Vec<(String, Rect)> = self
            .outputs
            .iter()
            .map(|output| (output.name(), output_rect(output)))
            .collect();
        let rects: Vec<Rect> = monitors.iter().map(|(_, rect)| *rect).collect();

        // A member that was moved onto another monitor on its own is not in the zone any more.
        if !rects.is_empty() {
            let dragging = self.zones.dragging;
            let mut leaving = Vec::new();
            for item in &self.zones.items {
                let Some((id, _)) = &item.zone else { continue };
                if Some(*id) == dragging {
                    continue;
                }
                let Some(zone) = self.zones.zone_index(*id).map(|i| &self.zones.zones[i]) else {
                    continue;
                };
                let Some(entry) = self.desk.find_by(|w| {
                    w.toplevel()
                        .is_some_and(|t| t.wl_surface() == &item.surface)
                }) else {
                    continue;
                };
                let geo = entry.window.geometry().size;
                let at = layout::center(entry.pos, (geo.w, geo.h));
                if let Some(i) = layout::nearest(at, &rects) {
                    if monitors[i].0 != zone.output {
                        leaving.push(item.resource.clone());
                    }
                }
            }
            for resource in leaving {
                self.zones_leave(&resource);
            }
        }

        // What the zones look like now.
        for i in 0..self.zones.zones.len() {
            let name = self.zones.zones[i].output.clone();
            let Some((_, rect)) = monitors.iter().find(|(n, _)| *n == name) else {
                continue;
            };
            let area = self.panel_free_rect(*rect);
            let zone = &mut self.zones.zones[i];
            zone.offset = geometry::offset_in(*rect, zone.origin);
            let size = (area.2, area.3);
            if size != zone.size {
                zone.size = size;
                for resource in zone.refs.iter().filter(|r| r.is_alive()) {
                    resource.size(size.0, size.1);
                }
            }
        }

        // Where the members are.
        for ii in 0..self.zones.items.len() {
            let Some((id, _)) = self.zones.items[ii].zone.clone() else {
                continue;
            };
            let (Some(zi), Some(pos)) = (
                self.zones.zone_index(id),
                self.desk.pos_of(|w| {
                    w.toplevel()
                        .is_some_and(|t| t.wl_surface() == &self.zones.items[ii].surface)
                }),
            ) else {
                continue;
            };
            let rel = geometry::to_zone(self.zones.zones[zi].origin, pos);
            if self.zones.items[ii].last != Some(rel) {
                self.zones.items[ii].last = Some(rel);
                self.zones.items[ii].resource.position(rel.0, rel.1);
            }
        }

        let ids: Vec<u64> = self.zones.zones.iter().map(|zone| zone.id).collect();
        for id in ids {
            self.zones_drop_if_unused(id);
        }
    }

    fn zones_leave(&mut self, item: &XxZoneItemV1) {
        let Some(ii) = self.zones.item_index(item) else {
            return;
        };
        self.zones.items[ii].last = None;
        if let Some((id, zone)) = self.zones.items[ii].zone.take() {
            if zone.is_alive() {
                zone.item_left(item);
            }
            self.zones_drop_if_unused(id);
        }
    }

    /// The monitors moved or went away: every zone keeps its place on its monitor, or goes to
    /// the closest one that is left. Its floating members go along.
    pub fn zones_follow_outputs(&mut self) {
        if self.zones.is_idle() || self.outputs.is_empty() {
            return;
        }
        for i in 0..self.zones.zones.len() {
            let (id, name, origin, offset) = {
                let zone = &self.zones.zones[i];
                (zone.id, zone.output.clone(), zone.origin, zone.offset)
            };
            let target = match self.outputs.iter().find(|o| o.name() == name) {
                Some(output) => Some((
                    name.clone(),
                    geometry::origin_in(output_rect(output), offset),
                )),
                None => {
                    // Its monitor is gone: the closest, from where the zone was.
                    let rects = self.output_rects();
                    layout::nearest((origin.0 as f64, origin.1 as f64), &rects)
                        .and_then(|j| self.outputs.get(j))
                        .map(|output| output.name())
                        .and_then(|name| {
                            self.zone_placement(&name)
                                .map(|(origin, _, _)| (name, origin))
                        })
                }
            };
            let Some((name, new_origin)) = target else {
                continue;
            };
            self.zones.zones[i].output = name;
            self.zone_translate(id, geometry::delta(origin, new_origin), None);
        }
    }

    // ---- moving a zone ----

    /// Move zone `id`, its floating members (all but `skip`) and what its other members float
    /// back to, by `d`.
    fn zone_translate(&mut self, id: u64, d: Pos, skip: Option<&Window>) {
        if d == (0, 0) {
            return;
        }
        for window in self.zone_members(id) {
            if Some(&window) == skip {
                continue;
            }
            let Some((pos, mode, restore)) = self
                .desk
                .find_by(|w| *w == window)
                .map(|entry| (entry.pos, entry.mode, entry.restore))
            else {
                continue;
            };
            if mode == WindowMode::Floating {
                self.desk
                    .set_pos_where(|w| *w == window, geometry::shifted(pos, d));
            } else if let Some((restore_pos, _)) = restore {
                self.desk
                    .set_restore_pos_where(|w| *w == window, geometry::shifted(restore_pos, d));
            }
        }
        let rect = self.zone_output_rect(id);
        if let Some(i) = self.zones.zone_index(id) {
            let zone = &mut self.zones.zones[i];
            zone.origin = geometry::shifted(zone.origin, d);
            if let Some(rect) = rect {
                zone.offset = geometry::offset_in(rect, zone.origin);
            }
        }
    }

    /// The position and size of the floating members of zone `id`.
    fn zone_floating(&self, id: u64) -> Vec<(Pos, Size)> {
        self.zone_members(id)
            .iter()
            .filter_map(|window| {
                self.desk
                    .find_by(|w| w == window)
                    .filter(|entry| entry.mode == WindowMode::Floating)
                    .map(|entry| {
                        let geo = entry.window.geometry().size;
                        (entry.pos, (geo.w, geo.h))
                    })
            })
            .collect()
    }

    /// Put zone `id` on the monitor its floating members are mostly on.
    fn zone_rehome(&mut self, id: u64) {
        let rects = self.output_rects();
        let Some(center) = geometry::center_of(&self.zone_floating(id)) else {
            return;
        };
        let Some(i) = layout::nearest(center, &rects) else {
            return;
        };
        let name = self.outputs[i].name();
        if let Some(zi) = self.zones.zone_index(id) {
            self.zones.zones[zi].output = name;
        }
    }

    /// Bring the floating members of zone `id` back inside the monitor of the zone, together.
    fn zone_settle(&mut self, id: u64) {
        let Some(rect) = self.zone_output_rect(id) else {
            return;
        };
        let Some((pos, size)) = geometry::bounds_of(&self.zone_floating(id)) else {
            return;
        };
        let d = geometry::delta(pos, layout::fit_into(pos, size, rect));
        self.zone_translate(id, d, None);
    }

    /// `window`, a member, was moved by something else (the overview): the zone goes along, to
    /// the monitor the window is on now. `from` is where the window was.
    pub fn zones_carry(&mut self, window: &Window, from: Pos) {
        if self.zones.is_idle() {
            return;
        }
        let Some(id) = self.zone_of_window(window) else {
            return;
        };
        let Some(to) = self.desk.pos_of(|w| w == window) else {
            return;
        };
        self.zone_translate(id, geometry::delta(from, to), Some(window));
        self.zone_rehome(id);
        self.zone_settle(id);
    }

    /// What an Alt+drag of `window` moves, when `window` is a floating member of a zone.
    pub fn zone_drag_for(&self, window: &Window) -> Option<ZoneDrag> {
        if self.zones.is_idle() || !self.desk.is_floating(|w| w == window) {
            return None;
        }
        let id = self.zone_of_window(window)?;
        let zone = &self.zones.zones[self.zones.zone_index(id)?];
        let mates = self
            .zone_members(id)
            .into_iter()
            .filter(|mate| mate != window)
            .filter_map(|mate| {
                self.desk
                    .find_by(|w| *w == mate)
                    .filter(|entry| entry.mode == WindowMode::Floating)
                    .map(|entry| (mate.clone(), entry.pos))
            })
            .collect();
        Some(ZoneDrag {
            zone: id,
            origin: zone.origin,
            mates,
        })
    }

    /// An Alt+drag of a zone begins.
    pub fn zone_drag_begin(&mut self, drag: &ZoneDrag) {
        self.zones.dragging = Some(drag.zone);
    }

    /// The pointer has taken the dragged window `d` away from where it started.
    pub fn zone_drag_to(&mut self, drag: &ZoneDrag, d: Pos) {
        for (window, start) in &drag.mates {
            self.desk
                .set_pos_where(|w| w == window, geometry::shifted(*start, d));
        }
        if let Some(i) = self.zones.zone_index(drag.zone) {
            self.zones.zones[i].origin = geometry::shifted(drag.origin, d);
        }
        self.zone_rehome(drag.zone);
        if let Some(rect) = self.zone_output_rect(drag.zone) {
            if let Some(i) = self.zones.zone_index(drag.zone) {
                let zone = &mut self.zones.zones[i];
                zone.offset = geometry::offset_in(rect, zone.origin);
            }
        }
    }

    /// An Alt+drag of a zone is over.
    pub fn zone_drag_end(&mut self, drag: ZoneDrag) {
        self.zones.dragging = None;
        self.zone_settle(drag.zone);
    }
}
