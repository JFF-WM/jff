//! The events behind `jff msg watch` (see the doc header of `ipc.rs` for the wire format).
//!
//! Nothing here knows about Smithay. `JffState::take_snapshot` copies what a panel cares about
//! into a `Snapshot` of plain values. `diff` compares two of them and returns the `Event`s that
//! turn the old one into the new one. Comparing instead of calling an `emit` from every place
//! that changes something means a change can't be forgotten, and a burst (a drag) costs one
//! event per loop iteration.

use std::collections::HashMap;

use crate::primitives::json::{json_opt, json_str};

// ---- groups -----------------------------------------------------------------------------------

/// A kind of event a client can ask for: `watch views workspaces` only sends those.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Windows appearing, going away, or changing title, app id, size, position, state.
    Views,
    /// The window the user works in changed.
    Focus,
    /// The shown workspace, or the workspace count, changed.
    Workspaces,
    /// Monitors added, removed, or changed.
    Outputs,
    /// Overview and screenshot selection opening or closing.
    Modes,
}

impl Group {
    /// Group names as typed on the command line, for error messages.
    pub const NAMES: &'static str = "views, focus, workspaces, outputs, modes";

    pub fn parse(word: &str) -> Option<Group> {
        match word {
            "views" => Some(Group::Views),
            "focus" => Some(Group::Focus),
            "workspaces" => Some(Group::Workspaces),
            "outputs" => Some(Group::Outputs),
            "modes" => Some(Group::Modes),
            _ => None,
        }
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// The groups one watcher asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupSet(u8);

impl GroupSet {
    pub const ALL: GroupSet = GroupSet(0b1_1111);

    /// No groups named means all of them.
    pub fn from_groups(groups: &[Group]) -> GroupSet {
        if groups.is_empty() {
            return GroupSet::ALL;
        }
        GroupSet(groups.iter().fold(0, |bits, group| bits | group.bit()))
    }

    pub fn contains(self, group: Group) -> bool {
        self.0 & group.bit() != 0
    }
}

// ---- the state a panel can see ----------------------------------------------------------------

/// One window, as `view_json` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: u64,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    pub fullscreen: bool,
    /// Whether the window has the keyboard. `diff` ignores it, `Snapshot::focused` is what
    /// produces `focus` events.
    pub focused: bool,
    pub output: Option<String>,
    /// The workspace the window lives on, counting from 1.
    pub workspace: usize,
}

impl View {
    /// The JSON object members, without braces (events splice them in flat).
    fn members(&self) -> String {
        format!(
            "\"id\":{},\"app_id\":{},\"title\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{},\
             \"maximized\":{},\"fullscreen\":{},\"focused\":{},\"output\":{},\"workspace\":{}",
            self.id,
            json_opt(self.app_id.as_deref()),
            json_opt(self.title.as_deref()),
            self.x,
            self.y,
            self.width,
            self.height,
            self.maximized,
            self.fullscreen,
            self.focused,
            json_opt(self.output.as_deref()),
            self.workspace,
        )
    }

    pub fn to_json(&self) -> String {
        format!("{{{}}}", self.members())
    }
}

/// One monitor, and the area it covers in the shared space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The monitor's primary plane is showing a client's buffer directly (direct scanout), not
    /// a frame jff composited. Always false on winit.
    pub scanout: bool,
}

impl OutputInfo {
    fn members(&self) -> String {
        format!(
            "\"name\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"scanout\":{}",
            json_str(&self.name),
            self.x,
            self.y,
            self.width,
            self.height,
            self.scanout
        )
    }

    pub fn to_json(&self) -> String {
        format!("{{{}}}", self.members())
    }
}

/// Everything `watch` reports, at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Top-to-bottom stacking order, like the `views` command.
    pub views: Vec<View>,
    /// The shown workspace, counting from 1.
    pub workspace: usize,
    pub count: usize,
    /// The view that has the keyboard. Always one of `views`.
    pub focused: Option<u64>,
    pub outputs: Vec<OutputInfo>,
    pub overview: bool,
    pub screenshot: bool,
}

impl Snapshot {
    /// The `snapshot` event, with only the members of `groups`.
    pub fn to_json(&self, groups: GroupSet) -> String {
        let mut out = String::from("{\"event\":\"snapshot\"");
        if groups.contains(Group::Views) {
            let views: Vec<String> = self.views.iter().map(View::to_json).collect();
            out.push_str(&format!(",\"views\":[{}]", views.join(",")));
        }
        if groups.contains(Group::Workspaces) {
            out.push_str(&format!(
                ",\"workspace\":{},\"count\":{}",
                self.workspace, self.count
            ));
        }
        if groups.contains(Group::Focus) {
            out.push_str(&format!(",\"focused\":{}", json_id(self.focused)));
        }
        if groups.contains(Group::Outputs) {
            let outputs: Vec<String> = self.outputs.iter().map(OutputInfo::to_json).collect();
            out.push_str(&format!(",\"outputs\":[{}]", outputs.join(",")));
        }
        if groups.contains(Group::Modes) {
            out.push_str(&format!(
                ",\"overview\":{},\"screenshot\":{}",
                self.overview, self.screenshot
            ));
        }
        out.push('}');
        out
    }
}

// ---- events -----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    ViewOpened(View),
    ViewClosed {
        id: u64,
    },
    Focus {
        id: Option<u64>,
        previous: Option<u64>,
    },
    Title {
        id: u64,
        title: Option<String>,
    },
    AppId {
        id: u64,
        app_id: Option<String>,
    },
    Geometry {
        id: u64,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Maximized {
        id: u64,
        on: bool,
    },
    Fullscreen {
        id: u64,
        on: bool,
    },
    ViewWorkspace {
        id: u64,
        workspace: usize,
    },
    Workspace {
        workspace: usize,
        count: usize,
    },
    OutputAdded(OutputInfo),
    OutputRemoved {
        name: String,
    },
    OutputChanged(OutputInfo),
    Overview {
        open: bool,
    },
    Screenshot {
        open: bool,
    },
}

fn json_id(id: Option<u64>) -> String {
    id.map_or_else(|| "null".to_string(), |id| id.to_string())
}

impl Event {
    /// The group a client has to ask for to get this event.
    pub fn group(&self) -> Group {
        match self {
            Event::ViewOpened(_)
            | Event::ViewClosed { .. }
            | Event::Title { .. }
            | Event::AppId { .. }
            | Event::Geometry { .. }
            | Event::Maximized { .. }
            | Event::Fullscreen { .. }
            | Event::ViewWorkspace { .. } => Group::Views,
            Event::Focus { .. } => Group::Focus,
            Event::Workspace { .. } => Group::Workspaces,
            Event::OutputAdded(_) | Event::OutputRemoved { .. } | Event::OutputChanged(_) => {
                Group::Outputs
            }
            Event::Overview { .. } | Event::Screenshot { .. } => Group::Modes,
        }
    }

    /// The event as one line of JSON, no trailing newline.
    pub fn to_json(&self) -> String {
        match self {
            Event::ViewOpened(view) => {
                format!("{{\"event\":\"view-opened\",{}}}", view.members())
            }
            Event::ViewClosed { id } => format!("{{\"event\":\"view-closed\",\"id\":{id}}}"),
            Event::Focus { id, previous } => format!(
                "{{\"event\":\"focus\",\"id\":{},\"previous\":{}}}",
                json_id(*id),
                json_id(*previous)
            ),
            Event::Title { id, title } => format!(
                "{{\"event\":\"title\",\"id\":{id},\"title\":{}}}",
                json_opt(title.as_deref())
            ),
            Event::AppId { id, app_id } => format!(
                "{{\"event\":\"app-id\",\"id\":{id},\"app_id\":{}}}",
                json_opt(app_id.as_deref())
            ),
            Event::Geometry {
                id,
                x,
                y,
                width,
                height,
            } => format!(
                "{{\"event\":\"geometry\",\"id\":{id},\"x\":{x},\"y\":{y},\"width\":{width},\
                 \"height\":{height}}}"
            ),
            Event::Maximized { id, on } => {
                format!("{{\"event\":\"maximized\",\"id\":{id},\"on\":{on}}}")
            }
            Event::Fullscreen { id, on } => {
                format!("{{\"event\":\"fullscreen\",\"id\":{id},\"on\":{on}}}")
            }
            Event::ViewWorkspace { id, workspace } => {
                format!("{{\"event\":\"view-workspace\",\"id\":{id},\"workspace\":{workspace}}}")
            }
            Event::Workspace { workspace, count } => {
                format!("{{\"event\":\"workspace\",\"workspace\":{workspace},\"count\":{count}}}")
            }
            Event::OutputAdded(output) => {
                format!("{{\"event\":\"output-added\",{}}}", output.members())
            }
            Event::OutputRemoved { name } => {
                format!(
                    "{{\"event\":\"output-removed\",\"name\":{}}}",
                    json_str(name)
                )
            }
            Event::OutputChanged(output) => {
                format!("{{\"event\":\"output-changed\",{}}}", output.members())
            }
            Event::Overview { open } => format!("{{\"event\":\"overview\",\"open\":{open}}}"),
            Event::Screenshot { open } => format!("{{\"event\":\"screenshot\",\"open\":{open}}}"),
        }
    }
}

/// The events that turn `old` into `new`, in the order a client should see them. Monitors are
/// added or changed first, then windows open (ascending id), then changes on windows already
/// there (ascending id, and per window: title, app id, geometry, maximized, fullscreen,
/// workspace), then the shown workspace, then focus, then windows close (ascending id, after
/// the focus event that moved away from them), then monitors removed, overview, screenshot.
///
/// Nothing changed means no events. A window's geometry is one event however many of x, y,
/// width, height changed, which is also what coalesces a drag into one event per iteration.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Vec<Event> {
    let mut events = Vec::new();

    for output in &new.outputs {
        match old.outputs.iter().find(|o| o.name == output.name) {
            None => events.push(Event::OutputAdded(output.clone())),
            Some(before) if before != output => events.push(Event::OutputChanged(output.clone())),
            Some(_) => {}
        }
    }

    let old_views: HashMap<u64, &View> = old.views.iter().map(|v| (v.id, v)).collect();
    let new_views: HashMap<u64, &View> = new.views.iter().map(|v| (v.id, v)).collect();

    let mut opened: Vec<&View> = new
        .views
        .iter()
        .filter(|v| !old_views.contains_key(&v.id))
        .collect();
    opened.sort_by_key(|v| v.id);
    events.extend(opened.into_iter().map(|v| Event::ViewOpened(v.clone())));

    let mut kept: Vec<(&View, &View)> = new
        .views
        .iter()
        .filter_map(|after| old_views.get(&after.id).map(|before| (*before, after)))
        .collect();
    kept.sort_by_key(|(_, after)| after.id);
    for (before, after) in kept {
        let id = after.id;
        if before.title != after.title {
            events.push(Event::Title {
                id,
                title: after.title.clone(),
            });
        }
        if before.app_id != after.app_id {
            events.push(Event::AppId {
                id,
                app_id: after.app_id.clone(),
            });
        }
        if (before.x, before.y, before.width, before.height)
            != (after.x, after.y, after.width, after.height)
        {
            events.push(Event::Geometry {
                id,
                x: after.x,
                y: after.y,
                width: after.width,
                height: after.height,
            });
        }
        if before.maximized != after.maximized {
            events.push(Event::Maximized {
                id,
                on: after.maximized,
            });
        }
        if before.fullscreen != after.fullscreen {
            events.push(Event::Fullscreen {
                id,
                on: after.fullscreen,
            });
        }
        if before.workspace != after.workspace {
            events.push(Event::ViewWorkspace {
                id,
                workspace: after.workspace,
            });
        }
    }

    if (old.workspace, old.count) != (new.workspace, new.count) {
        events.push(Event::Workspace {
            workspace: new.workspace,
            count: new.count,
        });
    }

    if old.focused != new.focused {
        events.push(Event::Focus {
            id: new.focused,
            previous: old.focused,
        });
    }

    let mut closed: Vec<u64> = old
        .views
        .iter()
        .map(|v| v.id)
        .filter(|id| !new_views.contains_key(id))
        .collect();
    closed.sort_unstable();
    events.extend(closed.into_iter().map(|id| Event::ViewClosed { id }));

    for output in &old.outputs {
        if !new.outputs.iter().any(|o| o.name == output.name) {
            events.push(Event::OutputRemoved {
                name: output.name.clone(),
            });
        }
    }

    if old.overview != new.overview {
        events.push(Event::Overview { open: new.overview });
    }
    if old.screenshot != new.screenshot {
        events.push(Event::Screenshot {
            open: new.screenshot,
        });
    }

    events
}
