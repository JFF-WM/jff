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

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: u64) -> View {
        View {
            id,
            app_id: Some("foot".to_string()),
            title: Some("~".to_string()),
            x: 40,
            y: 40,
            width: 800,
            height: 600,
            maximized: false,
            fullscreen: false,
            focused: false,
            output: Some("DP-1".to_string()),
            workspace: 1,
        }
    }

    fn output(name: &str, x: i32) -> OutputInfo {
        OutputInfo {
            name: name.to_string(),
            x,
            y: 0,
            width: 1920,
            height: 1080,
            scanout: false,
        }
    }

    fn snapshot(views: Vec<View>) -> Snapshot {
        Snapshot {
            views,
            workspace: 1,
            count: 9,
            focused: None,
            outputs: vec![output("DP-1", 0)],
            overview: false,
            screenshot: false,
        }
    }

    fn names(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .map(|e| {
                let json = e.to_json();
                let rest = json.strip_prefix("{\"event\":\"").unwrap();
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect()
    }

    #[test]
    fn nothing_changed_means_no_events() {
        let mut a = snapshot(vec![view(1), view(2)]);
        a.focused = Some(2);
        assert_eq!(diff(&a, &a.clone()), vec![]);
        assert_eq!(diff(&Snapshot::default(), &Snapshot::default()), vec![]);
    }

    #[test]
    fn the_focused_flag_of_a_view_alone_makes_no_event() {
        let old = snapshot(vec![view(1)]);
        let mut new = old.clone();
        new.views[0].focused = true;
        assert_eq!(diff(&old, &new), vec![]);
    }

    #[test]
    fn opening_a_view() {
        let old = snapshot(vec![]);
        let new = snapshot(vec![view(1)]);
        assert_eq!(diff(&old, &new), vec![Event::ViewOpened(view(1))]);
    }

    #[test]
    fn closing_a_view() {
        let old = snapshot(vec![view(1), view(2)]);
        let new = snapshot(vec![view(2)]);
        assert_eq!(diff(&old, &new), vec![Event::ViewClosed { id: 1 }]);
    }

    #[test]
    fn several_opened_and_closed_come_in_ascending_id_order() {
        let old = snapshot(vec![view(9), view(4)]);
        let new = snapshot(vec![view(8), view(6), view(7)]);
        assert_eq!(
            diff(&old, &new),
            vec![
                Event::ViewOpened(view(6)),
                Event::ViewOpened(view(7)),
                Event::ViewOpened(view(8)),
                Event::ViewClosed { id: 4 },
                Event::ViewClosed { id: 9 },
            ]
        );
    }

    #[test]
    fn focus_change() {
        let mut old = snapshot(vec![view(1), view(2)]);
        old.focused = Some(1);
        let mut new = old.clone();
        new.focused = Some(2);
        assert_eq!(
            diff(&old, &new),
            vec![Event::Focus {
                id: Some(2),
                previous: Some(1)
            }]
        );
        let mut none = old.clone();
        none.focused = None;
        assert_eq!(
            diff(&old, &none),
            vec![Event::Focus {
                id: None,
                previous: Some(1)
            }]
        );
        assert_eq!(
            diff(&none, &old),
            vec![Event::Focus {
                id: Some(1),
                previous: None
            }]
        );
    }

    #[test]
    fn a_new_view_is_opened_before_it_gets_the_focus() {
        let old = snapshot(vec![]);
        let mut new = snapshot(vec![view(1)]);
        new.focused = Some(1);
        new.views[0].focused = true;
        let events = diff(&old, &new);
        assert_eq!(names(&events), ["view-opened", "focus"]);
        assert_eq!(
            events[1],
            Event::Focus {
                id: Some(1),
                previous: None
            }
        );
    }

    #[test]
    fn a_closed_view_goes_after_the_focus_that_moved_away_from_it() {
        let mut old = snapshot(vec![view(1), view(2)]);
        old.focused = Some(2);
        let mut new = snapshot(vec![view(1)]);
        new.focused = Some(1);
        let events = diff(&old, &new);
        assert_eq!(names(&events), ["focus", "view-closed"]);
        assert_eq!(
            events[0],
            Event::Focus {
                id: Some(1),
                previous: Some(2)
            }
        );
        assert_eq!(events[1], Event::ViewClosed { id: 2 });
    }

    #[test]
    fn opening_one_view_and_closing_another_in_one_step() {
        let mut old = snapshot(vec![view(1)]);
        old.focused = Some(1);
        let mut new = snapshot(vec![view(2)]);
        new.focused = Some(2);
        assert_eq!(
            names(&diff(&old, &new)),
            ["view-opened", "focus", "view-closed"]
        );
    }

    #[test]
    fn title_and_app_id_changes() {
        let old = snapshot(vec![view(1), view(2)]);
        let mut new = old.clone();
        new.views[0].title = Some("~/Git/jff-wm".to_string());
        new.views[1].app_id = None;
        new.views[1].title = None;
        assert_eq!(
            diff(&old, &new),
            vec![
                Event::Title {
                    id: 1,
                    title: Some("~/Git/jff-wm".to_string())
                },
                Event::Title { id: 2, title: None },
                Event::AppId {
                    id: 2,
                    app_id: None
                },
            ]
        );
    }

    #[test]
    fn geometry_is_one_event_however_much_changed() {
        let old = snapshot(vec![view(1)]);
        let mut moved = old.clone();
        moved.views[0].x = 100;
        let mut both = old.clone();
        both.views[0].x = 100;
        both.views[0].y = 90;
        both.views[0].width = 640;
        both.views[0].height = 480;
        assert_eq!(
            diff(&old, &moved),
            vec![Event::Geometry {
                id: 1,
                x: 100,
                y: 40,
                width: 800,
                height: 600
            }]
        );
        assert_eq!(
            diff(&old, &both),
            vec![Event::Geometry {
                id: 1,
                x: 100,
                y: 90,
                width: 640,
                height: 480
            }]
        );
    }

    #[test]
    fn maximize_and_fullscreen() {
        let old = snapshot(vec![view(3)]);
        let mut new = old.clone();
        new.views[0].maximized = true;
        new.views[0].x = 0;
        new.views[0].y = 0;
        let events = diff(&old, &new);
        assert_eq!(names(&events), ["geometry", "maximized"]);
        assert_eq!(events[1], Event::Maximized { id: 3, on: true });
        assert_eq!(
            diff(&new, &old).last(),
            Some(&Event::Maximized { id: 3, on: false })
        );

        let mut full = old.clone();
        full.views[0].fullscreen = true;
        assert_eq!(
            diff(&old, &full),
            vec![Event::Fullscreen { id: 3, on: true }]
        );
        assert_eq!(
            diff(&full, &old),
            vec![Event::Fullscreen { id: 3, on: false }]
        );
    }

    #[test]
    fn a_view_changing_workspace() {
        let old = snapshot(vec![view(1)]);
        let mut new = old.clone();
        new.views[0].workspace = 4;
        assert_eq!(
            diff(&old, &new),
            vec![Event::ViewWorkspace {
                id: 1,
                workspace: 4
            }]
        );
    }

    #[test]
    fn the_shown_workspace_and_the_count() {
        let old = snapshot(vec![]);
        let mut shown = old.clone();
        shown.workspace = 3;
        let mut count = old.clone();
        count.count = 5;
        let mut both = shown.clone();
        both.count = 5;
        assert_eq!(
            diff(&old, &shown),
            vec![Event::Workspace {
                workspace: 3,
                count: 9
            }]
        );
        assert_eq!(
            diff(&old, &count),
            vec![Event::Workspace {
                workspace: 1,
                count: 5
            }]
        );
        assert_eq!(
            diff(&old, &both),
            vec![Event::Workspace {
                workspace: 3,
                count: 5
            }]
        );
    }

    #[test]
    fn switching_workspace_reports_the_workspace_before_the_focus() {
        let mut old = snapshot(vec![view(1), view(2)]);
        old.views[1].workspace = 2;
        old.focused = Some(1);
        let mut new = old.clone();
        new.workspace = 2;
        new.focused = Some(2);
        assert_eq!(names(&diff(&old, &new)), ["workspace", "focus"]);
    }

    #[test]
    fn outputs_added_removed_and_changed() {
        let old = snapshot(vec![]);
        let mut added = old.clone();
        added.outputs.push(output("HDMI-A-1", 1920));
        assert_eq!(
            diff(&old, &added),
            vec![Event::OutputAdded(output("HDMI-A-1", 1920))]
        );
        assert_eq!(
            diff(&added, &old),
            vec![Event::OutputRemoved {
                name: "HDMI-A-1".to_string()
            }]
        );

        let mut moved = added.clone();
        moved.outputs[1].x = 0;
        moved.outputs[1].width = 2560;
        assert_eq!(
            diff(&added, &moved),
            vec![Event::OutputChanged(OutputInfo {
                name: "HDMI-A-1".to_string(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1080,
                scanout: false,
            })]
        );
    }

    #[test]
    fn scanout_changes_are_output_changes() {
        let old = snapshot(vec![]);
        let mut new = old.clone();
        new.outputs[0].scanout = true;
        assert_eq!(
            diff(&old, &new),
            vec![Event::OutputChanged(OutputInfo {
                scanout: true,
                ..output("DP-1", 0)
            })]
        );
        assert_eq!(
            diff(&old, &new)[0].to_json(),
            "{\"event\":\"output-changed\",\"name\":\"DP-1\",\"x\":0,\"y\":0,\"width\":1920,\
             \"height\":1080,\"scanout\":true}"
        );
        assert_eq!(
            diff(&new, &old),
            vec![Event::OutputChanged(output("DP-1", 0))]
        );
    }

    #[test]
    fn overview_and_screenshot() {
        let old = snapshot(vec![]);
        let mut new = old.clone();
        new.overview = true;
        new.screenshot = true;
        assert_eq!(
            diff(&old, &new),
            vec![
                Event::Overview { open: true },
                Event::Screenshot { open: true }
            ]
        );
        assert_eq!(
            diff(&new, &old),
            vec![
                Event::Overview { open: false },
                Event::Screenshot { open: false }
            ]
        );
    }

    #[test]
    fn groups_are_parsed_and_collected() {
        assert_eq!(Group::parse("views"), Some(Group::Views));
        assert_eq!(Group::parse("focus"), Some(Group::Focus));
        assert_eq!(Group::parse("workspaces"), Some(Group::Workspaces));
        assert_eq!(Group::parse("outputs"), Some(Group::Outputs));
        assert_eq!(Group::parse("modes"), Some(Group::Modes));
        assert_eq!(Group::parse("view"), None);
        assert_eq!(Group::parse("Views"), None);

        let all = GroupSet::from_groups(&[]);
        for group in [
            Group::Views,
            Group::Focus,
            Group::Workspaces,
            Group::Outputs,
            Group::Modes,
        ] {
            assert!(all.contains(group));
        }
        let some = GroupSet::from_groups(&[Group::Views, Group::Modes, Group::Views]);
        assert!(some.contains(Group::Views));
        assert!(some.contains(Group::Modes));
        assert!(!some.contains(Group::Focus));
        assert!(!some.contains(Group::Workspaces));
        assert!(!some.contains(Group::Outputs));
    }

    #[test]
    fn every_event_belongs_to_its_group() {
        let v = view(1);
        let o = output("DP-1", 0);
        let cases = [
            (Event::ViewOpened(v.clone()), Group::Views),
            (Event::ViewClosed { id: 1 }, Group::Views),
            (Event::Title { id: 1, title: None }, Group::Views),
            (
                Event::AppId {
                    id: 1,
                    app_id: None,
                },
                Group::Views,
            ),
            (
                Event::Geometry {
                    id: 1,
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                Group::Views,
            ),
            (Event::Maximized { id: 1, on: true }, Group::Views),
            (Event::Fullscreen { id: 1, on: true }, Group::Views),
            (
                Event::ViewWorkspace {
                    id: 1,
                    workspace: 2,
                },
                Group::Views,
            ),
            (
                Event::Focus {
                    id: None,
                    previous: None,
                },
                Group::Focus,
            ),
            (
                Event::Workspace {
                    workspace: 1,
                    count: 2,
                },
                Group::Workspaces,
            ),
            (Event::OutputAdded(o.clone()), Group::Outputs),
            (
                Event::OutputRemoved {
                    name: "DP-1".to_string(),
                },
                Group::Outputs,
            ),
            (Event::OutputChanged(o), Group::Outputs),
            (Event::Overview { open: true }, Group::Modes),
            (Event::Screenshot { open: true }, Group::Modes),
        ];
        for (event, group) in cases {
            assert_eq!(event.group(), group, "{event:?}");
        }
    }

    #[test]
    fn a_view_is_written_like_the_documented_example() {
        let mut v = view(1);
        v.focused = true;
        assert_eq!(
            v.to_json(),
            "{\"id\":1,\"app_id\":\"foot\",\"title\":\"~\",\"x\":40,\"y\":40,\"width\":800,\
             \"height\":600,\"maximized\":false,\"fullscreen\":false,\"focused\":true,\
             \"output\":\"DP-1\",\"workspace\":1}"
        );
        v.app_id = None;
        v.title = None;
        v.output = None;
        assert!(v.to_json().contains("\"app_id\":null,\"title\":null"));
        assert!(v.to_json().contains("\"output\":null"));
    }

    #[test]
    fn events_are_written_as_one_json_line_each() {
        let cases: Vec<(Event, &str)> = vec![
            (
                Event::ViewOpened(view(5)),
                "{\"event\":\"view-opened\",\"id\":5,\"app_id\":\"foot\",\"title\":\"~\",\
                 \"x\":40,\"y\":40,\"width\":800,\"height\":600,\"maximized\":false,\
                 \"fullscreen\":false,\"focused\":false,\"output\":\"DP-1\",\"workspace\":1}",
            ),
            (
                Event::ViewClosed { id: 5 },
                "{\"event\":\"view-closed\",\"id\":5}",
            ),
            (
                Event::Focus {
                    id: Some(5),
                    previous: Some(3),
                },
                "{\"event\":\"focus\",\"id\":5,\"previous\":3}",
            ),
            (
                Event::Focus {
                    id: None,
                    previous: Some(3),
                },
                "{\"event\":\"focus\",\"id\":null,\"previous\":3}",
            ),
            (
                Event::Title {
                    id: 5,
                    title: Some("~/Git/jff-wm".to_string()),
                },
                "{\"event\":\"title\",\"id\":5,\"title\":\"~/Git/jff-wm\"}",
            ),
            (
                Event::Title { id: 5, title: None },
                "{\"event\":\"title\",\"id\":5,\"title\":null}",
            ),
            (
                Event::AppId {
                    id: 5,
                    app_id: Some("foot".to_string()),
                },
                "{\"event\":\"app-id\",\"id\":5,\"app_id\":\"foot\"}",
            ),
            (
                Event::Geometry {
                    id: 5,
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                },
                "{\"event\":\"geometry\",\"id\":5,\"x\":1,\"y\":2,\"width\":3,\"height\":4}",
            ),
            (
                Event::Maximized { id: 5, on: true },
                "{\"event\":\"maximized\",\"id\":5,\"on\":true}",
            ),
            (
                Event::Fullscreen { id: 5, on: false },
                "{\"event\":\"fullscreen\",\"id\":5,\"on\":false}",
            ),
            (
                Event::ViewWorkspace {
                    id: 5,
                    workspace: 2,
                },
                "{\"event\":\"view-workspace\",\"id\":5,\"workspace\":2}",
            ),
            (
                Event::Workspace {
                    workspace: 3,
                    count: 9,
                },
                "{\"event\":\"workspace\",\"workspace\":3,\"count\":9}",
            ),
            (
                Event::OutputAdded(output("DP-1", 0)),
                "{\"event\":\"output-added\",\"name\":\"DP-1\",\"x\":0,\"y\":0,\"width\":1920,\
                 \"height\":1080,\"scanout\":false}",
            ),
            (
                Event::OutputRemoved {
                    name: "DP-1".to_string(),
                },
                "{\"event\":\"output-removed\",\"name\":\"DP-1\"}",
            ),
            (
                Event::OutputChanged(output("DP-1", 0)),
                "{\"event\":\"output-changed\",\"name\":\"DP-1\",\"x\":0,\"y\":0,\"width\":1920,\
                 \"height\":1080,\"scanout\":false}",
            ),
            (
                Event::Overview { open: true },
                "{\"event\":\"overview\",\"open\":true}",
            ),
            (
                Event::Screenshot { open: false },
                "{\"event\":\"screenshot\",\"open\":false}",
            ),
        ];
        for (event, expected) in cases {
            let json = event.to_json();
            assert_eq!(json, expected);
            assert!(!json.contains('\n'));
        }
    }

    fn string_member(json: &str, key: &str) -> String {
        let marker = format!("\"{key}\":\"");
        let start = json.find(&marker).expect("member not found") + marker.len();
        let mut out = String::new();
        let mut chars = json[start..].chars();
        loop {
            match chars.next().expect("unterminated string") {
                '"' => return out,
                '\\' => match chars.next().unwrap() {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
                    }
                    other => panic!("unexpected escape \\{other}"),
                },
                c => {
                    assert!(c >= ' ', "raw control character in a JSON string");
                    out.push(c);
                }
            }
        }
    }

    #[test]
    fn awkward_titles_stay_valid_single_line_json() {
        let title = "say \"hi\" \\ back\nslash\ttab \u{1} ✓ héllo";
        let event = Event::Title {
            id: 7,
            title: Some(title.to_string()),
        };
        let json = event.to_json();
        assert!(!json.contains('\n'), "{json:?}");
        assert!(!json.contains('\r'));
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("✓ héllo"));
        assert_eq!(string_member(&json, "title"), title);

        let mut v = view(7);
        v.title = Some(title.to_string());
        v.app_id = Some(title.to_string());
        let json = Event::ViewOpened(v).to_json();
        assert!(!json.contains('\n'));
        assert_eq!(string_member(&json, "title"), title);
        assert_eq!(string_member(&json, "app_id"), title);

        let json = Event::OutputRemoved {
            name: title.to_string(),
        }
        .to_json();
        assert!(!json.contains('\n'));
        assert_eq!(string_member(&json, "name"), title);
    }

    #[test]
    fn the_snapshot_has_every_group_by_default() {
        let mut s = snapshot(vec![view(1), view(2)]);
        s.workspace = 2;
        s.focused = Some(2);
        s.views[1].focused = true;
        s.outputs.push(output("HDMI-A-1", 1920));
        let json = s.to_json(GroupSet::ALL);
        assert_eq!(
            json,
            format!(
                "{{\"event\":\"snapshot\",\"views\":[{},{}],\"workspace\":2,\"count\":9,\
                 \"focused\":2,\"outputs\":[{},{}],\"overview\":false,\"screenshot\":false}}",
                s.views[0].to_json(),
                s.views[1].to_json(),
                s.outputs[0].to_json(),
                s.outputs[1].to_json(),
            )
        );
        assert!(!json.contains('\n'));
    }

    #[test]
    fn an_empty_snapshot_is_valid() {
        let s = Snapshot {
            workspace: 1,
            count: 9,
            ..Snapshot::default()
        };
        assert_eq!(
            s.to_json(GroupSet::ALL),
            "{\"event\":\"snapshot\",\"views\":[],\"workspace\":1,\"count\":9,\
             \"focused\":null,\"outputs\":[],\"overview\":false,\"screenshot\":false}"
        );
    }

    #[test]
    fn the_snapshot_only_has_the_asked_for_groups() {
        let mut s = snapshot(vec![view(1)]);
        s.focused = Some(1);
        assert_eq!(
            s.to_json(GroupSet::from_groups(&[Group::Workspaces])),
            "{\"event\":\"snapshot\",\"workspace\":1,\"count\":9}"
        );
        assert_eq!(
            s.to_json(GroupSet::from_groups(&[Group::Focus, Group::Modes])),
            "{\"event\":\"snapshot\",\"focused\":1,\"overview\":false,\"screenshot\":false}"
        );
        let json = s.to_json(GroupSet::from_groups(&[Group::Views]));
        assert!(json.starts_with("{\"event\":\"snapshot\",\"views\":[{\"id\":1,"));
        assert!(!json.contains("workspace\":1,\"count"));
        assert!(!json.contains("outputs"));
        assert!(!json.contains("overview"));
    }
}
