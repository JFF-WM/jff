//! Tests for `jff_wm::ipc::events`.

use jff_wm::ipc::events::{
    diff, ContainerInfo, ContainerRef, Event, Group, GroupSet, LayerInfo, OutputColor, OutputHdr,
    OutputInfo, PopupInfo, Snapshot, WindowInfo,
};
use jff_wm::primitives::color::{ImageDescription, NamedPrimaries};

fn window(id: u64) -> WindowInfo {
    WindowInfo {
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
        urgent: false,
        output: Some("DP-1".to_string()),
        workspace: 1,
        pid: Some(4242),
        container: None,
        tag: None,
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
        adaptive_sync: false,
        color: OutputColor::default(),
        hdr: OutputHdr::default(),
    }
}

fn snapshot(windows: Vec<WindowInfo>) -> Snapshot {
    Snapshot {
        windows,
        workspace: 1,
        count: 9,
        focused: None,
        outputs: vec![output("DP-1", 0)],
        overview: false,
        screenshot: false,
        ..Snapshot::default()
    }
}

fn popup(id: u64, window: u64) -> PopupInfo {
    PopupInfo {
        id,
        window: Some(window),
        layer: None,
        layer_namespace: None,
        x: 100,
        y: 200,
        width: 150,
        height: 90,
    }
}

fn layer(id: u64) -> LayerInfo {
    LayerInfo {
        id,
        namespace: "panel".to_string(),
        layer: "top".to_string(),
        output: "DP-1".to_string(),
        x: 0,
        y: 1040,
        width: 1920,
        height: 40,
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
    let mut a = snapshot(vec![window(1), window(2)]);
    a.focused = Some(2);
    assert_eq!(diff(&a, &a.clone()), vec![]);
    assert_eq!(diff(&Snapshot::default(), &Snapshot::default()), vec![]);
}

#[test]
fn the_focused_flag_of_a_window_alone_makes_no_event() {
    let old = snapshot(vec![window(1)]);
    let mut new = old.clone();
    new.windows[0].focused = true;
    assert_eq!(diff(&old, &new), vec![]);
}

#[test]
fn opening_a_window() {
    let old = snapshot(vec![]);
    let new = snapshot(vec![window(1)]);
    assert_eq!(diff(&old, &new), vec![Event::WindowOpened(window(1))]);
}

#[test]
fn closing_a_window() {
    let old = snapshot(vec![window(1), window(2)]);
    let new = snapshot(vec![window(2)]);
    assert_eq!(diff(&old, &new), vec![Event::WindowClosed { id: 1 }]);
}

#[test]
fn several_opened_and_closed_come_in_ascending_id_order() {
    let old = snapshot(vec![window(9), window(4)]);
    let new = snapshot(vec![window(8), window(6), window(7)]);
    assert_eq!(
        diff(&old, &new),
        vec![
            Event::WindowOpened(window(6)),
            Event::WindowOpened(window(7)),
            Event::WindowOpened(window(8)),
            Event::WindowClosed { id: 4 },
            Event::WindowClosed { id: 9 },
        ]
    );
}

#[test]
fn focus_change() {
    let mut old = snapshot(vec![window(1), window(2)]);
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
fn a_new_window_is_opened_before_it_gets_the_focus() {
    let old = snapshot(vec![]);
    let mut new = snapshot(vec![window(1)]);
    new.focused = Some(1);
    new.windows[0].focused = true;
    let events = diff(&old, &new);
    assert_eq!(names(&events), ["window-opened", "focus"]);
    assert_eq!(
        events[1],
        Event::Focus {
            id: Some(1),
            previous: None
        }
    );
}

#[test]
fn a_closed_window_goes_after_the_focus_that_moved_away_from_it() {
    let mut old = snapshot(vec![window(1), window(2)]);
    old.focused = Some(2);
    let mut new = snapshot(vec![window(1)]);
    new.focused = Some(1);
    let events = diff(&old, &new);
    assert_eq!(names(&events), ["focus", "window-closed"]);
    assert_eq!(
        events[0],
        Event::Focus {
            id: Some(1),
            previous: Some(2)
        }
    );
    assert_eq!(events[1], Event::WindowClosed { id: 2 });
}

#[test]
fn opening_one_window_and_closing_another_in_one_step() {
    let mut old = snapshot(vec![window(1)]);
    old.focused = Some(1);
    let mut new = snapshot(vec![window(2)]);
    new.focused = Some(2);
    assert_eq!(
        names(&diff(&old, &new)),
        ["window-opened", "focus", "window-closed"]
    );
}

#[test]
fn title_and_app_id_changes() {
    let old = snapshot(vec![window(1), window(2)]);
    let mut new = old.clone();
    new.windows[0].title = Some("~/Git/jff-wm".to_string());
    new.windows[1].app_id = None;
    new.windows[1].title = None;
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
fn a_window_tag_appears_changes_and_goes() {
    let old = snapshot(vec![window(1)]);
    let mut tagged = old.clone();
    tagged.windows[0].tag = Some("browser".to_string());
    assert_eq!(
        diff(&old, &tagged),
        vec![Event::Tag {
            id: 1,
            tag: Some("browser".to_string())
        }]
    );
    assert!(diff(&tagged, &tagged).is_empty());
    assert_eq!(
        diff(&tagged, &old),
        vec![Event::Tag { id: 1, tag: None }]
    );
    assert!(tagged.windows[0]
        .to_json()
        .ends_with("\"container\":null,\"tag\":\"browser\"}"));
}

#[test]
fn a_drag_starts_moves_and_ends() {
    let old = snapshot(vec![window(1)]);
    let mut start = old.clone();
    start.dragging = Some(1);
    start.windows[0].x = 100;
    let mut end = start.clone();
    end.dragging = None;
    end.windows[0].x = 120;
    assert_eq!(
        diff(&old, &start),
        vec![
            Event::Drag { id: 1, on: true },
            Event::Geometry {
                id: 1,
                x: 100,
                y: 40,
                width: 800,
                height: 600
            }
        ]
    );
    assert_eq!(
        diff(&start, &end),
        vec![
            Event::Geometry {
                id: 1,
                x: 120,
                y: 40,
                width: 800,
                height: 600
            },
            Event::Drag { id: 1, on: false }
        ]
    );
    // Still dragging, nothing else changed: nothing to say.
    assert_eq!(diff(&start, &start), vec![]);
}

#[test]
fn drag_events_are_flat_json() {
    assert_eq!(
        Event::Drag { id: 3, on: true }.to_json(),
        "{\"event\":\"drag\",\"id\":3,\"on\":true}"
    );
}

#[test]
fn geometry_is_one_event_however_much_changed() {
    let old = snapshot(vec![window(1)]);
    let mut moved = old.clone();
    moved.windows[0].x = 100;
    let mut both = old.clone();
    both.windows[0].x = 100;
    both.windows[0].y = 90;
    both.windows[0].width = 640;
    both.windows[0].height = 480;
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
    let old = snapshot(vec![window(3)]);
    let mut new = old.clone();
    new.windows[0].maximized = true;
    new.windows[0].x = 0;
    new.windows[0].y = 0;
    let events = diff(&old, &new);
    assert_eq!(names(&events), ["geometry", "maximized"]);
    assert_eq!(events[1], Event::Maximized { id: 3, on: true });
    assert_eq!(
        diff(&new, &old).last(),
        Some(&Event::Maximized { id: 3, on: false })
    );

    let mut full = old.clone();
    full.windows[0].fullscreen = true;
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
fn urgent_is_an_event_and_a_member_of_the_window() {
    let old = Snapshot {
        windows: vec![window(3)],
        ..Snapshot::default()
    };
    let mut urgent = old.clone();
    urgent.windows[0].urgent = true;
    assert_eq!(diff(&old, &urgent), vec![Event::Urgent { id: 3, on: true }]);
    assert_eq!(
        diff(&urgent, &old),
        vec![Event::Urgent { id: 3, on: false }]
    );
    assert_eq!(
        Event::Urgent { id: 3, on: true }.to_json(),
        "{\"event\":\"urgent\",\"id\":3,\"on\":true}"
    );
    assert!(urgent.windows[0].to_json().contains("\"urgent\":true"));
    assert!(old.windows[0].to_json().contains("\"urgent\":false"));
}

#[test]
fn a_window_changing_workspace() {
    let old = snapshot(vec![window(1)]);
    let mut new = old.clone();
    new.windows[0].workspace = 4;
    assert_eq!(
        diff(&old, &new),
        vec![Event::WindowWorkspace {
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
    let mut old = snapshot(vec![window(1), window(2)]);
    old.windows[1].workspace = 2;
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
            adaptive_sync: false,
            color: OutputColor::default(),
            hdr: OutputHdr::default(),
        })]
    );
}

#[test]
fn adaptive_sync_changes_are_output_changes() {
    let old = snapshot(vec![]);
    let mut new = old.clone();
    new.outputs[0].adaptive_sync = true;
    assert_eq!(
        diff(&old, &new),
        vec![Event::OutputChanged(OutputInfo {
            adaptive_sync: true,
            ..output("DP-1", 0)
        })]
    );
    assert_eq!(
        diff(&old, &new)[0].to_json(),
        "{\"event\":\"output-changed\",\"name\":\"DP-1\",\"x\":0,\"y\":0,\"width\":1920,\
             \"height\":1080,\"scanout\":false,\"adaptive_sync\":true,\"color\":{\"profile\":null,\"primaries\":\"srgb\",\"transfer\":\"srgb\",\"white\":\"d65\",\"gamut\":\"unknown\",\"icc_readable\":false},\"hdr\":{\"requested\":false,\"active\":false,\"reason\":null,\"eotf\":null,\"peak\":null,\"peak_source\":null,\"sdr_white\":203,\"bits\":null,\"night_light\":false,\"content_light\":null,\"scanout\":{\"active\":false,\"reason\":null}}}"
    );
    assert_eq!(
        diff(&new, &old),
        vec![Event::OutputChanged(output("DP-1", 0))]
    );
}

const P3_PROFILE: &str = "/home/me/.local/share/color/icc/DP-1.icc";

/// A monitor whose profile says Display P3.
fn p3_color() -> OutputColor {
    let mut description = ImageDescription::srgb();
    description.primaries = NamedPrimaries::DisplayP3.chromaticities();
    OutputColor::new(Some(P3_PROFILE.to_string()), &description, true)
}

#[test]
fn color_changes_are_output_changes() {
    let old = snapshot(vec![]);
    let mut new = old.clone();
    new.outputs[0].color = p3_color();
    assert_eq!(
        diff(&old, &new),
        vec![Event::OutputChanged(OutputInfo {
            color: p3_color(),
            ..output("DP-1", 0)
        })]
    );
    assert_eq!(
        diff(&new, &old),
        vec![Event::OutputChanged(output("DP-1", 0))]
    );
    assert!(diff(&new, &new.clone()).is_empty());
}

#[test]
fn a_profiled_monitor_has_named_colors() {
    let color = p3_color();
    assert_eq!(color.primaries.as_deref(), Some("display-p3"));
    assert_eq!(color.transfer, "srgb");
    assert_eq!(color.white, "d65");
    assert_eq!(color.gamut, "wide");
    assert!(color.icc_readable);
    assert_eq!(
        color.to_json(),
        format!(
            "{{\"profile\":\"{P3_PROFILE}\",\"primaries\":\"display-p3\",\"transfer\":\"srgb\",\
             \"white\":\"d65\",\"gamut\":\"wide\",\"icc_readable\":true}}"
        )
    );
}

#[test]
fn a_monitor_without_a_profile_is_srgb_of_unknown_gamut() {
    let color = OutputColor::default();
    assert_eq!(color.profile, None);
    assert_eq!(color.primaries.as_deref(), Some("srgb"));
    assert_eq!(color.gamut, "unknown");
    assert!(!color.icc_readable);
    assert_eq!(
        color.to_json(),
        "{\"profile\":null,\"primaries\":\"srgb\",\"transfer\":\"srgb\",\"white\":\"d65\",\
         \"gamut\":\"unknown\",\"icc_readable\":false}"
    );
}

#[test]
fn a_profile_that_cannot_be_read_keeps_its_path_and_the_gamut_is_unknown() {
    // The description is the sRGB fallback, whatever it says about gamut is not known.
    let color = OutputColor::new(
        Some("/nonexistent/DP-1.icc".to_string()),
        &ImageDescription::srgb(),
        false,
    );
    assert_eq!(color.profile.as_deref(), Some("/nonexistent/DP-1.icc"));
    assert_eq!(color.gamut, "unknown");
    assert!(!color.icc_readable);
}

#[test]
fn a_profile_that_says_srgb_makes_the_gamut_srgb() {
    let color = OutputColor::new(
        Some(P3_PROFILE.to_string()),
        &ImageDescription::srgb(),
        true,
    );
    assert_eq!(color.gamut, "srgb");
    assert_eq!(color.primaries.as_deref(), Some("srgb"));
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
             \"height\":1080,\"scanout\":true,\"adaptive_sync\":false,\"color\":{\"profile\":null,\"primaries\":\"srgb\",\"transfer\":\"srgb\",\"white\":\"d65\",\"gamut\":\"unknown\",\"icc_readable\":false},\"hdr\":{\"requested\":false,\"active\":false,\"reason\":null,\"eotf\":null,\"peak\":null,\"peak_source\":null,\"sdr_white\":203,\"bits\":null,\"night_light\":false,\"content_light\":null,\"scanout\":{\"active\":false,\"reason\":null}}}"
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
    assert_eq!(Group::parse("windows"), Some(Group::Windows));
    assert_eq!(Group::parse("focus"), Some(Group::Focus));
    assert_eq!(Group::parse("workspaces"), Some(Group::Workspaces));
    assert_eq!(Group::parse("outputs"), Some(Group::Outputs));
    assert_eq!(Group::parse("modes"), Some(Group::Modes));
    assert_eq!(Group::parse("popups"), Some(Group::Popups));
    assert_eq!(Group::parse("layers"), Some(Group::Layers));
    assert_eq!(Group::parse("window"), None);
    assert_eq!(Group::parse("Windows"), None);

    let all = GroupSet::from_groups(&[]);
    for group in [
        Group::Windows,
        Group::Focus,
        Group::Workspaces,
        Group::Outputs,
        Group::Modes,
        Group::Popups,
        Group::Layers,
    ] {
        assert!(all.contains(group));
    }
    let some = GroupSet::from_groups(&[Group::Windows, Group::Modes, Group::Windows]);
    assert!(some.contains(Group::Windows));
    assert!(some.contains(Group::Modes));
    assert!(!some.contains(Group::Focus));
    assert!(!some.contains(Group::Workspaces));
    assert!(!some.contains(Group::Outputs));
}

#[test]
fn every_event_belongs_to_its_group() {
    let v = window(1);
    let o = output("DP-1", 0);
    let cases = [
        (Event::WindowOpened(v.clone()), Group::Windows),
        (Event::WindowClosed { id: 1 }, Group::Windows),
        (Event::Title { id: 1, title: None }, Group::Windows),
        (
            Event::AppId {
                id: 1,
                app_id: None,
            },
            Group::Windows,
        ),
        (
            Event::Tag {
                id: 1,
                tag: None,
            },
            Group::Windows,
        ),
        (
            Event::Geometry {
                id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            Group::Windows,
        ),
        (Event::Drag { id: 1, on: true }, Group::Windows),
        (Event::Maximized { id: 1, on: true }, Group::Windows),
        (Event::Fullscreen { id: 1, on: true }, Group::Windows),
        (Event::Urgent { id: 1, on: true }, Group::Windows),
        (
            Event::WindowWorkspace {
                id: 1,
                workspace: 2,
            },
            Group::Windows,
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
fn a_window_is_written_like_the_documented_example() {
    let mut v = window(1);
    v.focused = true;
    assert_eq!(
        v.to_json(),
        "{\"id\":1,\"app_id\":\"foot\",\"title\":\"~\",\"x\":40,\"y\":40,\"width\":800,\
             \"height\":600,\"maximized\":false,\"fullscreen\":false,\"focused\":true,\"urgent\":false,\
             \"output\":\"DP-1\",\"workspace\":1,\"pid\":4242,\"container\":null,\"tag\":null}"
    );
    v.app_id = None;
    v.title = None;
    v.output = None;
    assert!(v.to_json().contains("\"app_id\":null,\"title\":null"));
    assert!(v.to_json().contains("\"output\":null"));
    v.pid = None;
    assert!(v.to_json().contains("\"pid\":null,\"container\":null,\"tag\":null}"));
}

#[test]
fn a_window_in_a_container_names_it() {
    let mut v = window(1);
    v.container = Some(ContainerRef {
        id: 3,
        name: Some("Terminals".to_string()),
    });
    assert!(v
        .to_json()
        .ends_with("\"pid\":4242,\"container\":{\"id\":3,\"name\":\"Terminals\"}}"));
    v.container = Some(ContainerRef { id: 4, name: None });
    assert!(v
        .to_json()
        .ends_with("\"container\":{\"id\":4,\"name\":null}}"));
    // it is spliced flat into window-opened as well
    assert!(Event::WindowOpened(v)
        .to_json()
        .ends_with("\"container\":{\"id\":4,\"name\":null}}"));
}

#[test]
fn joining_leaving_or_renaming_a_container_is_a_window_container_event() {
    let named = |name: Option<&str>| {
        Some(ContainerRef {
            id: 2,
            name: name.map(str::to_string),
        })
    };
    let mut plain = window(1);
    let mut joined = window(1);
    joined.container = named(None);
    let events = diff(
        &snapshot(vec![plain.clone()]),
        &snapshot(vec![joined.clone()]),
    );
    assert_eq!(
        events,
        [Event::WindowContainer {
            id: 1,
            container: named(None)
        }]
    );
    assert_eq!(
        events[0].to_json(),
        "{\"event\":\"window-container\",\"id\":1,\"container\":{\"id\":2,\"name\":null}}"
    );
    assert_eq!(events[0].group(), Group::Windows);

    let mut renamed = window(1);
    renamed.container = named(Some("Mail"));
    assert_eq!(
        diff(&snapshot(vec![joined.clone()]), &snapshot(vec![renamed])).len(),
        1
    );

    plain.container = None;
    let left = diff(
        &snapshot(vec![joined.clone()]),
        &snapshot(vec![plain.clone()]),
    );
    assert_eq!(
        left[0].to_json(),
        "{\"event\":\"window-container\",\"id\":1,\"container\":null}"
    );
    // nothing changed, nothing said
    assert!(diff(&snapshot(vec![joined.clone()]), &snapshot(vec![joined])).is_empty());
}

#[test]
fn a_container_is_listed_with_its_windows_and_area() {
    let info = ContainerInfo {
        id: 1,
        name: Some("Terminals".to_string()),
        workspace: 2,
        output: Some("DP-1".to_string()),
        area: Some((10, 20, 1900, 1040)),
        windows: vec![3, 4],
        focused: Some(4),
        maximized: None,
    };
    assert_eq!(
        info.to_json(),
        "{\"id\":1,\"name\":\"Terminals\",\"workspace\":2,\"output\":\"DP-1\",\"x\":10,\"y\":20,\
         \"width\":1900,\"height\":1040,\"windows\":[3,4],\"focused\":4,\"maximized\":null}"
    );
    let bare = ContainerInfo {
        name: None,
        output: None,
        area: None,
        windows: vec![],
        focused: None,
        maximized: Some(3),
        ..info
    };
    assert_eq!(
        bare.to_json(),
        "{\"id\":1,\"name\":null,\"workspace\":2,\"output\":null,\"x\":null,\"y\":null,\
         \"width\":null,\"height\":null,\"windows\":[],\"focused\":null,\"maximized\":3}"
    );
}

#[test]
fn events_are_written_as_one_json_line_each() {
    let cases: Vec<(Event, &str)> = vec![
        (
            Event::WindowOpened(window(5)),
            "{\"event\":\"window-opened\",\"id\":5,\"app_id\":\"foot\",\"title\":\"~\",\
                 \"x\":40,\"y\":40,\"width\":800,\"height\":600,\"maximized\":false,\
                 \"fullscreen\":false,\"focused\":false,\"urgent\":false,\"output\":\"DP-1\",\"workspace\":1,\"pid\":4242,\
                 \"container\":null,\"tag\":null}",
        ),
        (
            Event::WindowClosed { id: 5 },
            "{\"event\":\"window-closed\",\"id\":5}",
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
            Event::Tag {
                id: 5,
                tag: Some("browser".to_string()),
            },
            "{\"event\":\"tag\",\"id\":5,\"tag\":\"browser\"}",
        ),
        (
            Event::Tag { id: 5, tag: None },
            "{\"event\":\"tag\",\"id\":5,\"tag\":null}",
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
            Event::WindowWorkspace {
                id: 5,
                workspace: 2,
            },
            "{\"event\":\"window-workspace\",\"id\":5,\"workspace\":2}",
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
                 \"height\":1080,\"scanout\":false,\"adaptive_sync\":false,\"color\":{\"profile\":null,\"primaries\":\"srgb\",\"transfer\":\"srgb\",\"white\":\"d65\",\"gamut\":\"unknown\",\"icc_readable\":false},\
                 \"hdr\":{\"requested\":false,\"active\":false,\"reason\":null,\"eotf\":null,\"peak\":null,\"peak_source\":null,\"sdr_white\":203,\"bits\":null,\"night_light\":false,\"content_light\":null,\"scanout\":{\"active\":false,\"reason\":null}}}",
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
                 \"height\":1080,\"scanout\":false,\"adaptive_sync\":false,\"color\":{\"profile\":null,\"primaries\":\"srgb\",\"transfer\":\"srgb\",\"white\":\"d65\",\"gamut\":\"unknown\",\"icc_readable\":false},\
                 \"hdr\":{\"requested\":false,\"active\":false,\"reason\":null,\"eotf\":null,\"peak\":null,\"peak_source\":null,\"sdr_white\":203,\"bits\":null,\"night_light\":false,\"content_light\":null,\"scanout\":{\"active\":false,\"reason\":null}}}",
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

    let mut v = window(7);
    v.title = Some(title.to_string());
    v.app_id = Some(title.to_string());
    let json = Event::WindowOpened(v).to_json();
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
    let mut s = snapshot(vec![window(1), window(2)]);
    s.workspace = 2;
    s.focused = Some(2);
    s.windows[1].focused = true;
    s.outputs.push(output("HDMI-A-1", 1920));
    let json = s.to_json(GroupSet::ALL);
    assert_eq!(
        json,
        format!(
            "{{\"event\":\"snapshot\",\"windows\":[{},{}],\"workspace\":2,\"count\":9,\
                 \"focused\":2,\"outputs\":[{},{}],\"overview\":false,\"screenshot\":false,\
                 \"locked\":false,\"popups\":[],\"layers\":[]}}",
            s.windows[0].to_json(),
            s.windows[1].to_json(),
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
        "{\"event\":\"snapshot\",\"windows\":[],\"workspace\":1,\"count\":9,\
             \"focused\":null,\"outputs\":[],\"overview\":false,\"screenshot\":false,\
             \"locked\":false,\"popups\":[],\"layers\":[]}"
    );
}

#[test]
fn the_snapshot_only_has_the_asked_for_groups() {
    let mut s = snapshot(vec![window(1)]);
    s.focused = Some(1);
    assert_eq!(
        s.to_json(GroupSet::from_groups(&[Group::Workspaces])),
        "{\"event\":\"snapshot\",\"workspace\":1,\"count\":9}"
    );
    assert_eq!(
        s.to_json(GroupSet::from_groups(&[Group::Focus, Group::Modes])),
        "{\"event\":\"snapshot\",\"focused\":1,\"overview\":false,\"screenshot\":false,\"locked\":false}"
    );
    let json = s.to_json(GroupSet::from_groups(&[Group::Windows]));
    assert!(json.starts_with("{\"event\":\"snapshot\",\"windows\":[{\"id\":1,"));
    assert!(!json.contains("workspace\":1,\"count"));
    assert!(!json.contains("outputs"));
    assert!(!json.contains("overview"));
}

#[test]
fn a_menu_opens_after_its_window_and_closes_before_it() {
    let mut old = snapshot(vec![window(1)]);
    old.popups = vec![popup(7, 1)];
    let mut new = snapshot(vec![window(1), window(2)]);
    new.popups = vec![popup(8, 2)];
    assert_eq!(
        names(&diff(&old, &new)),
        ["window-opened", "popup-opened", "popup-closed"]
    );

    let mut gone = snapshot(vec![]);
    gone.popups = vec![];
    let mut had = snapshot(vec![window(1)]);
    had.popups = vec![popup(7, 1)];
    assert_eq!(names(&diff(&had, &gone)), ["popup-closed", "window-closed"]);
}

#[test]
fn panels_open_first_and_close_last() {
    let old = snapshot(vec![window(1)]);
    let mut new = snapshot(vec![window(1), window(2)]);
    new.layers = vec![layer(3)];
    assert_eq!(names(&diff(&old, &new)), ["layer-opened", "window-opened"]);
    assert_eq!(names(&diff(&new, &old)), ["window-closed", "layer-closed"]);
}

#[test]
fn locking_the_session_is_an_event() {
    let old = snapshot(vec![]);
    let mut new = snapshot(vec![]);
    new.locked = true;
    let events = diff(&old, &new);
    assert_eq!(events, vec![Event::Lock { on: true }]);
    assert_eq!(events[0].group(), Group::Modes);
}

#[test]
fn popup_and_layer_events_are_flat_json() {
    assert_eq!(
        Event::PopupOpened(popup(7, 1)).to_json(),
        "{\"event\":\"popup-opened\",\"id\":7,\"window\":1,\"layer\":null,\"x\":100,\"y\":200,\
         \"width\":150,\"height\":90}"
    );
    assert_eq!(
        Event::LayerOpened(layer(3)).to_json(),
        "{\"event\":\"layer-opened\",\"id\":3,\"namespace\":\"panel\",\"layer\":\"top\",\
         \"output\":\"DP-1\",\"x\":0,\"y\":1040,\"width\":1920,\"height\":40}"
    );
    assert_eq!(Event::PopupClosed { id: 7 }.group(), Group::Popups);
    assert_eq!(Event::LayerClosed { id: 3 }.group(), Group::Layers);
}

#[test]
fn click_event_names_the_layer() {
    use jff_wm::ipc::events::{ClickInfo, ClickTarget};
    let event = Event::Click(ClickInfo {
        button: 273,
        x: 10,
        y: 20,
        target: ClickTarget::Layer {
            id: 4,
            namespace: "Nbar-Top".into(),
            layer: "top".into(),
            output: Some("DP-1".into()),
            popup: false,
        },
    });
    assert_eq!(event.group(), Group::Clicks);
    let json = event.to_json();
    assert!(json.contains("\"event\":\"click\""), "{json}");
    assert!(json.contains("\"namespace\":\"Nbar-Top\""), "{json}");
    assert!(json.contains("\"popup\":false"), "{json}");
    assert_eq!(Group::parse("clicks"), Some(Group::Clicks));
}

fn hdr_on() -> OutputHdr {
    OutputHdr {
        requested: true,
        active: true,
        eotf: Some("PQ".to_string()),
        peak: Some(1000),
        peak_source: Some("edid".to_string()),
        bits: Some(10),
        ..OutputHdr::default()
    }
}

#[test]
fn an_sdr_monitor_reports_hdr_as_off_at_the_default_white() {
    let hdr = OutputHdr::default();
    assert!(!hdr.requested && !hdr.active);
    assert_eq!(hdr.sdr_white, 203);
    assert_eq!(
        hdr.to_json(),
        "{\"requested\":false,\"active\":false,\"reason\":null,\"eotf\":null,\"peak\":null,\
         \"peak_source\":null,\"sdr_white\":203,\"bits\":null,\"night_light\":false,\
         \"content_light\":null,\"scanout\":{\"active\":false,\"reason\":null}}"
    );
}

#[test]
fn an_active_hdr_monitor_reports_its_signal() {
    let hdr = OutputHdr {
        night_light: true,
        max_cll: Some(800),
        max_fall: None,
        scanout: true,
        ..hdr_on()
    };
    assert_eq!(
        hdr.to_json(),
        "{\"requested\":true,\"active\":true,\"reason\":null,\"eotf\":\"PQ\",\"peak\":1000,\
         \"peak_source\":\"edid\",\"sdr_white\":203,\"bits\":10,\"night_light\":true,\
         \"content_light\":{\"max_cll\":800,\"max_fall\":null},\
         \"scanout\":{\"active\":true,\"reason\":null}}"
    );
}

#[test]
fn a_requested_hdr_that_is_not_active_says_why() {
    let hdr = OutputHdr {
        requested: true,
        reason: Some("the EDID has no HDR static metadata block".to_string()),
        ..OutputHdr::default()
    };
    assert!(hdr.to_json().contains(
        "\"requested\":true,\"active\":false,\"reason\":\"the EDID has no HDR static metadata block\""
    ));
}

#[test]
fn hdr_and_sdr_white_changes_are_output_changes() {
    let old = snapshot(vec![]);
    let mut on = old.clone();
    on.outputs[0].hdr = hdr_on();
    assert_eq!(
        diff(&old, &on),
        vec![Event::OutputChanged(OutputInfo {
            hdr: hdr_on(),
            ..output("DP-1", 0)
        })]
    );
    let json = diff(&old, &on)[0].to_json();
    assert!(json.contains("\"hdr\":{\"requested\":true,\"active\":true"));

    let mut brighter = on.clone();
    brighter.outputs[0].hdr.sdr_white = 300;
    let events = diff(&on, &brighter);
    assert_eq!(names(&events), ["output-changed"]);
    assert!(events[0].to_json().contains("\"sdr_white\":300"));

    assert!(diff(&on, &on.clone()).is_empty());
    assert_eq!(names(&diff(&on, &old)), ["output-changed"]);
}

#[test]
fn hdr_is_in_the_outputs_snapshot() {
    let mut snap = snapshot(vec![]);
    snap.outputs[0].hdr = hdr_on();
    let json = snap.to_json(GroupSet::from_groups(&[Group::Outputs]));
    assert!(json.contains("\"hdr\":{\"requested\":true,\"active\":true,\"reason\":null"));
}
