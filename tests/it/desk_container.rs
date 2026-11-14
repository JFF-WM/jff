//! Tests for the tiling containers of `jff_wm::desk`.

use jff_wm::desk::{ContainerError, ContainerId, Desk, Side, WindowId, WindowMode};
use jff_wm::primitives::container_drop::LeafDrop;
use jff_wm::primitives::tile_tree::{Axis, Edge};

type TestDesk = Desk<&'static str>;

fn top_down(d: &TestDesk) -> Vec<&'static str> {
    d.top_down().map(|e| e.window).collect()
}

fn bottom_up(d: &mut TestDesk) -> Vec<&'static str> {
    d.iter_mut().map(|e| e.window).collect()
}

fn id_of(d: &TestDesk, window: &'static str) -> WindowId {
    d.find_by(|w| *w == window)
        .expect("the window is on the desk")
        .id
}

fn window_of(d: &TestDesk, id: WindowId) -> &'static str {
    d.find_by_id(id).expect("the id names a window").window
}

/// A desk with the windows added in this order.
fn desk_of(windows: &[&'static str]) -> TestDesk {
    let mut d = Desk::default();
    for w in windows {
        d.add(*w);
    }
    d
}

/// `desk_of(windows)` with a container started on `first`, and every other window of `members`
/// split off the one before it along `axis`.
fn container_of(
    windows: &[&'static str],
    members: &[&'static str],
    axis: Axis,
) -> (TestDesk, ContainerId) {
    let mut d = desk_of(windows);
    let first = members[0];
    let c = d
        .create_container(|w| *w == first, None)
        .expect("the container starts");
    for pair in members.windows(2) {
        let (at, new) = (pair[0], pair[1]);
        d.split_container(c, |w| *w == at, axis, |w| *w == new)
            .expect("the window joins");
    }
    (d, c)
}

fn leaves(d: &TestDesk, c: ContainerId) -> Vec<&'static str> {
    d.container(c)
        .expect("the container exists")
        .tree()
        .leaves()
        .into_iter()
        .map(|id| window_of(d, id))
        .collect()
}

#[test]
fn windows_get_distinct_ids_and_start_outside_any_container() {
    let d = desk_of(&["a", "b", "c"]);
    let ids = [id_of(&d, "a"), id_of(&d, "b"), id_of(&d, "c")];
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
    assert_ne!(ids[0], ids[2]);
    assert!(d.all_top_down().all(|e| e.container.is_none()));
    assert_eq!(d.containers().count(), 0);
}

#[test]
fn a_new_container_sits_where_its_window_is() {
    let mut d = Desk::default();
    d.set_current(2);
    d.add_on("a", "DP-1");
    let c = d
        .create_container(|w| *w == "a", Some("editor".to_owned()))
        .expect("the container starts");
    let container = d.container(c).expect("the container exists");
    assert_eq!(container.id(), c);
    assert_eq!(container.name(), Some("editor"));
    assert_eq!(container.workspace(), 2);
    assert_eq!(container.output(), "DP-1");
    assert_eq!(container.focused(), id_of(&d, "a"));
    assert_eq!(container.tree().leaves(), [id_of(&d, "a")]);
    assert_eq!(container.background(), None);
    assert_eq!(d.container_of(|w| *w == "a"), Some(c));
    assert_eq!(d.members(c).len(), 1);
}

#[test]
fn container_ids_are_unique_and_not_reused() {
    let mut d = desk_of(&["a", "b"]);
    let first = d.create_container(|w| *w == "a", None).expect("starts");
    let second = d.create_container(|w| *w == "b", None).expect("starts");
    assert_ne!(first, second);
    d.remove_where(|w| *w == "b");
    d.add("c");
    let third = d.create_container(|w| *w == "c", None).expect("starts");
    assert_ne!(third, first);
    assert_ne!(third, second);
}

#[test]
fn a_window_is_in_at_most_one_container() {
    let mut d = desk_of(&["a", "b"]);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    assert_eq!(
        d.create_container(|w| *w == "a", None),
        Err(ContainerError::AlreadyInContainer)
    );
    assert_eq!(
        d.create_container(|w| *w == "zzz", None),
        Err(ContainerError::NoSuchWindow)
    );
    let other = d.create_container(|w| *w == "b", None).expect("starts");
    // "b" is in a container, so it cannot join this one.
    assert_eq!(
        d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b"),
        Err(ContainerError::AlreadyInContainer)
    );
    assert_eq!(d.container_of(|w| *w == "b"), Some(other));
    assert_eq!(leaves(&d, c), ["a"]);
}

#[test]
fn split_errors_leave_everything_as_it_was() {
    let mut d = desk_of(&["a", "b", "c"]);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    let before = bottom_up(&mut d);
    assert_eq!(
        d.split_container(c, |w| *w == "b", Axis::Vertical, |w| *w == "c"),
        Err(ContainerError::NotInContainer)
    );
    assert_eq!(
        d.split_container(c, |w| *w == "zzz", Axis::Vertical, |w| *w == "c"),
        Err(ContainerError::NoSuchWindow)
    );
    assert_eq!(
        d.split_container(c, |w| *w == "a", Axis::Vertical, |w| *w == "zzz"),
        Err(ContainerError::NoSuchWindow)
    );
    // A window cannot be split off itself.
    assert_eq!(
        d.split_container(c, |w| *w == "a", Axis::Vertical, |w| *w == "a"),
        Err(ContainerError::AlreadyInContainer)
    );
    assert_eq!(leaves(&d, c), ["a"]);
    assert_eq!(bottom_up(&mut d), before);
    assert_eq!(d.container_of(|w| *w == "c"), None);

    d.remove_where(|w| *w == "a");
    assert_eq!(
        d.split_container(c, |w| *w == "b", Axis::Vertical, |w| *w == "c"),
        Err(ContainerError::NoSuchContainer)
    );
}

#[test]
fn members_sit_together_in_tree_order() {
    // "b" starts under the windows that stay out of the container.
    let (mut d, c) = container_of(&["b", "x", "a", "y"], &["a", "b"], Axis::Horizontal);
    assert_eq!(leaves(&d, c), ["a", "b"]);
    assert_eq!(bottom_up(&mut d), ["x", "a", "b", "y"]);
    assert_eq!(top_down(&d), ["y", "b", "a", "x"]);
    let members: Vec<_> = d.members(c).iter().map(|e| e.window).collect();
    assert_eq!(members, ["a", "b"]);
}

#[test]
fn a_split_in_the_middle_keeps_the_order_of_the_tree() {
    // a | b, then c is split off a: the tree order is a, c, b.
    let mut d = desk_of(&["a", "b", "c"]);
    let k = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(k, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    d.split_container(k, |w| *w == "a", Axis::Vertical, |w| *w == "c")
        .expect("joins");
    assert_eq!(leaves(&d, k), ["a", "c", "b"]);
    assert_eq!(bottom_up(&mut d), ["a", "c", "b"]);
}

#[test]
fn a_joining_window_takes_the_workspace_and_monitor_of_the_container() {
    let mut d = Desk::default();
    d.add_on("a", "DP-1");
    d.set_current(1);
    d.add_on("b", "DP-2");
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    for window in ["a", "b"] {
        let e = d.find_by(|w| *w == window).expect("on the desk");
        assert_eq!(e.workspace, 0, "{window}");
        assert_eq!(e.output, "DP-1", "{window}");
    }
    assert_eq!(d.count_on(0), 2);
    assert_eq!(d.count_on(1), 0);
}

#[test]
fn tiling_derives_the_position_from_the_tree() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    let x_before = d.pos_of(|w| *w == "x");
    let rects = d.tile_container(c, (10, 20, 100, 50), 2);
    let a = id_of(&d, "a");
    let b = id_of(&d, "b");
    // 98 px to share, 49 each, and the gap between them.
    assert_eq!(rects, [(a, (10, 20, 49, 50)), (b, (61, 20, 49, 50))]);
    assert_eq!(d.pos_of(|w| *w == "a"), Some((10, 20)));
    assert_eq!(d.pos_of(|w| *w == "b"), Some((61, 20)));
    // A window outside is not touched.
    assert_eq!(d.pos_of(|w| *w == "x"), x_before);
}

#[test]
fn tiling_follows_the_tree_after_a_change() {
    let (mut d, c) = container_of(&["a", "b", "c"], &["a", "b", "c"], Axis::Horizontal);
    // a | (b | c)
    d.tile_container(c, (0, 0, 100, 50), 0);
    assert_eq!(d.pos_of(|w| *w == "b"), Some((50, 0)));
    assert_eq!(d.pos_of(|w| *w == "c"), Some((75, 0)));
    assert!(d.leave_container(|w| *w == "b"));
    d.tile_container(c, (0, 0, 100, 50), 0);
    assert_eq!(d.pos_of(|w| *w == "a"), Some((0, 0)));
    assert_eq!(d.pos_of(|w| *w == "c"), Some((50, 0)));
}

#[test]
fn tiling_an_unknown_container_does_nothing() {
    let (mut d, c) = container_of(&["a"], &["a"], Axis::Horizontal);
    d.remove_where(|w| *w == "a");
    assert!(d.tile_container(c, (0, 0, 100, 50), 0).is_empty());
}

#[test]
fn a_member_does_not_own_its_position() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, (0, 0, 100, 50), 0);
    d.set_pos_where(|w| *w == "b", (900, 900));
    assert_eq!(d.pos_of(|w| *w == "b"), Some((50, 0)));
    // Anything else still moves.
    d.set_pos_where(|w| *w == "x", (500, 300));
    assert_eq!(d.pos_of(|w| *w == "x"), Some((500, 300)));
}

#[test]
fn raising_a_member_raises_the_whole_container() {
    let (mut d, c) = container_of(&["x", "a", "b", "y"], &["a", "b"], Axis::Horizontal);
    d.raise_where(|w| *w == "a");
    assert_eq!(bottom_up(&mut d), ["x", "y", "a", "b"]);
    d.raise_where(|w| *w == "x");
    assert_eq!(bottom_up(&mut d), ["y", "a", "b", "x"]);
    d.raise_where(|w| *w == "b");
    assert_eq!(bottom_up(&mut d), ["y", "x", "a", "b"]);
    assert_eq!(leaves(&d, c), ["a", "b"]);
}

#[test]
fn leaving_a_container_closes_up_the_tree() {
    let (mut d, c) = container_of(&["a", "b", "c", "x"], &["a", "b", "c"], Axis::Vertical);
    assert!(d.leave_container(|w| *w == "b"));
    assert_eq!(leaves(&d, c), ["a", "c"]);
    assert_eq!(d.container_of(|w| *w == "b"), None);
    // The window is still on the desk, on top, and the others are still together.
    assert_eq!(bottom_up(&mut d), ["a", "c", "x", "b"]);
    // It was not in a container any more, so there is nothing to leave.
    assert!(!d.leave_container(|w| *w == "b"));
    assert!(!d.leave_container(|w| *w == "x"));
    assert!(!d.leave_container(|w| *w == "zzz"));
    assert_eq!(bottom_up(&mut d), ["a", "c", "x", "b"]);
}

#[test]
fn leaving_keeps_the_mode_of_the_window() {
    let mut d = desk_of(&["a", "b"]);
    d.set_mode_where(|w| *w == "a", WindowMode::Maximized, (0, 0), (300, 200));
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    assert!(d.leave_container(|w| *w == "a"));
    assert!(d.is_maximized(|w| *w == "a"));
}

#[test]
fn the_focus_moves_to_a_neighbour_when_its_window_leaves() {
    // a | (b / c)
    let mut d = desk_of(&["a", "b", "c"]);
    let k = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(k, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    d.split_container(k, |w| *w == "b", Axis::Vertical, |w| *w == "c")
        .expect("joins");
    // The focus stayed where it was while the others joined.
    assert_eq!(d.container(k).map(|c| c.focused()), Some(id_of(&d, "a")));

    assert!(d.set_container_focus(|w| *w == "c"));
    assert_eq!(d.container(k).map(|c| c.focused()), Some(id_of(&d, "c")));
    // c was the second half of (b / c): b takes its place, and the focus.
    assert!(d.leave_container(|w| *w == "c"));
    assert_eq!(d.container(k).map(|c| c.focused()), Some(id_of(&d, "b")));
    // A window that is not the focused one leaves, the focus stays.
    assert!(d.leave_container(|w| *w == "a"));
    assert_eq!(d.container(k).map(|c| c.focused()), Some(id_of(&d, "b")));

    assert!(!d.set_container_focus(|w| *w == "c"));
    assert!(!d.set_container_focus(|w| *w == "zzz"));
}

#[test]
fn the_container_goes_with_its_last_window() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    assert!(d.leave_container(|w| *w == "a"));
    assert!(d.container(c).is_some());
    assert!(d.leave_container(|w| *w == "b"));
    assert!(d.container(c).is_none());
    assert_eq!(d.containers().count(), 0);
    // Both are still there, outside any container.
    assert_eq!(d.len(), 2);
    assert!(d.all_top_down().all(|e| e.container.is_none()));
}

#[test]
fn removing_a_window_closes_up_its_container() {
    let (mut d, c) = container_of(&["a", "b", "c", "x"], &["a", "b", "c"], Axis::Horizontal);
    d.remove_where(|w| *w == "b");
    assert_eq!(leaves(&d, c), ["a", "c"]);
    assert_eq!(bottom_up(&mut d), ["a", "c", "x"]);
    d.remove_where(|w| *w == "x");
    assert_eq!(leaves(&d, c), ["a", "c"]);
    d.remove_where(|w| *w == "a" || *w == "c");
    assert!(d.container(c).is_none());
    assert!(d.is_empty());
}

#[test]
fn removing_the_focused_window_moves_the_focus() {
    let (mut d, c) = container_of(&["a", "b", "c"], &["a", "b", "c"], Axis::Horizontal);
    assert!(d.set_container_focus(|w| *w == "b"));
    // Both leaves of (b | c) go at once, and so does the focus with them.
    d.remove_where(|w| *w == "b" || *w == "c");
    assert_eq!(leaves(&d, c), ["a"]);
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "a")));
}

#[test]
fn a_member_takes_its_container_to_another_workspace() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    assert!(d.move_to_workspace(|w| *w == "a", 2));
    assert_eq!(d.container(c).map(|k| k.workspace()), Some(2));
    assert_eq!(d.count_on(2), 2);
    assert_eq!(d.count_on(0), 1);
    assert_eq!(d.workspace_of(|w| *w == "b"), Some(2));
    assert_eq!(d.workspace_of(|w| *w == "x"), Some(0));
    // On top of the workspace it went to, still together and in order.
    assert_eq!(bottom_up(&mut d), ["x", "a", "b"]);
    assert!(!d.move_to_workspace(|w| *w == "zzz", 1));
}

#[test]
fn a_container_moves_by_itself() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    assert!(d.move_container_to_workspace(c, 3));
    assert_eq!(d.workspace_of(|w| *w == "a"), Some(3));
    assert_eq!(d.workspace_of(|w| *w == "b"), Some(3));
    d.remove_where(|w| *w == "a" || *w == "b");
    assert!(!d.move_container_to_workspace(c, 1));
}

#[test]
fn a_member_that_leaves_can_be_sent_alone() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    assert!(d.leave_container(|w| *w == "b"));
    assert!(d.move_to_workspace(|w| *w == "b", 1));
    assert_eq!(d.workspace_of(|w| *w == "b"), Some(1));
    assert_eq!(d.workspace_of(|w| *w == "a"), Some(0));
    assert_eq!(d.container(c).map(|k| k.workspace()), Some(0));
}

#[test]
fn a_member_takes_its_container_to_another_monitor() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    d.set_per_output(true, "");
    d.set_current_on("HDMI-1", 3);
    assert!(d.set_output_where(|w| *w == "b", "HDMI-1"));
    let container = d.container(c).expect("the container exists");
    assert_eq!(container.output(), "HDMI-1");
    assert_eq!(container.workspace(), 3);
    for window in ["a", "b"] {
        let e = d.find_by(|w| *w == window).expect("on the desk");
        assert_eq!(e.output, "HDMI-1", "{window}");
        assert_eq!(e.workspace, 3, "{window}");
    }
    let x = d.find_by(|w| *w == "x").expect("on the desk");
    assert_eq!(x.output, "");
    assert_eq!(x.workspace, 0);
}

#[test]
fn assigning_outputs_moves_a_container_as_one() {
    let mut d = Desk::default();
    d.set_per_output(true, "DP-1");
    d.set_current_on("DP-2", 4);
    d.add_on("a", "DP-1");
    d.add_on("b", "DP-1");
    d.add_on("x", "DP-1");
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");

    let mut asked = Vec::new();
    d.assign_outputs(|e| {
        asked.push(e.window);
        Some("DP-2".to_owned())
    });
    // One question for the container, about its first window, and one for the other window.
    asked.sort_unstable();
    assert_eq!(asked, ["a", "x"]);
    let container = d.container(c).expect("the container exists");
    assert_eq!(container.output(), "DP-2");
    assert_eq!(container.workspace(), 4);
    for window in ["a", "b", "x"] {
        let e = d.find_by(|w| *w == window).expect("on the desk");
        assert_eq!(e.output, "DP-2", "{window}");
        assert_eq!(e.workspace, 4, "{window}");
    }
}

#[test]
fn assigning_the_same_output_changes_nothing() {
    let mut d = Desk::default();
    d.set_per_output(true, "DP-1");
    d.add_on("a", "DP-1");
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.assign_outputs(|_| Some("DP-1".to_owned()));
    d.assign_outputs(|_| None);
    assert_eq!(
        d.container(c).map(|k| k.output().to_owned()),
        Some("DP-1".to_owned())
    );
}

#[test]
fn fewer_workspaces_pull_a_container_in_with_its_members() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    assert!(d.move_container_to_workspace(c, 5));
    d.clamp_workspaces(3);
    assert_eq!(d.container(c).map(|k| k.workspace()), Some(2));
    assert_eq!(d.workspace_of(|w| *w == "a"), Some(2));
    assert_eq!(d.workspace_of(|w| *w == "b"), Some(2));
}

#[test]
fn only_the_windows_of_the_shown_workspace_are_walked() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    assert!(d.move_container_to_workspace(c, 1));
    assert_eq!(top_down(&d), ["x"]);
    d.set_current(1);
    assert_eq!(top_down(&d), ["b", "a"]);
    assert_eq!(d.all_top_down().count(), 3);
}

#[test]
fn a_container_can_be_named_and_renamed() {
    let (mut d, c) = container_of(&["a"], &["a"], Axis::Horizontal);
    assert_eq!(d.container(c).and_then(|k| k.name()), None);
    assert!(d.set_container_name(c, Some("mail".to_owned())));
    assert_eq!(d.container(c).and_then(|k| k.name()), Some("mail"));
    assert!(d.set_container_name(c, None));
    assert_eq!(d.container(c).and_then(|k| k.name()), None);
    d.remove_where(|w| *w == "a");
    assert!(!d.set_container_name(c, Some("gone".to_owned())));
}

#[test]
fn several_containers_and_loose_windows_share_a_workspace() {
    let mut d = desk_of(&["a", "b", "c", "d", "x", "y"]);
    let one = d.create_container(|w| *w == "a", None).expect("starts");
    let two = d.create_container(|w| *w == "c", None).expect("starts");
    d.split_container(one, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    d.split_container(two, |w| *w == "c", Axis::Vertical, |w| *w == "d")
        .expect("joins");
    assert_eq!(leaves(&d, one), ["a", "b"]);
    assert_eq!(leaves(&d, two), ["c", "d"]);
    assert_eq!(d.container_of(|w| *w == "x"), None);
    assert_eq!(d.containers().count(), 2);
    // Each container is one block, whatever is around it.
    let order = bottom_up(&mut d);
    let at = |window: &str| {
        order
            .iter()
            .position(|w| *w == window)
            .expect("in the stack")
    };
    assert_eq!(at("b"), at("a") + 1);
    assert_eq!(at("d"), at("c") + 1);
    // Raising one leaves the other alone.
    d.raise_where(|w| *w == "a");
    let order = bottom_up(&mut d);
    assert_eq!(&order[order.len() - 2..], ["a", "b"]);
    let at = |window: &str| {
        order
            .iter()
            .position(|w| *w == window)
            .expect("in the stack")
    };
    assert_eq!(at("d"), at("c") + 1);
}

/// `desk_of(windows)` with `snapped` snapped to `side` and a container started beside it for
/// `dragged`.
fn container_beside(
    windows: &[&'static str],
    snapped: &'static str,
    dragged: &'static str,
    side: Side,
) -> (TestDesk, ContainerId) {
    let mut d = desk_of(windows);
    assert!(d.set_mode_where(
        |w| *w == snapped,
        WindowMode::Snapped(side),
        (0, 0),
        (50, 50)
    ));
    let c = d
        .create_container_beside(|w| *w == snapped, |w| *w == dragged, side, None)
        .expect("the container starts");
    (d, c)
}

#[test]
fn a_container_beside_a_left_snap_puts_the_snapped_window_on_the_left() {
    let (mut d, c) = container_beside(&["a", "b"], "a", "b", Side::Left);
    assert_eq!(leaves(&d, c), ["a", "b"]);
    let (a, b) = (id_of(&d, "a"), id_of(&d, "b"));
    let rects = d.tile_container(c, (0, 0, 100, 50), 0);
    assert_eq!(rects, [(a, (0, 0, 50, 50)), (b, (50, 0, 50, 50))]);
}

#[test]
fn a_container_beside_a_right_snap_leaves_the_snapped_window_on_the_right() {
    let (mut d, c) = container_beside(&["a", "b"], "a", "b", Side::Right);
    assert_eq!(leaves(&d, c), ["b", "a"]);
    let (a, b) = (id_of(&d, "a"), id_of(&d, "b"));
    let rects = d.tile_container(c, (0, 0, 100, 50), 0);
    assert_eq!(rects, [(b, (0, 0, 50, 50)), (a, (50, 0, 50, 50))]);
}

#[test]
fn a_container_beside_a_top_or_bottom_snap_splits_one_above_the_other() {
    let (mut d, c) = container_beside(&["a", "b"], "a", "b", Side::Top);
    assert_eq!(leaves(&d, c), ["a", "b"]);
    let (a, b) = (id_of(&d, "a"), id_of(&d, "b"));
    assert_eq!(
        d.tile_container(c, (0, 0, 50, 100), 0),
        [(a, (0, 0, 50, 50)), (b, (0, 50, 50, 50))]
    );

    let (mut d, c) = container_beside(&["a", "b"], "a", "b", Side::Bottom);
    assert_eq!(leaves(&d, c), ["b", "a"]);
    let (a, b) = (id_of(&d, "a"), id_of(&d, "b"));
    assert_eq!(
        d.tile_container(c, (0, 0, 50, 100), 0),
        [(b, (0, 0, 50, 50)), (a, (0, 50, 50, 50))]
    );
}

#[test]
fn a_container_beside_a_snap_sits_where_the_snapped_window_is() {
    let mut d = Desk::default();
    d.add_on("a", "DP-1");
    d.set_current(1);
    d.add_on("b", "DP-2");
    // Either order of the two leaves, the container is at the snapped window.
    for side in [Side::Left, Side::Right] {
        let mut d2 = Desk::default();
        d2.add_on("a", "DP-1");
        d2.set_current(1);
        d2.add_on("b", "DP-2");
        let c = d2
            .create_container_beside(|w| *w == "a", |w| *w == "b", side, None)
            .expect("starts");
        let container = d2.container(c).expect("exists");
        assert_eq!((container.workspace(), container.output()), (0, "DP-1"));
        for window in ["a", "b"] {
            let e = d2.find_by(|w| *w == window).expect("on the desk");
            assert_eq!((e.workspace, e.output.as_str()), (0, "DP-1"), "{window}");
        }
        assert_eq!(d2.count_on(0), 2);
        assert_eq!(d2.count_on(1), 0);
    }
    assert_eq!(d.containers().count(), 0);
}

#[test]
fn a_container_beside_a_snap_is_named_and_focused_on_the_dragged_window() {
    let mut d = desk_of(&["a", "b"]);
    let c = d
        .create_container_beside(
            |w| *w == "a",
            |w| *w == "b",
            Side::Left,
            Some("term".to_owned()),
        )
        .expect("starts");
    let container = d.container(c).expect("exists");
    assert_eq!(container.name(), Some("term"));
    assert_eq!(container.focused(), id_of(&d, "b"));
    assert_eq!(container.background(), None);
    assert_eq!(d.container_of(|w| *w == "a"), Some(c));
    assert_eq!(d.container_of(|w| *w == "b"), Some(c));

    let (d, c) = container_beside(&["a", "b"], "a", "b", Side::Right);
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "b")));
    assert_eq!(d.container(c).and_then(|k| k.name()), None);
}

#[test]
fn a_container_beside_a_snap_keeps_the_modes_for_the_caller_to_decide() {
    let (d, _) = container_beside(&["a", "b"], "a", "b", Side::Left);
    assert_eq!(d.mode_of(|w| *w == "a"), WindowMode::Snapped(Side::Left));
    assert_eq!(d.mode_of(|w| *w == "b"), WindowMode::Floating);
}

#[test]
fn a_container_beside_a_snap_keeps_its_windows_together_in_the_stack() {
    let mut d = desk_of(&["a", "x", "y", "b", "z"]);
    let c = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Right, None)
        .expect("starts");
    assert_eq!(leaves(&d, c), ["b", "a"]);
    let order = bottom_up(&mut d);
    let at = |window: &str| {
        order
            .iter()
            .position(|w| *w == window)
            .expect("in the stack")
    };
    assert_eq!(at("a"), at("b") + 1);
    assert_eq!(d.container_of(|w| *w == "x"), None);
}

#[test]
fn a_container_beside_a_snap_that_cannot_start_changes_nothing() {
    let mut d = desk_of(&["a", "b", "c"]);
    let before = bottom_up(&mut d);
    let start = |d: &mut TestDesk, snapped: &'static str, dragged: &'static str| {
        d.create_container_beside(|w| *w == snapped, |w| *w == dragged, Side::Left, None)
    };
    assert_eq!(start(&mut d, "zz", "b"), Err(ContainerError::NoSuchWindow));
    assert_eq!(start(&mut d, "a", "zz"), Err(ContainerError::NoSuchWindow));
    assert_eq!(start(&mut d, "a", "a"), Err(ContainerError::SameWindow));
    assert_eq!(d.containers().count(), 0);
    assert!(d.all_top_down().all(|e| e.container.is_none()));
    assert_eq!(bottom_up(&mut d), before);

    // A member of a container, on either side, is in one already.
    let one = d.create_container(|w| *w == "a", None).expect("starts");
    assert_eq!(
        start(&mut d, "a", "b"),
        Err(ContainerError::AlreadyInContainer)
    );
    assert_eq!(
        start(&mut d, "b", "a"),
        Err(ContainerError::AlreadyInContainer)
    );
    assert_eq!(d.containers().count(), 1);
    assert_eq!(d.container_of(|w| *w == "b"), None);
    assert_eq!(leaves(&d, one), ["a"]);

    // The failed tries used no container id.
    let next = d.create_container(|w| *w == "b", None).expect("starts");
    assert_eq!(next.get(), one.get() + 1);
}

#[test]
fn a_container_beside_a_snap_does_not_disturb_the_others_on_the_workspace() {
    let mut d = desk_of(&["a", "b", "c", "d", "x"]);
    let one = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Left, None)
        .expect("starts");
    let two = d
        .create_container_beside(|w| *w == "c", |w| *w == "d", Side::Top, None)
        .expect("starts");
    assert_ne!(one, two);
    assert_eq!(leaves(&d, one), ["a", "b"]);
    assert_eq!(leaves(&d, two), ["c", "d"]);
    assert_eq!(d.container_of(|w| *w == "x"), None);
    assert_eq!(d.containers().count(), 2);
}

#[test]
fn a_container_has_no_area_until_it_is_laid_out() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    assert_eq!(d.container(c).and_then(|k| k.area()), None);
    d.tile_container(c, (10, 20, 100, 50), 2);
    assert_eq!(
        d.container(c).and_then(|k| k.area()),
        Some((10, 20, 100, 50))
    );
    // the next layout replaces it
    d.tile_container(c, (0, 0, 640, 480), 0);
    assert_eq!(
        d.container(c).and_then(|k| k.area()),
        Some((0, 0, 640, 480))
    );
}

#[test]
fn laying_out_one_container_leaves_the_area_of_another() {
    let mut d = desk_of(&["a", "b", "c", "d"]);
    let one = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Left, None)
        .expect("starts");
    let two = d
        .create_container_beside(|w| *w == "c", |w| *w == "d", Side::Left, None)
        .expect("starts");
    d.tile_container(one, (0, 0, 100, 50), 0);
    assert_eq!(
        d.container(one).and_then(|k| k.area()),
        Some((0, 0, 100, 50))
    );
    assert_eq!(d.container(two).and_then(|k| k.area()), None);
    // a container that is not there has none to set
    d.remove_where(|w| *w == "a");
    d.remove_where(|w| *w == "b");
    assert!(d.tile_container(one, (5, 5, 5, 5), 0).is_empty());
}

#[test]
fn stepping_goes_to_the_next_or_previous_leaf_in_tree_order_and_wraps() {
    let (mut d, c) = container_of(&["a", "b", "c", "x"], &["a", "b", "c"], Axis::Horizontal);
    assert_eq!(leaves(&d, c), ["a", "b", "c"]);
    let step = |d: &TestDesk, from: &'static str, forward: bool| {
        d.step_in_container(|w| *w == from, forward)
            .map(|e| e.window)
    };
    assert_eq!(step(&d, "a", true), Some("b"));
    assert_eq!(step(&d, "b", true), Some("c"));
    assert_eq!(step(&d, "c", true), Some("a"));
    assert_eq!(step(&d, "a", false), Some("c"));
    assert_eq!(step(&d, "c", false), Some("b"));
    // stepping changes nothing
    assert_eq!(bottom_up(&mut d), ["a", "b", "c", "x"]);
}

#[test]
fn stepping_follows_the_tree_not_the_stack() {
    // a | (b / c), then the stack is changed around
    let mut d = desk_of(&["a", "b", "c"]);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    d.split_container(c, |w| *w == "b", Axis::Vertical, |w| *w == "c")
        .expect("joins");
    d.raise_where(|w| *w == "b");
    let step = |from: &'static str| d.step_in_container(|w| *w == from, true).map(|e| e.window);
    assert_eq!(step("a"), Some("b"));
    assert_eq!(step("b"), Some("c"));
    assert_eq!(step("c"), Some("a"));
}

#[test]
fn stepping_in_a_container_of_one_stays_and_outside_one_is_nothing() {
    let (d, _) = container_of(&["a", "x"], &["a"], Axis::Horizontal);
    assert_eq!(
        d.step_in_container(|w| *w == "a", true).map(|e| e.window),
        Some("a")
    );
    assert_eq!(
        d.step_in_container(|w| *w == "a", false).map(|e| e.window),
        Some("a")
    );
    assert!(d.step_in_container(|w| *w == "x", true).is_none());
    assert!(d.step_in_container(|w| *w == "zzz", true).is_none());
}

#[test]
fn a_container_is_found_by_name_on_a_workspace() {
    let mut d = desk_of(&["a", "b", "c", "d"]);
    let one = d
        .create_container(|w| *w == "a", Some("mail".into()))
        .expect("starts");
    let two = d
        .create_container(|w| *w == "b", Some("mail".into()))
        .expect("starts");
    let three = d
        .create_container(|w| *w == "c", Some("web".into()))
        .expect("starts");
    let four = d.create_container(|w| *w == "d", None).expect("starts");
    // the first one of that name, oldest first
    assert_eq!(d.container_named(0, "mail"), Some(one));
    assert_eq!(d.container_named(0, "web"), Some(three));
    assert_eq!(d.container_named(0, "nope"), None);
    assert_eq!(d.container_named(0, "Mail"), None);
    // only on the workspace asked about
    assert!(d.move_container_to_workspace(one, 1));
    assert_eq!(d.container_named(0, "mail"), Some(two));
    assert_eq!(d.container_named(1, "mail"), Some(one));
    assert_eq!(d.container_named(2, "mail"), None);
    // one without a name is never found by one
    assert_eq!(d.container_named(0, ""), None);
    let ids = |ws: usize| {
        d.containers_on(ws)
            .iter()
            .map(|c| c.id())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(0), [two, three, four]);
    assert_eq!(ids(1), [one]);
    assert!(ids(2).is_empty());
}

#[test]
fn a_maximized_member_fills_the_area_and_the_others_keep_their_rectangles() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    let area = (0, 0, 1000, 500);
    d.tile_container(c, area, 0);
    assert!(!d.is_maximized(|w| *w == "a"));
    assert_eq!(d.zoom_in_container(|w| *w == "a", true), Ok(()));
    assert!(d.is_maximized(|w| *w == "a"));
    assert!(!d.is_maximized(|w| *w == "b"));
    assert_eq!(
        d.container(c).and_then(|k| k.zoomed()),
        Some(id_of(&d, "a"))
    );
    let tiles = d.tile_container(c, area, 0);
    assert_eq!(tiles.len(), 2);
    assert_eq!(tiles[0], (id_of(&d, "a"), area));
    assert_eq!(tiles[1], (id_of(&d, "b"), (500, 0, 500, 500)));
    // the container and its tree are as they were, and its area is the one given
    assert_eq!(leaves(&d, c), ["a", "b"]);
    assert_eq!(d.container(c).and_then(|k| k.area()), Some(area));
    assert_eq!(d.pos_of(|w| *w == "a"), Some((0, 0)));
}

#[test]
fn the_maximized_member_is_on_top_of_the_others() {
    let (mut d, c) = container_of(&["a", "b", "c", "x"], &["a", "b", "c"], Axis::Horizontal);
    assert_eq!(bottom_up(&mut d), ["a", "b", "c", "x"]);
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    assert_eq!(bottom_up(&mut d), ["b", "c", "a", "x"]);
    // raising the container keeps it so
    d.raise_where(|w| *w == "x");
    d.raise_where(|w| *w == "b");
    assert_eq!(bottom_up(&mut d), ["x", "b", "c", "a"]);
    // putting it back puts the tree's order back
    d.zoom_in_container(|w| *w == "a", false)
        .expect("puts back");
    assert_eq!(bottom_up(&mut d), ["x", "a", "b", "c"]);
    assert_eq!(d.container(c).and_then(|k| k.zoomed()), None);
}

#[test]
fn only_one_member_is_maximized_at_a_time() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    assert!(!d.is_maximized(|w| *w == "a"));
    assert!(d.is_maximized(|w| *w == "b"));
    // putting back one that is not maximized changes nothing
    d.zoom_in_container(|w| *w == "a", false).expect("nothing");
    assert!(d.is_maximized(|w| *w == "b"));
    d.zoom_in_container(|w| *w == "b", false)
        .expect("puts back");
    assert_eq!(d.container(c).and_then(|k| k.zoomed()), None);
}

#[test]
fn maximizing_does_not_dissolve_the_container() {
    let (mut d, c) = container_of(&["a", "b", "c"], &["a", "b", "c"], Axis::Vertical);
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    assert_eq!(d.container_of(|w| *w == "a"), Some(c));
    assert_eq!(d.container_of(|w| *w == "b"), Some(c));
    assert_eq!(d.container_of(|w| *w == "c"), Some(c));
    assert_eq!(d.members(c).len(), 3);
    // a member does not get a mode of its own for it
    assert_eq!(d.mode_of(|w| *w == "b"), WindowMode::Floating);
}

#[test]
fn maximizing_needs_a_member() {
    let mut d = desk_of(&["a", "x"]);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    assert_eq!(
        d.zoom_in_container(|w| *w == "x", true),
        Err(ContainerError::NotInContainer)
    );
    assert_eq!(
        d.zoom_in_container(|w| *w == "zzz", true),
        Err(ContainerError::NoSuchWindow)
    );
    assert_eq!(d.container(c).and_then(|k| k.zoomed()), None);
    assert!(!d.is_maximized(|w| *w == "x"));
}

#[test]
fn a_loose_window_is_maximized_by_its_mode_as_before() {
    let mut d = desk_of(&["a", "x"]);
    d.create_container(|w| *w == "a", None).expect("starts");
    d.set_mode_where(|w| *w == "x", WindowMode::Maximized, (0, 0), (300, 200));
    assert!(d.is_maximized(|w| *w == "x"));
    assert!(!d.is_maximized(|w| *w == "a"));
    assert!(!d.is_maximized(|w| *w == "zzz"));
}

#[test]
fn closing_the_maximized_member_shows_the_others_again() {
    let (mut d, c) = container_of(&["a", "b", "c"], &["a", "b", "c"], Axis::Horizontal);
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    d.remove_where(|w| *w == "b");
    assert_eq!(leaves(&d, c), ["a", "c"]);
    assert_eq!(d.container(c).and_then(|k| k.zoomed()), None);
    assert!(!d.is_maximized(|w| *w == "a"));
    assert!(!d.is_maximized(|w| *w == "c"));
}

#[test]
fn closing_another_member_keeps_the_maximized_one() {
    let (mut d, c) = container_of(&["a", "b", "c"], &["a", "b", "c"], Axis::Horizontal);
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    d.remove_where(|w| *w == "c");
    assert_eq!(leaves(&d, c), ["a", "b"]);
    assert!(d.is_maximized(|w| *w == "b"));
}

#[test]
fn a_member_that_leaves_is_no_longer_maximized() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    assert!(d.leave_container(|w| *w == "a"));
    assert_eq!(d.container(c).and_then(|k| k.zoomed()), None);
    assert!(!d.is_maximized(|w| *w == "a"));
}

#[test]
fn closing_a_leaf_collapses_its_split_and_the_others_take_the_room() {
    // a | (b / c): closing c leaves a | b, each half of the area
    let mut d = desk_of(&["a", "b", "c"]);
    let k = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(k, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    d.split_container(k, |w| *w == "b", Axis::Vertical, |w| *w == "c")
        .expect("joins");
    let area = (0, 0, 1000, 600);
    let before = d.tile_container(k, area, 0);
    assert_eq!(before[2].1, (500, 300, 500, 300));
    d.remove_where(|w| *w == "c");
    let after = d.tile_container(k, area, 0);
    assert_eq!(
        after,
        [
            (id_of(&d, "a"), (0, 0, 500, 600)),
            (id_of(&d, "b"), (500, 0, 500, 600))
        ]
    );
}

#[test]
fn closing_the_last_leaf_removes_the_container() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.remove_where(|w| *w == "a");
    assert!(d.container(c).is_some());
    d.remove_where(|w| *w == "b");
    assert!(d.container(c).is_none());
    assert_eq!(d.containers().count(), 0);
    assert!(d.containers_on(0).is_empty());
}

#[test]
fn a_fullscreen_member_keeps_the_monitor_and_stays_on_top_of_its_container() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, (0, 0, 1000, 500), 0);
    d.set_mode_where(|w| *w == "a", WindowMode::Fullscreen, (0, 0), (500, 500));
    d.raise_where(|w| *w == "a");
    assert_eq!(bottom_up(&mut d), ["x", "b", "a"]);
    // the next layout does not move it back to its place in the tree
    d.set_mode_where(
        |w| *w == "b",
        WindowMode::Fullscreen,
        (-1920, 0),
        (1920, 1080),
    );
    d.tile_container(c, (0, 0, 1000, 500), 0);
    assert_eq!(d.pos_of(|w| *w == "b"), Some((-1920, 0)));
    assert_eq!(d.pos_of(|w| *w == "a"), Some((0, 0)));
    assert_eq!(leaves(&d, c), ["a", "b"]);
}

fn width_of(d: &TestDesk, tiles: &[(WindowId, (i32, i32, i32, i32))], w: &'static str) -> i32 {
    tiles
        .iter()
        .find(|(id, _)| *id == id_of(d, w))
        .map(|(_, rect)| rect.2)
        .expect("laid out")
}

#[test]
fn dragging_the_shared_edge_moves_both_windows() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    let area = (0, 0, 1000, 500);
    d.tile_container(c, area, 0);
    assert_eq!(
        d.resize_container(|w| *w == "a", Edge::Right, 700, 0, 80),
        Ok(Some(0.7))
    );
    let tiles = d.tile_container(c, area, 0);
    assert_eq!(width_of(&d, &tiles, "a"), 700);
    assert_eq!(width_of(&d, &tiles, "b"), 300);
    assert_eq!(d.pos_of(|w| *w == "b"), Some((700, 0)));
    // the same divider dragged from the other window
    assert_eq!(
        d.resize_container(|w| *w == "b", Edge::Left, 400, 0, 80),
        Ok(Some(0.4))
    );
    let tiles = d.tile_container(c, area, 0);
    assert_eq!(width_of(&d, &tiles, "a"), 400);
    assert_eq!(width_of(&d, &tiles, "b"), 600);
}

#[test]
fn a_dragged_edge_leaves_room_for_the_other_window() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    let area = (0, 0, 1000, 500);
    d.tile_container(c, area, 0);
    d.resize_container(|w| *w == "a", Edge::Right, 5000, 0, 80)
        .expect("a member");
    let tiles = d.tile_container(c, area, 0);
    assert_eq!(width_of(&d, &tiles, "b"), 80);
    d.resize_container(|w| *w == "a", Edge::Right, -5000, 0, 80)
        .expect("a member");
    let tiles = d.tile_container(c, area, 0);
    assert_eq!(width_of(&d, &tiles, "a"), 80);
}

#[test]
fn the_outer_edges_of_a_container_do_not_move() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, (0, 0, 1000, 500), 0);
    assert_eq!(
        d.resize_container(|w| *w == "a", Edge::Left, 100, 0, 80),
        Ok(None)
    );
    assert_eq!(
        d.resize_container(|w| *w == "a", Edge::Top, 100, 0, 80),
        Ok(None)
    );
    assert_eq!(
        d.resize_container(|w| *w == "b", Edge::Right, 900, 0, 80),
        Ok(None)
    );
    assert_eq!(
        d.resize_container(|w| *w == "b", Edge::Bottom, 400, 0, 80),
        Ok(None)
    );
}

#[test]
fn resizing_needs_a_laid_out_member() {
    let mut d = desk_of(&["a", "b", "x"]);
    let c = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Left, None)
        .expect("starts");
    // not laid out yet: nothing to measure against
    assert_eq!(
        d.resize_container(|w| *w == "a", Edge::Right, 300, 0, 80),
        Ok(None)
    );
    d.tile_container(c, (0, 0, 1000, 500), 0);
    assert_eq!(
        d.resize_container(|w| *w == "x", Edge::Right, 300, 0, 80),
        Err(ContainerError::NotInContainer)
    );
    assert_eq!(
        d.resize_container(|w| *w == "zzz", Edge::Right, 300, 0, 80),
        Err(ContainerError::NoSuchWindow)
    );
}

#[test]
fn a_nested_divider_moves_only_its_own_pair() {
    // a | (b / c): the vertical divider between b and c leaves a alone
    let mut d = desk_of(&["a", "b", "c"]);
    let k = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(k, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("joins");
    d.split_container(k, |w| *w == "b", Axis::Vertical, |w| *w == "c")
        .expect("joins");
    let area = (0, 0, 1000, 600);
    d.tile_container(k, area, 0);
    assert_eq!(
        d.resize_container(|w| *w == "b", Edge::Bottom, 450, 0, 80),
        Ok(Some(0.75))
    );
    let tiles = d.tile_container(k, area, 0);
    assert_eq!(tiles[0].1, (0, 0, 500, 600));
    assert_eq!(tiles[1].1, (500, 0, 500, 450));
    assert_eq!(tiles[2].1, (500, 450, 500, 150));
    // b's left edge is the divider with a
    assert_eq!(
        d.resize_container(|w| *w == "c", Edge::Left, 300, 0, 80),
        Ok(Some(0.3))
    );
    let tiles = d.tile_container(k, area, 0);
    assert_eq!(tiles[0].1, (0, 0, 300, 600));
    assert_eq!(tiles[1].1, (300, 0, 700, 450));
    assert_eq!(tiles[2].1, (300, 450, 700, 150));
}

#[test]
fn the_gap_stays_between_the_halves_of_a_dragged_edge() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    let area = (0, 0, 1000, 500);
    d.tile_container(c, area, 10);
    d.resize_container(|w| *w == "a", Edge::Right, 600, 10, 80)
        .expect("a member");
    let tiles = d.tile_container(c, area, 10);
    assert_eq!(tiles[0].1, (0, 0, 600, 500));
    assert_eq!(tiles[1].1, (610, 0, 390, 500));
}

/// A desk with a container of `members` (each split off the one before it) laid out in `area`
/// with no gap, and the loose `others` on top of it.
fn laid_out(
    members: &[&'static str],
    others: &[&'static str],
    axis: Axis,
    area: (i32, i32, i32, i32),
) -> (TestDesk, ContainerId) {
    let all: Vec<&'static str> = members.iter().chain(others).copied().collect();
    let (mut d, c) = container_of(&all, members, axis);
    d.tile_container(c, area, 0);
    (d, c)
}

fn rect_of(
    d: &mut TestDesk,
    c: ContainerId,
    area: (i32, i32, i32, i32),
    window: &'static str,
) -> (i32, i32, i32, i32) {
    let id = id_of(d, window);
    d.tile_container(c, area, 0)
        .into_iter()
        .find(|(w, _)| *w == id)
        .expect("the window is laid out")
        .1
}

#[test]
fn a_window_joins_by_splitting_the_focused_leaf_in_its_second_half() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    assert_eq!(d.join_container(c, None, |w| *w == "n", 0), Ok(()), "joins");
    // The focus inside the container was on a, the first member: n is split off it.
    assert_eq!(leaves(&d, c), ["a", "n", "b"]);
    assert_eq!(d.container_of(|w| *w == "n"), Some(c));
    assert_eq!(d.members(c).len(), 3);
}

#[test]
fn a_joining_window_is_cut_along_the_longer_side_of_the_leaf() {
    // One wide leaf: side by side, the new window on the right.
    let wide = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a"], &["n"], Axis::Horizontal, wide);
    d.join_container(c, None, |w| *w == "n", 0).expect("joins");
    assert_eq!(rect_of(&mut d, c, wide, "a"), (0, 0, 500, 400));
    assert_eq!(rect_of(&mut d, c, wide, "n"), (500, 0, 500, 400));

    // One tall leaf: one above the other, the new window below.
    let tall = (0, 0, 400, 1000);
    let (mut d, c) = laid_out(&["a"], &["n"], Axis::Horizontal, tall);
    d.join_container(c, None, |w| *w == "n", 0).expect("joins");
    assert_eq!(rect_of(&mut d, c, tall, "a"), (0, 0, 400, 500));
    assert_eq!(rect_of(&mut d, c, tall, "n"), (0, 500, 400, 500));
}

#[test]
fn the_axis_is_taken_from_the_leaf_and_not_from_the_container() {
    // Two windows side by side, 500x1000 each: both tall, so a joiner is stacked on the leaf.
    let area = (0, 0, 1000, 1000);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    let b = id_of(&d, "b");
    d.join_container(c, Some(b), |w| *w == "n", 0)
        .expect("joins");
    assert_eq!(rect_of(&mut d, c, area, "b"), (500, 0, 500, 500));
    assert_eq!(rect_of(&mut d, c, area, "n"), (500, 500, 500, 500));
    assert_eq!(rect_of(&mut d, c, area, "a"), (0, 0, 500, 1000));
}

#[test]
fn a_leaf_given_by_the_caller_is_the_one_that_is_split() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    let b = id_of(&d, "b");
    d.join_container(c, Some(b), |w| *w == "n", 0)
        .expect("joins");
    assert_eq!(leaves(&d, c), ["a", "b", "n"]);
    assert_eq!(rect_of(&mut d, c, area, "a"), (0, 0, 500, 400));
}

#[test]
fn the_joined_window_is_focused_in_its_container() {
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, (0, 0, 1000, 400));
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "a")));
    d.join_container(c, None, |w| *w == "n", 0).expect("joins");
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "n")));
}

#[test]
fn a_joined_window_does_not_float_snap_or_maximize() {
    for mode in [
        WindowMode::Floating,
        WindowMode::Maximized,
        WindowMode::Snapped(Side::Left),
    ] {
        let (mut d, c) = laid_out(&["a"], &["n"], Axis::Horizontal, (0, 0, 1000, 400));
        if mode != WindowMode::Floating {
            d.set_mode_where(|w| *w == "n", mode, (0, 0), (300, 200));
        }
        d.join_container(c, None, |w| *w == "n", 0).expect("joins");
        let entry = d.find_by(|w| *w == "n").expect("on the desk");
        assert_eq!(entry.mode, WindowMode::Floating, "{mode:?}");
        assert_eq!(entry.before_fullscreen, WindowMode::Floating, "{mode:?}");
        assert_eq!(entry.restore, None, "{mode:?}");
        assert!(!d.is_maximized(|w| *w == "n"), "{mode:?}");
    }
}

#[test]
fn a_joined_window_goes_where_the_container_is_and_stays_with_its_members() {
    let mut d = Desk::default();
    for w in ["a", "x", "b", "y"] {
        d.add_on(w, "DP-1");
    }
    d.set_current(1);
    d.add_on("n", "DP-2");
    d.set_current(0);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("splits");
    d.tile_container(c, (0, 0, 1000, 400), 0);
    d.join_container(c, None, |w| *w == "n", 0).expect("joins");
    let n = d.find_by(|w| *w == "n").expect("on the desk");
    assert_eq!((n.workspace, n.output.as_str()), (0, "DP-1"));
    let order = bottom_up(&mut d);
    let at = |window: &str| order.iter().position(|w| *w == window).expect("in stack");
    assert_eq!(at("n"), at("a") + 1);
    assert_eq!(at("b"), at("n") + 1);
    assert_eq!(d.container_of(|w| *w == "x"), None);
    assert_eq!(d.container_of(|w| *w == "y"), None);
}

#[test]
fn a_joining_window_becomes_the_maximized_one_when_a_member_is_maximized() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    let b = id_of(&d, "b");
    d.join_container(c, Some(b), |w| *w == "n", 0)
        .expect("joins");
    let n = id_of(&d, "n");
    assert_eq!(d.container(c).and_then(|k| k.zoomed()), Some(n));
    assert!(d.is_maximized(|w| *w == "n"));
    assert!(!d.is_maximized(|w| *w == "b"));
    // The tree keeps b where it was, n fills the container and is on top.
    assert_eq!(leaves(&d, c), ["a", "b", "n"]);
    let tiles = d.tile_container(c, area, 0);
    assert_eq!(tiles.iter().find(|(w, _)| *w == n).map(|t| t.1), Some(area));
    assert_eq!(bottom_up(&mut d).last(), Some(&"n"));
}

#[test]
fn joining_a_maximized_container_cuts_the_leaf_the_tree_gives_not_the_full_area() {
    // b is maximized and fills 1000x400, but its place in the tree is 500x400: cut it side by
    // side, the same as when nothing is maximized.
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    d.tile_container(c, area, 0);
    let b = id_of(&d, "b");
    d.join_container(c, Some(b), |w| *w == "n", 0)
        .expect("joins");
    d.zoom_in_container(|w| *w == "n", false)
        .expect("puts back");
    assert_eq!(rect_of(&mut d, c, area, "b"), (500, 0, 250, 400));
    assert_eq!(rect_of(&mut d, c, area, "n"), (750, 0, 250, 400));
}

#[test]
fn a_container_that_was_never_laid_out_is_cut_side_by_side() {
    let (mut d, c) = container_of(&["a", "n"], &["a"], Axis::Horizontal);
    assert_eq!(d.container(c).and_then(|k| k.area()), None);
    d.join_container(c, None, |w| *w == "n", 0).expect("joins");
    assert_eq!(leaves(&d, c), ["a", "n"]);
    let tiles = d.tile_container(c, (0, 0, 1000, 1000), 0);
    assert_eq!(tiles[1].1, (500, 0, 500, 1000));
}

#[test]
fn a_join_that_cannot_happen_changes_nothing() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n", "m"], Axis::Horizontal, area);
    let other = d.create_container(|w| *w == "m", None).expect("starts");
    let join = |d: &mut TestDesk, c: ContainerId, at: Option<WindowId>, new: &'static str| {
        d.join_container(c, at, |w| *w == new, 0)
    };
    assert_eq!(
        join(&mut d, c, None, "zz"),
        Err(ContainerError::NoSuchWindow)
    );
    assert_eq!(
        join(&mut d, c, None, "m"),
        Err(ContainerError::AlreadyInContainer)
    );
    assert_eq!(
        join(&mut d, c, None, "a"),
        Err(ContainerError::AlreadyInContainer)
    );
    // A leaf of another container is not one of this one.
    let m = id_of(&d, "m");
    assert_eq!(
        join(&mut d, c, Some(m), "n"),
        Err(ContainerError::NotInContainer)
    );
    assert_eq!(
        join(&mut d, other, None, "zz"),
        Err(ContainerError::NoSuchWindow)
    );
    d.remove_where(|w| *w == "m");
    assert_eq!(
        join(&mut d, other, None, "n"),
        Err(ContainerError::NoSuchContainer)
    );
    assert_eq!(leaves(&d, c), ["a", "b"]);
    assert_eq!(d.container_of(|w| *w == "n"), None);
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "a")));
}

#[test]
fn a_new_window_joins_the_container_the_focus_is_in() {
    let (d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, (0, 0, 1000, 400));
    assert_eq!(d.container_to_join(|w| *w == "b", |w| *w == "n"), Some(c));
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), Some(c));
}

#[test]
fn a_new_window_does_not_join_when_the_focus_is_in_no_container() {
    let (d, _) = laid_out(
        &["a", "b"],
        &["n", "x"],
        Axis::Horizontal,
        (0, 0, 1000, 400),
    );
    assert_eq!(d.container_to_join(|w| *w == "x", |w| *w == "n"), None);
    assert_eq!(d.container_to_join(|w| *w == "zz", |w| *w == "n"), None);
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "zz"), None);
}

#[test]
fn a_member_never_joins_a_container_as_a_new_window() {
    let (d, _) = laid_out(&["a", "b"], &[], Axis::Horizontal, (0, 0, 1000, 400));
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "b"), None);
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "a"), None);
}

#[test]
fn a_new_window_on_another_workspace_or_monitor_does_not_join() {
    let mut d = Desk::default();
    d.set_per_output(true, "DP-1");
    d.add_on("a", "DP-1");
    d.add_on("b", "DP-1");
    d.add_on("far", "DP-2");
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("splits");
    d.tile_container(c, (0, 0, 1000, 400), 0);
    d.add_on("n", "DP-1");
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), Some(c));
    // Another monitor.
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "far"), None);
    // Another workspace on the same monitor.
    d.set_current_on("DP-1", 1);
    d.add_on("later", "DP-1");
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "later"), None);
}

#[test]
fn a_new_window_does_not_join_a_container_that_is_not_shown() {
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, (0, 0, 1000, 400));
    d.set_current(1);
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), None);
    d.set_current(0);
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), Some(c));
}

#[test]
fn a_new_window_does_not_join_a_container_that_was_never_laid_out() {
    let (d, _) = container_of(&["a", "n"], &["a"], Axis::Horizontal);
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), None);
}

#[test]
fn a_new_window_does_not_join_when_the_focused_member_is_fullscreen() {
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, (0, 0, 1000, 400));
    d.set_mode_where(|w| *w == "b", WindowMode::Fullscreen, (0, 0), (1000, 400));
    assert_eq!(d.container_to_join(|w| *w == "b", |w| *w == "n"), None);
    // The keyboard is on a member that is not fullscreen, but the container's focus is.
    d.set_container_focus(|w| *w == "b");
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), None);
    // Back from fullscreen it joins again.
    d.float_in_place_where(|w| *w == "b");
    assert_eq!(d.container_to_join(|w| *w == "b", |w| *w == "n"), Some(c));
}

#[test]
fn a_maximized_but_not_fullscreen_member_still_takes_a_new_window() {
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, (0, 0, 1000, 400));
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    assert_eq!(d.container_to_join(|w| *w == "a", |w| *w == "n"), Some(c));
}

#[test]
fn the_container_under_a_point_is_found_when_it_is_shown_and_laid_out() {
    let (mut d, c) = laid_out(&["a", "b"], &["x"], Axis::Horizontal, (100, 50, 1000, 400));
    assert_eq!(d.container_at((100.0, 50.0)), Some(c));
    assert_eq!(d.container_at((1099.9, 449.9)), Some(c));
    assert_eq!(d.container_at((1100.0, 100.0)), None);
    assert_eq!(d.container_at((99.9, 100.0)), None);
    assert_eq!(d.container_at((500.0, 450.0)), None);
    d.set_current(1);
    assert_eq!(d.container_at((500.0, 100.0)), None);
}

#[test]
fn a_container_that_was_never_laid_out_is_under_no_point() {
    let (d, _) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    assert_eq!(d.container_at((0.0, 0.0)), None);
    assert_eq!(d.container_at((500.0, 500.0)), None);
}

#[test]
fn of_two_containers_over_each_other_the_topmost_is_under_the_point() {
    let mut d = desk_of(&["a", "b", "c", "d"]);
    let one = d.create_container(|w| *w == "a", None).expect("starts");
    let two = d.create_container(|w| *w == "c", None).expect("starts");
    d.split_container(one, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("splits");
    d.split_container(two, |w| *w == "c", Axis::Horizontal, |w| *w == "d")
        .expect("splits");
    let area = (0, 0, 1000, 400);
    d.tile_container(one, area, 0);
    d.tile_container(two, area, 0);
    assert_eq!(d.container_at((10.0, 10.0)), Some(two));
    d.raise_where(|w| *w == "a");
    assert_eq!(d.container_at((10.0, 10.0)), Some(one));
}

#[test]
fn the_leaf_under_a_point_is_the_member_whose_rectangle_holds_it() {
    let area = (0, 0, 1000, 400);
    let (d, c) = laid_out(&["a", "b"], &[], Axis::Horizontal, area);
    let (a, b) = (id_of(&d, "a"), id_of(&d, "b"));
    assert_eq!(d.leaf_at(c, (10.0, 10.0), 0), Some(a));
    assert_eq!(d.leaf_at(c, (499.9, 300.0), 0), Some(a));
    assert_eq!(d.leaf_at(c, (500.0, 300.0), 0), Some(b));
    assert_eq!(d.leaf_at(c, (990.0, 390.0), 0), Some(b));
}

#[test]
fn a_point_in_a_gap_or_outside_gives_the_focused_leaf() {
    let area = (0, 0, 1000, 400);
    let mut d = desk_of(&["a", "b"]);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("splits");
    d.tile_container(c, area, 20);
    let a = id_of(&d, "a");
    // a is 0..490, the gap 490..510, b 510..1000.
    assert_eq!(d.leaf_at(c, (500.0, 100.0), 20), Some(a));
    assert_eq!(d.leaf_at(c, (-50.0, 100.0), 20), Some(a));
    d.set_container_focus(|w| *w == "b");
    assert_eq!(d.leaf_at(c, (500.0, 100.0), 20), Some(id_of(&d, "b")));
}

#[test]
fn the_leaf_under_a_point_of_a_maximized_container_is_the_maximized_one() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &[], Axis::Horizontal, area);
    d.zoom_in_container(|w| *w == "b", true).expect("zooms");
    assert_eq!(d.leaf_at(c, (10.0, 10.0), 0), Some(id_of(&d, "b")));
}

#[test]
fn a_container_that_is_gone_has_no_leaf() {
    let (mut d, c) = laid_out(&["a"], &[], Axis::Horizontal, (0, 0, 1000, 400));
    d.remove_where(|w| *w == "a");
    assert_eq!(d.leaf_at(c, (10.0, 10.0), 0), None);
}

/// What the move grab does at the drop, in the desk's terms: the container under the pointer,
/// the leaf under it, the join, and the layout again.
fn drop_on(
    d: &mut TestDesk,
    window: &'static str,
    at: (f64, f64),
    area: (i32, i32, i32, i32),
) -> Option<ContainerId> {
    let c = d.container_at(at)?;
    let leaf = d.leaf_at(c, at, 0)?;
    d.join_container(c, Some(leaf), |w| *w == window, 0).ok()?;
    d.tile_container(c, area, 0);
    Some(c)
}

#[test]
fn a_loose_window_dropped_over_a_leaf_is_split_off_that_leaf() {
    let area = (0, 0, 1000, 800);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    // The container is 500x800 per leaf, tall: the leaf under the pointer is cut one above the
    // other, the dropped window in its lower half.
    assert_eq!(drop_on(&mut d, "n", (700.0, 100.0), area), Some(c));
    assert_eq!(leaves(&d, c), ["a", "b", "n"]);
    assert_eq!(rect_of(&mut d, c, area, "a"), (0, 0, 500, 800));
    assert_eq!(rect_of(&mut d, c, area, "b"), (500, 0, 500, 400));
    assert_eq!(rect_of(&mut d, c, area, "n"), (500, 400, 500, 400));
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "n")));
    assert_eq!(d.container_of(|w| *w == "n"), Some(c));
}

#[test]
fn a_maximized_window_dropped_over_a_container_joins_it_and_is_put_back() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    d.set_mode_where(|w| *w == "n", WindowMode::Maximized, (0, 0), (800, 300));
    assert_eq!(drop_on(&mut d, "n", (100.0, 100.0), area), Some(c));
    assert_eq!(leaves(&d, c), ["a", "n", "b"]);
    assert_eq!(d.mode_of(|w| *w == "n"), WindowMode::Floating);
    assert!(!d.is_maximized(|w| *w == "n"));
    assert_eq!(d.find_by(|w| *w == "n").map(|e| e.restore), Some(None));
}

#[test]
fn a_window_dropped_outside_every_container_does_not_join() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    assert_eq!(drop_on(&mut d, "n", (1500.0, 100.0), area), None);
    assert_eq!(d.container_of(|w| *w == "n"), None);
    assert_eq!(d.members(c).len(), 2);
}

#[test]
fn a_member_dropped_over_another_container_is_not_taken_from_its_own() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &[], Axis::Horizontal, area);
    assert_eq!(drop_on(&mut d, "a", (700.0, 100.0), area), None);
    assert_eq!(leaves(&d, c), ["a", "b"]);
}

#[test]
fn a_window_dropped_over_a_maximized_container_joins_as_the_maximized_one() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    assert_eq!(drop_on(&mut d, "n", (900.0, 100.0), area), Some(c));
    // The pointer was over what is shown, a, and n is split off it.
    assert_eq!(leaves(&d, c), ["a", "n", "b"]);
    assert_eq!(
        d.container(c).and_then(|k| k.zoomed()),
        Some(id_of(&d, "n"))
    );
}

#[test]
fn closing_the_joined_window_collapses_its_split_again() {
    let area = (0, 0, 1000, 400);
    let (mut d, c) = laid_out(&["a", "b"], &["n"], Axis::Horizontal, area);
    d.join_container(c, None, |w| *w == "n", 0).expect("joins");
    d.remove_where(|w| *w == "n");
    assert_eq!(leaves(&d, c), ["a", "b"]);
    assert_eq!(rect_of(&mut d, c, area, "a"), (0, 0, 500, 400));
    assert_eq!(rect_of(&mut d, c, area, "b"), (500, 0, 500, 400));
}

/// The area the moves below lay their containers out in.
fn pair_area() -> (i32, i32, i32, i32) {
    (0, 0, 1000, 500)
}

fn move_to(
    d: &mut TestDesk,
    c: ContainerId,
    window: &'static str,
    target: &'static str,
    drop: LeafDrop,
) -> Result<(), ContainerError> {
    let target = id_of(d, target);
    let result = d.move_member(|w| *w == window, c, target, drop, 0);
    d.tile_container(c, pair_area(), 0);
    result
}

#[test]
fn a_member_dropped_on_the_middle_of_another_swaps_places_with_it() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b", "x"], Axis::Horizontal);
    d.tile_container(c, pair_area(), 0);
    assert_eq!(leaves(&d, c), ["a", "b", "x"]);
    assert_eq!(move_to(&mut d, c, "a", "x", LeafDrop::Swap), Ok(()));
    assert_eq!(leaves(&d, c), ["x", "b", "a"]);
    assert_eq!(d.container(c).map(|k| k.focused()), Some(id_of(&d, "a")));
}

#[test]
fn a_member_dropped_on_an_edge_goes_beside_the_leaf_on_that_side() {
    let area = pair_area();
    // a | b: put a on the right of b.
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, area, 0);
    assert_eq!(
        move_to(&mut d, c, "a", "b", LeafDrop::Beside(Side::Right)),
        Ok(())
    );
    assert_eq!(leaves(&d, c), ["b", "a"]);
    assert_eq!(rect_of(&mut d, c, area, "b"), (0, 0, 500, 500));
    assert_eq!(rect_of(&mut d, c, area, "a"), (500, 0, 500, 500));

    // On top of it: the dragged window is the first half of a vertical split.
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b", "x"], Axis::Horizontal);
    d.tile_container(c, area, 0);
    assert_eq!(
        move_to(&mut d, c, "x", "a", LeafDrop::Beside(Side::Top)),
        Ok(())
    );
    assert_eq!(leaves(&d, c), ["x", "a", "b"]);
    // a | (b | x) with x gone from the right: (x / a) | b.
    assert_eq!(rect_of(&mut d, c, area, "x"), (0, 0, 500, 250));
    assert_eq!(rect_of(&mut d, c, area, "a"), (0, 250, 500, 250));
    assert_eq!(rect_of(&mut d, c, area, "b"), (500, 0, 500, 500));

    // Under it, and on its left.
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b", "x"], Axis::Horizontal);
    d.tile_container(c, area, 0);
    assert_eq!(
        move_to(&mut d, c, "x", "a", LeafDrop::Beside(Side::Bottom)),
        Ok(())
    );
    assert_eq!(leaves(&d, c), ["a", "x", "b"]);
    assert_eq!(
        move_to(&mut d, c, "b", "a", LeafDrop::Beside(Side::Left)),
        Ok(())
    );
    assert_eq!(leaves(&d, c), ["b", "a", "x"]);
}

#[test]
fn moving_a_member_inside_its_container_keeps_every_window_in_it() {
    let (mut d, c) = container_of(&["a", "b", "c", "n"], &["a", "b", "c"], Axis::Horizontal);
    d.tile_container(c, pair_area(), 0);
    for (window, target, drop) in [
        ("a", "c", LeafDrop::Beside(Side::Left)),
        ("b", "a", LeafDrop::Beside(Side::Bottom)),
        ("c", "b", LeafDrop::Swap),
        ("a", "c", LeafDrop::Beside(Side::Top)),
    ] {
        assert_eq!(move_to(&mut d, c, window, target, drop), Ok(()));
        let mut now = leaves(&d, c);
        now.sort_unstable();
        assert_eq!(now, ["a", "b", "c"]);
        for w in ["a", "b", "c"] {
            assert_eq!(d.container_of(|x| *x == w), Some(c));
        }
    }
    assert_eq!(d.container_of(|w| *w == "n"), None);
}

#[test]
fn a_member_moved_inside_its_container_stays_with_the_others_in_the_stack() {
    let mut d = desk_of(&["a", "b", "c", "x"]);
    let c = d.create_container(|w| *w == "a", None).expect("starts");
    d.split_container(c, |w| *w == "a", Axis::Horizontal, |w| *w == "b")
        .expect("splits");
    d.split_container(c, |w| *w == "b", Axis::Horizontal, |w| *w == "c")
        .expect("splits");
    d.tile_container(c, pair_area(), 0);
    assert_eq!(move_to(&mut d, c, "a", "c", LeafDrop::Swap), Ok(()));
    assert_eq!(leaves(&d, c), ["c", "b", "a"]);
    // The stack follows the tree, bottom up.
    assert_eq!(bottom_up(&mut d), ["c", "b", "a", "x"]);
}

#[test]
fn a_member_dropped_on_itself_changes_nothing() {
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, pair_area(), 0);
    let focus = d.container(c).map(|k| k.focused());
    for drop in [LeafDrop::Swap, LeafDrop::Beside(Side::Left)] {
        assert_eq!(move_to(&mut d, c, "a", "a", drop), Ok(()));
        assert_eq!(leaves(&d, c), ["a", "b"]);
        assert_eq!(d.container(c).map(|k| k.focused()), focus);
    }
}

#[test]
fn the_maximized_member_that_is_moved_stays_maximized() {
    let (mut d, c) = container_of(&["a", "b", "x"], &["a", "b", "x"], Axis::Horizontal);
    d.tile_container(c, pair_area(), 0);
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    assert_eq!(move_to(&mut d, c, "a", "x", LeafDrop::Swap), Ok(()));
    assert_eq!(
        d.container(c).and_then(|k| k.zoomed()),
        Some(id_of(&d, "a"))
    );
    assert_eq!(leaves(&d, c), ["x", "b", "a"]);
}

#[test]
fn a_member_dropped_on_another_container_leaves_its_own_and_joins_it() {
    let mut d = desk_of(&["a", "b", "c", "d"]);
    let one = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Left, None)
        .expect("starts");
    let two = d
        .create_container_beside(|w| *w == "c", |w| *w == "d", Side::Left, None)
        .expect("starts");
    let area = pair_area();
    d.tile_container(one, area, 0);
    d.tile_container(two, area, 0);
    let c = id_of(&d, "c");
    // The middle: split off c along its longer side. It is 500x500, so side by side.
    assert_eq!(
        d.move_member(|w| *w == "a", two, c, LeafDrop::Swap, 0),
        Ok(())
    );
    assert_eq!(d.container_of(|w| *w == "a"), Some(two));
    assert_eq!(leaves(&d, two), ["c", "a", "d"]);
    assert_eq!(leaves(&d, one), ["b"]);
    assert_eq!(d.container(two).map(|k| k.focused()), Some(id_of(&d, "a")));
    assert_eq!(d.mode_of(|w| *w == "a"), WindowMode::Floating);

    // An edge: beside, on that side. The last member of a container takes it along.
    let d_id = id_of(&d, "d");
    assert_eq!(
        d.move_member(|w| *w == "b", two, d_id, LeafDrop::Beside(Side::Left), 0),
        Ok(())
    );
    assert_eq!(leaves(&d, two), ["c", "a", "b", "d"]);
    assert_eq!(d.container(one).map(|k| k.id()), None);
    assert_eq!(d.containers().count(), 1);
}

#[test]
fn a_member_moved_to_another_container_goes_where_that_one_is() {
    let mut d = Desk::default();
    for w in ["a", "b", "c", "d"] {
        d.add_on(w, "DP-1");
    }
    d.set_current(1);
    d.add_on("e", "DP-2");
    d.add_on("f", "DP-2");
    d.set_current(0);
    let one = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Left, None)
        .expect("starts");
    d.set_current(1);
    let two = d
        .create_container_beside(|w| *w == "e", |w| *w == "f", Side::Left, None)
        .expect("starts");
    d.set_current(0);
    let e = id_of(&d, "e");
    assert_eq!(
        d.move_member(|w| *w == "a", two, e, LeafDrop::Swap, 0),
        Ok(())
    );
    let a = d.find_by(|w| *w == "a").expect("on the desk");
    assert_eq!(
        a.workspace,
        d.container(two).map(|k| k.workspace()).unwrap_or(99)
    );
    assert_eq!(a.output, "DP-2");
    assert_eq!(d.container(one).map(|k| k.id()), Some(one));
}

#[test]
fn a_member_that_cannot_be_moved_changes_nothing() {
    let mut d = desk_of(&["a", "b", "x", "y"]);
    let one = d
        .create_container_beside(|w| *w == "a", |w| *w == "b", Side::Left, None)
        .expect("starts");
    let two = d.create_container(|w| *w == "y", None).expect("starts");
    d.tile_container(one, pair_area(), 0);
    let (a, b, y, x) = (
        id_of(&d, "a"),
        id_of(&d, "b"),
        id_of(&d, "y"),
        id_of(&d, "x"),
    );
    let swap = LeafDrop::Swap;
    // A window in no container joins with `join_container`.
    assert_eq!(
        d.move_member(|w| *w == "x", one, a, swap, 0),
        Err(ContainerError::NotInContainer)
    );
    assert_eq!(
        d.move_member(|w| *w == "zz", one, a, swap, 0),
        Err(ContainerError::NoSuchWindow)
    );
    // The target is not in the container, or not on the desk.
    assert_eq!(
        d.move_member(|w| *w == "a", one, y, swap, 0),
        Err(ContainerError::NotInContainer)
    );
    assert_eq!(
        d.move_member(|w| *w == "a", one, x, swap, 0),
        Err(ContainerError::NotInContainer)
    );
    d.remove_where(|w| *w == "y");
    assert_eq!(
        d.move_member(|w| *w == "a", one, y, swap, 0),
        Err(ContainerError::NoSuchWindow)
    );
    assert_eq!(
        d.move_member(|w| *w == "a", two, b, swap, 0),
        Err(ContainerError::NoSuchContainer)
    );
    assert_eq!(leaves(&d, one), ["a", "b"]);
    assert_eq!(d.container_of(|w| *w == "x"), None);
}

#[test]
fn the_drop_target_is_the_leaf_under_the_point_and_its_rectangle() {
    let area = pair_area();
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, area, 0);
    let (a, b) = (id_of(&d, "a"), id_of(&d, "b"));
    assert_eq!(
        d.drop_target(c, (10.0, 10.0), 0),
        Some((a, (0, 0, 500, 500)))
    );
    assert_eq!(
        d.drop_target(c, (700.0, 400.0), 0),
        Some((b, (500, 0, 500, 500)))
    );
    assert_eq!(d.drop_target(c, (1200.0, 400.0), 0), None);
    d.tile_container(c, area, 40);
    assert_eq!(d.drop_target(c, (500.0, 100.0), 40), None);
}

#[test]
fn the_drop_target_of_a_maximized_container_is_the_maximized_member_and_the_whole_area() {
    let area = pair_area();
    let (mut d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    d.tile_container(c, area, 0);
    d.zoom_in_container(|w| *w == "a", true).expect("zooms");
    let a = id_of(&d, "a");
    assert_eq!(d.drop_target(c, (900.0, 400.0), 0), Some((a, area)));
    assert_eq!(d.drop_target(c, (1200.0, 400.0), 0), None);
}

#[test]
fn a_container_that_was_never_laid_out_has_no_drop_target() {
    let (d, c) = container_of(&["a", "b"], &["a", "b"], Axis::Horizontal);
    assert_eq!(d.drop_target(c, (10.0, 10.0), 0), None);
}

// ---- apply_container_layout ----

mod apply_layout {
    use super::*;
    use jff_wm::desk::LayoutApplied;
    use jff_wm::primitives::layout_template::Template;

    fn tags(
        tags: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&&'static str) -> Option<String> {
        move |w| {
            tags.iter()
                .find(|(window, _)| window == w)
                .map(|(_, tag)| (*tag).to_owned())
        }
    }

    #[test]
    fn a_tagged_window_takes_the_slot_with_its_tag() {
        // `k1` and `k2` are two terminals, `ff` the browser, split in three: k1 | k2 | ff.
        let (mut d, c) = container_of(&["k1", "k2", "ff"], &["k1", "k2", "ff"], Axis::Horizontal);
        let template = Template::parse("h0.7(@browser,_)").unwrap();

        let applied = d
            .apply_container_layout(c, &template, tags(&[("ff", "browser")]), 0)
            .unwrap();

        // The browser is on the left, in the 70% slot, and the terminals share the rest.
        assert_eq!(leaves(&d, c)[0], "ff");
        assert_eq!(
            applied,
            LayoutApplied {
                slots: 2,
                empty: 0,
                extra: 1
            }
        );
        assert_eq!(leaves(&d, c).len(), 3);
    }

    #[test]
    fn the_ratios_of_the_layout_are_the_ones_the_tree_has_after() {
        let (mut d, c) = container_of(&["k1", "k2"], &["k1", "k2"], Axis::Horizontal);
        let template = Template::parse("h0.7(_,_)").unwrap();
        d.apply_container_layout(c, &template, tags(&[]), 0)
            .unwrap();
        let tiles = d.tile_container(c, (0, 0, 1000, 500), 0);
        let width = |w: &'static str| {
            let id = id_of(&d, w);
            tiles.iter().find(|(t, _)| *t == id).unwrap().1 .2
        };
        assert_eq!(width("k1"), 700);
        assert_eq!(width("k2"), 300);
    }

    #[test]
    fn applying_again_keeps_the_places_of_windows_no_tag_decides() {
        let (mut d, c) = container_of(&["k1", "k2"], &["k1", "k2"], Axis::Horizontal);
        let template = Template::parse("h0.7(_,_)").unwrap();
        // The user swapped the two.
        let k2 = id_of(&d, "k2");
        d.move_member(|w| *w == "k1", c, k2, LeafDrop::Swap, 0)
            .unwrap();
        assert_eq!(leaves(&d, c), vec!["k2", "k1"]);
        d.apply_container_layout(c, &template, tags(&[]), 0)
            .unwrap();
        assert_eq!(leaves(&d, c), vec!["k2", "k1"]);
    }

    #[test]
    fn slots_with_no_window_are_closed_up() {
        let (mut d, c) = container_of(&["k1", "k2"], &["k1", "k2"], Axis::Horizontal);
        let template = Template::parse("h0.5(_,v0.5(@browser,v0.5(_,_)))").unwrap();
        let applied = d
            .apply_container_layout(c, &template, tags(&[]), 0)
            .unwrap();
        // Four slots, two windows: nothing is lost, the two windows are in the container.
        assert_eq!(applied.slots, 4);
        assert_eq!(applied.empty, 2);
        assert_eq!(leaves(&d, c).len(), 2);
    }

    #[test]
    fn the_members_and_the_focus_are_the_same_after() {
        let (mut d, c) = container_of(&["k1", "k2", "ff"], &["k1", "k2", "ff"], Axis::Horizontal);
        let focused = d.container(c).unwrap().focused();
        let template = Template::parse("h0.5(@browser,_)").unwrap();
        d.apply_container_layout(c, &template, tags(&[("ff", "browser")]), 0)
            .unwrap();
        let mut now = leaves(&d, c);
        now.sort_unstable();
        assert_eq!(now, vec!["ff", "k1", "k2"]);
        assert_eq!(d.container(c).unwrap().focused(), focused);
    }

    #[test]
    fn the_stack_follows_the_new_tree_order() {
        let (mut d, c) = container_of(&["k1", "k2", "ff"], &["k1", "k2", "ff"], Axis::Horizontal);
        let template = Template::parse("h0.5(@browser,_)").unwrap();
        d.apply_container_layout(c, &template, tags(&[("ff", "browser")]), 0)
            .unwrap();
        let order = leaves(&d, c);
        assert_eq!(bottom_up(&mut d), order);
    }

    #[test]
    fn a_container_that_is_gone_is_an_error_and_changes_nothing() {
        let (mut d, c) = container_of(&["k1"], &["k1"], Axis::Horizontal);
        d.leave_container(|w| *w == "k1");
        let template = Template::parse("_").unwrap();
        assert_eq!(
            d.apply_container_layout(c, &template, tags(&[]), 0),
            Err(ContainerError::NoSuchContainer)
        );
    }
}
